//! File path management for multi-file compilation.
//!
//! This module handles mapping FileIds to source file paths, which is needed
//! for module resolution and relative imports.

use std::collections::HashMap;

use rue_span::FileId;

use super::Sema;
use crate::path_norm::normalize_module_path;

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

    /// Reverse lookup: find the FileId for a given source file path.
    ///
    /// Used by module member access to key into per-file tables (e.g. the
    /// module-binding consts of a facade file) when only the module's
    /// resolved path is known.
    pub(crate) fn get_file_id(&self, path: &str) -> Option<FileId> {
        self.file_paths
            .iter()
            .find(|(_, p)| p.as_str() == path)
            .map(|(id, _)| *id)
    }

    /// Resolve a source-file path to its canonical [`FileId`](rue_span::FileId),
    /// tolerating equivalent spellings of the same file.
    ///
    /// A module imported as `@import("./helper.rue")` records its resolved path
    /// with the leading `./` (or other `.` components) still attached, while
    /// the file table stores the plain `helper.rue`. An exact
    /// [`get_file_id`](Self::get_file_id) match would miss it, so member access
    /// through that binding spuriously failed (E0707, RUE-240). Comparing by
    /// normalized path (dropping `.` components) makes both spellings resolve
    /// to the same file, matching spec 10.2:4 (an import resolving to an
    /// already-loaded file refers to that same module). Normalization also
    /// collapses `..`, so a `../`-relative `@import` and a command-line-listed
    /// file resolve to the same file (RUE-317).
    pub(crate) fn canonical_file_id(&self, path: &str) -> Option<FileId> {
        if let Some(id) = self.get_file_id(path) {
            return Some(id);
        }
        let norm = normalize_module_path(path);
        self.file_paths
            .iter()
            .find(|(_, p)| normalize_module_path(p) == norm)
            .map(|(id, _)| *id)
    }
}
