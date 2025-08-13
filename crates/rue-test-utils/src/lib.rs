//! Common test utilities for the Rue compiler
//!
//! This crate provides shared testing infrastructure including:
//! - Compilation helpers
//! - Program execution utilities
//! - Output normalization
//! - Test fixture management

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use once_cell::sync::Lazy;
use regex::Regex;
use std::fs;
use std::process::{Command, Stdio};
use tempfile::{NamedTempFile, TempDir};

/// Get the project root directory
pub fn get_project_root() -> Utf8PathBuf {
    // Try different strategies to find project root

    // Strategy 1: Look for Cargo.toml in parent directories
    let mut current = std::env::current_dir().unwrap();
    loop {
        if current.join("Cargo.toml").exists() && current.join("crates").exists() {
            return Utf8PathBuf::try_from(current).unwrap();
        }
        if !current.pop() {
            break;
        }
    }

    // Strategy 2: Use environment variable if set
    if let Ok(root) = std::env::var("RUE_PROJECT_ROOT") {
        return Utf8PathBuf::from(root);
    }

    // Strategy 3: Assume we're in a subdirectory of the project
    Utf8PathBuf::from(".")
}

/// Result of compiling a Rue program
pub struct CompilationResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub output_path: Option<Utf8PathBuf>,
}

// ============================================================================
// High-level test assertions
// ============================================================================

/// Assert that a Rue program compiles successfully
pub fn assert_compiles(source: &str) -> Result<()> {
    let compiler = RueCompiler::new()?;
    let result = compiler.compile_source(source)?;

    if !result.success {
        anyhow::bail!(
            "Compilation failed:\nstdout: {}\nstderr: {}",
            result.stdout,
            result.stderr
        );
    }

    Ok(())
}

/// Assert that a Rue program fails to compile with expected error
pub fn assert_compile_error(source: &str, expected_error: &str) -> Result<()> {
    let compiler = RueCompiler::new()?;
    let result = compiler.compile_source(source)?;

    if result.success {
        anyhow::bail!("Expected compilation to fail, but it succeeded");
    }

    let error_msg = format!("{}\n{}", result.stdout, result.stderr);
    if !error_msg
        .to_lowercase()
        .contains(&expected_error.to_lowercase())
    {
        anyhow::bail!(
            "Expected error containing '{}', got:\nstdout: {}\nstderr: {}",
            expected_error,
            result.stdout,
            result.stderr
        );
    }

    Ok(())
}

/// Assert that a Rue program runs with expected exit code
pub fn assert_runs_with_exit_code(source: &str, expected_exit_code: i32) -> Result<()> {
    let compiler = RueCompiler::new()?;
    let result = compiler.compile_and_run_source(source)?;

    if result.exit_code != expected_exit_code {
        anyhow::bail!(
            "Exit code mismatch: expected {}, got {}\nstdout: {}\nstderr: {}",
            expected_exit_code,
            result.exit_code,
            result.stdout,
            result.stderr
        );
    }

    Ok(())
}

/// Assert that a Rue program produces expected output
pub fn assert_program_output(source: &str, expected_stdout: &str) -> Result<()> {
    let compiler = RueCompiler::new()?;
    let result = compiler.compile_and_run_source(source)?;

    if result.stdout.trim() != expected_stdout.trim() {
        anyhow::bail!(
            "Output mismatch:\nExpected:\n{}\nGot:\n{}",
            expected_stdout,
            result.stdout
        );
    }

    Ok(())
}

/// Result of running a compiled program
pub struct ExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
}

/// Helper for compiling Rue programs
pub struct RueCompiler {
    rue_binary: Utf8PathBuf,
    temp_dir: TempDir,
}

impl RueCompiler {
    /// Create a new compiler instance
    pub fn new() -> Result<Self> {
        let project_root = get_project_root();

        // Find the rue binary
        let rue_binary = if let Ok(path) = std::env::var("RUE_BINARY") {
            Utf8PathBuf::from(path)
        } else {
            // Try common locations
            for path in [
                "target/debug/rue",
                "target/release/rue",
                "buck-out/v2/gen/root/crates/rue/__rue__/rue",
            ] {
                let full_path = project_root.join(path);
                if full_path.exists() {
                    return Ok(Self {
                        rue_binary: full_path,
                        temp_dir: TempDir::new()?,
                    });
                }
            }

            // Fall back to cargo run
            Utf8PathBuf::from("cargo")
        };

        Ok(Self {
            rue_binary,
            temp_dir: TempDir::new()?,
        })
    }

