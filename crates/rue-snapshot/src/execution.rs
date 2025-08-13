//! Execution snapshot support for testing compiled programs
//!
//! This module provides snapshot testing for program execution results,
//! including exit codes, stdout, stderr, and compilation warnings.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Snapshot of program execution results
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
}

/// Snapshot of compiler output (errors and warnings)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompilerSnapshot {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Vec<String>>,
}

/// Format for snapshot files
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SnapshotFormat {
    /// TOML format - human readable, good for manual editing
    Toml,
    /// JSON format - more universal, good for tooling
    Json,
    /// Auto-detect based on file extension or content
    Auto,
}

/// Builder for creating and managing snapshot tests
pub struct SnapshotTestBuilder {
    name: String,
    snapshot_dir: Option<PathBuf>,
    update_mode: bool,
    format: SnapshotFormat,
}

impl SnapshotTestBuilder {
    /// Create a new snapshot test builder
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            snapshot_dir: None,
            update_mode: std::env::var("UPDATE_SNAPSHOTS").is_ok(),
            format: SnapshotFormat::Auto,
        }
    }

    /// Set the snapshot directory
    pub fn with_snapshot_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.snapshot_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set the snapshot format
    pub fn with_format(mut self, format: SnapshotFormat) -> Self {
        self.format = format;
        self
    }

    /// Force update mode (overrides environment variable)
    pub fn with_update_mode(mut self, update: bool) -> Self {
        self.update_mode = update;
        self
    }

    /// Test an execution snapshot
    pub fn test_execution(&self, snapshot: &ExecutionSnapshot) -> Result<(), String> {
        let snapshot_path = self.get_snapshot_path("execution.toml");

        if self.update_mode {
            // Write the snapshot
            let toml_str = toml::to_string_pretty(snapshot)
                .map_err(|e| format!("Failed to serialize snapshot: {}", e))?;
            fs::write(&snapshot_path, toml_str)
                .map_err(|e| format!("Failed to write snapshot: {}", e))?;
            Ok(())
        } else {
            // Compare with existing snapshot
            if !snapshot_path.exists() {
                return Err(format!(
                    "No snapshot found at {}. Run with UPDATE_SNAPSHOTS=1 to create.",
                    snapshot_path.display()
                ));
            }

            let existing_str = fs::read_to_string(&snapshot_path)
                .map_err(|e| format!("Failed to read snapshot: {}", e))?;
            let existing: ExecutionSnapshot = toml::from_str(&existing_str)
                .map_err(|e| format!("Failed to parse snapshot: {}", e))?;

            if existing != *snapshot {
                return Err(format!(
                    "Snapshot mismatch for {}:\nExpected:\n{:#?}\nActual:\n{:#?}",
                    self.name, existing, snapshot
                ));
            }

            Ok(())
        }
    }

    /// Test a compiler output snapshot
    pub fn test_compiler(&self, snapshot: &CompilerSnapshot) -> Result<(), String> {
        let snapshot_path = self.get_snapshot_path("compiler.toml");

        if self.update_mode {
            // Write the snapshot
            let toml_str = toml::to_string_pretty(snapshot)
                .map_err(|e| format!("Failed to serialize snapshot: {}", e))?;
            fs::write(&snapshot_path, toml_str)
                .map_err(|e| format!("Failed to write snapshot: {}", e))?;
            Ok(())
        } else {
            // Compare with existing snapshot
            if !snapshot_path.exists() {
                return Err(format!(
                    "No snapshot found at {}. Run with UPDATE_SNAPSHOTS=1 to create.",
                    snapshot_path.display()
                ));
            }

            let existing_str = fs::read_to_string(&snapshot_path)
                .map_err(|e| format!("Failed to read snapshot: {}", e))?;
            let existing: CompilerSnapshot = toml::from_str(&existing_str)
                .map_err(|e| format!("Failed to parse snapshot: {}", e))?;

            if existing != *snapshot {
                return Err(format!(
                    "Snapshot mismatch for {}:\nExpected:\n{:#?}\nActual:\n{:#?}",
                    self.name, existing, snapshot
                ));
            }

            Ok(())
        }
    }

    /// Get the path for a snapshot file
    fn get_snapshot_path(&self, suffix: &str) -> PathBuf {
        let dir = self.snapshot_dir.clone().unwrap_or_else(|| {
            // Default to snapshots directory relative to project root
            PathBuf::from("tests/snapshots")
        });

        // Create directory if it doesn't exist
        let _ = fs::create_dir_all(&dir);

        dir.join(format!("{}_{}", self.name, suffix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let toml_str = toml::to_string_pretty(&snapshot).unwrap();
        assert!(toml_str.contains("exit_code = 0"));
        assert!(toml_str.contains("Hello, world!"));

        // Verify deserialization works
        let deserialized: ExecutionSnapshot = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized, snapshot);
    }

    #[test]
    fn test_compiler_snapshot() {
        let snapshot = CompilerSnapshot {
            errors: vec!["Syntax error".to_string()],
            warnings: vec!["Unused variable".to_string()],
            info: None,
        };

        // Verify serialization
        let json_str = serde_json::to_string_pretty(&snapshot).unwrap();
        assert!(json_str.contains("Syntax error"));
        assert!(json_str.contains("Unused variable"));
    }
}
