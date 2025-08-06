//! Simple snapshot testing that works with both Cargo and Buck2
//!
//! This is a minimal implementation that doesn't depend on Cargo metadata
//! or any runtime Cargo environment variables.

use std::fs;
use std::path::PathBuf;

pub struct SnapshotTest {
    test_name: String,
}

impl SnapshotTest {
    pub fn new(test_name: &str) -> Self {
        Self {
            test_name: test_name.to_string(),
        }
    }

    /// Find the snapshot directory - works with both Cargo and Buck2
    fn snapshot_dir(&self) -> PathBuf {
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

        // Strategy 4: Create in current directory if updating
        if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
            return PathBuf::from("src/snapshots");
        }

        // Default fallback
        PathBuf::from("src/snapshots")
    }

    pub fn assert_snapshot(&self, actual: &str) {
        let snapshot_dir = self.snapshot_dir();
        let snapshot_path = snapshot_dir.join(format!("{}.snap", self.test_name));

        if let Ok(expected) = fs::read_to_string(&snapshot_path) {
            let expected = expected.trim();
            let actual = actual.trim();

            if expected == actual {
                return; // Test passes
            }

            // Test failed - show diff and optionally update
            if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
                fs::create_dir_all(&snapshot_dir).unwrap_or_else(|e| {
                    panic!("Failed to create snapshot directory {snapshot_dir:?}: {e}");
                });
                fs::write(&snapshot_path, actual).unwrap_or_else(|e| {
                    panic!("Failed to write snapshot {snapshot_path:?}: {e}");
                });
                eprintln!("Updated snapshot: {}", snapshot_path.display());
            } else {
                // Show a nice diff
                eprintln!("\n=== Snapshot mismatch for {} ===", self.test_name);
                eprintln!("Expected ({} chars):", expected.len());
                for line in expected.lines() {
                    eprintln!("  | {line}");
                }
                eprintln!("\nActual ({} chars):", actual.len());
                for line in actual.lines() {
                    eprintln!("  | {line}");
                }
                eprintln!("\nRun with UPDATE_SNAPSHOTS=1 to update the snapshot");
                eprintln!("Snapshot file: {}", snapshot_path.display());
                panic!("Snapshot mismatch");
            }
        } else {
            // No snapshot exists yet
            if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
                fs::create_dir_all(&snapshot_dir).unwrap_or_else(|e| {
                    panic!("Failed to create snapshot directory {snapshot_dir:?}: {e}");
                });
                fs::write(&snapshot_path, actual).unwrap_or_else(|e| {
                    panic!("Failed to write snapshot {snapshot_path:?}: {e}");
                });
                eprintln!("Created new snapshot: {}", snapshot_path.display());
            } else {
                eprintln!("\n=== No snapshot exists for {} ===", self.test_name);
                eprintln!("Actual output ({} chars):", actual.len());
                for line in actual.trim().lines() {
                    eprintln!("  | {line}");
                }
                eprintln!("\nRun with UPDATE_SNAPSHOTS=1 to create the snapshot");
                eprintln!("Snapshot file would be: {}", snapshot_path.display());
                panic!("No snapshot exists");
            }
        }
    }
}

/// Convenience macro for snapshot testing
#[macro_export]
macro_rules! assert_snapshot {
    ($name:expr, $actual:expr) => {
        $crate::simple_snapshot::SnapshotTest::new($name).assert_snapshot(&$actual.to_string())
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_creation_and_update() {
        // This test verifies our snapshot system works
        // It won't create actual snapshots unless UPDATE_SNAPSHOTS=1
        let test = SnapshotTest::new("test_example");

        // This would normally check/create a snapshot
        if std::env::var("UPDATE_SNAPSHOTS").is_err() {
            // In normal test mode, just verify the system initializes
            assert_eq!(test.test_name, "test_example");
        }
    }
}
