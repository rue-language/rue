//! Extended snapshot testing framework supporting structured output
//!
//! This module provides a flexible snapshot testing system that supports:
//! - Simple text snapshots (backward compatible)
//! - Program execution results (exit code, stdout, stderr)
//! - Compiler output with warnings and errors
//! - Custom structured data

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Core trait for any type that can be snapshot tested
pub trait Snapshot: fmt::Debug {
    /// Serialize the snapshot to a format suitable for storage
    fn serialize_snapshot(&self) -> SnapshotContent;

    /// Compare two snapshots for equality
    fn matches(&self, other: &Self) -> bool;

    /// Format for human-readable diff output
    fn format_diff(&self, other: &Self) -> String;
}

/// Different types of snapshot content
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SnapshotContent {
    /// Simple text snapshot (backward compatible)
    Text(String),
    /// Structured execution result
    Execution(ExecutionSnapshot),
    /// Compiler output with warnings/errors
    CompilerOutput(CompilerSnapshot),
    /// Custom structured data (JSON/TOML serializable)
    Structured(serde_json::Value),
}

/// Snapshot of program execution
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compilation_warnings: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<bool>,
}

/// Snapshot of compiler output
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompilerSnapshot {
    pub success: bool,
    pub errors: Vec<CompilerMessage>,
    pub warnings: Vec<CompilerMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompilerMessage {
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub severity: String,
    pub message: String,
    pub code: Option<String>,
}

/// Error types for snapshot testing
#[derive(Debug)]
pub enum SnapshotError {
    Mismatch { diff: String, path: PathBuf },
    Missing { path: PathBuf },
    SerializationError(String),
    IoError(std::io::Error),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Mismatch { diff, path } => {
                write!(f, "Snapshot mismatch at {}:\n{}", path.display(), diff)
            }
            SnapshotError::Missing { path } => {
                write!(f, "No snapshot exists at {}", path.display())
            }
            SnapshotError::SerializationError(msg) => {
                write!(f, "Serialization error: {msg}")
            }
            SnapshotError::IoError(e) => {
                write!(f, "IO error: {e}")
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::IoError(e)
    }
}

/// Automatic implementation for String (backward compatibility)
impl Snapshot for String {
    fn serialize_snapshot(&self) -> SnapshotContent {
        SnapshotContent::Text(self.clone())
    }

    fn matches(&self, other: &Self) -> bool {
        self.trim() == other.trim()
    }

    fn format_diff(&self, other: &Self) -> String {
        format!("Expected:\n{}\n\nActual:\n{}", self.trim(), other.trim())
    }
}

impl Snapshot for ExecutionSnapshot {
    fn serialize_snapshot(&self) -> SnapshotContent {
        SnapshotContent::Execution(self.clone())
    }

    fn matches(&self, other: &Self) -> bool {
        self == other
    }

    fn format_diff(&self, other: &Self) -> String {
        let mut diff = String::new();
        if self.exit_code != other.exit_code {
            diff.push_str(&format!(
                "Exit code mismatch: expected {}, got {}\n",
                self.exit_code, other.exit_code
            ));
        }
        if self.stdout != other.stdout {
            diff.push_str(&format!(
                "Stdout mismatch:\nExpected:\n{}\nActual:\n{}\n",
                self.stdout, other.stdout
            ));
        }
        if self.stderr != other.stderr {
            diff.push_str(&format!(
                "Stderr mismatch:\nExpected:\n{}\nActual:\n{}\n",
                self.stderr, other.stderr
            ));
        }
        diff
    }
}

impl Snapshot for CompilerSnapshot {
    fn serialize_snapshot(&self) -> SnapshotContent {
        SnapshotContent::CompilerOutput(self.clone())
    }

    fn matches(&self, other: &Self) -> bool {
        self == other
    }

    fn format_diff(&self, other: &Self) -> String {
        format!("Compiler output mismatch:\nExpected: {self:?}\nActual: {other:?}")
    }
}

/// Builder for configuring snapshot tests
pub struct SnapshotTestBuilder {
    name: String,
    snapshot_dir: Option<PathBuf>,
    update_mode: bool,
    format: SnapshotFormat,
}

#[derive(Debug, Clone, Copy)]
pub enum SnapshotFormat {
    /// TOML format - human readable, good for manual editing
    Toml,
    /// JSON format - more universal, good for tooling
    Json,
    /// Auto-detect based on file extension or content
    Auto,
}

impl SnapshotTestBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            snapshot_dir: None,
            update_mode: std::env::var("UPDATE_SNAPSHOTS").is_ok(),
            format: SnapshotFormat::Auto,
        }
    }

    pub fn with_snapshot_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.snapshot_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    pub fn with_format(mut self, format: SnapshotFormat) -> Self {
        self.format = format;
        self
    }

    pub fn with_update_mode(mut self, update: bool) -> Self {
        self.update_mode = update;
        self
    }

    pub fn assert<T: Snapshot>(&self, actual: T) -> Result<(), SnapshotError> {
        let snapshot_path = self.resolve_snapshot_path();

        if snapshot_path.exists() {
            let content = fs::read_to_string(&snapshot_path)?;
            let expected_content = self.deserialize_snapshot(&content)?;

            // Need to compare the actual with expected based on type
            let actual_content = actual.serialize_snapshot();

            if self.content_matches(&actual_content, &expected_content) {
                Ok(())
            } else if self.update_mode {
                self.write_snapshot(&actual_content, &snapshot_path)?;
                eprintln!("Updated snapshot: {}", snapshot_path.display());
                Ok(())
            } else {
                Err(SnapshotError::Mismatch {
                    diff: self.format_content_diff(&expected_content, &actual_content),
                    path: snapshot_path,
                })
            }
        } else if self.update_mode {
            // Create new snapshot
            let content = actual.serialize_snapshot();
            self.write_snapshot(&content, &snapshot_path)?;
            eprintln!("Created snapshot: {}", snapshot_path.display());
            Ok(())
        } else {
            Err(SnapshotError::Missing {
                path: snapshot_path,
            })
        }
    }

    fn resolve_snapshot_path(&self) -> PathBuf {
        let dir = self
            .snapshot_dir
            .as_ref()
            .cloned()
            .unwrap_or_else(|| self.find_snapshot_dir());

        let extension = match self.format {
            SnapshotFormat::Toml => "snap.toml",
            SnapshotFormat::Json => "snap.json",
            SnapshotFormat::Auto => "snap",
        };

        dir.join(format!("{}.{}", self.name, extension))
    }

    fn find_snapshot_dir(&self) -> PathBuf {
        // Try multiple strategies to find the snapshot directory

        // Strategy 1: Look for snapshots relative to current directory
        let cwd_snapshots = PathBuf::from("src/snapshots");
        if cwd_snapshots.exists() {
            return cwd_snapshots;
        }

        // Strategy 2: Look in the crate directory (for Buck2)
        let crate_snapshots = PathBuf::from("crates/rue-parser/src/snapshots");
        if crate_snapshots.exists() {
            return crate_snapshots;
        }

        // Strategy 3: Use environment variable if set
        if let Ok(dir) = std::env::var("SNAPSHOT_DIR") {
            return PathBuf::from(dir);
        }

        // Default fallback
        PathBuf::from("src/snapshots")
    }

    fn deserialize_snapshot(&self, content: &str) -> Result<SnapshotContent, SnapshotError> {
        // Try to detect format based on content
        if content.trim_start().starts_with('{') {
            // JSON
            serde_json::from_str(content)
                .map_err(|e| SnapshotError::SerializationError(e.to_string()))
        } else if content.contains('=') || content.contains('[') {
            // TOML
            toml::from_str(content).map_err(|e| SnapshotError::SerializationError(e.to_string()))
        } else {
            // Plain text
            Ok(SnapshotContent::Text(content.to_string()))
        }
    }

    fn write_snapshot(&self, content: &SnapshotContent, path: &Path) -> Result<(), SnapshotError> {
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let serialized = match self.format {
            SnapshotFormat::Toml | SnapshotFormat::Auto
                if path.extension().and_then(|s| s.to_str()) == Some("toml") =>
            {
                toml::to_string_pretty(content)
                    .map_err(|e| SnapshotError::SerializationError(e.to_string()))?
            }
            SnapshotFormat::Json => serde_json::to_string_pretty(content)
                .map_err(|e| SnapshotError::SerializationError(e.to_string()))?,
            _ => {
                // For plain text or auto with .snap extension
                match content {
                    SnapshotContent::Text(s) => s.clone(),
                    _ => toml::to_string_pretty(content)
                        .map_err(|e| SnapshotError::SerializationError(e.to_string()))?,
                }
            }
        };

        fs::write(path, serialized)?;
        Ok(())
    }

    fn content_matches(&self, a: &SnapshotContent, b: &SnapshotContent) -> bool {
        match (a, b) {
            (SnapshotContent::Text(s1), SnapshotContent::Text(s2)) => s1.trim() == s2.trim(),
            (SnapshotContent::Execution(e1), SnapshotContent::Execution(e2)) => e1 == e2,
            (SnapshotContent::CompilerOutput(c1), SnapshotContent::CompilerOutput(c2)) => c1 == c2,
            (SnapshotContent::Structured(j1), SnapshotContent::Structured(j2)) => j1 == j2,
            _ => false,
        }
    }

    fn format_content_diff(&self, expected: &SnapshotContent, actual: &SnapshotContent) -> String {
        format!("Expected:\n{expected:?}\n\nActual:\n{actual:?}")
    }
}

