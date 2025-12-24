//! Shared test runner infrastructure for Rue compiler tests.
//!
//! This crate provides common functionality for running compiler tests,
//! including test case parsing, execution, and output comparison.

use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A section header in a test file.
#[derive(Debug, Deserialize)]
pub struct Section {
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub description: String,
    /// Optional reference to spec chapter (e.g., "3.1")
    #[allow(dead_code)]
    #[serde(default)]
    pub spec_chapter: Option<String>,
}

/// A single test case.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub name: String,
    pub source: String,
    /// Expected exit code (for successful compilation)
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// If true, compilation should fail
    #[serde(default)]
    pub compile_fail: bool,
    /// If true, only compile (don't run) - useful for infinite loops
    #[serde(default)]
    pub compile_only: bool,
    /// Optional substring that should appear in the error message
    #[serde(default)]
    pub error_contains: Option<String>,
    /// Expected exact error output (golden test)
    #[serde(default)]
    pub expected_error: Option<String>,
    /// Expected tokens dump (golden test)
    #[serde(default)]
    pub expected_tokens: Option<String>,
    /// Expected AST dump (golden test)
    #[serde(default)]
    pub expected_ast: Option<String>,
    /// Expected RIR dump (golden test)
    #[serde(default)]
    pub expected_rir: Option<String>,
    /// Expected AIR dump (golden test)
    #[serde(default)]
    pub expected_air: Option<String>,
    /// Expected MIR dump (golden test)
    #[serde(default)]
    pub expected_mir: Option<String>,
    /// Expected runtime error message (program compiles but fails at runtime)
    #[serde(default)]
    pub runtime_error: Option<String>,
    /// Expected exit code for runtime errors (defaults to 101)
    #[serde(default)]
    pub runtime_exit_code: Option<i32>,
    /// Skip this test
    #[serde(default)]
    pub skip: bool,
    /// Substrings that should appear in warning messages
    #[serde(default)]
    pub warning_contains: Option<Vec<String>>,
    /// Expected number of warnings
    #[serde(default)]
    pub expected_warning_count: Option<usize>,
    /// If true, verify no warnings were emitted
    #[serde(default)]
    pub no_warnings: bool,
    /// Spec paragraph references (e.g., ["3.1:1", "3.1:2"])
    #[allow(dead_code)]
    #[serde(default)]
    pub spec: Vec<String>,
    /// Preview feature required to run this test (e.g., "mutable_strings").
    /// Tests with this field are compiled with `--preview <feature>` and
    /// are allowed to fail without failing the overall test suite.
    #[serde(default)]
    pub preview: Option<String>,
    /// Target architecture for MIR golden tests (e.g., "x86-64-linux", "aarch64-macos").
    /// Required for MIR tests; omitting it causes a test failure.
    #[serde(default)]
    pub target: Option<String>,
}

/// A test file containing a section and its cases.
#[derive(Debug, Deserialize)]
pub struct TestFile {
    pub section: Section,
    #[serde(default)]
    pub case: Vec<Case>,
}

/// Result of running a test.
pub type TestResult = Result<(), String>;

/// Recursively collect all TOML files from a directory.
pub fn collect_toml_files(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_toml_files(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "toml") {
                files.push(path);
            }
        }
    }
}

/// Load all test files from a directory (including subdirectories).
pub fn load_test_files(cases_dir: &Path) -> Vec<(String, TestFile)> {
    let mut specs = Vec::new();

    if !cases_dir.exists() {
        eprintln!(
            "Warning: cases directory not found: {}",
            cases_dir.display()
        );
        return specs;
    }

    // Collect all TOML files recursively
    let mut toml_files = Vec::new();
    collect_toml_files(cases_dir, &mut toml_files);

    for path in toml_files {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error reading {}: {}", path.display(), e);
                continue;
            }
        };

        match toml::from_str::<TestFile>(&content) {
            Ok(spec) => {
                // Build a relative path from cases_dir to create the identifier
                // e.g., "expressions/match" for "cases/expressions/match.toml"
                let relative = path
                    .strip_prefix(cases_dir)
                    .unwrap_or(&path)
                    .with_extension("");
                let identifier = relative
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                specs.push((identifier, spec));
            }
            Err(e) => {
                eprintln!("Error parsing {}: {}", path.display(), e);
            }
        }
    }

    // Sort by identifier for deterministic ordering
    specs.sort_by(|a, b| a.0.cmp(&b.0));
    specs
}