    /// Compile a Rue program from source text
    pub fn compile_source(&self, source: &str) -> Result<CompilationResult> {
        // Create temporary source file
        let source_file = self.temp_dir.path().join("test.rue");
        fs::write(&source_file, source)?;

        let source_path = Utf8PathBuf::try_from(source_file)?;
        self.compile_file(&source_path)
    }

    /// Compile a Rue file
    pub fn compile_file(&self, source_path: &Utf8Path) -> Result<CompilationResult> {
        let output_path = self.temp_dir.path().join("test_output");
        let output_path = Utf8PathBuf::try_from(output_path)?;

        let output = if self.rue_binary.ends_with("cargo") {
            // Use cargo run
            Command::new(&self.rue_binary)
                .args(["run", "-p", "rue", "--quiet", "--"])
                .arg(source_path)
                .arg("-o")
                .arg(&output_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()?
        } else {
            // Use binary directly
            Command::new(&self.rue_binary)
                .arg(source_path)
                .arg("-o")
                .arg(&output_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()?
        };

        Ok(CompilationResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            output_path: if output.status.success() {
                Some(output_path)
            } else {
                None
            },
        })
    }

    /// Compile and run a Rue program
    pub fn compile_and_run_source(&self, source: &str) -> Result<ExecutionResult> {
        self.compile_and_run(source)
    }

    pub fn compile_and_run(&self, source: &str) -> Result<ExecutionResult> {
        let compilation = self.compile_source(source)?;

        if !compilation.success {
            return Ok(ExecutionResult {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Compilation failed:\n{}", compilation.stderr),
                duration_ms: 0,
            });
        }

        let binary_path = compilation
            .output_path
            .ok_or_else(|| anyhow::anyhow!("No output path from compilation"))?;

        self.run_binary(&binary_path)
    }

    /// Run a compiled binary
    pub fn run_binary(&self, binary_path: &Utf8Path) -> Result<ExecutionResult> {
        let start = std::time::Instant::now();

        let output = Command::new(binary_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        let duration_ms = start.elapsed().as_millis();

        Ok(ExecutionResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms,
        })
    }

    /// Run a binary with input
    pub fn run_with_input(&self, binary_path: &Utf8Path, input: &str) -> Result<ExecutionResult> {
        let start = std::time::Instant::now();

        let mut child = Command::new(binary_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Write input
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(input.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        let duration_ms = start.elapsed().as_millis();

        Ok(ExecutionResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms,
        })
    }
}

/// Create a test fixture file
pub struct TestFixture {
    _file: NamedTempFile,
    path: Utf8PathBuf,
}

impl TestFixture {
    /// Create a new test fixture with given content
    pub fn new(_name: &str, content: &str) -> Result<Self> {
        let mut file = NamedTempFile::new()?;

        // Write content
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        file.flush()?;

        let path = Utf8PathBuf::try_from(file.path().to_path_buf())?;

        Ok(Self { _file: file, path })
    }

    /// Get the path to the fixture file
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

/// Normalize compiler output for stable comparison
pub fn normalize_output(text: &str) -> String {
    let mut result = text.to_string();

    // Normalize paths
    static PATH_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(/[^\s:]+|[A-Z]:\\[^\s:]+|\\\\[^\s:]+)").expect("Invalid path regex")
    });

    result = PATH_REGEX.replace_all(&result, "[PATH]").to_string();

    // Normalize line endings
    result = result.replace("\r\n", "\n");

    // Normalize temporary file names
    static TEMP_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"test_temp_\d+_ThreadId\([^)]+\)").expect("Invalid temp regex"));

    result = TEMP_REGEX
        .replace_all(&result, "test_temp_[ID]")
        .to_string();

    result
}

/// Assert that a program produces expected output
pub fn assert_output(source: &str, expected_stdout: &str) -> Result<()> {
    let compiler = RueCompiler::new()?;
    let result = compiler.compile_and_run(source)?;

    let actual = result.stdout.trim();
    let expected = expected_stdout.trim();

    if actual != expected {
        anyhow::bail!(
            "Output mismatch:\nExpected:\n{}\nActual:\n{}",
            expected,
            actual
        );
    }

    Ok(())
}

