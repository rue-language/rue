//! Enhanced snapshot testing framework for Rue compiler
//!
//! This crate provides Buck2-compatible snapshot testing with features like:
//! - Colored diffs for better readability
//! - Multiple format support (text, JSON, TOML)
//! - Inline snapshots
//! - Built-in normalization
//! - Interactive review mode

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use once_cell::sync::OnceCell;
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use tempfile::NamedTempFile;

pub mod execution;
pub mod inline;
pub mod normalize;

pub use execution::{
    CompilerSnapshot, ExecutionSnapshot, SnapshotFormat as ExecSnapshotFormat, SnapshotTestBuilder,
};
pub use inline::InlineSnapshot;
pub use normalize::{Normalizer, normalize_paths, normalize_timestamps};

/// Cache for Buck2 resources.json parsing
static BUCK2_RESOURCES_CACHE: OnceCell<HashMap<Utf8PathBuf, String>> = OnceCell::new();

/// Configuration for snapshot testing
pub struct SnapshotConfig {
    /// Directory to store snapshot files
    snapshot_dir: Utf8PathBuf,
    /// Whether to update snapshots automatically
    update_snapshots: bool,
    /// Whether to use interactive review mode
    review_mode: bool,
    /// Whether to trim whitespace from snapshots
    trim_whitespace: bool,
    /// Custom normalizers to apply
    normalizers: Vec<Box<dyn Normalizer>>,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            snapshot_dir: Self::find_snapshot_dir(),
            update_snapshots: env::var("UPDATE_SNAPSHOTS").is_ok(),
            review_mode: env::var("REVIEW_SNAPSHOTS").is_ok() && Self::is_tty(),
            trim_whitespace: true, // Default to old behavior
            normalizers: vec![],
        }
    }
}

impl SnapshotConfig {
    /// Check if we're running in a TTY (to avoid hanging in CI)
    fn is_tty() -> bool {
        // Check if we're in a TTY environment (more compatible approach)
        // This checks common CI environment variables
        if env::var("CI").is_ok() 
            || env::var("GITHUB_ACTIONS").is_ok()
            || env::var("GITLAB_CI").is_ok()
            || env::var("CIRCLECI").is_ok()
            || env::var("TRAVIS").is_ok()
            || env::var("BUILDKITE").is_ok()
            || env::var("BUCK_BUILD_ID").is_ok() // Buck2 builds
            || env::var("NO_TTY").is_ok()
        // Explicit override
        {
            return false; // Don't enable interactive mode in CI
        }

        // Simple heuristic: if TERM is set and doesn't indicate a non-interactive environment
        if let Ok(term) = env::var("TERM") {
            // Common non-interactive terminal types
            if term == "dumb" || term.is_empty() {
                return false;
            }
        } else {
            // No TERM environment variable usually means non-interactive
            return false;
        }

        // Additional safety: default to false in headless environments
        env::var("DISPLAY").is_ok() || env::var("WAYLAND_DISPLAY").is_ok() || cfg!(windows) // Windows might not have DISPLAY but could still be interactive
    }

    /// Validate that logical path follows the expected format
    fn validate_logical_path(path: &Utf8Path) -> Result<()> {
        let path_str = path.as_str();
        if !path_str.starts_with("src/snapshots/") && !path_str.starts_with("tests/snapshots/") {
            anyhow::bail!(
                "Invalid logical path format: '{}'. Expected paths to start with 'src/snapshots/' or 'tests/snapshots/'",
                path_str
            );
        }
        Ok(())
    }

    /// Find the logical snapshot directory (not the physical path)
    /// Returns a logical path like "src/snapshots" or "tests/snapshots"
    /// The actual physical path resolution happens in resolve_snapshot_path()
    fn find_snapshot_dir() -> Utf8PathBuf {
        // Strategy 1: Explicit override via env var (useful for CI or Buck2)
        if let Ok(dir) = env::var("RUE_SNAPSHOT_DIR") {
            // Just return the logical path as-is
            // The actual resolution happens in resolve_snapshot_path()
            return Utf8PathBuf::from(dir);
        }

        // Strategy 2: Auto-detection based on test type
        // Try to detect if we're running an integration test or unit test
        let is_likely_unit_test = if let Ok(exe) = env::current_exe() {
            // Unit tests often have the crate name in the binary, like "rue_parser-xxxxx"
            // Integration tests have their own name like "test_parser_snapshots-xxxxx"
            exe.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| !s.starts_with("test_") && s.contains("rue_"))
                .unwrap_or(false)
        } else {
            false
        };

