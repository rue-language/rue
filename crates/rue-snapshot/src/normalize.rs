//! Normalization utilities for snapshot testing
//!
//! Provides common normalizers to make snapshots stable across different
//! environments and runs.

use once_cell::sync::Lazy;
use regex::Regex;

/// Trait for snapshot normalizers
pub trait Normalizer: Send + Sync {
    /// Normalize the given text
    fn normalize(&self, text: &str) -> String;
}

/// Function normalizer wrapper
pub struct FnNormalizer<F>
where
    F: Fn(&str) -> String + Send + Sync,
{
    f: F,
}

impl<F> FnNormalizer<F>
where
    F: Fn(&str) -> String + Send + Sync,
{
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F> Normalizer for FnNormalizer<F>
where
    F: Fn(&str) -> String + Send + Sync,
{
    fn normalize(&self, text: &str) -> String {
        (self.f)(text)
    }
}

/// Normalize file paths to be platform-independent
pub fn normalize_paths(text: &str) -> String {
    static PATH_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?m)(/[^\s:]+|[A-Z]:[/\\][^\s:]+|\\\\[^\s:]+)").expect("Invalid path regex")
    });

    let mut result = text.to_string();

    // Normalize Windows paths to Unix style
    result = result.replace('\\', "/");

    // Strip absolute paths, keeping only the last component or two
    result = PATH_REGEX
        .replace_all(&result, |caps: &regex::Captures| {
            let path = &caps[1];
            // Remove Windows drive letter if present
            let path = if path.len() > 2 && path.chars().nth(1) == Some(':') {
                &path[2..]
            } else {
                path
            };

            if let Some(pos) = path.rfind('/') {
                if let Some(prev_pos) = path[..pos].rfind('/') {
                    // Keep last two components
                    format!("...{}", &path[prev_pos..])
                } else {
                    // Keep last component
                    format!("...{}", &path[pos..])
                }
            } else {
                path.to_string()
            }
        })
        .to_string();

    result
}

/// Normalize timestamps and dates
pub fn normalize_timestamps(text: &str) -> String {
    static TIMESTAMP_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?([+-]\d{2}:\d{2}|Z)?")
            .expect("Invalid timestamp regex")
    });

    static TIME_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\b\d{1,2}:\d{2}:\d{2}(\.\d+)?\b").expect("Invalid time regex"));

    let mut result = TIMESTAMP_REGEX.replace_all(text, "[TIMESTAMP]").to_string();
    result = TIME_REGEX.replace_all(&result, "[TIME]").to_string();

    result
}

/// Normalize memory addresses and pointers
pub fn normalize_addresses(text: &str) -> String {
    static ADDRESS_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"0x[0-9a-fA-F]+").expect("Invalid address regex"));

    ADDRESS_REGEX.replace_all(text, "0x[ADDRESS]").to_string()
}

/// Normalize temporary file/directory names
pub fn normalize_temp_names(text: &str) -> String {
    static TEMP_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(/tmp|/var/folders|C:\\TEMP|%TEMP%)[^\s]*").expect("Invalid temp path regex")
    });

    TEMP_REGEX.replace_all(text, "[TEMP_DIR]").to_string()
}

/// Normalize line endings to Unix style
pub fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Normalize compiler-generated names (like t0, t1, etc.)
pub fn normalize_generated_names(text: &str) -> String {
    static GEN_NAME_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bt\d+\b").expect("Invalid generated name regex"));

    let mut counter = 0;
    let mut replacements = std::collections::HashMap::new();

    let result = GEN_NAME_REGEX.replace_all(text, |caps: &regex::Captures| {
        let name = &caps[0];
        replacements
            .entry(name.to_string())
            .or_insert_with(|| {
                let replacement = format!("t{}", counter);
                counter += 1;
                replacement
            })
            .clone()
    });

    result.to_string()
}

/// Composite normalizer that applies multiple normalizations
pub struct CompositeNormalizer {
    normalizers: Vec<Box<dyn Normalizer>>,
}

impl Default for CompositeNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositeNormalizer {
    pub fn new() -> Self {
        Self {
            normalizers: vec![],
        }
    }

    pub fn with<N: Normalizer + 'static>(mut self, normalizer: N) -> Self {
        self.normalizers.push(Box::new(normalizer));
        self
    }

    /// Create a standard normalizer with common normalizations
    pub fn standard() -> Self {
        Self::new()
            .with(FnNormalizer::new(normalize_line_endings))
            .with(FnNormalizer::new(normalize_paths))
            .with(FnNormalizer::new(normalize_addresses))
            .with(FnNormalizer::new(normalize_temp_names))
    }

    /// Create a normalizer for compiler output
    pub fn for_compiler_output() -> Self {
        Self::standard()
            .with(FnNormalizer::new(normalize_generated_names))
            .with(FnNormalizer::new(normalize_timestamps))
    }
}

impl Normalizer for CompositeNormalizer {
    fn normalize(&self, text: &str) -> String {
        let mut result = text.to_string();
        for normalizer in &self.normalizers {
            result = normalizer.normalize(&result);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_paths() {
        assert_eq!(
            normalize_paths("/home/user/project/src/main.rs:42"),
            ".../src/main.rs:42"
        );

        assert_eq!(
            normalize_paths("C:\\Users\\name\\project\\src\\main.rs"),
            ".../src/main.rs"
        );
    }

    #[test]
    fn test_normalize_timestamps() {
        assert_eq!(
            normalize_timestamps("2024-01-15T10:30:45.123Z"),
            "[TIMESTAMP]"
        );

        assert_eq!(normalize_timestamps("at 14:23:45 today"), "at [TIME] today");
    }

    #[test]
    fn test_normalize_addresses() {
        assert_eq!(
            normalize_addresses("pointer at 0xdeadbeef"),
            "pointer at 0x[ADDRESS]"
        );

        assert_eq!(
            normalize_addresses("address 0x7fff1234abcd"),
            "address 0x[ADDRESS]"
        );
    }

    #[test]
    fn test_normalize_temp_names() {
        assert_eq!(
            normalize_temp_names("/tmp/rust-xyz123/output"),
            "[TEMP_DIR]"
        );

        assert_eq!(
            normalize_temp_names("/var/folders/abc/def/T/temp"),
            "[TEMP_DIR]"
        );
    }

    #[test]
    fn test_normalize_generated_names() {
        let input = "t0 = t1 + t2; t3 = t0 * t1";
        let output = normalize_generated_names(input);

        // Should consistently rename temporaries
        assert!(output.contains("t0"));
        assert!(output.contains("t1"));
        assert!(output.contains("t2"));
        assert!(output.contains("t3"));
    }

    #[test]
    fn test_composite_normalizer() {
        let normalizer = CompositeNormalizer::standard();

        let input = "/home/user/file.rs at 0xdeadbeef\r\n/tmp/test";
        let output = normalizer.normalize(input);

        // The normalizer keeps last two path components for context
        assert!(output.contains(".../user/file.rs"));
        assert!(output.contains("0x[ADDRESS]"));
        assert!(output.contains("[TEMP_DIR]"));
        assert!(!output.contains("\r"));
    }
}
