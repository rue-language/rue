use libtest_mimic::{Arguments, Failed, Trial};
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

mod traceability;

/// A section of the spec containing multiple test cases.
#[derive(Debug, Deserialize)]
struct Section {
    id: String,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    description: String,
    /// Optional reference to spec chapter (e.g., "3.1")
    #[allow(dead_code)]
    #[serde(default)]
    spec_chapter: Option<String>,
}

/// A single test case.
#[derive(Debug, Clone, Deserialize)]
struct Case {
    name: String,
    source: String,
    /// Expected exit code (for successful compilation)
    exit_code: Option<i32>,
    /// If true, compilation should fail
    #[serde(default)]
    compile_fail: bool,
    /// If true, only compile (don't run) - useful for infinite loops
    #[serde(default)]
    compile_only: bool,
    /// Optional substring that should appear in the error message
    #[serde(default)]
    error_contains: Option<String>,
    /// Expected exact error output (golden test)
    #[serde(default)]
    expected_error: Option<String>,
    /// Expected tokens dump (golden test)
    #[serde(default)]
    expected_tokens: Option<String>,
    /// Expected AST dump (golden test)
    #[serde(default)]
    expected_ast: Option<String>,
    /// Expected RIR dump (golden test)
    #[serde(default)]
    expected_rir: Option<String>,
    /// Expected AIR dump (golden test)
    #[serde(default)]
    expected_air: Option<String>,
    /// Expected MIR dump (golden test)
    #[serde(default)]
    expected_mir: Option<String>,
    /// Expected runtime error message (program compiles but fails at runtime)
    #[serde(default)]
    runtime_error: Option<String>,
    /// Expected exit code for runtime errors (defaults to 101)
    #[serde(default)]
    runtime_exit_code: Option<i32>,
    #[serde(default)]
    skip: bool,
    /// Substrings that should appear in warning messages
    #[serde(default)]
    warning_contains: Option<Vec<String>>,
    /// Expected number of warnings
    #[serde(default)]
    expected_warning_count: Option<usize>,
    /// If true, verify no warnings were emitted
    #[serde(default)]
    no_warnings: bool,
    /// Spec paragraph references (e.g., ["3.1:1", "3.1:2"])
    #[allow(dead_code)]
    #[serde(default)]
    spec: Vec<String>,
}

/// A spec file containing a section and its cases.
#[derive(Debug, Deserialize)]
struct SpecFile {
    section: Section,
    #[serde(default)]
    case: Vec<Case>,
}

