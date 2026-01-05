//! Shared test runner infrastructure for Rue compiler tests.
//!
//! This crate provides common functionality for running compiler tests,
//! including test case parsing, execution, and output comparison.

use rue_error::PreviewFeature;
use serde::Deserialize;

/// Default timeout for test execution in milliseconds (10 seconds).
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Exit code used by the Rue runtime for runtime errors (division by zero, overflow, etc.).
///
/// This matches the convention used by Rust's test harness and the Rue runtime.
/// When a Rue program encounters a runtime error, it exits with this code.
pub const RUNTIME_ERROR_EXIT_CODE: i32 = 101;
use std::collections::HashMap;
use std::fs;
use std::io::{Read as IoRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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

/// A parameter set for parameterized tests.
///
/// Each parameter set generates one test instance. Parameters can:
/// - Provide values for `{placeholder}` substitution in string fields
/// - Override case fields like `exit_code`, `compile_fail`, etc.
/// - Add extra spec references via `spec_extra`
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ParamSet {
    /// All parameter values as a flat map.
    /// Special keys: `exit_code`, `compile_fail`, `skip`, `spec_extra`, etc.
    /// Other keys are used for `{key}` substitution in templates.
    #[serde(flatten)]
    pub values: HashMap<String, toml::Value>,
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
    /// Expected CFG dump (golden test)
    #[serde(default)]
    pub expected_cfg: Option<String>,
    /// Expected runtime error message (program compiles but fails at runtime)
    #[serde(default)]
    pub runtime_error: Option<String>,
    /// Expected exit code for runtime errors (defaults to [`RUNTIME_ERROR_EXIT_CODE`])
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
    /// Expected stdout output after successful execution (e.g., from @dbg calls)
    #[serde(default)]
    pub expected_stdout: Option<String>,
    /// Preview feature required to run this test (e.g., "mutable_strings").
    /// Tests with this field are compiled with `--preview <feature>` and
    /// are allowed to fail without failing the overall test suite,
    /// unless `preview_should_pass` is true.
    #[serde(default)]
    pub preview: Option<String>,
    /// If true, this preview test should pass and will fail the suite if it doesn't.
    /// Use this to mark preview tests that are expected to work after implementation.
    /// This provides real test output for implemented portions of preview features.
    #[serde(default)]
    pub preview_should_pass: bool,
    /// Target architecture (e.g., "x86-64-linux", "aarch64-macos").
    /// When specified, the compiler is invoked with `--target <target>`.
    /// Required for MIR golden tests; optional for other test types.
    #[serde(default)]
    pub target: Option<String>,
    /// Optimization level (0, 1, 2, or 3).
    /// When specified, the compiler is invoked with `-O<level>`.
    /// Defaults to 0 (no optimization) if not specified.
    #[serde(default)]
    pub opt_level: Option<u8>,
    /// Timeout for test execution in milliseconds.
    /// Defaults to [`DEFAULT_TIMEOUT_MS`] if not specified.
    /// If the test exceeds this timeout, it will be killed and marked as failed.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Input to provide to the program's stdin during execution.
    /// This is useful for testing programs that read from stdin (e.g., @read_line).
    /// The input is piped to the program before execution starts.
    #[serde(default)]
    pub stdin: Option<String>,
    /// Expected stderr output (substring match).
    /// For runtime errors, use `runtime_error` instead. This field is for
    /// checking stderr content in successful runs (e.g., panic messages).
    #[serde(default)]
    pub stderr_contains: Option<String>,
    /// Parameter sets for generating multiple test instances from a template.
    /// When present, this case is expanded into multiple cases, one per param set.
    /// Template placeholders like `{type}` in `source` and `name` are substituted.
    #[serde(default)]
    pub params: Vec<ParamSet>,
    /// Auxiliary source files for multi-file tests (for module imports).
    /// Each entry maps a relative filename to its source content.
    /// Example: `{ "math.rue" = "pub fn add(a: i32, b: i32) -> i32 { a + b }" }`
    #[serde(default)]
    pub aux_files: HashMap<String, String>,
    /// If true, pass aux_files to the compiler on the command line (multi-file compilation).
    /// If false (default), aux_files are just written to disk for @import to find.
    /// Use this when tests need to call functions from imported modules.
    #[serde(default)]
    pub pass_aux_files: bool,
    /// List of target triples on which this test should run.
    /// If specified, the test is skipped on hosts that don't match any of the targets.
    /// Example: `only_on = ["x86-64-linux", "aarch64-linux"]`
    /// If not specified, the test runs on all platforms.
    #[serde(default)]
    pub only_on: Vec<String>,
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

/// Get the current host target triple in Rue's format.
///
/// Returns strings like "x86-64-linux", "aarch64-linux", "aarch64-macos".
pub fn get_host_target() -> &'static str {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        "x86-64-linux"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        "aarch64-linux"
    }
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        "aarch64-macos"
    }
    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "x86-64-macos"
    }
    #[cfg(not(any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
    )))]
    {
        "unknown"
    }
}

