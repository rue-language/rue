use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use regex::Regex;
use similar::{ChangeTag, TextDiff};
use std::fs;

/// Manages golden snapshots for test comparison
pub struct SnapshotManager {
    snapshot_dir: Utf8PathBuf,
    update_mode: bool,
}

/// Result of comparing against a snapshot
#[derive(Debug, Clone)]
pub enum SnapshotResult {
    /// Content matches the snapshot
    Match,
    /// Content differs from snapshot (includes diff)
    Mismatch(String),
    /// New snapshot was created
    New,
}

impl SnapshotManager {
    pub fn new(snapshot_dir: &Utf8Path) -> Result<Self> {
        fs::create_dir_all(snapshot_dir)
            .with_context(|| format!("Failed to create snapshot directory: {}", snapshot_dir))?;

        Ok(Self {
            snapshot_dir: snapshot_dir.to_owned(),
            update_mode: false,
        })
    }

    /// Create a new snapshot manager in update mode
    pub fn with_update_mode(snapshot_dir: &Utf8Path) -> Result<Self> {
        let mut manager = Self::new(snapshot_dir)?;
        manager.update_mode = true;
        Ok(manager)
    }

    /// Compare content against a named snapshot
    pub fn compare_snapshot(&self, name: &str, content: &str) -> Result<SnapshotResult> {
        let snapshot_path = self.snapshot_dir.join(format!("{}.snap", name));

        // Normalize the content before comparison
        let normalized_content = self.normalize_content(content);

        if self.update_mode {
            // Always update in update mode
            self.write_snapshot(&snapshot_path, &normalized_content)?;
            return Ok(SnapshotResult::New);
        }

        if !snapshot_path.exists() {
            // Create new snapshot
            self.write_snapshot(&snapshot_path, &normalized_content)?;
            return Ok(SnapshotResult::New);
        }

        // Read existing snapshot
        let existing_content = fs::read_to_string(&snapshot_path)
            .with_context(|| format!("Failed to read snapshot: {}", snapshot_path))?;

        let existing_normalized = self.normalize_content(&existing_content);

        if normalized_content == existing_normalized {
            Ok(SnapshotResult::Match)
        } else {
            let diff = self.generate_diff(&existing_normalized, &normalized_content, name);
            Ok(SnapshotResult::Mismatch(diff))
        }
    }

    /// Normalize content to make it stable across runs
    fn normalize_content(&self, content: &str) -> String {
        let mut normalized = content.to_string();

        // Normalize timestamps - handle both ISO format and the microsecond format used in logs
        let timestamp_regex = Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z?").unwrap();
        normalized = timestamp_regex
            .replace_all(&normalized, "TIMESTAMP")
            .to_string();

        // Also normalize the simpler timestamp format used in tracing output
        let simple_timestamp_regex = Regex::new(r"TIMESTAMP\.\d+Z").unwrap();
        normalized = simple_timestamp_regex
            .replace_all(&normalized, "TIMESTAMP")
            .to_string();

        // Normalize memory addresses
        let addr_regex = Regex::new(r"0x[0-9a-fA-F]+").unwrap();
        normalized = addr_regex.replace_all(&normalized, "0xADDRESS").to_string();

        // Normalize temporary file paths
        let temp_regex = Regex::new(r"/tmp/[a-zA-Z0-9._-]+").unwrap();
        normalized = temp_regex
            .replace_all(&normalized, "/tmp/TEMPFILE")
            .to_string();

        // Normalize absolute paths to workspace relative
        if let Ok(workspace_root) = std::env::var("CARGO_MANIFEST_DIR") {
            let workspace_regex = Regex::new(&regex::escape(&workspace_root)).unwrap();
            normalized = workspace_regex
                .replace_all(&normalized, "$WORKSPACE")
                .to_string();
        }

        // Normalize line endings
        normalized = normalized.replace("\r\n", "\n");

        // Trim trailing whitespace from each line
        normalized = normalized
            .lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n");

        // Ensure final newline
        if !normalized.ends_with('\n') && !normalized.is_empty() {
            normalized.push('\n');
        }

        normalized
    }

    /// Generate a colored diff between two strings
    fn generate_diff(&self, old: &str, new: &str, context: &str) -> String {
        let diff = TextDiff::from_lines(old, new);
        let mut result = Vec::new();

        result.push(format!("Snapshot mismatch for: {}", context));
        result.push("".to_string());

        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            result.push(format!("{}{}", sign, change));
        }

