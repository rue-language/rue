//! Corpus tests using insta snapshot testing
//!
//! These tests compile and run Rue programs, capturing their output
//! and comparing against snapshots.

mod common;
mod test_utils;

use common::get_project_root;
use rue_insta_utils::execution::ExecutionSnapshot;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use test_utils::get_rue_binary;

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

    // Get the rue binary
    let rue_binary = get_rue_binary();

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

    let elapsed = start.elapsed();
    let timeout = if elapsed > timeout_duration {
        Some(elapsed)
    } else {
        None
    };

    Ok(ExecutionSnapshot {
        exit_code: run_output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&run_output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&run_output.stderr).to_string(),
        compilation_warnings: warnings,
        timeout,
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

/// Helper to assert execution snapshot using insta
fn assert_corpus_snapshot(name: &str, snapshot: &ExecutionSnapshot) {
    let project_root = get_project_root();
    let snapshot_dir = project_root.join("tests/snapshots/corpus");

    rue_insta_utils::configure_insta(snapshot_dir.to_str().unwrap()).bind(|| {
        rue_insta_utils::assert_execution_snapshot!(name, snapshot);
    });
}

// Macro to generate tests for all corpus files
macro_rules! corpus_test {
    ($name:ident, $path:expr) => {
        #[test]
        fn $name() {
            let path = Path::new($path);
            let test_name = test_name_from_path(path);

            let result = compile_and_run(path).expect("Failed to compile and run program");
            assert_corpus_snapshot(&test_name, &result);
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
        let project_root = get_project_root();
        let corpus_dir = project_root.join("tests/fixtures/corpus");

        if !corpus_dir.exists() {
            eprintln!("Corpus directory not found: {:?}", corpus_dir);
            return;
        }

        let mut failures = Vec::new();
        let mut successes = 0;

        for entry in walkdir::WalkDir::new(&corpus_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rue") {
                let test_name = test_name_from_path(path);
                println!("Testing: {}", test_name);

                match compile_and_run(path) {
                    Ok(snapshot) => {
                        assert_corpus_snapshot(&test_name, &snapshot);
                        successes += 1;
                    }
                    Err(e) => {
                        failures.push((test_name, e));
                    }
                }
            }
        }

        println!("\n=== Batch Test Results ===");
        println!("Successes: {}", successes);
        println!("Failures: {}", failures.len());

        if !failures.is_empty() {
            println!("\nFailed tests:");
            for (name, error) in &failures {
                println!("  - {}: {}", name, error);
            }
            panic!("Some corpus tests failed");
        }
    }
}