/// Check if a test should be skipped based on `only_on` restrictions.
///
/// Returns `Some(reason)` if the test should be skipped, `None` if it should run.
pub fn should_skip_for_platform(only_on: &[String]) -> Option<String> {
    if only_on.is_empty() {
        return None;
    }

    let host = get_host_target();
    if only_on.iter().any(|target| target == host) {
        None
    } else {
        Some(format!(
            "test only runs on {:?}, current host is {}",
            only_on, host
        ))
    }
}

/// Convert a TOML value to a string for template substitution.
fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        // Arrays and tables are stringified as TOML
        other => other.to_string(),
    }
}

/// Substitute `{key}` placeholders in a string with values from the param set.
fn substitute_placeholders(template: &str, params: &HashMap<String, toml::Value>) -> String {
    let mut result = template.to_string();
    for (key, value) in params {
        let placeholder = format!("{{{}}}", key);
        let replacement = toml_value_to_string(value);
        result = result.replace(&placeholder, &replacement);
    }
    result
}

/// Expand a single case with params into multiple concrete cases.
/// If the case has no params, returns the case unchanged (in a vec).
pub fn expand_case(case: Case) -> Vec<Case> {
    if case.params.is_empty() {
        return vec![case];
    }

    case.params
        .iter()
        .map(|param_set| {
            let params = &param_set.values;
            let mut expanded = Case {
                // Substitute placeholders in string fields
                name: substitute_placeholders(&case.name, params),
                source: substitute_placeholders(&case.source, params),
                error_contains: case
                    .error_contains
                    .as_ref()
                    .map(|s| substitute_placeholders(s, params)),
                expected_error: case
                    .expected_error
                    .as_ref()
                    .map(|s| substitute_placeholders(s, params)),
                runtime_error: case
                    .runtime_error
                    .as_ref()
                    .map(|s| substitute_placeholders(s, params)),
                expected_stdout: case
                    .expected_stdout
                    .as_ref()
                    .map(|s| substitute_placeholders(s, params)),
                stdin: case
                    .stdin
                    .as_ref()
                    .map(|s| substitute_placeholders(s, params)),
                stderr_contains: case
                    .stderr_contains
                    .as_ref()
                    .map(|s| substitute_placeholders(s, params)),

                // Copy non-template fields with potential overrides
                exit_code: case.exit_code,
                compile_fail: case.compile_fail,
                compile_only: case.compile_only,
                expected_tokens: case.expected_tokens.clone(),
                expected_ast: case.expected_ast.clone(),
                expected_rir: case.expected_rir.clone(),
                expected_air: case.expected_air.clone(),
                expected_mir: case.expected_mir.clone(),
                expected_cfg: case.expected_cfg.clone(),
                runtime_exit_code: case.runtime_exit_code,
                skip: case.skip,
                warning_contains: case.warning_contains.clone(),
                expected_warning_count: case.expected_warning_count,
                no_warnings: case.no_warnings,
                spec: case.spec.clone(),
                preview: case.preview.clone(),
                preview_should_pass: case.preview_should_pass,
                target: case.target.clone(),
                opt_level: case.opt_level,
                timeout_ms: case.timeout_ms,
                aux_files: case.aux_files.clone(),
                pass_aux_files: case.pass_aux_files,
                only_on: case.only_on.clone(),

                // Clear params on expanded case
                params: vec![],
            };

            // Apply field overrides from params
            if let Some(value) = params.get("exit_code") {
                if let Some(i) = value.as_integer() {
                    expanded.exit_code = Some(i as i32);
                }
            }
            if let Some(value) = params.get("compile_fail") {
                if let Some(b) = value.as_bool() {
                    expanded.compile_fail = b;
                }
            }
            if let Some(value) = params.get("compile_only") {
                if let Some(b) = value.as_bool() {
                    expanded.compile_only = b;
                }
            }
            if let Some(value) = params.get("skip") {
                if let Some(b) = value.as_bool() {
                    expanded.skip = b;
                }
            }
            if let Some(value) = params.get("runtime_exit_code") {
                if let Some(i) = value.as_integer() {
                    expanded.runtime_exit_code = Some(i as i32);
                }
            }
            if let Some(value) = params.get("no_warnings") {
                if let Some(b) = value.as_bool() {
                    expanded.no_warnings = b;
                }
            }
            if let Some(value) = params.get("opt_level") {
                if let Some(i) = value.as_integer() {
                    expanded.opt_level = Some(i as u8);
                }
            }
            if let Some(value) = params.get("target") {
                if let Some(s) = value.as_str() {
                    expanded.target = Some(s.to_string());
                }
            }
            if let Some(value) = params.get("preview") {
                if let Some(s) = value.as_str() {
                    expanded.preview = Some(s.to_string());
                }
            }
            if let Some(value) = params.get("preview_should_pass") {
                if let Some(b) = value.as_bool() {
                    expanded.preview_should_pass = b;
                }
            }
            if let Some(value) = params.get("timeout_ms") {
                if let Some(i) = value.as_integer() {
                    expanded.timeout_ms = Some(i as u64);
                }
            }

            // Merge spec_extra into spec
            if let Some(value) = params.get("spec_extra") {
                if let Some(arr) = value.as_array() {
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            expanded.spec.push(s.to_string());
                        }
                    }
                }
            }

            expanded
        })
        .collect()
}