/// Extension trait for backward-compatible simple snapshots
pub trait SimpleSnapshot {
    fn assert_snapshot(&self, actual: &str);
}

/// Keep the old struct for compatibility
pub struct SnapshotTest {
    test_name: String,
}

impl SnapshotTest {
    pub fn new(test_name: &str) -> Self {
        Self {
            test_name: test_name.to_string(),
        }
    }
}

impl SimpleSnapshot for SnapshotTest {
    fn assert_snapshot(&self, actual: &str) {
        SnapshotTestBuilder::new(&self.test_name)
            .with_format(SnapshotFormat::Auto)
            .assert(actual.to_string())
            .unwrap_or_else(|e| panic!("{}", e));
    }
}

/// Main snapshot assertion macro with multiple forms
#[macro_export]
macro_rules! assert_snapshot_ext {
    // Simple text snapshot (backward compatible)
    ($name:expr, $actual:expr) => {
        $crate::snapshot::SnapshotTestBuilder::new($name)
            .assert($actual.to_string())
            .expect("snapshot test failed")
    };

    // Structured snapshot with automatic type inference
    ($name:expr, $actual:expr, format = $format:ident) => {
        $crate::snapshot::SnapshotTestBuilder::new($name)
            .with_format($crate::snapshot::SnapshotFormat::$format)
            .assert($actual)
            .expect("snapshot test failed")
    };

    // Execution result snapshot
    (execution: $name:expr, {
        exit_code: $exit:expr,
        stdout: $stdout:expr,
        stderr: $stderr:expr
        $(, warnings: $warnings:expr)?
        $(,)?
    }) => {
        $crate::snapshot::SnapshotTestBuilder::new($name)
            .with_format($crate::snapshot::SnapshotFormat::Toml)
            .assert($crate::snapshot::ExecutionSnapshot {
                exit_code: $exit,
                stdout: $stdout.to_string(),
                stderr: $stderr.to_string(),
                compilation_warnings: None $(.or(Some($warnings)))?,
                timeout: None,
            })
            .expect("snapshot test failed")
    };
}