/// Simple helper functions for common testing patterns
/// These reduce boilerplate without being too magical
/// Compile source code to AST
pub fn compile_to_ast(source: &str) -> Result<rue_ast::CstRoot> {
    rue_parser::parse_with_recovery(source, "test.rue").map_err(|diagnostics| {
        anyhow::anyhow!("Parse failed with {} diagnostics", diagnostics.len())
    })
}

/// Compile source code through typechecking to get typed HIR
pub fn compile_to_typecheck(source: &str) -> Result<rue_semantic::AnalysisResult> {
    let cst = compile_to_ast(source)?;
    rue_semantic::analyze_cst(&cst)
        .map_err(|e| anyhow::anyhow!("Semantic analysis failed: {}", e.message))
}

/// Compile source code to MIR
pub fn compile_to_mir(source: &str) -> Result<rue_ir::mir::MirProgram> {
    let analysis = compile_to_typecheck(source)?;
    let type_context = rue_semantic::scope_to_type_context(&analysis.scope);
    let mir = rue_lowering::lower_hir_to_mir(&analysis.hir, type_context);
    Ok(mir)
}

/// Assert that an expression has the expected type
pub fn assert_type_of(source: &str, expected_type: &str) -> Result<()> {
    let analysis = compile_to_typecheck(source)?;

    // For now, just check that it typechecks successfully
    // In a more complete implementation, we'd need to extract the type of the expression
    // and compare it to the expected type string

    // Find the main function and check its return type
    if let Some(main_sig) = analysis.scope.functions.get("main") {
        let actual_type = format!("{}", main_sig.return_type);
        if actual_type != expected_type {
            anyhow::bail!(
                "Type mismatch: expected '{}', found '{}'",
                expected_type,
                actual_type
            );
        }
    } else {
        anyhow::bail!("No main function found to check type");
    }

    Ok(())
}

/// Assert that parsing fails for the given source
pub fn assert_parse_fails(source: &str) -> Result<()> {
    match rue_parser::parse_with_recovery(source, "test.rue") {
        Ok(_) => anyhow::bail!("Expected parsing to fail, but it succeeded"),
        Err(_diagnostics) => Ok(()),
    }
}

/// Assert that typechecking fails for the given source
pub fn assert_typecheck_fails(source: &str) -> Result<()> {
    let cst = compile_to_ast(source)?;
    match rue_semantic::analyze_cst(&cst) {
        Ok(_) => anyhow::bail!("Expected typechecking to fail, but it succeeded"),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_root() {
        let root = get_project_root();
        assert!(root.join("Cargo.toml").exists() || root.as_str() == ".");
    }

    #[test]
    fn test_normalize_output() {
        let input = "/home/user/file.rs:42: error\r\ntest_temp_123_ThreadId(456)";
        let normalized = normalize_output(input);

        assert!(normalized.contains("[PATH]:42: error"));
        assert!(normalized.contains("test_temp_[ID]"));
        assert!(!normalized.contains("\r"));
    }

    #[test]
    fn test_fixture_creation() {
        let fixture = TestFixture::new("test.rue", "fn main() -> i32 { 0 }")
            .expect("Failed to create fixture");

        assert!(fixture.path().exists());

        let content = fs::read_to_string(fixture.path()).expect("Failed to read fixture");
        assert_eq!(content, "fn main() -> i32 { 0 }");
    }

    #[test]
    fn test_helper_functions() {
        // Test compile_to_ast
        let source = "fn main() -> i32 { 42 }";
        let ast = compile_to_ast(source).expect("Should parse successfully");
        assert!(!ast.items.is_empty());

        // Test compile_to_typecheck
        let analysis = compile_to_typecheck(source).expect("Should typecheck successfully");
        assert!(analysis.scope.functions.contains_key("main"));

        // Test compile_to_mir
        let mir = compile_to_mir(source).expect("Should compile to MIR successfully");
        assert!(!mir.functions.is_empty());

        // Test assert_type_of
        assert_type_of(source, "i32").expect("Should have i32 return type");

        // Test assert_parse_fails
        assert_parse_fails("invalid syntax @@@@").expect("Should fail to parse");

        // Test assert_typecheck_fails
        assert_typecheck_fails("fn main() -> i32 { undefined_var }")
            .expect("Should fail typechecking");
    }
}