/// Expand all parameterized cases in a test file.
pub fn expand_test_file(mut test_file: TestFile) -> TestFile {
    let expanded_cases: Vec<Case> = test_file.case.drain(..).flat_map(expand_case).collect();
    test_file.case = expanded_cases;
    test_file
}

/// An error indicating an unknown preview feature name was used in a test.
#[derive(Debug, Clone)]
pub struct UnknownPreviewFeatureError {
    /// The invalid feature name found in the test.
    pub feature_name: String,
    /// The name of the test case using this feature.
    pub test_name: String,
    /// The section ID the test belongs to.
    pub section_id: String,
}

impl std::fmt::Display for UnknownPreviewFeatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown preview feature '{}' in test '{}::{}'; valid features are: {}",
            self.feature_name,
            self.section_id,
            self.test_name,
            PreviewFeature::all_names()
        )
    }
}

impl std::error::Error for UnknownPreviewFeatureError {}

/// Validate all preview feature names in a test file.
///
/// Returns a list of errors for any unknown preview feature names.
/// An empty list means all preview features are valid (or no preview features are used).
pub fn validate_preview_features(test_file: &TestFile) -> Vec<UnknownPreviewFeatureError> {
    let mut errors = Vec::new();

    for case in &test_file.case {
        if let Some(ref feature_name) = case.preview {
            // Try to parse as a valid PreviewFeature
            if feature_name.parse::<PreviewFeature>().is_err() {
                errors.push(UnknownPreviewFeatureError {
                    feature_name: feature_name.clone(),
                    test_name: case.name.clone(),
                    section_id: test_file.section.id.clone(),
                });
            }
        }
    }

    errors
}

/// Recursively collect all files with the given extension from a directory.
pub fn collect_files_by_ext(dir: &Path, ext: &str, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files_by_ext(&path, ext, files);
            } else if path.extension().is_some_and(|e| e == ext) {
                files.push(path);
            }
        }
    }
}

/// Recursively collect all TOML files from a directory.
///
/// This is a convenience wrapper around [`collect_files_by_ext`].
pub fn collect_toml_files(dir: &Path, files: &mut Vec<PathBuf>) {
    collect_files_by_ext(dir, "toml", files);
}

