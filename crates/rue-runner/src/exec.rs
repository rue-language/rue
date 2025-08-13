use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use regex::Regex;
use rue_snapshot::{Snapshot, SnapshotConfig};
use std::process::{Command, Stdio};
use tempfile::TempDir;

use crate::directives::{TestKind, TestSpec, build_test_spec};
use crate::discover::TestFile;
use crate::spec::SpecLoader;

/// Executes tests of various kinds
pub struct TestExecutor {
    rue_binary: Utf8PathBuf,
    spec_loader: SpecLoader,
}

/// Result of executing a test
#[derive(Debug, Clone)]
pub enum TestResult {
    Pass,
    Fail(String),
    Skip(String),
}

/// Output from running a test
#[derive(Debug, Clone)]
pub struct TestOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl TestExecutor {
    pub fn new(rue_binary: &Utf8Path, spec_loader: &SpecLoader) -> Result<Self> {
        Ok(Self {
            rue_binary: rue_binary.to_owned(),
            spec_loader: spec_loader.clone(),
        })
    }

    /// Execute a test file with all its directives
    pub fn execute_test(
        &self,
        test_file: &TestFile,
        snapshot_config: &SnapshotConfig,
    ) -> Result<TestResult> {
        if test_file.directives.is_empty() {
            return Ok(TestResult::Skip("No test directives found".to_string()));
        }

        // Build test spec from directives
        let test_spec = match build_test_spec(&test_file.directives) {
            Ok(spec) => spec,
            Err(e) => return Ok(TestResult::Fail(format!("Invalid test directives: {}", e))),
        };

        // Check if test should be skipped
        if let Some(skip_reason) = &test_spec.skip_reason {
            return Ok(TestResult::Skip(skip_reason.clone()));
        }

        // Validate spec references (currently just warn, don't fail)
        if test_spec.is_normative() {
            let spec_refs = test_spec.spec_references();
            if let Err(e) = self.spec_loader.validate_spec_references(&spec_refs) {
                // TODO: Re-enable spec validation once spec references are standardized
                tracing::warn!("Spec validation disabled: {}", e);
                // return Ok(TestResult::Fail(format!("Invalid spec reference: {}", e)));
            }
        }

        // Execute the test based on its kind
        self.execute_test_spec(test_file, &test_spec, snapshot_config)
    }

    fn execute_test_spec(
        &self,
        test_file: &TestFile,
        test_spec: &TestSpec,
        snapshot_config: &SnapshotConfig,
    ) -> Result<TestResult> {
        match test_spec.kind {
            TestKind::CompilePass => self.execute_compile_pass(test_file),
            TestKind::CompileFail => self.execute_compile_fail(test_file),
            TestKind::RunPass => self.execute_run_pass(test_file, test_spec),
            TestKind::RunFail => self.execute_run_fail(test_file, test_spec),
            TestKind::SnapshotMir => self.execute_snapshot_mir(test_file, snapshot_config),
            TestKind::SnapshotAsm => self.execute_snapshot_asm(test_file, snapshot_config),
        }
    }

    fn execute_compile_pass(&self, test_file: &TestFile) -> Result<TestResult> {
        let output = self.compile_file(&test_file.path)?;

        if output.exit_code == 0 {
            Ok(TestResult::Pass)
        } else {
            Ok(TestResult::Fail(format!(
                "Expected compilation to succeed, but it failed with exit code {}\nstderr: {}",
                output.exit_code, output.stderr
            )))
        }
    }

    fn execute_compile_fail(&self, test_file: &TestFile) -> Result<TestResult> {
        let output = self.compile_file(&test_file.path)?;

        if output.exit_code != 0 {
            Ok(TestResult::Pass)
        } else {
            Ok(TestResult::Fail(
                "Expected compilation to fail, but it succeeded".to_string(),
            ))
        }
    }

