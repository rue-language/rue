//! Execution snapshot support for testing compiled programs
//!
//! This module provides snapshot testing for program execution results using insta,
//! including exit codes, stdout, stderr, and compilation warnings.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Snapshot of program execution results
///
/// This struct is compatible with the old `rue-snapshot::ExecutionSnapshot`
/// but uses insta for the actual snapshot testing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compilation_warnings: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
}

impl ExecutionSnapshot {
    /// Create a successful execution snapshot
    pub fn success(stdout: String) -> Self {
        Self {
            exit_code: 0,
            stdout,
            stderr: String::new(),
            compilation_warnings: None,
            timeout: None,
        }
    }

    /// Create a failed execution snapshot
    pub fn failure(exit_code: i32, stderr: String) -> Self {
        Self {
            exit_code,
            stdout: String::new(),
            stderr,
            compilation_warnings: None,
            timeout: None,
        }
    }

    /// Normalize the snapshot using a normalizer function
    pub fn normalize<F>(self, normalizer: F) -> Self
    where
        F: Fn(&str) -> String,
    {
        Self {
            exit_code: self.exit_code,
            stdout: normalizer(&self.stdout),
            stderr: normalizer(&self.stderr),
            compilation_warnings: self.compilation_warnings,
            timeout: self.timeout,
        }
    }
}

/// Assert an execution snapshot using insta with TOML format
///
/// This is the primary way to snapshot execution results.
///
/// # Example
///
/// ```rust
/// let snapshot = ExecutionSnapshot::success("Hello, world!\n".to_string());
/// assert_execution_snapshot!("test_hello", &snapshot);
/// ```
#[macro_export]
macro_rules! assert_execution_snapshot {
    ($name:expr, $snapshot:expr) => {
        $crate::assert_execution_snapshot!($name, $snapshot, "tests/snapshots")
    };
    ($name:expr, $snapshot:expr, $snapshot_dir:expr) => {{
        // Serialize to TOML for human-readable snapshots
        let toml_str = ::toml::to_string_pretty($snapshot)
            .expect("Failed to serialize execution snapshot to TOML");

        $crate::configure_insta($snapshot_dir).bind(|| {
            ::insta::assert_snapshot!($name, toml_str);
        });
    }};
}

/// Snapshot of compiler output (errors and warnings)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompilerSnapshot {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Vec<String>>,
}

/// Assert a compiler snapshot using insta with TOML format
#[macro_export]
macro_rules! assert_compiler_snapshot {
    ($name:expr, $snapshot:expr) => {
        $crate::assert_compiler_snapshot!($name, $snapshot, "tests/snapshots")
    };
    ($name:expr, $snapshot:expr, $snapshot_dir:expr) => {{
        let toml_str = ::toml::to_string_pretty($snapshot)
            .expect("Failed to serialize compiler snapshot to TOML");

        $crate::configure_insta($snapshot_dir).bind(|| {
            ::insta::assert_snapshot!($name, toml_str);
        });
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_snapshot_success() {
        let snapshot = ExecutionSnapshot::success("test output".to_string());
        assert_eq!(snapshot.exit_code, 0);
        assert_eq!(snapshot.stdout, "test output");
        assert!(snapshot.stderr.is_empty());
    }

    #[test]
    fn test_execution_snapshot_failure() {
        let snapshot = ExecutionSnapshot::failure(1, "error message".to_string());
        assert_eq!(snapshot.exit_code, 1);
        assert_eq!(snapshot.stderr, "error message");
        assert!(snapshot.stdout.is_empty());
    }

    #[test]
    fn test_execution_snapshot_normalize() {
        let snapshot = ExecutionSnapshot::success("temp: /tmp/foo".to_string());
        let normalized = snapshot.normalize(|s| s.replace("/tmp/foo", "[TEMP]"));
        assert_eq!(normalized.stdout, "temp: [TEMP]");
    }
}