/// Load all test files from a directory (including subdirectories).
///
/// This function validates that all preview feature names in tests are known.
/// If any unknown preview features are found, an error is printed for each
/// invalid feature and the function panics with a summary.
///
/// # Panics
///
/// Panics if any test uses an unknown preview feature name. This prevents
/// tests from being silently skipped due to typos in feature names.
pub fn load_test_files(cases_dir: &Path) -> Vec<(String, TestFile)> {
    let mut specs = Vec::new();
    let mut preview_errors: Vec<UnknownPreviewFeatureError> = Vec::new();

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
                // Expand any parameterized test cases
                let spec = expand_test_file(spec);

                // Validate preview feature names
                preview_errors.extend(validate_preview_features(&spec));

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

    // Report all preview feature errors and fail if any were found
    if !preview_errors.is_empty() {
        eprintln!(
            "\nError: Found {} unknown preview feature name(s):",
            preview_errors.len()
        );
        for error in &preview_errors {
            eprintln!("  - {}", error);
        }
        panic!(
            "Test loading failed: {} test(s) use unknown preview feature names. \
             See errors above for details.",
            preview_errors.len()
        );
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

/// Map emit stage flag to the header name used in the compiler output.
/// For example, "rir" -> "RIR", "tokens" -> "Tokens"
fn stage_to_header_name(stage: &str) -> &'static str {
    match stage {
        "tokens" => "Tokens",
        "ast" => "AST",
        "rir" => "RIR",
        "air" => "AIR",
        "cfg" => "CFG",
        "mir" => "MIR",
        "asm" => "ASM",
        _ => panic!("Unknown stage: {}", stage),
    }
}

/// Run a golden test for a specific IR stage.
///
/// This helper runs `rue --emit <stage>` on the source file and compares
/// the output against the expected golden output.
fn run_golden_ir_test(
    rue_binary: &Path,
    source_path: &Path,
    stage: &str,
    expected: &str,
    build_command: impl Fn(&Path) -> Command,
) -> TestResult {
    let output = build_command(rue_binary)
        .arg("--emit")
        .arg(stage)
        .arg(source_path)
        .output()
        .map_err(|e| format!("Failed to run rue --emit {}: {}", stage, e))?;

    if !output.status.success() {
        return Err(format!(
            "rue --emit {} failed:\n{}",
            stage,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let actual = String::from_utf8_lossy(&output.stdout);
    // Strip the "=== STAGE ===" or "=== STAGE (target) ===" header for golden comparison
    let header_name = stage_to_header_name(stage);
    let actual = strip_emit_header(&actual, header_name);
    check_golden(&actual, expected, header_name)
}

/// Helper to read all bytes from a reader.
fn read_all_bytes<R: IoRead>(mut reader: R) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap_or_default();
    bytes
}

/// Run a command with a timeout and optional stdin input.
///
/// This function spawns a child process, writes to its stdin if provided,
/// and polls for completion, killing the process if it exceeds the specified timeout.
///
/// # Arguments
/// * `cmd` - The command to run (already configured with arguments)
/// * `timeout` - Maximum duration to wait for the process to complete
/// * `stdin_input` - Optional input to write to the process's stdin
///
/// # Returns
/// * `Ok(Output)` - The process output (stdout, stderr, exit status)
/// * `Err(String)` - Error message if the process timed out or failed to start
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
    stdin_input: Option<&str>,
) -> Result<Output, String> {
    let mut child = cmd
        .stdin(if stdin_input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn process: {}", e))?;

    // Write stdin input if provided
    if let Some(input) = stdin_input {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| format!("Failed to write stdin: {}", e))?;
            // Closing stdin signals EOF to the child process
        }
    }

    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process finished - collect output
                let stdout = child.stdout.take().map(read_all_bytes).unwrap_or_default();
                let stderr = child.stderr.take().map(read_all_bytes).unwrap_or_default();
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                // Still running - check timeout
                if start.elapsed() > timeout {
                    // Kill the process and return timeout error
                    let _ = child.kill();
                    let _ = child.wait(); // Reap the zombie process
                    return Err(format!(
                        "Test execution timed out after {} ms",
                        timeout.as_millis()
                    ));
                }
                // Sleep briefly before polling again
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                return Err(format!("Failed to wait for process: {}", e));
            }
        }
    }
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

    // Write auxiliary files for multi-file tests (module imports)
    let mut aux_paths = Vec::new();
    for (filename, content) in &case.aux_files {
        // Create subdirectories if needed (e.g., "foo/bar.rue")
        let aux_path = temp_dir.path().join(filename);
        if let Some(parent) = aux_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create dir for {}: {}", filename, e))?;
        }
        fs::write(&aux_path, content)
            .map_err(|e| format!("Failed to write aux file {}: {}", filename, e))?;
        aux_paths.push(aux_path);
    }

    // Build base command with target, preview, and optimization flags if needed
    let build_command = |binary: &Path| -> Command {
        let mut cmd = Command::new(binary);
        if let Some(ref target) = case.target {
            cmd.arg("--target").arg(target);
        }
        if let Some(ref feature) = case.preview {
            cmd.arg("--preview").arg(feature);
        }
        if let Some(level) = case.opt_level {
            cmd.arg(format!("-O{}", level));
        }
        cmd
    };

    // Check for golden IR tests (tokens, AST, RIR, AIR, CFG, MIR)
    if case.expected_tokens.is_some()
        || case.expected_ast.is_some()
        || case.expected_rir.is_some()
        || case.expected_air.is_some()
        || case.expected_cfg.is_some()
        || case.expected_mir.is_some()
    {
        // Run dump commands and check golden output
        if let Some(ref expected) = case.expected_tokens {
            run_golden_ir_test(rue_binary, &source_path, "tokens", expected, &build_command)?;
        }

        if let Some(ref expected) = case.expected_ast {
            run_golden_ir_test(rue_binary, &source_path, "ast", expected, &build_command)?;
        }

        if let Some(ref expected) = case.expected_rir {
            run_golden_ir_test(rue_binary, &source_path, "rir", expected, &build_command)?;
        }

        if let Some(ref expected) = case.expected_air {
            run_golden_ir_test(rue_binary, &source_path, "air", expected, &build_command)?;
        }

        if let Some(ref expected) = case.expected_cfg {
            run_golden_ir_test(rue_binary, &source_path, "cfg", expected, &build_command)?;
        }

        if let Some(ref expected) = case.expected_mir {
            // MIR golden tests require an explicit target since MIR is architecture-specific.
            if case.target.is_none() {
                return Err(
                    "MIR golden tests require a 'target' field (e.g., target = \"x86-64-linux\")"
                        .to_string(),
                );
            }
            run_golden_ir_test(rue_binary, &source_path, "mir", expected, &build_command)?;
        }

        return Ok(());
    }

    // Compile with rue
    // By default, aux_files are just written to disk for @import to find.
    // When pass_aux_files is true, they're passed on the command line for
    // multi-file compilation (needed when tests call functions from imported modules).
    let mut compile_cmd = build_command(rue_binary);
    compile_cmd.arg(&source_path);
    if case.pass_aux_files {
        for aux_path in &aux_paths {
            compile_cmd.arg(aux_path);
        }
    }
    compile_cmd.arg("-o").arg(&output_path);
    let compile_output = compile_cmd
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

    // Run the compiled binary with timeout and optional stdin
    let timeout = Duration::from_millis(case.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let run_output = run_with_timeout(Command::new(&output_path), timeout, case.stdin.as_deref())?;

    let actual_exit_code = run_output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&run_output.stderr);

    // Handle runtime error tests
    if let Some(ref expected_error) = case.runtime_error {
        let expected_exit = case.runtime_exit_code.unwrap_or(RUNTIME_ERROR_EXIT_CODE);

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

    // Check expected stdout output (e.g., from @dbg calls)
    if let Some(ref expected) = case.expected_stdout {
        let stdout = String::from_utf8_lossy(&run_output.stdout);
        let expected_normalized = normalize_golden(expected);
        let actual_normalized = normalize_golden(&stdout);
        if actual_normalized != expected_normalized {
            return Err(format!(
                "Stdout mismatch:\n--- expected ---\n{}\n--- actual ---\n{}\n  source: {}",
                expected_normalized, actual_normalized, case.source
            ));
        }
    }

    // Check stderr contains expected substring (for non-error cases)
    if let Some(ref expected) = case.stderr_contains {
        if !stderr.contains(expected.as_str()) {
            return Err(format!(
                "Stderr mismatch:\n  expected to contain: {}\n  actual stderr: {}\n  source: {}",
                expected, stderr, case.source
            ));
        }
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

/// Find a directory by checking an environment variable, then a list of possible paths.
///
/// This function provides a consistent way to locate directories across different
/// working directory contexts (project root, crate directory, etc.).
///
/// # Arguments
/// * `env_var` - Environment variable to check first (e.g., "RUE_SPEC_DIR")
/// * `possible_paths` - List of relative paths to try if env var is not set
/// * `fallback` - Default path to return if no existing path is found
///
/// # Returns
/// The first existing path found, or the fallback if none exist.
pub fn find_dir(env_var: &str, possible_paths: &[&str], fallback: &str) -> PathBuf {
    std::env::var(env_var)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            for path in possible_paths {
                let p = Path::new(path);
                if p.exists() {
                    return p.to_path_buf();
                }
            }
            Path::new(fallback).to_path_buf()
        })
}