        result.join("\n")
    }

    /// Write content to a snapshot file
    fn write_snapshot(&self, path: &Utf8Path, content: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory: {}", parent))?;
        }

        fs::write(path, content).with_context(|| format!("Failed to write snapshot: {}", path))?;

        tracing::info!("Updated snapshot: {}", path);
        Ok(())
    }

    /// List all existing snapshots
    pub fn list_snapshots(&self) -> Result<Vec<String>> {
        let mut snapshots = Vec::new();

        if !self.snapshot_dir.exists() {
            return Ok(snapshots);
        }

        for entry in fs::read_dir(&self.snapshot_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(file_name) = path.file_name()
                && let Some(name) = file_name.to_str()
                && name.ends_with(".snap")
            {
                snapshots.push(name.to_string());
            }
        }

        snapshots.sort();
        Ok(snapshots)
    }

    /// Remove a snapshot file
    pub fn remove_snapshot(&self, name: &str) -> Result<()> {
        let snapshot_path = self.snapshot_dir.join(format!("{}.snap", name));

        if snapshot_path.exists() {
            fs::remove_file(&snapshot_path)
                .with_context(|| format!("Failed to remove snapshot: {}", snapshot_path))?;
            tracing::info!("Removed snapshot: {}", snapshot_path);
        }

        Ok(())
    }

    /// Get the path to a snapshot file
    pub fn snapshot_path(&self, name: &str) -> Utf8PathBuf {
        self.snapshot_dir.join(format!("{}.snap", name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_new_snapshot_creation() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let manager = SnapshotManager::new(&temp_path).unwrap();
        let result = manager.compare_snapshot("test", "Hello, world!").unwrap();

        match result {
            SnapshotResult::New => {
                let snapshot_path = manager.snapshot_path("test");
                assert!(snapshot_path.exists());
                let content = fs::read_to_string(snapshot_path).unwrap();
                assert_eq!(content, "Hello, world!\n");
            }
            _ => panic!("Expected new snapshot result"),
        }
    }

    #[test]
    fn test_snapshot_match() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let manager = SnapshotManager::new(&temp_path).unwrap();

        // Create initial snapshot
        manager.compare_snapshot("test", "Hello, world!").unwrap();

        // Compare with same content
        let result = manager.compare_snapshot("test", "Hello, world!").unwrap();

        match result {
            SnapshotResult::Match => {}
            _ => panic!("Expected matching result"),
        }
    }

    #[test]
    fn test_snapshot_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let manager = SnapshotManager::new(&temp_path).unwrap();

        // Create initial snapshot
        manager.compare_snapshot("test", "Hello, world!").unwrap();

        // Compare with different content
        let result = manager.compare_snapshot("test", "Goodbye, world!").unwrap();

        match result {
            SnapshotResult::Mismatch(diff) => {
                assert!(diff.contains("Hello, world!"));
                assert!(diff.contains("Goodbye, world!"));
            }
            _ => panic!("Expected mismatch result"),
        }
    }

    #[test]
    fn test_content_normalization() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let manager = SnapshotManager::new(&temp_path).unwrap();

        let content_with_address = "Memory allocated at 0x7f1234567890\nOperation completed";
        let content_normalized = manager.normalize_content(content_with_address);

        assert!(content_normalized.contains("0xADDRESS"));
        assert!(!content_normalized.contains("0x7f1234567890"));
    }

    #[test]
    fn test_update_mode() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let manager = SnapshotManager::with_update_mode(&temp_path).unwrap();

        // In update mode, everything should be "new"
        let result = manager.compare_snapshot("test", "Hello, world!").unwrap();
        match result {
            SnapshotResult::New => {}
            _ => panic!("Expected new result in update mode"),
        }

        // Even comparing again should be "new" in update mode
        let result = manager
            .compare_snapshot("test", "Different content")
            .unwrap();
        match result {
            SnapshotResult::New => {}
            _ => panic!("Expected new result in update mode"),
        }
    }

    #[test]
    fn test_list_snapshots() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let manager = SnapshotManager::new(&temp_path).unwrap();

        // Initially empty
        let snapshots = manager.list_snapshots().unwrap();
        assert!(snapshots.is_empty());

        // Create some snapshots
        manager.compare_snapshot("test1", "content1").unwrap();
        manager.compare_snapshot("test2", "content2").unwrap();

        let snapshots = manager.list_snapshots().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.contains(&"test1.snap".to_string()));
        assert!(snapshots.contains(&"test2.snap".to_string()));
    }
}
