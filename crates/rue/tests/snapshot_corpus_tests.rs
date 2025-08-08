//! Corpus tests using the new snapshot testing framework
//!
//! These tests compile and run Rue programs, capturing their output
//! and comparing against snapshots.

mod common;

use common::get_project_root;
use rue_parser::snapshot::{ExecutionSnapshot, SnapshotFormat, SnapshotTestBuilder};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

/// Compile and run a Rue program, capturing output
fn compile_and_run(source_path: &Path) -> Result<ExecutionSnapshot, String> {
    let project_root = get_project_root();

    // Make path absolute from project root
    let absolute_path = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        project_root.join(source_path)
    };

    // Create a temporary directory for the executable
    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let exe_path = temp_dir.path().join("test_exe");

    // Always build rue to ensure we're using the latest version
    // Cargo will skip the build if nothing has changed
    let build_output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg("rue")
        .current_dir(project_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to build rue compiler: {e}"))?;

    if !build_output.status.success() {
        return Err("Failed to build rue compiler".to_string());
    }

    let rue_binary = project_root.join("target/release/rue");

    // Compile the program using the rue binary directly
    let compile_output = Command::new(&rue_binary)
        .arg(&absolute_path)
        .arg("-o")
        .arg(&exe_path)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to execute compiler: {e}"))?;

    if !compile_output.status.success() {
        let stderr = String::from_utf8_lossy(&compile_output.stderr);
        return Ok(ExecutionSnapshot {
            exit_code: compile_output.status.code().unwrap_or(-1),
            stdout: String::new(),
            stderr: stderr.to_string(),
            compilation_warnings: None,
            timeout: None,
        });
    }

    // Don't capture compilation warnings as they contain transient details like temp paths
    // and build timings that change on every run
    let warnings = None;

    // Run the compiled executable with timeout
    let start = std::time::Instant::now();
    let timeout_duration = Duration::from_secs(5);

    let run_output = Command::new(&exe_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to execute program: {e}"))?;

    let timed_out = start.elapsed() > timeout_duration;

    Ok(ExecutionSnapshot {
        exit_code: run_output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&run_output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&run_output.stderr).to_string(),
        compilation_warnings: warnings,
        timeout: Some(timed_out),
    })
}

/// Generate a test name from a file path
fn test_name_from_path(path: &Path) -> String {
    path.strip_prefix("tests/fixtures/corpus/")
        .unwrap_or(path)
        .with_extension("")
        .to_string_lossy()
        .replace('/', "_")
}

// Macro to generate tests for all corpus files
macro_rules! corpus_test {
    ($name:ident, $path:expr) => {
        #[test]
        fn $name() {
            let path = Path::new($path);
            let test_name = test_name_from_path(path);

            let result = compile_and_run(path).expect("Failed to compile and run program");

            let project_root = get_project_root();
            SnapshotTestBuilder::new(&test_name)
                .with_snapshot_dir(project_root.join("tests/snapshots/corpus"))
                .with_format(SnapshotFormat::Toml)
                .assert(result)
                .expect("Snapshot test failed");
        }
    };
}

// Examples that should work
corpus_test!(test_factorial, "examples/basic/factorial.rue");
corpus_test!(test_fibonacci, "examples/basic/fibonacci.rue");
corpus_test!(test_countdown, "examples/basic/countdown.rue");
corpus_test!(test_simple, "examples/basic/simple.rue");
corpus_test!(test_if_demo, "examples/control_flow/if_demo.rue");
corpus_test!(test_while_demo, "examples/control_flow/while_demo.rue");
corpus_test!(test_casting, "examples/advanced/casting.rue");
corpus_test!(
    test_assignment_demo,
    "examples/advanced/assignment_demo.rue"
);

// Test fixtures
corpus_test!(
    test_add_order,
    "tests/fixtures/corpus/arithmetic/test_add_order.rue"
);
corpus_test!(
    test_isolated_add,
    "tests/fixtures/corpus/arithmetic/test_isolated_add.rue"
);
corpus_test!(
    test_fib_minimal,
    "tests/fixtures/corpus/functions/test_fib_minimal.rue"
);

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[ignore] // Run with --ignored to test all corpus files
    fn test_all_corpus_files() {
        let corpus_dir = PathBuf::from("tests/fixtures/corpus");

        fn test_directory(dir: &Path) {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        test_directory(&path);
                    } else if path.extension().and_then(|s| s.to_str()) == Some("rue") {
                        println!("Testing: {}", path.display());

                        let test_name = test_name_from_path(&path);
                        let result = compile_and_run(&path).unwrap_or_else(|_| {
                            panic!("Failed to compile and run: {}", path.display())
                        });

                        let project_root = get_project_root();
                        SnapshotTestBuilder::new(&test_name)
                            .with_snapshot_dir(project_root.join("tests/snapshots/corpus"))
                            .with_format(SnapshotFormat::Toml)
                            .assert(result)
                            .unwrap_or_else(|_| {
                                panic!("Snapshot test failed for: {}", path.display())
                            });
                    }
                }
            }
        }

        test_directory(&corpus_dir);
    }
}
