//! Standard redaction functions for normalizing snapshot output
//!
//! These functions are compatible with the old `rue-snapshot::normalize` module
//! but are designed to work with insta's redaction system.

use regex::Regex;

/// Normalize timestamps in text
///
/// Replaces ISO 8601 timestamps with `[TIMESTAMP]`
pub fn normalize_timestamps(text: &str) -> String {
    // ISO 8601 timestamps
    let re = Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})?")
        .unwrap();
    re.replace_all(text, "[TIMESTAMP]").to_string()
}

/// Normalize temporary file paths
///
/// Replaces paths like `/tmp/...` with `[TEMP_PATH]`
pub fn normalize_temp_paths(text: &str) -> String {
    let unix_re = Regex::new(r"/tmp/[^\s\"']+").unwrap();
    let windows_re = Regex::new(r"C:\\Temp\\[^\s\"']+").unwrap();

    let text = unix_re.replace_all(text, "[TEMP_PATH]");
    windows_re.replace_all(&text, "[TEMP_PATH]").to_string()
}

/// Normalize generated temporary names
///
/// Replaces compiler-generated names like `t0`, `t1` with `[TEMP_VAR]`
pub fn normalize_temp_names(text: &str) -> String {
    let re = Regex::new(r"\bt\d+\b").unwrap();
    re.replace_all(text, "[TEMP_VAR]").to_string()
}

/// Normalize memory addresses
///
/// Replaces hexadecimal addresses like `0x7fff...` with `[ADDRESS]`
pub fn normalize_addresses(text: &str) -> String {
    let re = Regex::new(r"\b0x[0-9a-fA-F]+\b").unwrap();
    re.replace_all(text, "[ADDRESS]").to_string()
}

/// Normalize paths to be relative
///
/// This is more complex and typically requires knowledge of the project root
pub fn normalize_paths(text: &str, project_root: &str) -> String {
    text.replace(project_root, "[PROJECT_ROOT]")
}

/// Apply all standard normalizations
///
/// This is equivalent to `CompositeNormalizer::standard()` from rue-snapshot
pub fn normalize_all(text: &str) -> String {
    let text = normalize_timestamps(text);
    let text = normalize_temp_paths(&text);
    let text = normalize_temp_names(&text);
    normalize_addresses(&text)
}

/// Normalize compiler output
///
/// This is equivalent to `CompositeNormalizer::for_compiler_output()` from rue-snapshot
pub fn normalize_compiler_output(text: &str) -> String {
    normalize_all(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_timestamps() {
        let input = "Compiled at 2024-01-15T10:30:00Z";
        let output = normalize_timestamps(input);
        assert_eq!(output, "Compiled at [TIMESTAMP]");
    }

    #[test]
    fn test_normalize_temp_paths() {
        let input = "File: /tmp/rust_out_12345/test.o";
        let output = normalize_temp_paths(input);
        assert_eq!(output, "File: [TEMP_PATH]");
    }

    #[test]
    fn test_normalize_temp_names() {
        let input = "let t0 = 42; let t1 = t0 + 1;";
        let output = normalize_temp_names(input);
        assert_eq!(output, "let [TEMP_VAR] = 42; let [TEMP_VAR] = [TEMP_VAR] + 1;");
    }

    #[test]
    fn test_normalize_addresses() {
        let input = "Pointer: 0x7fff5fc3d8b0";
        let output = normalize_addresses(input);
        assert_eq!(output, "Pointer: [ADDRESS]");
    }

    #[test]
    fn test_normalize_all() {
        let input = "t0 at 0x12345 on 2024-01-01T00:00:00Z in /tmp/test";
        let output = normalize_all(input);
        assert_eq!(
            output,
            "[TEMP_VAR] at [ADDRESS] on [TIMESTAMP] in [TEMP_PATH]"
        );
    }
}