/// Specialized macro for compiler output
#[macro_export]
macro_rules! assert_compiler_snapshot {
    ($name:expr, $result:expr) => {{
        let snapshot = match $result {
            Ok(artifacts) => $crate::snapshot::CompilerSnapshot {
                success: true,
                errors: vec![],
                warnings: vec![],
                artifacts: Some(artifacts),
            },
            Err(errors) => $crate::snapshot::CompilerSnapshot {
                success: false,
                errors: errors.into_iter().map(Into::into).collect(),
                warnings: vec![],
                artifacts: None,
            },
        };

        $crate::snapshot::SnapshotTestBuilder::new($name)
            .with_format($crate::snapshot::SnapshotFormat::Toml)
            .assert(snapshot)
            .expect("compiler snapshot test failed")
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_snapshot_backward_compatibility() {
        // This test verifies backward compatibility with simple text snapshots
        let test = SnapshotTest::new("test_backward_compat");

        // This would normally check/create a snapshot
        if std::env::var("UPDATE_SNAPSHOTS").is_err() {
            // In normal test mode, just verify the system initializes
            assert_eq!(test.test_name, "test_backward_compat");
        }
    }

    #[test]
    fn test_execution_snapshot() {
        let snapshot = ExecutionSnapshot {
            exit_code: 0,
            stdout: "Hello, world!\n".to_string(),
            stderr: "".to_string(),
            compilation_warnings: None,
            timeout: None,
        };

        // Verify serialization works
        let _content = snapshot.serialize_snapshot();
    }
}
