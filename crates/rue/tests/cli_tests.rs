//! CLI tests using the rue snapshot testing framework
//!
//! These tests verify the CLI behavior of the rue compiler using
//! the existing snapshot testing infrastructure.

mod common;
mod test_utils;

use common::get_project_root;
use rue_snapshot::{
    ExecSnapshotFormat as SnapshotFormat, ExecutionSnapshot, SnapshotTestBuilder,
    normalize::{
        CompositeNormalizer, FnNormalizer, Normalizer, normalize_temp_names, normalize_timestamps,
    },
};
use std::fs;
use std::process::{Command, Stdio};
use tempfile::TempDir;
use test_utils::get_rue_binary;

/// Normalize an execution snapshot for stable testing
fn normalize_execution_snapshot(snapshot: &ExecutionSnapshot) -> ExecutionSnapshot {
    let normalizer = CompositeNormalizer::new()
        .with(FnNormalizer::new(normalize_timestamps))
        .with(FnNormalizer::new(normalize_temp_names));

    ExecutionSnapshot {
        exit_code: snapshot.exit_code,
        stdout: normalizer.normalize(&snapshot.stdout),
        stderr: normalizer.normalize(&snapshot.stderr),
        compilation_warnings: snapshot.compilation_warnings.clone(),
        timeout: snapshot.timeout,
    }
}

/// Create a snapshot test for CLI tests with normalization
fn test_cli_snapshot(name: &str, snapshot: &ExecutionSnapshot) -> Result<(), String> {
    let normalized = normalize_execution_snapshot(snapshot);
    let project_root = get_project_root();
    SnapshotTestBuilder::new(name)
        .with_snapshot_dir(project_root.join("tests/snapshots/cli"))
        .with_format(SnapshotFormat::Toml)
        .test_execution(&normalized)
}

/// Run the rue compiler with given arguments, capturing stdout/stderr
fn run_rue_cli(args: &[&str], stdin_content: Option<&str>) -> Result<ExecutionSnapshot, String> {
    let project_root = get_project_root();

    // Get the rue binary
    let rue_binary = get_rue_binary();

    // Set up the command
    let mut cmd = Command::new(&rue_binary);

    // Check if verbose flag is present to set appropriate log level
    let log_level = if args.contains(&"-v")
        || args.contains(&"--verbose")
        || args.contains(&"-vv")
        || args.contains(&"-vvv")
    {
        "rue=info" // Allow verbose output when requested
    } else {
        "rue=warn" // Suppress debug logs for normal tests
    };

    cmd.args(args)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", log_level);

    // Handle stdin if provided
    if stdin_content.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn rue compiler: {e}"))?;

    // Write stdin content if provided
    if let Some(content) = stdin_content {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(content.as_bytes())
                .map_err(|e| format!("Failed to write to stdin: {e}"))?;
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for rue compiler: {e}"))?;

    Ok(ExecutionSnapshot {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        compilation_warnings: None,
        timeout: None,
    })
}

/// Create a temporary file with given content and return its path
fn create_temp_file(
    content: &str,
    extension: &str,
) -> Result<(TempDir, std::path::PathBuf), String> {
    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp directory: {e}"))?;
    let temp_file = temp_dir.path().join(format!("test.{}", extension));
    fs::write(&temp_file, content).map_err(|e| format!("Failed to write temp file: {e}"))?;
    Ok((temp_dir, temp_file))
}

#[test]
fn test_cli_mutual_exclusivity_emit_asm_and_mir() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["--emit-asm", "--emit-mir", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_mutual_exclusivity_emit_asm_and_mir", &result)
        .expect("Snapshot test failed");
}

#[test]
fn test_cli_mutual_exclusivity_emit_asm_and_compile_only() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["--emit-asm", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_mutual_exclusivity_emit_asm_and_compile_only", &result)
        .expect("Snapshot test failed");
}

#[test]
fn test_cli_mutual_exclusivity_emit_mir_and_compile_only() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["--emit-mir", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_mutual_exclusivity_emit_mir_and_compile_only", &result)
        .expect("Snapshot test failed");
}

#[test]
fn test_cli_mutual_exclusivity_all_three() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &[
            "--emit-asm",
            "--emit-mir",
            "--compile-only",
            temp_file.to_str().unwrap(),
        ],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_mutual_exclusivity_all_three", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_optimization_level_o0() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["-O0", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_optimization_level_o0", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_optimization_level_o1() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["-O1", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_optimization_level_o1", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_optimization_level_o2() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["-O2", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_optimization_level_o2", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_optimization_level_o3() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["-O3", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_optimization_level_o3", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_optimization_level_invalid() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["-O5", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_optimization_level_invalid", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_quiet_flag() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["--quiet", "--compile-only", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_quiet_flag", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_verbose_flag() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(&["-v", "--compile-only", temp_file.to_str().unwrap()], None)
        .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_verbose_flag", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_stdin_input() {
    let result = run_rue_cli(
        &["--compile-only", "-"],
        Some(
            r#"fn main() -> i64 {
    return 42;
}"#,
        ),
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_stdin_input", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_emit_mir_to_stdout() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &["--emit-mir", "-o", "-", temp_file.to_str().unwrap()],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_emit_mir_to_stdout", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_unknown_log_format() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &[
            "--log-format",
            "unknown",
            "--compile-only",
            temp_file.to_str().unwrap(),
        ],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_unknown_log_format", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_no_input_file() {
    let result = run_rue_cli(&["--compile-only"], None).expect("Failed to run rue CLI");

    test_cli_snapshot("cli_no_input_file", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_syntax_error() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42  // Missing semicolon
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(&["--compile-only", temp_file.to_str().unwrap()], None)
        .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_syntax_error", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_valid_log_format_json() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let result = run_rue_cli(
        &[
            "--log-format",
            "json",
            "--compile-only",
            temp_file.to_str().unwrap(),
        ],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_valid_log_format_json", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_help_message() {
    let result = run_rue_cli(&["--help"], None).expect("Failed to run rue CLI");

    test_cli_snapshot("cli_help_message", &result).expect("Snapshot test failed");
}

#[test]
fn test_cli_emit_asm_successful() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"fn main() -> i64 {
    return 42;
}"#,
        "rue",
    )
    .expect("Failed to create temp file");

    // Create a temp output directory for the assembly file
    let temp_out_dir = TempDir::new().expect("Failed to create output temp dir");
    let output_path = temp_out_dir.path().join("test.s");

    let result = run_rue_cli(
        &[
            "--emit-asm",
            "-o",
            output_path.to_str().unwrap(),
            temp_file.to_str().unwrap(),
        ],
        None,
    )
    .expect("Failed to run rue CLI");

    test_cli_snapshot("cli_emit_asm_successful", &result).expect("Snapshot test failed");
}
