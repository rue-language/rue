//! CLI tests using insta snapshot testing
//!
//! These tests verify the CLI behavior of the rue compiler using
//! the insta snapshot testing framework.

mod common;
mod test_utils;

use common::get_project_root;
use rue_insta_utils::{
    execution::ExecutionSnapshot,
    redactions::{normalize_temp_paths, normalize_timestamps},
};
use std::fs;
use std::process::{Command, Stdio};
use tempfile::TempDir;
use test_utils::get_rue_binary;

/// Normalize an execution snapshot for stable testing
fn normalize_execution_snapshot(snapshot: ExecutionSnapshot) -> ExecutionSnapshot {
    snapshot.normalize(|s| {
        let s = normalize_timestamps(s);
        normalize_temp_paths(&s)
    })
}

/// Create a snapshot test for CLI tests with normalization
fn assert_cli_snapshot(name: &str, snapshot: &ExecutionSnapshot) {
    let normalized = normalize_execution_snapshot(snapshot.clone());
    let project_root = get_project_root();
    let snapshot_dir = project_root.join("tests/snapshots/cli");

    rue_insta_utils::configure_insta(snapshot_dir.to_str().unwrap()).bind(|| {
        rue_insta_utils::assert_execution_snapshot!(name, &normalized);
    });
}

/// Run the rue compiler with given arguments, capturing stdout/stderr
fn run_rue_cli(args: &[&str], stdin_content: Option<&str>) -> Result<ExecutionSnapshot, String> {
    let project_root = get_project_root();

    // Get the rue binary
    let rue_binary = get_rue_binary();

    // Set up the command
    let mut cmd = Command::new(&rue_binary);
    cmd.args(args)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

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

// ===== Help and Version Tests =====

#[test]
fn test_help_flag() {
    let snapshot = run_rue_cli(&["--help"], None).expect("Failed to run");
    assert_cli_snapshot("cli_help", &snapshot);
}

#[test]
fn test_version_flag() {
    let snapshot = run_rue_cli(&["--version"], None).expect("Failed to run");
    assert_cli_snapshot("cli_version", &snapshot);
}

// ===== Error Tests =====

#[test]
fn test_no_input_file() {
    let snapshot = run_rue_cli(&[], None).expect("Failed to run");
    assert_cli_snapshot("cli_no_input", &snapshot);
}

#[test]
fn test_nonexistent_file() {
    let snapshot = run_rue_cli(&["nonexistent.rue"], None).expect("Failed to run");
    assert_cli_snapshot("cli_nonexistent_file", &snapshot);
}

#[test]
fn test_invalid_syntax() {
    let (_temp_dir, temp_file) =
        create_temp_file("fn main() {", "rue").expect("Failed to create temp file");

    let snapshot = run_rue_cli(&[temp_file.to_str().unwrap()], None).expect("Failed to run");
    assert_cli_snapshot("cli_invalid_syntax", &snapshot);
}

// ===== Compilation Tests =====

#[test]
fn test_simple_compile() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"
fn main() -> i32 {
    42
}
"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let out_dir = TempDir::new().expect("Failed to create output dir");
    let out_file = out_dir.path().join("output");

    let snapshot = run_rue_cli(
        &[
            temp_file.to_str().unwrap(),
            "-o",
            out_file.to_str().unwrap(),
        ],
        None,
    )
    .expect("Failed to run");

    assert_cli_snapshot("cli_simple_compile", &snapshot);
}

#[test]
fn test_compile_with_verbose() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"
fn main() -> i32 {
    42
}
"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let out_dir = TempDir::new().expect("Failed to create output dir");
    let out_file = out_dir.path().join("output");

    let snapshot = run_rue_cli(
        &[
            temp_file.to_str().unwrap(),
            "-o",
            out_file.to_str().unwrap(),
            "-v",
        ],
        None,
    )
    .expect("Failed to run");

    assert_cli_snapshot("cli_compile_verbose", &snapshot);
}

// ===== Type Error Tests =====

#[test]
fn test_type_error() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"
fn main() -> i32 {
    let x: bool = 42;
    x
}
"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let snapshot = run_rue_cli(&[temp_file.to_str().unwrap()], None).expect("Failed to run");
    assert_cli_snapshot("cli_type_error", &snapshot);
}

#[test]
fn test_undefined_variable() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"
fn main() -> i32 {
    undefined_var
}
"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let snapshot = run_rue_cli(&[temp_file.to_str().unwrap()], None).expect("Failed to run");
    assert_cli_snapshot("cli_undefined_variable", &snapshot);
}

// ===== Output Flag Tests =====

#[test]
fn test_output_flag() {
    let (_temp_dir, temp_file) = create_temp_file(
        r#"
fn main() -> i32 {
    0
}
"#,
        "rue",
    )
    .expect("Failed to create temp file");

    let out_dir = TempDir::new().expect("Failed to create output dir");
    let out_file = out_dir.path().join("my_program");

    let snapshot = run_rue_cli(
        &[
            temp_file.to_str().unwrap(),
            "-o",
            out_file.to_str().unwrap(),
        ],
        None,
    )
    .expect("Failed to run");

    assert_cli_snapshot("cli_output_flag", &snapshot);

    // Verify output file was created
    assert!(out_file.exists(), "Output file was not created");
}
