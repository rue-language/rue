//! Insta snapshot testing utilities with Buck2 integration
//!
//! This module provides helpers for using the `insta` crate with Buck2's build system.
//! It handles path resolution for snapshot files in Buck2's sandboxed environment.

use std::path::{Path, PathBuf};

/// Configure insta settings for Buck2 snapshot testing
///
/// This function sets up insta to work correctly with Buck2's sandboxed build environment.
/// It resolves the snapshot directory path and configures insta to use it.
///
/// # Usage
///
/// ```rust
/// use insta_utils::configure_insta;
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

    settings
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
/// assert_buck2_debug_snapshot!("test_name", value);
/// ```
#[macro_export]
macro_rules! assert_buck2_debug_snapshot {
    ($name:expr, $value:expr) => {
        $crate::insta_utils::configure_insta("tests/snapshots").bind(|| {
            ::insta::assert_debug_snapshot!($name, $value);
        });
    };
}

/// Assert a snapshot with Buck2-compatible settings
///
/// This is a convenience macro that configures insta for Buck2 and asserts a snapshot.
///
/// # Example
///
/// ```rust
/// assert_buck2_snapshot!("test_name", &output);
/// ```
#[macro_export]
macro_rules! assert_buck2_snapshot {
    ($name:expr, $value:expr) => {
        $crate::insta_utils::configure_insta("tests/snapshots").bind(|| {
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
}