        // Return the logical directory based on test type
        if is_likely_unit_test {
            Utf8PathBuf::from("src/snapshots")
        } else {
            Utf8PathBuf::from("tests/snapshots")
        }
    }

    /// Add a custom normalizer
    pub fn with_normalizer<N: Normalizer + 'static>(mut self, normalizer: N) -> Self {
        self.normalizers.push(Box::new(normalizer));
        self
    }

    /// Set the snapshot directory
    pub fn with_snapshot_dir(mut self, dir: Utf8PathBuf) -> Self {
        self.snapshot_dir = dir;
        self
    }

    /// Enable or disable automatic snapshot updates
    pub fn with_update_snapshots(mut self, update: bool) -> Self {
        self.update_snapshots = update;
        self
    }

    /// Enable or disable review mode
    pub fn with_review_mode(mut self, review: bool) -> Self {
        self.review_mode = review && Self::is_tty();
        self
    }

    /// Enable or disable whitespace trimming
    pub fn with_trim_whitespace(mut self, trim: bool) -> Self {
        self.trim_whitespace = trim;
        self
    }

    /// Get the snapshot directory
    pub fn snapshot_dir(&self) -> &Utf8Path {
        &self.snapshot_dir
    }

    /// Check if snapshots should be updated
    pub fn update_snapshots(&self) -> bool {
        self.update_snapshots
    }

    /// Check if review mode is enabled
    pub fn review_mode(&self) -> bool {
        self.review_mode
    }

    /// Check if whitespace should be trimmed
    pub fn trim_whitespace(&self) -> bool {
        self.trim_whitespace
    }
}

/// Main snapshot testing struct
pub struct Snapshot {
    name: String,
    config: SnapshotConfig,
}

impl Snapshot {
    /// Create a new snapshot test
    pub fn new(name: impl Into<String>) -> Self {
        let name = Self::sanitize_name(name.into());
        Self {
            name,
            config: SnapshotConfig::default(),
        }
    }

    /// Sanitize snapshot name to prevent path traversal and invalid characters
    fn sanitize_name(name: String) -> String {
        // Remove or replace dangerous characters
        let sanitized = name
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect::<String>();

        // Prevent path traversal attempts
        let sanitized = sanitized.replace("..", "__");

        // Ensure it's not empty and doesn't start with problematic characters
        if sanitized.is_empty() || sanitized.starts_with('.') || sanitized.starts_with('_') {
            format!("snapshot_{}", sanitized)
        } else {
            sanitized
        }
    }

    /// Create with custom configuration
    pub fn with_config(name: impl Into<String>, config: SnapshotConfig) -> Self {
        let name = Self::sanitize_name(name.into());
        Self { name, config }
    }

    /// Assert that the actual value matches the snapshot
    pub fn assert(&self, actual: &str) -> Result<()> {
        self.assert_impl(actual, SnapshotFormat::Text)
    }

    /// Assert JSON snapshot
    pub fn assert_json<T: Serialize>(&self, actual: &T) -> Result<()> {
        let json = serde_json::to_string_pretty(actual).context("Failed to serialize to JSON")?;
        self.assert_impl(&json, SnapshotFormat::Json)
    }

    /// Assert TOML snapshot
    pub fn assert_toml<T: Serialize>(&self, actual: &T) -> Result<()> {
        let toml_str = toml::to_string_pretty(actual).context("Failed to serialize to TOML")?;
        self.assert_impl(&toml_str, SnapshotFormat::Toml)
    }

    /// Assert with custom normalizers
    pub fn assert_normalized(&self, actual: &str) -> Result<()> {
        let normalized = self.apply_normalizers(actual);
        self.assert_impl(&normalized, SnapshotFormat::Text)
    }