    fn execute_run_pass(&self, test_file: &TestFile, test_spec: &TestSpec) -> Result<TestResult> {
        // First compile
        let compile_output = self.compile_file(&test_file.path)?;
        if compile_output.exit_code != 0 {
            return Ok(TestResult::Fail(format!(
                "Compilation failed before running test: {}",
                compile_output.stderr
            )));
        }

        // Then run
        let run_output = self.run_compiled_binary(&test_file.path)?;

        // Check expected exit code (default to 0 for run-pass)
        let expected_exit = test_spec.expected_exit_code().unwrap_or(0);
        if run_output.exit_code != expected_exit {
            return Ok(TestResult::Fail(format!(
                "Exit code mismatch\nexpected: {}\nactual: {}",
                expected_exit, run_output.exit_code
            )));
        }

        // Check expected output if specified
        if let Some(expected_stdout) = test_spec.expected_stdout()
            && !self.matches_pattern(&run_output.stdout, expected_stdout)
        {
            return Ok(TestResult::Fail(format!(
                "stdout mismatch\nexpected pattern: {}\nactual: {}",
                expected_stdout, run_output.stdout
            )));
        }

        if let Some(expected_stderr) = test_spec.expected_stderr()
            && !self.matches_pattern(&run_output.stderr, expected_stderr)
        {
            return Ok(TestResult::Fail(format!(
                "stderr mismatch\nexpected pattern: {}\nactual: {}",
                expected_stderr, run_output.stderr
            )));
        }

        Ok(TestResult::Pass)
    }

    fn execute_run_fail(&self, test_file: &TestFile, test_spec: &TestSpec) -> Result<TestResult> {
        // First compile
        let compile_output = self.compile_file(&test_file.path)?;
        if compile_output.exit_code != 0 {
            return Ok(TestResult::Fail(format!(
                "Compilation failed before running test: {}",
                compile_output.stderr
            )));
        }

        // Then run
        let run_output = self.run_compiled_binary(&test_file.path)?;

        if run_output.exit_code == 0 {
            return Ok(TestResult::Fail(
                "Expected program to fail, but it succeeded".to_string(),
            ));
        }

        // Check expected exit code if specified
        if let Some(expected_exit_code) = test_spec.expected_exit_code()
            && run_output.exit_code != expected_exit_code
        {
            return Ok(TestResult::Fail(format!(
                "exit code mismatch\nexpected: {}\nactual: {}",
                expected_exit_code, run_output.exit_code
            )));
        }

        // Check expected stderr if specified
        if let Some(expected_stderr) = test_spec.expected_stderr()
            && !self.matches_pattern(&run_output.stderr, expected_stderr)
        {
            return Ok(TestResult::Fail(format!(
                "stderr mismatch\nexpected pattern: {}\nactual: {}",
                expected_stderr, run_output.stderr
            )));
        }

        Ok(TestResult::Pass)
    }