/// Recursively collect all TOML files from a directory.
fn collect_toml_files(dir: &Path, files: &mut Vec<PathBuf>) {
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

/// Load all spec files from the cases directory (including subdirectories).
fn load_spec_files(cases_dir: &Path) -> Vec<(String, SpecFile)> {
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

        match toml::from_str::<SpecFile>(&content) {
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
/// Also normalizes file paths to "<source>" for error message comparisons.
fn normalize_golden(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Normalize error output for golden test comparison.
/// Replaces the temp file path with a placeholder "<source>".
fn normalize_error_output(s: &str, source_path: &Path) -> String {
    let path_str = source_path.to_string_lossy();
    let normalized = s.replace(path_str.as_ref(), "<source>");
    normalize_golden(&normalized)
}

/// Strip the emit header (e.g., "=== RIR ===") from the output.
fn strip_emit_header(output: &str, stage: &str) -> String {
    let header = format!("=== {} ===", stage);
    output
        .lines()
        .filter(|line| line.trim() != header)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compare actual output against expected golden output.
fn check_golden(actual: &str, expected: &str, label: &str) -> Result<(), Failed> {
    let actual_normalized = normalize_golden(actual);
    let expected_normalized = normalize_golden(expected);

    if actual_normalized != expected_normalized {
        return Err(format!(
            "{} mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n",
            label, expected_normalized, actual_normalized
        )
        .into());
    }
    Ok(())
}

/// Run a single test case.
fn run_test_case(case: &Case, rue_binary: &Path) -> Result<(), Failed> {
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

    // Check for golden IR tests (tokens, AST, RIR, AIR, MIR)
    if case.expected_tokens.is_some()
        || case.expected_ast.is_some()
        || case.expected_rir.is_some()
        || case.expected_air.is_some()
        || case.expected_mir.is_some()
    {
        // Run dump commands and check golden output
        if let Some(ref expected) = case.expected_tokens {
            let output = Command::new(rue_binary)
                .arg("--emit")
                .arg("tokens")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit tokens: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit tokens failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== Tokens ===" header for golden comparison
            let actual = strip_emit_header(&actual, "Tokens");
            check_golden(&actual, expected, "Tokens")?;
        }

        if let Some(ref expected) = case.expected_ast {
            let output = Command::new(rue_binary)
                .arg("--emit")
                .arg("ast")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit ast: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit ast failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== AST ===" header for golden comparison
            let actual = strip_emit_header(&actual, "AST");
            check_golden(&actual, expected, "AST")?;
        }

        if let Some(ref expected) = case.expected_rir {
            let output = Command::new(rue_binary)
                .arg("--emit")
                .arg("rir")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit rir: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit rir failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== RIR ===" header for golden comparison
            let actual = strip_emit_header(&actual, "RIR");
            check_golden(&actual, expected, "RIR")?;
        }

        if let Some(ref expected) = case.expected_air {
            let output = Command::new(rue_binary)
                .arg("--emit")
                .arg("air")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit air: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit air failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== AIR ===" header for golden comparison
            let actual = strip_emit_header(&actual, "AIR");
            check_golden(&actual, expected, "AIR")?;
        }

        if let Some(ref expected) = case.expected_mir {
            let output = Command::new(rue_binary)
                .arg("--emit")
                .arg("mir")
                .arg(&source_path)
                .output()
                .map_err(|e| format!("Failed to run rue --emit mir: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "rue --emit mir failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }

            let actual = String::from_utf8_lossy(&output.stdout);
            // Strip the "=== MIR ===" header for golden comparison
            let actual = strip_emit_header(&actual, "MIR");
            check_golden(&actual, expected, "MIR")?;
        }

        return Ok(());
    }

    // Compile with rue
    let compile_output = Command::new(rue_binary)
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
            )
            .into());
        }

        // Check exact error message (golden test)
        if let Some(ref expected) = case.expected_error {
            let actual_normalized = normalize_error_output(&stderr, &source_path);
            let expected_normalized = normalize_golden(expected);
            if actual_normalized != expected_normalized {
                return Err(format!(
                    "Error mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n",
                    expected_normalized, actual_normalized
                )
                .into());
            }
        }

        // Check error message contains substring
        if let Some(ref expected_error) = case.error_contains {
            if !stderr.contains(expected_error) {
                return Err(format!(
                    "Error message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                    expected_error, stderr, case.source
                )
                .into());
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
        )
        .into());
    }

    // Check warning-related assertions
    let compile_stderr = stderr.to_string();

    // Check if no warnings expected
    if case.no_warnings {
        if compile_stderr.contains("warning:") {
            return Err(format!(
                "Expected no warnings but got:\n{}\n  source: {}",
                compile_stderr, case.source
            )
            .into());
        }
    }

    // Check expected warning count
    if let Some(expected_count) = case.expected_warning_count {
        let actual_count = compile_stderr.matches("warning:").count();
        if actual_count != expected_count {
            return Err(format!(
                "Warning count mismatch:\n  expected: {}\n  actual: {}\n  stderr: {}\n  source: {}",
                expected_count, actual_count, compile_stderr, case.source
            )
            .into());
        }
    }

    // Check that warnings contain expected substrings
    if let Some(ref expected_warnings) = case.warning_contains {
        for expected in expected_warnings {
            if !compile_stderr.contains(expected) {
                return Err(format!(
                    "Warning message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                    expected, compile_stderr, case.source
                )
                .into());
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
            )
            .into());
        }

        // Check that stderr contains the expected error message
        if !stderr.contains(expected_error.as_str()) {
            return Err(format!(
                "Runtime error message mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                expected_error, stderr, case.source
            )
            .into());
        }

        return Ok(());
    }

    // Normal exit code test
    let expected_exit_code = case.exit_code.ok_or_else(|| {
        "Test case should have exit_code when compile_fail is false and runtime_error is not set".to_string()
    })?;

    if actual_exit_code != expected_exit_code {
        return Err(format!(
            "Exit code mismatch:\n  expected: {}\n  actual: {}\n  source: {}",
            expected_exit_code, actual_exit_code, case.source
        )
        .into());
    }

    Ok(())
}

/// Find the spec directory.
fn find_spec_dir() -> PathBuf {
    std::env::var("RUE_SPEC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let possible_paths = ["docs/spec/src", "../docs/spec/src", "../../docs/spec/src"];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new("docs/spec/src").to_path_buf()
        })
}

