mod common;

use common::get_project_root;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
#[expect(dead_code)]
struct TestCase {
    file: String,
    description: String,
    expected_exit_code: Option<i32>,
    expected_stdout: Option<String>,
    expected_stderr: Option<String>,
}

/// Parse the README.md file to extract test cases
fn parse_readme(readme_path: &Path) -> HashMap<String, TestCase> {
    let content = fs::read_to_string(readme_path).expect("Failed to read README.md");
    let mut test_cases = HashMap::new();
    let mut in_table = false;
    let mut past_header = false;

    for line in content.lines() {
        let line = line.trim();

        // Start of table
        if line.starts_with("| File |") {
            in_table = true;
            continue;
        }

        // Table separator
        if in_table && line.starts_with("|---") {
            past_header = true;
            continue;
        }

        // End of table
        if in_table && !line.starts_with('|') {
            break;
        }

        // Parse table row
        if in_table && past_header && line.starts_with('|') {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() >= 6 {
                let file = parts[1].to_string();
                let description = parts[2].to_string();
                let exit_code_str = parts[3];
                let stdout_str = parts[4];
                let stderr_str = parts[5];

                let expected_exit_code = if exit_code_str == "?" {
                    None
                } else {
                    exit_code_str.parse::<i32>().ok()
                };

                let expected_stdout = if stdout_str.is_empty() {
                    None
                } else if stdout_str.contains("(") && stdout_str.contains(")") {
                    // Special cases marked with parentheses need manual handling
                    None
                } else {
                    // Convert HTML line breaks to actual newlines
                    Some(stdout_str.replace("<br>", "\n"))
                };

                let expected_stderr = if stderr_str.is_empty() {
                    None
                } else if stderr_str.contains("(") {
                    // Special cases marked with parentheses need manual handling
                    None
                } else {
                    Some(stderr_str.to_string())
                };

                let test_case = TestCase {
                    file: file.clone(),
                    description,
                    expected_exit_code,
                    expected_stdout,
                    expected_stderr,
                };

                test_cases.insert(file, test_case);
            }
        }
    }

    test_cases
}