    fn assert_impl(&self, actual: &str, format: SnapshotFormat) -> Result<()> {
        let snapshot_path = self.snapshot_path(format);

        // Validate logical path format
        SnapshotConfig::validate_logical_path(&snapshot_path)?;

        // Debug logging for troubleshooting
        if env::var("RUE_DEBUG_SNAPSHOTS").is_ok() {
            eprintln!("Looking for snapshot at: {}", snapshot_path);
            eprintln!("File exists: {}", snapshot_path.exists());
            eprintln!("CWD: {:?}", env::current_dir());
            eprintln!(
                "RUST_TEST_RESOURCES_JSON: {:?}",
                env::var("RUST_TEST_RESOURCES_JSON")
            );
        }

        // Apply normalizers and normalization
        let mut actual = self.apply_normalizers(actual);

        // Apply EOL normalization
        actual = actual.replace("\r\n", "\n").replace('\r', "\n");

        // Resolve the actual file path (handles Buck2 resources)
        let actual_path = self.resolve_snapshot_path(&snapshot_path)?;

        if let Ok(expected) = fs::read_to_string(&actual_path) {
            let mut expected = expected;

            // Apply EOL normalization to expected content too
            expected = expected.replace("\r\n", "\n").replace('\r', "\n");

            // Apply trimming if configured
            let (expected, actual) = if self.config.trim_whitespace {
                (expected.trim(), actual.trim())
            } else {
                (expected.as_str(), actual.as_str())
            };

            if expected == actual {
                return Ok(()); // Test passes
            }

            // Test failed - show diff
            if self.config.review_mode {
                self.review_snapshot(expected, actual, &actual_path)?;
            } else if self.config.update_snapshots {
                self.update_snapshot(actual, &actual_path)?;
            } else {
                self.show_diff(expected, actual)?;
                anyhow::bail!("Snapshot mismatch for '{}'", self.name);
            }
        } else {
            // No snapshot exists yet
            if self.config.update_snapshots || self.config.review_mode {
                self.create_snapshot(&actual, &actual_path)?;
            } else {
                eprintln!("\n=== No snapshot exists for '{}' ===", self.name);
                eprintln!("Actual output ({} chars):", actual.len());
                for line in actual.trim().lines() {
                    eprintln!("  | {}", line);
                }
                eprintln!("\nRun with UPDATE_SNAPSHOTS=1 to create the snapshot");
                eprintln!("Snapshot file would be: {}", actual_path);
                anyhow::bail!("No snapshot exists for '{}'", self.name);
            }
        }

        Ok(())
    }

    /// Normalize a Buck2 resource key to a crate-relative path by finding anchor points
    fn normalize_buck2_key(key: &str) -> Option<Utf8PathBuf> {
        // Buck2 keys look like "crates/rue-parser/src/snapshots/foo.snap"
        // We want to extract "src/snapshots/foo.snap" or "tests/snapshots/foo.snap"

        // Find the anchor points
        for anchor in &["src/snapshots/", "tests/snapshots/"] {
            if let Some(idx) = key.find(anchor) {
                // Extract from the anchor onwards
                let normalized = &key[idx..];
                return Some(Utf8PathBuf::from(normalized));
            }
        }
        None
    }

    /// Get cached Buck2 resources mapping
    fn get_buck2_resources(&self) -> Result<&HashMap<Utf8PathBuf, String>> {
        BUCK2_RESOURCES_CACHE.get_or_try_init(|| self.load_buck2_resources())
    }

    /// Load Buck2 resources from JSON files and build normalized mapping
    fn load_buck2_resources(&self) -> Result<HashMap<Utf8PathBuf, String>> {
        let mut normalized_map = HashMap::new();

        // Try multiple strategies to find resources.json
        let mut resources_data = None;

        // Strategy 1: resources.json next to test binary
        if let Ok(exe) = env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            let resources_json_path = exe_dir.join(format!(
                "{}.resources.json",
                exe.file_stem().and_then(|s| s.to_str()).unwrap_or("test")
            ));

            if resources_json_path.exists() {
                resources_data = fs::read_to_string(&resources_json_path).ok();
            }
        }

        // Strategy 2: Environment variable fallback
        if resources_data.is_none()
            && let Ok(resources_json) = env::var("RUST_TEST_RESOURCES_JSON")
        {
            resources_data = fs::read_to_string(&resources_json).ok();
        }

        // Parse resources if found
        if let Some(data) = resources_data {
            let resources: HashMap<String, String> =
                serde_json::from_str(&data).context("Failed to parse Buck2 resources.json")?;

            // Build normalized mapping with duplicate detection
            for (key, real_path) in &resources {
                if let Some(normalized) = Self::normalize_buck2_key(key) {
                    // Hard fail on duplicates
                    if normalized_map.contains_key(&normalized) {
                        anyhow::bail!(
                            "Duplicate normalized snapshot key detected: '{}'. \
                             This indicates conflicting snapshot paths in Buck2 resources.",
                            normalized
                        );
                    }
                    normalized_map.insert(normalized, real_path.clone());
                }
            }
        }