/// Find the rue binary in common locations.
///
/// When multiple Buck2 build outputs exist (each in a UUID-named directory),
/// this function selects the most recently modified binary to avoid using
/// stale binaries from previous builds.
pub fn find_rue_binary() -> PathBuf {
    std::env::var("RUE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Try to find it in common buck output locations
            // First try the buck2 output (has UUID in path)
            let buck_root = Path::new("buck-out/v2/gen/root");
            if buck_root.exists() {
                if let Ok(entries) = std::fs::read_dir(buck_root) {
                    // Collect all valid rue binaries with their modification times
                    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = entries
                        .flatten()
                        .filter_map(|entry| {
                            let rue_path = entry.path().join("crates/rue/__rue__/rue");
                            if rue_path.exists() {
                                // Get modification time; skip if we can't read it
                                rue_path.metadata().ok().and_then(|meta| {
                                    meta.modified().ok().map(|mtime| (rue_path, mtime))
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    // Sort by modification time (newest first) and return the most recent
                    candidates.sort_by(|a, b| b.1.cmp(&a.1));
                    if let Some((path, _)) = candidates.into_iter().next() {
                        return path;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_placeholders_basic() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i32".to_string()));
        params.insert("value".to_string(), toml::Value::Integer(42));

        let result = substitute_placeholders("fn main() -> {type} { {value} }", &params);
        assert_eq!(result, "fn main() -> i32 { 42 }");
    }

    #[test]
    fn test_substitute_placeholders_multiple_occurrences() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i64".to_string()));

        let result = substitute_placeholders("{type} and {type} again", &params);
        assert_eq!(result, "i64 and i64 again");
    }

    #[test]
    fn test_substitute_placeholders_no_match() {
        let params = HashMap::new();
        let result = substitute_placeholders("no placeholders here", &params);
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn test_expand_case_no_params() {
        let case = Case {
            name: "test".to_string(),
            source: "fn main() {}".to_string(),
            exit_code: Some(0),
            compile_fail: false,
            compile_only: false,
            error_contains: None,
            expected_error: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec!["1.0:1".to_string()],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![],
            aux_files: HashMap::new(),
            pass_aux_files: false,
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].name, "test");
    }

    #[test]
    fn test_expand_case_with_params() {
        let mut param1 = HashMap::new();
        param1.insert("type".to_string(), toml::Value::String("i8".to_string()));
        param1.insert("exit_code".to_string(), toml::Value::Integer(42));

        let mut param2 = HashMap::new();
        param2.insert("type".to_string(), toml::Value::String("i16".to_string()));
        param2.insert("exit_code".to_string(), toml::Value::Integer(100));

        let case = Case {
            name: "{type}_return".to_string(),
            source: "fn main() -> {type} { 0 }".to_string(),
            exit_code: None, // Will be overridden
            compile_fail: false,
            compile_only: false,
            error_contains: None,
            expected_error: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec!["3.1:1".to_string()],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![ParamSet { values: param1 }, ParamSet { values: param2 }],
            aux_files: HashMap::new(),
            pass_aux_files: false,
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 2);

        assert_eq!(expanded[0].name, "i8_return");
        assert_eq!(expanded[0].source, "fn main() -> i8 { 0 }");
        assert_eq!(expanded[0].exit_code, Some(42));
        assert!(expanded[0].params.is_empty());

        assert_eq!(expanded[1].name, "i16_return");
        assert_eq!(expanded[1].source, "fn main() -> i16 { 0 }");
        assert_eq!(expanded[1].exit_code, Some(100));
        assert!(expanded[1].params.is_empty());
    }

    #[test]
    fn test_expand_case_spec_extra() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i8".to_string()));
        params.insert(
            "spec_extra".to_string(),
            toml::Value::Array(vec![toml::Value::String("3.1:2".to_string())]),
        );

        let case = Case {
            name: "{type}_test".to_string(),
            source: "fn main() {}".to_string(),
            exit_code: Some(0),
            compile_fail: false,
            compile_only: false,
            error_contains: None,
            expected_error: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec!["3.1:1".to_string()],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![ParamSet { values: params }],
            aux_files: HashMap::new(),
            pass_aux_files: false,
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].spec, vec!["3.1:1", "3.1:2"]);
    }

    #[test]
    fn test_expand_case_compile_fail_override() {
        let mut params = HashMap::new();
        params.insert("type".to_string(), toml::Value::String("i8".to_string()));
        params.insert("compile_fail".to_string(), toml::Value::Boolean(true));
        params.insert(
            "error_msg".to_string(),
            toml::Value::String("type mismatch".to_string()),
        );

        let case = Case {
            name: "{type}_error".to_string(),
            source: "fn main() -> {type} { true }".to_string(),
            exit_code: None,
            compile_fail: false, // Will be overridden
            compile_only: false,
            error_contains: Some("{error_msg}".to_string()),
            expected_error: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec![],
            expected_stdout: None,
            preview: None,
            preview_should_pass: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![ParamSet { values: params }],
            aux_files: HashMap::new(),
            pass_aux_files: false,
            only_on: vec![],
        };

        let expanded = expand_case(case);
        assert_eq!(expanded.len(), 1);
        assert!(expanded[0].compile_fail);
        assert_eq!(
            expanded[0].error_contains,
            Some("type mismatch".to_string())
        );
    }

    #[test]
    fn test_toml_value_to_string() {
        assert_eq!(
            toml_value_to_string(&toml::Value::String("hello".to_string())),
            "hello"
        );
        assert_eq!(toml_value_to_string(&toml::Value::Integer(42)), "42");
        assert_eq!(toml_value_to_string(&toml::Value::Float(3.14)), "3.14");
        assert_eq!(toml_value_to_string(&toml::Value::Boolean(true)), "true");
    }

    // Tests for normalize_golden
    #[test]
    fn test_normalize_golden_trims_trailing_whitespace() {
        let input = "line1   \nline2  \nline3\t\t";
        let expected = "line1\nline2\nline3";
        assert_eq!(normalize_golden(input), expected);
    }

    #[test]
    fn test_normalize_golden_trims_leading_and_trailing_empty_lines() {
        let input = "\n\nline1\nline2\n\n";
        let expected = "line1\nline2";
        assert_eq!(normalize_golden(input), expected);
    }

    #[test]
    fn test_normalize_golden_preserves_internal_indentation() {
        // Leading whitespace on the first line is trimmed by the final .trim() call,
        // but internal indentation (relative to the first line) is preserved.
        let input = "line1\n    indented line\n  less indented";
        let expected = "line1\n    indented line\n  less indented";
        assert_eq!(normalize_golden(input), expected);
    }

    #[test]
    fn test_normalize_golden_empty_string() {
        assert_eq!(normalize_golden(""), "");
    }

    #[test]
    fn test_normalize_golden_only_whitespace() {
        assert_eq!(normalize_golden("   \n  \t  \n  "), "");
    }

    #[test]
    fn test_normalize_golden_single_line() {
        assert_eq!(normalize_golden("hello world  "), "hello world");
    }

    #[test]
    fn test_normalize_golden_mixed_line_endings() {
        // normalize_golden uses .lines() which handles \r\n, \n, and \r
        let input = "line1\r\nline2\nline3";
        let result = normalize_golden(input);
        // Result should have normalized line endings
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
    }

    // Tests for normalize_error_output
    #[test]
    fn test_normalize_error_output_replaces_path() {
        let source_path = Path::new("/tmp/test123/source.rue");
        let input = "error[E001]: type mismatch at /tmp/test123/source.rue:5:10";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "error[E001]: type mismatch at <source>:5:10");
    }

    #[test]
    fn test_normalize_error_output_multiple_occurrences() {
        let source_path = Path::new("/path/to/file.rue");
        let input = "error at /path/to/file.rue:1\nnote: see /path/to/file.rue:2";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "error at <source>:1\nnote: see <source>:2");
    }

    #[test]
    fn test_normalize_error_output_no_path_present() {
        let source_path = Path::new("/nonexistent/path.rue");
        let input = "error: something went wrong";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "error: something went wrong");
    }

    #[test]
    fn test_normalize_error_output_also_normalizes_whitespace() {
        let source_path = Path::new("/tmp/test.rue");
        let input = "/tmp/test.rue:1  \n  /tmp/test.rue:2  ";
        let result = normalize_error_output(input, source_path);
        assert_eq!(result, "<source>:1\n  <source>:2");
    }

    // Tests for strip_emit_header
    #[test]
    fn test_strip_emit_header_simple() {
        let input = "=== RIR ===\nfn main() {\n  ret 0\n}";
        let result = strip_emit_header(input, "RIR");
        assert_eq!(result, "fn main() {\n  ret 0\n}");
    }

    #[test]
    fn test_strip_emit_header_with_target() {
        let input = "=== MIR (x86-64-linux) ===\nmov rax, 0\nret";
        let result = strip_emit_header(input, "MIR");
        assert_eq!(result, "mov rax, 0\nret");
    }

    #[test]
    fn test_strip_emit_header_with_macos_target() {
        let input = "=== MIR (aarch64-macos) ===\nmov x0, #0\nret";
        let result = strip_emit_header(input, "MIR");
        assert_eq!(result, "mov x0, #0\nret");
    }

    #[test]
    fn test_strip_emit_header_no_header_present() {
        let input = "fn main() {\n  ret 0\n}";
        let result = strip_emit_header(input, "RIR");
        assert_eq!(result, "fn main() {\n  ret 0\n}");
    }

    #[test]
    fn test_strip_emit_header_wrong_stage() {
        let input = "=== AST ===\nsome ast content";
        let result = strip_emit_header(input, "RIR");
        // Should not strip AST header when looking for RIR
        assert_eq!(result, "=== AST ===\nsome ast content");
    }

    #[test]
    fn test_strip_emit_header_multiple_headers() {
        let input = "=== Tokens ===\ntoken1\n=== AST ===\nast content";
        let result = strip_emit_header(input, "Tokens");
        assert_eq!(result, "token1\n=== AST ===\nast content");
    }

    #[test]
    fn test_strip_emit_header_preserves_similar_text() {
        // Ensure we don't strip lines that merely contain the stage name
        let input = "=== RIR ===\nThis is RIR output\nRIR is great";
        let result = strip_emit_header(input, "RIR");
        assert_eq!(result, "This is RIR output\nRIR is great");
    }

    // Tests for check_golden
    #[test]
    fn test_check_golden_matching() {
        let actual = "line1\nline2";
        let expected = "line1\nline2";
        assert!(check_golden(actual, expected, "Test").is_ok());
    }

    #[test]
    fn test_check_golden_matching_with_whitespace_differences() {
        let actual = "line1  \nline2\t";
        let expected = "line1\nline2";
        assert!(check_golden(actual, expected, "Test").is_ok());
    }

    #[test]
    fn test_check_golden_mismatch() {
        let actual = "line1\nline2";
        let expected = "line1\nline3";
        let result = check_golden(actual, expected, "Test");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Test mismatch"));
        assert!(err.contains("expected"));
        assert!(err.contains("actual"));
    }

    #[test]
    fn test_check_golden_empty_strings() {
        assert!(check_golden("", "", "Test").is_ok());
    }

    #[test]
    fn test_check_golden_whitespace_only() {
        assert!(check_golden("  \n  ", "\t\n\t", "Test").is_ok());
    }

    #[test]
    fn test_check_golden_leading_trailing_differences() {
        let actual = "\n\nline1\n\n";
        let expected = "line1";
        assert!(check_golden(actual, expected, "Test").is_ok());
    }

    // Tests for run_with_timeout
    #[test]
    fn test_run_with_timeout_completes_normally() {
        // A simple command that completes quickly
        let cmd = Command::new("echo");
        let result = run_with_timeout(cmd, Duration::from_secs(5), None);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn test_run_with_timeout_captures_stdout() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let result = run_with_timeout(cmd, Duration::from_secs(5), None);
        assert!(result.is_ok());
        let output = result.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("hello"));
    }

    #[test]
    fn test_run_with_timeout_kills_slow_process() {
        // Sleep for 10 seconds but timeout after 100ms
        let mut cmd = Command::new("sleep");
        cmd.arg("10");
        let result = run_with_timeout(cmd, Duration::from_millis(100), None);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("timed out"),
            "Error should mention timeout: {}",
            err
        );
    }

    #[test]
    fn test_run_with_timeout_captures_exit_code() {
        // Use a command that exits with a non-zero status
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exit 42");
        let result = run_with_timeout(cmd, Duration::from_secs(5), None);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.status.code(), Some(42));
    }

    #[test]
    fn test_run_with_timeout_pipes_stdin() {
        // Use cat to echo back stdin
        let cmd = Command::new("cat");
        let result = run_with_timeout(cmd, Duration::from_secs(5), Some("hello from stdin"));
        assert!(result.is_ok());
        let output = result.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout, "hello from stdin");
    }

    #[test]
    fn test_run_with_timeout_stdin_with_newlines() {
        // Use cat to echo back stdin with newlines
        let cmd = Command::new("cat");
        let result = run_with_timeout(cmd, Duration::from_secs(5), Some("line1\nline2\n"));
        assert!(result.is_ok());
        let output = result.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout, "line1\nline2\n");
    }

    // Tests for validate_preview_features

    fn make_test_case(name: &str, preview: Option<&str>) -> Case {
        Case {
            name: name.to_string(),
            source: "fn main() {}".to_string(),
            exit_code: Some(0),
            compile_fail: false,
            compile_only: false,
            error_contains: None,
            expected_error: None,
            expected_tokens: None,
            expected_ast: None,
            expected_rir: None,
            expected_air: None,
            expected_mir: None,
            expected_cfg: None,
            runtime_error: None,
            runtime_exit_code: None,
            skip: false,
            warning_contains: None,
            expected_warning_count: None,
            no_warnings: false,
            spec: vec![],
            expected_stdout: None,
            preview: preview.map(|s| s.to_string()),
            preview_should_pass: false,
            target: None,
            opt_level: None,
            timeout_ms: None,
            stdin: None,
            stderr_contains: None,
            params: vec![],
            aux_files: HashMap::new(),
            pass_aux_files: false,
            only_on: vec![],
        }
    }

    fn make_test_file(section_id: &str, cases: Vec<Case>) -> TestFile {
        TestFile {
            section: Section {
                id: section_id.to_string(),
                name: "Test Section".to_string(),
                description: String::new(),
                spec_chapter: None,
            },
            case: cases,
        }
    }

    #[test]
    fn test_validate_preview_features_no_preview() {
        // Test with no preview features - should return no errors
        let test_file = make_test_file(
            "test",
            vec![
                make_test_case("basic_test", None),
                make_test_case("another_test", None),
            ],
        );

        let errors = validate_preview_features(&test_file);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_preview_features_valid_feature() {
        // Test with a valid preview feature
        let test_file = make_test_file(
            "test",
            vec![make_test_case("preview_test", Some("test_infra"))],
        );

        let errors = validate_preview_features(&test_file);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_preview_features_unknown_feature() {
        // Test with an unknown preview feature
        let test_file = make_test_file(
            "expressions",
            vec![make_test_case("bad_test", Some("nonexistent_feature"))],
        );

        let errors = validate_preview_features(&test_file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].feature_name, "nonexistent_feature");
        assert_eq!(errors[0].test_name, "bad_test");
        assert_eq!(errors[0].section_id, "expressions");
    }

    #[test]
    fn test_validate_preview_features_typo() {
        // Test with a typo in the preview feature name (common case)
        let test_file = make_test_file(
            "items",
            vec![
                make_test_case("typo_test", Some("test_infr")), // Missing 'a'
            ],
        );

        let errors = validate_preview_features(&test_file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].feature_name, "test_infr");
    }

    #[test]
    fn test_validate_preview_features_multiple_errors() {
        // Test with multiple unknown preview features
        let test_file = make_test_file(
            "test",
            vec![
                make_test_case("good_test", Some("test_infra")), // Valid
                make_test_case("bad_test_1", Some("unknown1")),  // Invalid
                make_test_case("normal_test", None),             // No preview
                make_test_case("bad_test_2", Some("unknown2")),  // Invalid
            ],
        );

        let errors = validate_preview_features(&test_file);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].feature_name, "unknown1");
        assert_eq!(errors[1].feature_name, "unknown2");
    }

    #[test]
    fn test_unknown_preview_feature_error_display() {
        let error = UnknownPreviewFeatureError {
            feature_name: "bad_feature".to_string(),
            test_name: "my_test".to_string(),
            section_id: "section.id".to_string(),
        };

        let msg = error.to_string();
        assert!(msg.contains("bad_feature"), "Should contain feature name");
        assert!(msg.contains("my_test"), "Should contain test name");
        assert!(msg.contains("section.id"), "Should contain section ID");
        assert!(msg.contains("test_infra"), "Should list valid features");
    }
}
