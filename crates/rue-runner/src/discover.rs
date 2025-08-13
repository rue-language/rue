use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use std::fs;
use walkdir::WalkDir;

use crate::directives::TestDirective;

/// Discovers and collects test files from the filesystem
pub struct TestDiscoverer {
    search_paths: Vec<Utf8PathBuf>,
}

/// Represents a discovered test file with its directives
#[derive(Debug, Clone)]
pub struct TestFile {
    pub path: Utf8PathBuf,
    pub directives: Vec<TestDirective>,
}

impl TestDiscoverer {
    pub fn new(search_paths: &[Utf8PathBuf]) -> Self {
        Self {
            search_paths: search_paths.to_vec(),
        }
    }

    /// Discover all .rue test files in the configured search paths
    pub fn discover(&self) -> Result<Vec<TestFile>> {
        let mut test_files = Vec::new();

        for search_path in &self.search_paths {
            if !search_path.exists() {
                tracing::warn!("Search path does not exist: {}", search_path);
                continue;
            }

            let files = self
                .discover_in_path(search_path)
                .with_context(|| format!("Failed to discover tests in {}", search_path))?;

            test_files.extend(files);
        }

        // Sort for deterministic ordering
        test_files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(test_files)
    }

    fn discover_in_path(&self, path: &Utf8Path) -> Result<Vec<TestFile>> {
        let mut test_files = Vec::new();

        for entry in WalkDir::new(path).follow_links(true) {
            let entry =
                entry.with_context(|| format!("Failed to read directory entry in {}", path))?;
            let path = Utf8PathBuf::try_from(entry.path().to_path_buf())
                .with_context(|| format!("Invalid UTF-8 path: {}", entry.path().display()))?;

            if path.extension() == Some("rue") {
                let test_file = self
                    .parse_test_file(&path)
                    .with_context(|| format!("Failed to parse test file {}", path))?;

                test_files.push(test_file);
            }
        }

        Ok(test_files)
    }

    fn parse_test_file(&self, path: &Utf8Path) -> Result<TestFile> {
        let content =
            fs::read_to_string(path).with_context(|| format!("Failed to read file {}", path))?;

        let directives = crate::directives::parse_directives(&content)
            .with_context(|| format!("Failed to parse directives in {}", path))?;

        Ok(TestFile {
            path: path.to_owned(),
            directives,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_discover_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        let discoverer = TestDiscoverer::new(&[temp_path]);
        let tests = discoverer.discover().unwrap();

        assert!(tests.is_empty());
    }

    #[test]
    fn test_discover_rue_files() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = Utf8PathBuf::try_from(temp_dir.path().to_path_buf()).unwrap();

        // Create a test file
        let test_file = temp_path.join("test.rue");
        fs::write(
            &test_file,
            "//!@ id test_basic\n//!@ kind compile-pass\nfn main() {}",
        )
        .unwrap();

        let discoverer = TestDiscoverer::new(&[temp_path]);
        let tests = discoverer.discover().unwrap();

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path.file_name(), Some("test.rue"));
        assert_eq!(tests[0].directives.len(), 2); // id and kind directives
    }

    #[test]
    fn test_discover_nonexistent_path() {
        let nonexistent = Utf8PathBuf::from("/nonexistent/path");
        let discoverer = TestDiscoverer::new(&[nonexistent]);

        // Should not fail, just log a warning
        let tests = discoverer.discover().unwrap();
        assert!(tests.is_empty());
    }
}
