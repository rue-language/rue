//! File path management for multi-file compilation.
//!
//! This module handles mapping FileIds to source file paths, which is needed
//! for module resolution and relative imports.

use std::collections::HashMap;

use rue_span::FileId;

use super::Sema;

impl<'a> Sema<'a> {
    /// Set file paths for module resolution in multi-file compilation.
    ///
    /// This maps FileIds to their corresponding source file paths,
    /// enabling relative import resolution during @import.
    pub fn set_file_paths(&mut self, file_paths: HashMap<FileId, String>) {
        self.file_paths = file_paths;
    }

    /// Get the source file path for a span.
    ///
    /// Looks up the file path using the span's file_id.
    pub(crate) fn get_source_path(&self, span: rue_span::Span) -> Option<&str> {
        self.file_paths.get(&span.file_id).map(|s| s.as_str())
    }

    /// Get the file path for a given FileId.
    pub(crate) fn get_file_path(&self, file_id: FileId) -> Option<&str> {
        self.file_paths.get(&file_id).map(|s| s.as_str())
    }

    /// Check if the compilation involves imports (multi-file compilation).
    ///
    /// When imports are present, lazy analysis is used to only analyze
    /// functions reachable from main(). For single-file compilation,
    /// eager analysis is used for backwards compatibility.
    pub(crate) fn has_imports(&self) -> bool {
        !self.module_registry.is_empty()
    }
}