/// Find the cases directory.
fn find_cases_dir() -> PathBuf {
    std::env::var("RUE_SPEC_CASES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let possible_paths = ["crates/rue-spec/cases", "cases", "../rue-spec/cases"];
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new("cases").to_path_buf()
        })
}

/// Run the traceability report.
fn run_traceability(detailed: bool) {
    let spec_dir = find_spec_dir();
    let cases_dir = find_cases_dir();

    if !spec_dir.exists() {
        eprintln!("Error: Spec directory not found: {}", spec_dir.display());
        eprintln!("Set RUE_SPEC_DIR environment variable or run from project root.");
        std::process::exit(1);
    }

    if !cases_dir.exists() {
        eprintln!("Error: Cases directory not found: {}", cases_dir.display());
        eprintln!("Set RUE_SPEC_CASES environment variable or run from project root.");
        std::process::exit(1);
    }

    let report = traceability::generate_report(&spec_dir, &cases_dir);

    if detailed {
        report.print_detailed();
    } else {
        report.print_summary();
    }

    // Exit with error if there are uncovered normative paragraphs or orphan references
    // Informative paragraphs don't require test coverage
    if report.normative_uncovered_count() > 0 || !report.orphan_references.is_empty() {
        std::process::exit(1);
    }
}

fn main() {
    // Check for traceability flag before parsing libtest args
    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args.iter().any(|a| a == "--traceability") {
        let detailed = raw_args.iter().any(|a| a == "--detailed");
        run_traceability(detailed);
        return;
    }

    if raw_args.iter().any(|a| a == "--help-traceability") {
        println!("Traceability Report Options:");
        println!();
        println!("  --traceability     Generate spec coverage report");
        println!("  --detailed         Show detailed traceability matrix");
        println!();
        println!("Environment Variables:");
        println!("  RUE_SPEC_DIR       Path to spec markdown files (default: docs/spec/src)");
        println!("  RUE_SPEC_CASES     Path to test case files (default: crates/rue-spec/cases)");
        return;
    }

    let args = Arguments::from_args();

    // Find the rue binary - it should be built alongside this test runner
    // For now, we'll look for it relative to the current directory or via an env var
    let rue_binary = std::env::var("RUE_BINARY")
        .map(std::path::PathBuf::from)
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
        });

    // Find the cases directory
    let cases_dir = find_cases_dir();

    // Load all spec files
    let specs = load_spec_files(&cases_dir);

    // Convert to trials
    let tests: Vec<Trial> = specs
        .into_iter()
        .flat_map(|(_, spec)| {
            let section_id = spec.section.id.clone();
            let rue_binary = rue_binary.clone();

            spec.case.into_iter().map(move |case| {
                let test_name = format!("{}::{}", section_id, case.name);
                let skip = case.skip;
                let rue_binary = rue_binary.clone();

                let mut trial = Trial::test(test_name, move || run_test_case(&case, &rue_binary));

                if skip {
                    trial = trial.with_ignored_flag(true);
                }

                trial
            })
        })
        .collect();

    if tests.is_empty() {
        eprintln!("Warning: No test cases found in {}", cases_dir.display());
        eprintln!("Make sure spec files exist and have the correct format.");
    }

    libtest_mimic::run(&args, tests).exit();
}
