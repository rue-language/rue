//! Inline snapshot support
//!
//! Allows storing expected values directly in test code, similar to
//! the expect-test crate but Buck2-compatible.

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use tracing::debug;

/// Registry of inline snapshots for updating
static INLINE_SNAPSHOTS: Lazy<Mutex<Vec<InlineSnapshotUpdate>>> =
    Lazy::new(|| Mutex::new(Vec::new()));

/// Information needed to update an inline snapshot
#[derive(Debug, Clone)]
struct InlineSnapshotUpdate {
    file: String,
    line: u32,
    _old_value: String,
    new_value: String,
}

/// Inline snapshot value
#[derive(Debug, Clone)]
pub struct InlineSnapshot {
    value: String,
    file: String,
    line: u32,
}

impl InlineSnapshot {
    /// Create a new inline snapshot
    pub fn new(value: impl Into<String>, file: impl Into<String>, line: u32) -> Self {
        Self {
            value: value.into(),
            file: file.into(),
            line,
        }
    }

    /// Assert that the actual value matches the inline snapshot
    pub fn assert(&self, actual: &str) -> Result<()> {
        let actual = actual.trim();
        let expected = self.value.trim();

        if actual == expected {
            return Ok(());
        }

        if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
            // Queue update for later
            let mut snapshots = INLINE_SNAPSHOTS.lock().unwrap();
            snapshots.push(InlineSnapshotUpdate {
                file: self.file.clone(),
                line: self.line,
                _old_value: self.value.clone(),
                new_value: actual.to_string(),
            });

            debug!(
                "Inline snapshot update queued: {}:{} old: {:?} new: {:?}",
                self.file, self.line, expected, actual
            );

            Ok(())
        } else {
            eprintln!(
                "\n=== Inline snapshot mismatch at {}:{} ===",
                self.file, self.line
            );
            eprintln!("Expected:");
            for line in expected.lines() {
                eprintln!("  | {}", line);
            }
            eprintln!("Actual:");
            for line in actual.lines() {
                eprintln!("  | {}", line);
            }
            eprintln!("\nRun with UPDATE_SNAPSHOTS=1 to update");

            anyhow::bail!("Inline snapshot mismatch")
        }
    }
}

/// Apply all queued inline snapshot updates
///
/// This should be called at the end of the test run when UPDATE_SNAPSHOTS=1
pub fn apply_inline_updates() -> Result<()> {
    let snapshots = INLINE_SNAPSHOTS.lock().unwrap();

    if snapshots.is_empty() {
        return Ok(());
    }

    // Group updates by file
    let mut updates_by_file = std::collections::HashMap::new();
    for update in snapshots.iter() {
        updates_by_file
            .entry(update.file.clone())
            .or_insert_with(Vec::new)
            .push(update.clone());
    }

    // Apply updates to each file
    for (file, updates) in updates_by_file {
        update_file(&file, updates)?;
    }

    Ok(())
}

fn update_file(file_path: &str, mut updates: Vec<InlineSnapshotUpdate>) -> Result<()> {
    let content = std::fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path))?;

    // Sort updates by line number in reverse order to maintain line numbers
    updates.sort_by(|a, b| b.line.cmp(&a.line));

    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

    for update in updates {
        // Find the line and update it
        // This is a simple implementation - a real one would need proper parsing
        let line_idx = (update.line - 1) as usize;
        if line_idx < lines.len() {
            // Try to preserve formatting
            if let Some(indent) = lines[line_idx].find(|c: char| !c.is_whitespace()) {
                let indent_str = &lines[line_idx][..indent];

                // Format the new value with proper indentation
                let formatted = if update.new_value.contains('\n') {
                    // Multi-line value
                    let mut result = format!("{}@r###\"\n", indent_str);
                    for line in update.new_value.lines() {
                        result.push_str(&format!("{}    {}\n", indent_str, line));
                    }
                    result.push_str(&format!("{}\"###", indent_str));
                    result
                } else {
                    // Single-line value
                    format!("{}@\"{}\"", indent_str, update.new_value)
                };

                // Replace the line
                lines[line_idx] = formatted;
            }
        }
    }

    // Write back the updated content
    let new_content = lines.join("\n");
    std::fs::write(file_path, new_content)
        .with_context(|| format!("Failed to write file: {}", file_path))?;

    eprintln!("Updated inline snapshots in {}", file_path);

    Ok(())
}

/// Macro for inline snapshot testing
#[macro_export]
macro_rules! assert_inline_snapshot {
    ($actual:expr, @$expected:literal) => {{
        let snapshot = $crate::inline::InlineSnapshot::new($expected, file!(), line!());
        snapshot.assert(&$actual.to_string())?
    }};
    // Initial placeholder for new snapshots
    ($actual:expr, @"") => {{
        if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
            let snapshot = $crate::inline::InlineSnapshot::new("", file!(), line!());
            snapshot.assert(&$actual.to_string())?
        } else {
            panic!("Empty inline snapshot - run with UPDATE_SNAPSHOTS=1 to generate");
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inline_snapshot_matching() {
        let snapshot = InlineSnapshot::new("expected", "test.rs".to_string(), 10);

        // Should pass with matching content
        snapshot.assert("expected").unwrap();

        // Should fail with different content (when not updating)
        if std::env::var("UPDATE_SNAPSHOTS").is_err() {
            assert!(snapshot.assert("different").is_err());
        }
    }

    #[test]
    fn test_inline_snapshot_trimming() {
        let snapshot = InlineSnapshot::new("  expected  ", "test.rs".to_string(), 10);

        // Should match after trimming
        snapshot.assert("expected").unwrap();
        snapshot.assert("  expected  ").unwrap();
    }
}