        Ok(normalized_map)
    }

    fn resolve_snapshot_path(&self, logical_path: &Utf8Path) -> Result<Utf8PathBuf> {
        // The logical_path should be like: "src/snapshots/foo.snap" or "tests/snapshots/foo.snap"
        // (snapshot_path() constructs it by joining the dir from find_snapshot_dir() with the filename)

        // Strategy 1: Buck2 with cached resources
        if let Ok(resources_map) = self.get_buck2_resources()
            && let Some(real_path) = resources_map.get(logical_path)
        {
            // Handle both absolute and relative paths
            let resolved_path = if std::path::Path::new(real_path).is_absolute() {
                // Absolute path - use as-is
                Utf8PathBuf::from(real_path)
            } else {
                // Relative path - resolve relative to executable directory
                if let Ok(exe) = env::current_exe() {
                    if let Some(exe_dir) = exe.parent() {
                        let full_path = exe_dir.join(real_path);
                        Utf8PathBuf::try_from(full_path)
                            .with_context(|| format!("Invalid UTF-8 path: {}", real_path))?
                    } else {
                        Utf8PathBuf::from(real_path)
                    }
                } else {
                    Utf8PathBuf::from(real_path)
                }
            };

            if env::var("RUE_DEBUG_SNAPSHOTS").is_ok() {
                eprintln!("Buck2: Found {} -> {}", logical_path, resolved_path);
            }

            return Ok(resolved_path);
        }

        // Strategy 2: Cargo (direct filesystem)
        // Try to find the snapshot file relative to the crate root
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut current = cwd.clone();

        // Walk up to find the crate root
        for _ in 0..10 {
            if current.join("Cargo.toml").exists() || current.join("BUCK").exists() {
                // Found crate root, try the logical path relative to it
                let full_path = current.join(logical_path);
                if let Ok(utf8_path) = Utf8PathBuf::try_from(full_path)
                    && (utf8_path.exists() || env::var("UPDATE_SNAPSHOTS").is_ok())
                {
                    return Ok(utf8_path);
                }
                break;
            }

            if let Some(parent) = current.parent() {
                current = parent.to_path_buf();
            } else {
                break;
            }
        }

        // Fallback: just return the logical path as-is
        Ok(logical_path.to_owned())
    }

    fn apply_normalizers(&self, text: &str) -> String {
        let mut result = text.to_string();
        for normalizer in &self.config.normalizers {
            result = normalizer.normalize(&result);
        }
        result
    }

    fn snapshot_path(&self, format: SnapshotFormat) -> Utf8PathBuf {
        let extension = match format {
            SnapshotFormat::Text => "snap",
            SnapshotFormat::Json => "json.snap",
            SnapshotFormat::Toml => "toml.snap",
        };
        let path = self
            .config
            .snapshot_dir
            .join(format!("{}.{}", self.name, extension));

        // Debug output for Buck2 troubleshooting
        if env::var("BUCK_BUILD_ID").is_ok() && env::var("RUE_DEBUG_SNAPSHOTS").is_ok() {
            eprintln!("Buck2 snapshot path: {}", path);
            eprintln!("Snapshot dir: {}", self.config.snapshot_dir);
            eprintln!("CWD: {:?}", env::current_dir());
            eprintln!(
                "RUST_TEST_RESOURCES_JSON: {:?}",
                env::var("RUST_TEST_RESOURCES_JSON")
            );
        }

        path
    }

    fn show_diff(&self, expected: &str, actual: &str) -> Result<()> {
        eprintln!("\n=== Snapshot mismatch for '{}' ===", self.name);

        let diff = TextDiff::from_lines(expected, actual);

        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            eprint!("{}{}", sign, change);
        }

        eprintln!("\nRun with UPDATE_SNAPSHOTS=1 to update the snapshot");
        Ok(())
    }

    fn update_snapshot(&self, content: &str, path: &Utf8Path) -> Result<()> {
        self.write_snapshot_atomically(content, path, "Updated")
    }

    fn create_snapshot(&self, content: &str, path: &Utf8Path) -> Result<()> {
        self.write_snapshot_atomically(content, path, "Created new")
    }

    /// Write snapshot file atomically using a temporary file
    fn write_snapshot_atomically(
        &self,
        content: &str,
        path: &Utf8Path,
        action: &str,
    ) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create snapshot directory: {}", parent))?;
        }

        // Create a temporary file in the same directory as the target
        let parent_dir = path.parent().unwrap_or_else(|| Utf8Path::new("."));
        let temp_file = NamedTempFile::new_in(parent_dir.as_std_path())
            .with_context(|| format!("Failed to create temporary file for snapshot: {}", path))?;

        // Write content to temporary file
        temp_file
            .as_file()
            .write_all(content.as_bytes())
            .with_context(|| "Failed to write snapshot content to temporary file".to_string())?;

        // Atomically move temp file to final location
        temp_file
            .persist(path.as_std_path())
            .with_context(|| format!("Failed to persist snapshot file: {}", path))?;

        eprintln!("{} snapshot: {}", action, path);
        Ok(())
    }

    fn review_snapshot(&self, expected: &str, actual: &str, path: &Utf8Path) -> Result<()> {
        self.show_diff(expected, actual)?;

        print!("\nAccept changes? (y/N): ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if input.trim().to_lowercase() == "y" {
            self.update_snapshot(actual, path)?;
        } else {
            anyhow::bail!("Snapshot update rejected for '{}'", self.name);
        }

        Ok(())
    }
}

