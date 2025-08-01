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
    let executable = compile_file(&db, file).map_err(|e| format!("Compilation failed: {e}"))?;

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

// Tests for i32 overflow wrapping behavior
#[test]
fn test_i32_addition_overflow() {
    let source = r#"
        fn main() {
            let x: i32 = 2147483647;  // i32::MAX
            let one: i32 = 1;
            let result: i32 = x + one;
            println_i32(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "-2147483648"); // Should wrap to i32::MIN
}

#[test]
fn test_i32_subtraction_overflow() {
    let source = r#"
        fn main() {
            let x: i32 = 0 - 2147483647;
            let x: i32 = x - 1;  // i32::MIN
            let one: i32 = 1;
            let result: i32 = x - one;
            println_i32(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "2147483647"); // Should wrap to i32::MAX
}

#[test]
fn test_i32_multiplication_overflow() {
    let source = r#"
        fn main() {
            let x: i32 = 1000000;
            let y: i32 = 3000;
            let result: i32 = x * y;
            println_i32(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // 1000000 * 3000 = 3000000000 which overflows i32
    // In two's complement: 3000000000 % 2^32 - 2^32 = -1294967296
    assert_eq!(output.trim(), "-1294967296");
}

#[test]
fn test_i32_multiple_overflow_operations() {
    let source = r#"
        fn main() {
            let max: i32 = 2147483647;
            let ten: i32 = 10;
            let result: i32 = max + ten;  // Overflow
            println_i32(result);
            let result2: i32 = result - ten; // Wrap back
            println_i32(result2);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "-2147483639\n2147483647");
}

// Tests for i64 overflow wrapping behavior
#[test]
fn test_i64_addition_overflow() {
    let source = r#"
        fn main() {
            let x: i64 = 9223372036854775807;  // i64::MAX
            let one: i64 = 1;
            let result: i64 = x + one;
            println_i64(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "-9223372036854775808"); // Should wrap to i64::MIN
}

#[test]
fn test_i64_subtraction_overflow() {
    let source = r#"
        fn main() {
            let zero: i64 = 0;
            let max: i64 = 9223372036854775807;
            let one: i64 = 1;
            let min: i64 = zero - max - one;  // i64::MIN
            let result: i64 = min - one;
            println_i64(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "9223372036854775807"); // Should wrap to i64::MAX
}

#[test]
fn test_i64_multiplication_overflow() {
    let source = r#"
        fn main() {
            let x: i64 = 1000000000000000;  // 10^15
            let y: i64 = 10000000;           // 10^7
            let result: i64 = x * y;         // 10^22 overflows i64
            println_i64(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // 10^22 = 10000000000000000000000
    // This wraps around in two's complement arithmetic to positive
    assert_eq!(output.trim(), "1864712049423024128");
}

#[test]
fn test_i64_division_no_overflow() {
    // Division can only overflow in one case: i64::MIN / -1
    // But Rue doesn't support unary minus on literals yet, so we can't test this directly
    // Instead, test that normal division doesn't overflow
    let source = r#"
        fn main() {
            let x: i64 = 9223372036854775807;  // i64::MAX
            let y: i64 = 2;
            let result: i64 = x / y;
            println_i64(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(output.trim(), "4611686018427387903");
}

#[test]
fn test_i64_complex_overflow_sequence() {
    let source = r#"
        fn main() {
            let max: i64 = 9223372036854775807;
            let big: i64 = 1000000000000;
            let result: i64 = max + big;  // Overflow to negative
            println_i64(result);
            let result2: i64 = result - big; // Should NOT wrap back to max
            println_i64(result2);
            let two: i64 = 2;
            let result3: i64 = result * two;  // Further overflow
            println_i64(result3);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // max + big wraps to negative: -9223371036854775809
    // that negative - big wraps to positive: 9223372036854775807 (MAX again!)
    // that negative * 2 = 1999999999998
    assert_eq!(
        output.trim(),
        "-9223371036854775809\n9223372036854775807\n1999999999998"
    );
}

// Tests that verify wrapping behavior matches two's complement arithmetic
#[test]
fn test_i32_wrapping_matches_spec() {
    // Verify that overflow wraps using two's complement arithmetic
    let source = r#"
        fn main() {
            // Test MAX + 1 = MIN
            let max: i32 = 2147483647;
            let min: i32 = max + 1;
            println_i32(min);
            
            // Test MIN - 1 = MAX
            let max2: i32 = min - 1;
            println_i32(max2);
            
            // Test wrapping in multiplication
            let a: i32 = 46341; // sqrt(i32::MAX) + 1
            let result: i32 = a * a;
            println_i32(result);
            
            // Test wrapping preserves bit patterns
            let x: i32 = 2147483640;
            let y: i32 = 20;
            let wrapped: i32 = x + y;
            println_i32(wrapped);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // MAX + 1 = MIN (-2147483648)
    // MIN - 1 = MAX (2147483647)
    // 46341 * 46341 = 2147488281, which wraps to -2147479015
    // 2147483640 + 20 = 2147483660, which wraps to -2147483636
    assert_eq!(
        output.trim(),
        "-2147483648\n2147483647\n-2147479015\n-2147483636"
    );
}

#[test]
fn test_i64_wrapping_matches_spec() {
    // Verify that i64 overflow wraps using two's complement arithmetic
    let source = r#"
        fn main() {
            // Test MAX + 1 = MIN
            let max: i64 = 9223372036854775807;
            let one: i64 = 1;
            let min: i64 = max + one;
            println_i64(min);
            
            // Test MIN - 1 = MAX
            let max2: i64 = min - one;
            println_i64(max2);
            
            // Test specific wrapping calculation
            let x: i64 = 9223372036854775800;
            let y: i64 = 20;
            let wrapped: i64 = x + y;
            println_i64(wrapped);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // MAX + 1 = MIN (-9223372036854775808)
    // MIN - 1 = MAX (9223372036854775807)
    // 9223372036854775800 + 20 wraps to negative
    assert_eq!(
        output.trim(),
        "-9223372036854775808\n9223372036854775807\n-9223372036854775796"
    );
}

#[test]
fn test_modulo_wrapping_behavior() {
    // Test that modulo operations work correctly with wrapped values
    let source = r#"
        fn main() {
            let max: i32 = 2147483647;
            let wrapped: i32 = max + 2;  // Wraps to MIN + 1
            let ten: i32 = 10;
            let result: i32 = wrapped % ten;
            println_i32(wrapped);
            println_i32(result);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // max + 2 = -2147483647 (properly wrapped in i32)
    // -2147483647 % 10 = -7 (using truncated division)
    assert_eq!(output.trim(), "-2147483647\n-7");
}

#[test]
fn test_chained_overflow_operations() {
    // Test that multiple overflows in sequence behave correctly
    let source = r#"
        fn main() {
            let x: i32 = 2000000000;
            let y: i32 = 2000000000;
            let sum: i32 = x + y;  // 4000000000 overflows
            println_i32(sum);
            
            let z: i32 = sum + x;  // Further overflow
            println_i32(z);
            
            let w: i32 = z - y;    // Wrap back
            println_i32(w);
        }
    "#;

    let (exit_code, output) = compile_and_run(source, None).unwrap();
    assert_eq!(exit_code, 0);
    // 2000000000 + 2000000000 = 4000000000, wraps to -294967296
    // -294967296 + 2000000000 = 1705032704
    // 1705032704 - 2000000000 = -294967296
    assert_eq!(output.trim(), "-294967296\n1705032704\n-294967296");
}