    fn execute_snapshot_mir(
        &self,
        test_file: &TestFile,
        snapshot_config: &SnapshotConfig,
    ) -> Result<TestResult> {
        let mir_output = self.emit_mir(&test_file.path)?;

        if mir_output.exit_code != 0 {
            return Ok(TestResult::Fail(format!(
                "Failed to emit MIR: {}",
                mir_output.stderr
            )));
        }

        let snapshot_name = format!("{}.mir", test_file.path.file_stem().unwrap());
        let config = SnapshotConfig::default()
            .with_snapshot_dir(snapshot_config.snapshot_dir().to_owned())
            .with_update_snapshots(snapshot_config.update_snapshots())
            .with_review_mode(snapshot_config.review_mode());
        let snapshot = Snapshot::with_config(snapshot_name, config);

        match snapshot.assert(&mir_output.stdout) {
            Ok(_) => Ok(TestResult::Pass),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("New snapshot created") || err_str.contains("Updated snapshot")
                {
                    Ok(TestResult::Pass)
                } else {
                    Ok(TestResult::Fail(format!("MIR snapshot mismatch: {}", e)))
                }
            }
        }
    }

    fn execute_snapshot_asm(
        &self,
        test_file: &TestFile,
        snapshot_config: &SnapshotConfig,
    ) -> Result<TestResult> {
        let asm_output = self.emit_assembly(&test_file.path)?;

        if asm_output.exit_code != 0 {
            return Ok(TestResult::Fail(format!(
                "Failed to emit assembly: {}",
                asm_output.stderr
            )));
        }

        let snapshot_name = format!("{}.asm", test_file.path.file_stem().unwrap());
        let config = SnapshotConfig::default()
            .with_snapshot_dir(snapshot_config.snapshot_dir().to_owned())
            .with_update_snapshots(snapshot_config.update_snapshots())
            .with_review_mode(snapshot_config.review_mode());
        let snapshot = Snapshot::with_config(snapshot_name, config);

        match snapshot.assert(&asm_output.stdout) {
            Ok(_) => Ok(TestResult::Pass),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("New snapshot created") || err_str.contains("Updated snapshot")
                {
                    Ok(TestResult::Pass)
                } else {
                    Ok(TestResult::Fail(format!(
                        "Assembly snapshot mismatch: {}",
                        e
                    )))
                }
            }
        }
    }

    fn compile_file(&self, file_path: &Utf8Path) -> Result<TestOutput> {
        let output = Command::new(&self.rue_binary)
            .arg("--compile-only")
            .arg(file_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to execute rue compiler: {}", self.rue_binary))?;

        Ok(TestOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn run_compiled_binary(&self, source_path: &Utf8Path) -> Result<TestOutput> {
        // Create temporary directory for compilation
        let temp_dir = TempDir::new().context("Failed to create temporary directory")?;
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf())
            .context("Failed to convert temp path to UTF-8")?;

        let binary_path = temp_path.join(source_path.file_stem().unwrap());

        // Compile to binary
        let compile_output = Command::new(&self.rue_binary)
            .arg("-o")
            .arg(&binary_path)
            .arg(source_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to compile {}", source_path))?;

        if !compile_output.status.success() {
            return Ok(TestOutput {
                stdout: String::from_utf8_lossy(&compile_output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&compile_output.stderr).to_string(),
                exit_code: compile_output.status.code().unwrap_or(-1),
            });
        }

        // Run the binary
        let run_output = Command::new(&binary_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to execute compiled binary: {}", binary_path))?;

        Ok(TestOutput {
            stdout: String::from_utf8_lossy(&run_output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&run_output.stderr).to_string(),
            exit_code: run_output.status.code().unwrap_or(-1),
        })
    }

    fn emit_mir(&self, file_path: &Utf8Path) -> Result<TestOutput> {
        let output = Command::new(&self.rue_binary)
            .arg("--emit-mir")
            .arg(file_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to emit MIR for: {}", file_path))?;

        Ok(TestOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn emit_assembly(&self, file_path: &Utf8Path) -> Result<TestOutput> {
        let output = Command::new(&self.rue_binary)
            .arg("-S")
            .arg(file_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to emit assembly for: {}", file_path))?;

        Ok(TestOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn matches_pattern(&self, text: &str, pattern: &str) -> bool {
        // Support both literal strings and regex patterns
        if let Some(pattern) = pattern.strip_prefix("regex:") {
            // Remove "regex:" prefix
            if let Ok(regex) = Regex::new(pattern) {
                regex.is_match(text)
            } else {
                false // Invalid regex fails the match
            }
        } else {
            // Literal string match (with trimming)
            text.trim().contains(pattern.trim())
        }
    }
}

impl SpecLoader {
    /// Clone the spec loader for use in the executor
    fn clone(&self) -> Self {
        Self {
            spec_file: self.spec_file.clone(),
            specification: self.specification.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Specification;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_pattern_matching() {
        let spec = Specification::default_test_spec();
        let toml_content = toml::to_string_pretty(&spec).unwrap();

        let temp_file = NamedTempFile::new().unwrap();
        fs::write(temp_file.path(), toml_content).unwrap();

        let temp_path = Utf8PathBuf::try_from(temp_file.path().to_path_buf()).unwrap();
        let spec_loader = SpecLoader::new(&temp_path).unwrap();
        let executor = TestExecutor::new(&Utf8PathBuf::from("rue"), &spec_loader).unwrap();

        // Test literal pattern matching
        assert!(executor.matches_pattern("hello world", "hello"));
        assert!(!executor.matches_pattern("hello world", "goodbye"));

        // Test regex pattern matching
        assert!(executor.matches_pattern("error: line 42", "regex:error: line \\d+"));
        assert!(!executor.matches_pattern("warning: line 42", "regex:error: line \\d+"));
    }

    #[test]
    fn test_test_output_creation() {
        let output = TestOutput {
            stdout: "Hello, world!".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
        };

        assert_eq!(output.stdout, "Hello, world!");
        assert_eq!(output.exit_code, 0);
    }
}