/// Normalize a string for golden test comparison.
/// This trims trailing whitespace from each line and ensures consistent line endings.
pub fn normalize_golden(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Normalize error output for golden test comparison.
/// Replaces the temp file path with a placeholder "<source>".
pub fn normalize_error_output(s: &str, source_path: &Path) -> String {
    let path_str = source_path.to_string_lossy();
    let normalized = s.replace(path_str.as_ref(), "<source>");
    normalize_golden(&normalized)
}

/// Strip the emit header (e.g., "=== RIR ===" or "=== MIR (aarch64-macos) ===") from the output.
pub fn strip_emit_header(output: &str, stage: &str) -> String {
    // Match headers like "=== MIR ===" or "=== MIR (x86-64-linux) ===" or "=== MIR (aarch64-macos) ==="
    let prefix = format!("=== {} ", stage);
    let exact = format!("=== {} ===", stage);
    output
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Filter out both "=== STAGE ===" and "=== STAGE (target) ==="
            trimmed != exact && !(trimmed.starts_with(&prefix) && trimmed.ends_with("==="))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compare actual output against expected golden output.
pub fn check_golden(actual: &str, expected: &str, label: &str) -> TestResult {
    let actual_normalized = normalize_golden(actual);
    let expected_normalized = normalize_golden(expected);

    if actual_normalized != expected_normalized {
        return Err(format!(
            "{} mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n",
            label, expected_normalized, actual_normalized
        ));
    }
    Ok(())
}

/// Run a single test case.
pub fn run_test_case(case: &Case, rue_binary: &Path) -> TestResult {
    // Create a temporary directory for this test
    let temp_dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let source_path = temp_dir.path().join("test.rue");
    let output_path = temp_dir.path().join("test");

    // Write source to file
    let mut source_file = fs::File::create(&source_path)
        .map_err(|e| format!("Failed to create source file: {}", e))?;
    source_file
        .write_all(case.source.as_bytes())
        .map_err(|e| format!("Failed to write source: {}", e))?;

    // Build base command with preview flags if needed
    let build_command = |binary: &Path| -> Command {
        let mut cmd = Command::new(binary);
        if let Some(ref feature) = case.preview {
            cmd.arg("--preview").arg(feature);
        }
        cmd
    };

    // Check for golden IR tests (tokens, AST, RIR, AIR, MIR)
    if case.expected_tokens.is_some()
        || case.expected_ast.is_some()
        || case.expected_rir.is_some()
        || case.expected_air.is_some()
        || case.expected_mir.is_some()
    {
        // Run dump commands and check golden output
        if let Some(ref expected) = case.expected_tokens {
            let output = build_command(rue_binary)
                .arg("--emit")
                .arg("tokens")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit tokens: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit tokens failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== Tokens ===" header for golden comparison
            let actual = strip_emit_header(&actual, "Tokens");
            check_golden(&actual, expected, "Tokens")?;
        }

        if let Some(ref expected) = case.expected_ast {
            let output = build_command(rue_binary)
                .arg("--emit")
                .arg("ast")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit ast: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit ast failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== AST ===" header for golden comparison
            let actual = strip_emit_header(&actual, "AST");
            check_golden(&actual, expected, "AST")?;
        }

        if let Some(ref expected) = case.expected_rir {
            let output = build_command(rue_binary)
                .arg("--emit")
                .arg("rir")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit rir: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit rir failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== RIR ===" header for golden comparison
            let actual = strip_emit_header(&actual, "RIR");
            check_golden(&actual, expected, "RIR")?;
        }

        if let Some(ref expected) = case.expected_air {
            let output = build_command(rue_binary)
                .arg("--emit")
                .arg("air")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit air: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit air failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== AIR ===" header for golden comparison
            let actual = strip_emit_header(&actual, "AIR");
            check_golden(&actual, expected, "AIR")?;
        }

        if let Some(ref expected) = case.expected_mir {
            // MIR golden tests require an explicit target since MIR is architecture-specific.
            let target = case.target.as_ref().ok_or_else(|| {
                "MIR golden tests require a 'target' field (e.g., target = \"x86-64-linux\")"
                    .to_string()
            })?;

            let output = build_command(rue_binary)
                .arg("--target")
                .arg(target)
                .arg("--emit")
                .arg("mir")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit mir: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit mir failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== MIR (target) ===" header for golden comparison
            let actual = strip_emit_header(&actual, "MIR");
            check_golden(&actual, expected, "MIR")?;
        }

        return Ok(());
    }

    // Compile with rue
    let compile_output = build_command(rue_binary)
        .arg(&source_path)
        .arg(&output_path)
        .output()
        .map_err(|e| format!("Failed to run rue compiler: {}", e))?;

    let compile_succeeded = compile_output.status.success();
    let stderr = String::from_utf8_lossy(&compile_output.stderr);

    if case.compile_fail {
        // Expected to fail compilation
        if compile_succeeded {
            return Err(format!(
                "Expected compilation to fail, but it succeeded\n  source: {}",
                case.source
            ));
        }

        // Check exact error message (golden test)
        if let Some(ref expected) = case.expected_error {
            let actual_normalized = normalize_error_output(&stderr, &source_path);
            let expected_normalized = normalize_golden(expected);
            if actual_normalized != expected_normalized {
                return Err(format!(
                    "Error mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n",
                    expected_normalized, actual_normalized
                ));
            }
        }

        // Check error message contains substring
        if let Some(ref expected_error) = case.error_contains {
            if !stderr.contains(expected_error) {
                return Err(format!(
                    "Error message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                    expected_error, stderr, case.source
                ));
            }
        }

        return Ok(());
    }

    // Expected to succeed
    if !compile_succeeded {
        return Err(format!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile_output.stdout),
            stderr
        ));
    }

    // Check warning-related assertions
    let compile_stderr = stderr.to_string();

    // Check if no warnings expected
    if case.no_warnings {
        if compile_stderr.contains("warning:") {
            return Err(format!(
                "Expected no warnings but got:\n{}\n  source: {}",
                compile_stderr, case.source
            ));
        }
    }

    // Check expected warning count
    if let Some(expected_count) = case.expected_warning_count {
        let actual_count = compile_stderr.matches("warning:").count();
        if actual_count != expected_count {
            return Err(format!(
                "Warning count mismatch:\n  expected: {}\n  actual: {}\n  stderr: {}\n  source: {}",
                expected_count, actual_count, compile_stderr, case.source
            ));
        }
    }

    // Check that warnings contain expected substrings
    if let Some(ref expected_warnings) = case.warning_contains {
        for expected in expected_warnings {
            if !compile_stderr.contains(expected) {
                return Err(format!(
                    "Warning message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                    expected, compile_stderr, case.source
                ));
            }
        }
    }

    // If compile_only, we're done after successful compilation
    if case.compile_only {
        return Ok(());
    }

    // Run the compiled binary
    let run_output = Command::new(&output_path)
        .output()
        .map_err(|e| format!("Failed to run compiled binary: {}", e))?;

    let actual_exit_code = run_output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&run_output.stderr);

    // Handle runtime error tests
    if let Some(ref expected_error) = case.runtime_error {
        // Default exit code for runtime errors is 101
        let expected_exit = case.runtime_exit_code.unwrap_or(101);

        // Check exit code
        if actual_exit_code != expected_exit {
            return Err(format!(
                "Runtime error exit code mismatch:\n  expected: {}\n  actual: {}\n  source: {}",
                expected_exit, actual_exit_code, case.source
            ));
        }

        // Check that stderr contains the expected error message
        if !stderr.contains(expected_error.as_str()) {
            return Err(format!(
                "Runtime error message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                expected_error, stderr, case.source
            ));
        }

        return Ok(());
    }

    // Normal exit code test
    let expected_exit_code = case.exit_code.ok_or_else(|| {
        "Test case should have exit_code when compile_fail is false and runtime_error is not set"
            .to_string()
    })?;

    if actual_exit_code != expected_exit_code {
        return Err(format!(
            "Exit code mismatch:\n  expected: {}\n  actual: {}\n  source: {}",
            expected_exit_code, actual_exit_code, case.source
        ));
    }

    Ok(())
}

/// Find the rue binary in common locations.
pub fn find_rue_binary() -> PathBuf {
    std::env::var("RUE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Try to find it in common buck output locations
            // First try the buck2 output (has UUID in path)
            let buck_root = Path::new("buck-out/v2/gen/root");
            if buck_root.exists() {
                if let Ok(entries) = std::fs::read_dir(buck_root) {
                    for entry in entries.flatten() {
                        let rue_path = entry.path().join("crates/rue/__rue__/rue");
                        if rue_path.exists() {
                            return rue_path;
                        }
                    }
                }
            }
            let possible_paths = ["../rue/rue", "./rue"];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            // Default - will likely fail but with a clear error
            Path::new("rue").to_path_buf()
        })
}