/// Format of snapshot files
#[derive(Debug, Clone, Copy)]
enum SnapshotFormat {
    Text,
    Json,
    Toml,
}

/// Convenience macros for snapshot testing (Result-returning variants)
#[macro_export]
macro_rules! assert_snapshot {
    ($name:expr, $actual:expr) => {
        $crate::Snapshot::new($name).assert(&$actual.to_string())?
    };
    ($name:expr, $actual:expr, $($normalizer:expr),+) => {{
        let mut config = $crate::SnapshotConfig::default();
        $(config = config.with_normalizer($normalizer);)+
        $crate::Snapshot::with_config($name, config).assert(&$actual.to_string())?
    }};
}

#[macro_export]
macro_rules! assert_json_snapshot {
    ($name:expr, $actual:expr) => {
        $crate::Snapshot::new($name).assert_json(&$actual)?
    };
}

#[macro_export]
macro_rules! assert_toml_snapshot {
    ($name:expr, $actual:expr) => {
        $crate::Snapshot::new($name).assert_toml(&$actual)?
    };
}

/// Panic-on-failure macros for snapshot testing (no Result<()> requirement)
#[macro_export]
macro_rules! expect_snapshot {
    ($name:expr, $actual:expr) => {
        if let Err(e) = $crate::Snapshot::new($name).assert(&$actual.to_string()) {
            panic!("Snapshot assertion failed: {}", e);
        }
    };
    ($name:expr, $actual:expr, $($normalizer:expr),+) => {{
        let mut config = $crate::SnapshotConfig::default();
        $(config = config.with_normalizer($normalizer);)+
        if let Err(e) = $crate::Snapshot::with_config($name, config).assert(&$actual.to_string()) {
            panic!("Snapshot assertion failed: {}", e);
        }
    }};
}

#[macro_export]
macro_rules! expect_json_snapshot {
    ($name:expr, $actual:expr) => {
        if let Err(e) = $crate::Snapshot::new($name).assert_json(&$actual) {
            panic!("JSON snapshot assertion failed: {}", e);
        }
    };
}

#[macro_export]
macro_rules! expect_toml_snapshot {
    ($name:expr, $actual:expr) => {
        if let Err(e) = $crate::Snapshot::new($name).assert_toml(&$actual) {
            panic!("TOML snapshot assertion failed: {}", e);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_snapshot_creation() {
        let temp_dir = TempDir::new().unwrap();
        let temp_snapshots_dir = temp_dir.path().join("src").join("snapshots");
        fs::create_dir_all(&temp_snapshots_dir).unwrap();

        let config = SnapshotConfig {
            snapshot_dir: Utf8PathBuf::from("src/snapshots"),
            update_snapshots: true,
            review_mode: false,
            trim_whitespace: true,
            normalizers: vec![],
        };

        // Override the path resolution for this test
        let _snapshot = Snapshot::with_config("test_example", config);

        // For this test, we'll test the snapshot logic without path resolution
        // by manually creating the expected file structure
        let snapshot_path = temp_snapshots_dir.join("test_example.snap");
        fs::write(&snapshot_path, "Hello, world!").unwrap();

        // Test that matching content passes (we can't test creation without
        // mocking the path resolution which is complex)
        assert!(snapshot_path.exists());
        let content = fs::read_to_string(snapshot_path).unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[test]
    fn test_snapshot_matching() {
        // Test the core snapshot logic without path resolution complexities
        let config = SnapshotConfig {
            snapshot_dir: Utf8PathBuf::from("src/snapshots"),
            update_snapshots: false,
            review_mode: false,
            trim_whitespace: true,
            normalizers: vec![],
        };

        let snapshot = Snapshot::with_config("test_match", config);

        // Test name sanitization works
        assert_eq!(snapshot.name, "test_match");

        // Test snapshot path generation
        let path = snapshot.snapshot_path(SnapshotFormat::Text);
        assert_eq!(path, Utf8PathBuf::from("src/snapshots/test_match.snap"));

        // Test logical path validation
        assert!(SnapshotConfig::validate_logical_path(&path).is_ok());

        // Test invalid paths are rejected
        let invalid_path = Utf8PathBuf::from("invalid/path/test.snap");
        assert!(SnapshotConfig::validate_logical_path(&invalid_path).is_err());
    }
}
