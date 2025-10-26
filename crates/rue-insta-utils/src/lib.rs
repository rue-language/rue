//! Insta snapshot testing utilities with Buck2 integration
//!
//! This crate provides utilities for using the `insta` snapshot testing library
//! with Buck2's build system. It replaces the homegrown `rue-snapshot` crate.
//!
//! # Features
//!
//! - Buck2 path resolution for sandboxed builds
//! - Execution snapshot support (stdout/stderr/exit codes)
//! - Standard normalizers/redactions for compiler output
//! - Helper macros for common snapshot patterns
//!
//! # Example
//!
//! ```rust
//! use rue_insta_utils::{configure_insta, assert_rust_snapshot};
//!
//! #[test]
//! fn test_parser() {
//!     let ast = parse("fn main() { 42 }");
//!     assert_rust_snapshot!("parser_simple", ast);
//! }
//! ```

use std::path::{Path, PathBuf};

pub mod execution;
pub mod redactions;

/// Configure insta settings for Buck2 snapshot testing
///
/// This function sets up insta to work correctly with Buck2's sandboxed build environment.
/// It resolves the snapshot directory path and configures insta to use it.
///
/// # Arguments
///
/// * `snapshot_dir` - Relative path to snapshot directory (e.g., "tests/snapshots")
///
/// # Usage
///
/// ```rust
/// use rue_insta_utils::configure_insta;
///
/// #[test]
/// fn test_something() {
///     configure_insta("tests/snapshots").bind(|| {
///         insta::assert_snapshot!(output);
///     });
/// }
/// ```
pub fn configure_insta(snapshot_dir: &str) -> insta::Settings {
    let mut settings = insta::Settings::clone_current();

    // Try to resolve the snapshot path for Buck2
    if let Some(resolved_path) = resolve_buck2_snapshot_path(snapshot_dir) {
        settings.set_snapshot_path(resolved_path);
    } else {
        // Fallback to relative path from workspace root
        settings.set_snapshot_path(snapshot_dir);
    }

    // Disable automatic snapshot suffix to match our naming convention
    settings.set_prepend_module_to_snapshot(false);

    // Add standard compiler redactions
    add_standard_redactions(&mut settings);

    settings
}

/// Configure insta with standard compiler redactions
///
/// This adds common redactions for things that change between runs:
/// - Memory addresses (0x...)
/// - Timestamps
/// - Temporary paths
/// - Generated names
pub fn configure_insta_with_redactions(snapshot_dir: &str) -> insta::Settings {
    let mut settings = configure_insta(snapshot_dir);
    add_standard_redactions(&mut settings);
    settings
}

/// Add standard redactions for compiler output
fn add_standard_redactions(settings: &mut insta::Settings) {
    // Memory addresses
    settings.add_redaction(r"\b0x[0-9a-fA-F]+\b", "[ADDRESS]");

    // ISO 8601 timestamps
    settings.add_redaction(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})?",
        "[TIMESTAMP]",
    );

    // Temporary paths
    settings.add_redaction(r"/tmp/[^\s\"']+", "[TEMP_PATH]");
    settings.add_redaction(r"C:\\Temp\\[^\s\"']+", "[TEMP_PATH]");

    // Generated variable names (t0, t1, etc.)
    settings.add_redaction(r"\bt\d+\b", "[TEMP_VAR]");
}

/// Resolve snapshot directory path for Buck2 sandboxed builds
///
/// Buck2 runs tests in a sandbox, so we need to resolve paths correctly.
/// This function checks for Buck2 resources.json or falls back to filesystem traversal.
fn resolve_buck2_snapshot_path(snapshot_dir: &str) -> Option<PathBuf> {
    // First, try to find resources.json (Buck2's resource mapping)
    if let Ok(resources_json) = std::env::var("BUCK_RESOURCES_JSON") {
        if let Ok(content) = std::fs::read_to_string(&resources_json) {
            if let Ok(resources) = serde_json::from_str::<serde_json::Value>(&content) {
                // Look for snapshot directory in resources
                if let Some(obj) = resources.as_object() {
                    for (logical_path, physical_path) in obj {
                        if logical_path.contains(snapshot_dir) {
                            if let Some(path_str) = physical_path.as_str() {
                                let path = PathBuf::from(path_str);
                                if let Some(parent) = path.parent() {
                                    return Some(parent.to_path_buf());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: traverse up to find the crate root (containing BUCK file)
    if let Ok(current_dir) = std::env::current_dir() {
        let mut dir = current_dir.as_path();
        loop {
            if dir.join("BUCK").exists() || dir.join("Cargo.toml").exists() {
                let snapshot_path = dir.join(snapshot_dir);
                if snapshot_path.exists() {
                    return Some(snapshot_path);
                }
            }

            dir = dir.parent()?;
        }
    }

    None
}

/// Assert a debug snapshot with Buck2-compatible settings
///
/// This is a convenience macro that configures insta for Buck2 and asserts a debug snapshot.
///
/// # Example
///
/// ```rust
/// assert_rust_snapshot!("test_name", value);
/// ```
#[macro_export]
macro_rules! assert_rust_snapshot {
    ($name:expr, $value:expr) => {
        $crate::assert_rust_snapshot!($name, $value, "tests/snapshots")
    };
    ($name:expr, $value:expr, $snapshot_dir:expr) => {
        $crate::configure_insta($snapshot_dir).bind(|| {
            ::insta::assert_debug_snapshot!($name, $value);
        });
    };
}

/// Assert a text snapshot with Buck2-compatible settings
///
/// # Example
///
/// ```rust
/// assert_text_snapshot!("test_name", &output);
/// ```
#[macro_export]
macro_rules! assert_text_snapshot {
    ($name:expr, $value:expr) => {
        $crate::assert_text_snapshot!($name, $value, "tests/snapshots")
    };
    ($name:expr, $value:expr, $snapshot_dir:expr) => {
        $crate::configure_insta($snapshot_dir).bind(|| {
            ::insta::assert_snapshot!($name, $value);
        });
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configure_insta() {
        let settings = configure_insta("tests/snapshots");
        // Just verify it doesn't panic
        drop(settings);
    }

    #[test]
    fn test_resolve_path_fallback() {
        // Should return None if path doesn't exist
        let result = resolve_buck2_snapshot_path("nonexistent/path");
        // We can't assert anything specific since it depends on the environment
        drop(result);
    }
}