/// Run a single test case with timeout
fn run_test_case(
    test_case: &TestCase,
    samples_dir: &Path,
    project_root: &Path,
) -> Result<(), String> {
    let source_path = samples_dir.join(&test_case.file);
    let executable_name = test_case.file.trim_end_matches(".rue");
    let executable_path = samples_dir.join(executable_name);

    // Ensure the source file exists
    if !source_path.exists() {
        return Err(format!("Source file {source_path:?} does not exist"));
    }

    // Clean up any existing executable
    if executable_path.exists() {
        fs::remove_file(&executable_path)
            .map_err(|e| format!("Failed to remove existing executable: {e}"))?;
    }

    // Compile the rue program
    let compile_output = if std::env::var("CARGO_MANIFEST_DIR").is_err() {
        // Buck2 build environment
        Command::new("buck2")
            .args(["run", "//crates/rue:rue", "--"])
            .arg(&source_path)
            .current_dir(project_root)
            .output()
            .map_err(|e| format!("Failed to execute rue compiler via Buck2: {e}"))?
    } else {
        // Cargo build environment
        Command::new("cargo")
            .args(["run", "-p", "rue", "--"])
            .arg(&source_path)
            .current_dir(project_root)
            .output()
            .map_err(|e| format!("Failed to execute rue compiler via Cargo: {e}"))?
    };

    if !compile_output.status.success() {
        return Err(format!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile_output.stdout),
            String::from_utf8_lossy(&compile_output.stderr)
        ));
    }

    // Verify the executable was created
    if !executable_path.exists() {
        return Err(format!("Executable {executable_path:?} was not created"));
    }

    // Run the compiled executable with a timeout
    let start = Instant::now();
    let timeout = Duration::from_secs(5); // 5 second timeout

    let mut child = Command::new(&executable_path)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute compiled program: {e}"))?;

    // Wait for the process with timeout
    let run_output = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process has exited
                let output = child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to get output: {e}"))?;

                break std::process::Output {
                    status,
                    stdout: output.stdout,
                    stderr: output.stderr,
                };
            }
            Ok(None) => {
                // Still running
                if start.elapsed() > timeout {
                    // Timeout exceeded, kill the process
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "Test timed out after {} seconds. Program appears to be in an infinite loop.",
                        timeout.as_secs()
                    ));
                }
                // Sleep briefly before checking again
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!("Error waiting for process: {e}"));
            }
        }
    };

    // Check exit code if expected
    if let Some(expected_exit_code) = test_case.expected_exit_code {
        let actual_exit_code = run_output.status.code().unwrap_or(-1);
        if actual_exit_code != expected_exit_code {
            return Err(format!(
                "Exit code mismatch: expected {}, got {}\nstdout: {}\nstderr: {}",
                expected_exit_code,
                actual_exit_code,
                String::from_utf8_lossy(&run_output.stdout),
                String::from_utf8_lossy(&run_output.stderr)
            ));
        }
    }

    // Check stdout if expected
    if let Some(ref expected_stdout) = test_case.expected_stdout {
        let actual_stdout = String::from_utf8_lossy(&run_output.stdout);
        if actual_stdout.trim() != expected_stdout.trim() {
            return Err(format!(
                "Stdout mismatch\nExpected:\n{expected_stdout}\nActual:\n{actual_stdout}"
            ));
        }
    }

    // Check stderr if expected
    if let Some(ref expected_stderr) = test_case.expected_stderr {
        let actual_stderr = String::from_utf8_lossy(&run_output.stderr);
        if actual_stderr.trim() != expected_stderr.trim() {
            return Err(format!(
                "Stderr mismatch\nExpected:\n{expected_stderr}\nActual:\n{actual_stderr}"
            ));
        }
    }

    // Clean up the executable
    fs::remove_file(&executable_path)
        .map_err(|e| format!("Failed to remove test executable: {e}"))?;

    Ok(())
}

#[test]
fn test_samples_corpus() {
    let project_root = get_project_root();
    let samples_dir = project_root.join("samples");
    let readme_path = samples_dir.join("README.md");

    // Parse test cases from README
    let test_cases = parse_readme(&readme_path);
    println!("Found {} test cases in README.md", test_cases.len());

    // Check for .rue files not in README
    let mut unlisted_files = Vec::new();
    if let Ok(entries) = fs::read_dir(&samples_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "rue") {
                if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
                    if !test_cases.contains_key(filename) {
                        unlisted_files.push(filename.to_string());
                    }
                }
            }
        }
    }

    if !unlisted_files.is_empty() {
        println!(
            "\nWARNING: Found {} .rue files not listed in README.md:",
            unlisted_files.len()
        );
        for file in &unlisted_files {
            println!("  - {file}");
        }
        println!();
    }

    // Run tests
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for (filename, test_case) in &test_cases {
        print!("Testing {filename} ... ");
        std::io::Write::flush(&mut std::io::stdout()).unwrap();

        // Skip tests without expected exit codes for now
        if test_case.expected_exit_code.is_none() {
            println!("SKIPPED (no expected exit code)");
            skipped += 1;
            continue;
        }

        match run_test_case(test_case, &samples_dir, project_root) {
            Ok(()) => {
                println!("PASSED");
                passed += 1;
            }
            Err(e) => {
                println!("FAILED");
                println!("  Error: {e}");
                failed += 1;
            }
        }
    }

    println!("\nTest Summary:");
    println!("  Passed: {passed}");
    println!("  Failed: {failed}");
    println!("  Skipped: {skipped}");
    println!("  Total: {}", test_cases.len());

    if failed > 0 {
        panic!("{failed} tests failed");
    }
}
