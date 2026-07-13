//! File path management for multi-file compilation.
//!
//! This module handles mapping FileIds to source file paths, which is needed
//! for module resolution and relative imports.

use std::collections::HashMap;

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

use super::Sema;
use crate::path_norm::normalize_module_path;

impl Sema<'_> {
    fn binding_import_base_dirs(&self, span: Span) -> Vec<String> {
        use std::path::Path;
        let dir_of = |path: &str| {
            Path::new(path)
                .parent()
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let importer = self.get_source_path(span).map(&dir_of);
        let root = self
            .root_file_id
            .and_then(|id| self.file_paths.get(&id))
            .or_else(|| self.file_paths.iter().min_by_key(|(id, _)| id.index()).map(|(_, p)| p))
            .map(|path| dir_of(path));
        let mut dirs = Vec::new();
        if let Some(dir) = importer { dirs.push(dir); }
        if let Some(dir) = root && !dirs.contains(&dir) { dirs.push(dir); }
        if dirs.is_empty() { dirs.push(String::new()); }
        dirs
    }

    pub(crate) fn resolve_import_path(
        &self,
        import_path: &str,
        span: Span,
    ) -> CompileResult<String> {
        let dirs = self.binding_import_base_dirs(span);
        let refs = dirs.iter().map(String::as_str).collect::<Vec<_>>();
        let module = if import_path == "std" {
            super::module_path::ModulePath::Std
        } else {
            super::module_path::ModulePath::parse(import_path)
        };
        let std_dir = std::env::var("RUE_STD_PATH").ok();
        match module.resolve_in_dirs_with_std_dir(
            &refs,
            self.file_paths.values(),
            std_dir.as_deref(),
        ) {
            super::module_path::DirResolution::Resolved(path) => Ok(path),
            super::module_path::DirResolution::Ambiguous { file_module, dir_module } => {
                Err(CompileError::new(
                    ErrorKind::AmbiguousModule(Box::new(rue_error::AmbiguousModuleData {
                        path: import_path.to_owned(),
                        file_module,
                        dir_module,
                    })),
                    span,
                ))
            }
            super::module_path::DirResolution::NotFound if import_path == "std" => {
                Err(CompileError::new(ErrorKind::StdLibNotFound, span))
            }
            super::module_path::DirResolution::NotFound => Err(CompileError::new(
                ErrorKind::ModuleNotFound {
                    path: import_path.to_owned(),
                    candidates: super::module_path::import_candidate_groups(import_path, &dirs, None)
                        .into_iter()
                        .flatten()
                        .collect(),
                },
                span,
            )),
        }
    }
}

impl<'a, D: super::DeclarationPhase> Sema<'a, D> {
    /// Set the designated semantic root module.
    ///
    /// FileIds are diagnostic handles, not semantic ranks. Multi-source
    /// callers provide the root explicitly so permuting otherwise arbitrary
    /// FileId assignments cannot change root-relative import behavior.
    pub fn set_root_file_id(&mut self, root_file_id: FileId) {
        self.root_file_id = Some(root_file_id);
    }

    /// Set file paths for module resolution in multi-file compilation.
    ///
    /// This maps FileIds to their corresponding source file paths,
    /// enabling relative import resolution during @import.
    pub fn set_file_paths(&mut self, file_paths: HashMap<FileId, String>) {
        self.file_paths = file_paths;
        self.refresh_type_symbol_paths();
    }

    /// Set stable source identities used when generated symbols need a module
    /// component. These are deliberately separate from physical file paths so
    /// relocating a source tree cannot change machine-level names.
    pub fn set_symbol_paths(&mut self, symbol_paths: HashMap<FileId, String>) {
        self.symbol_paths = symbol_paths;
        self.refresh_type_symbol_paths();
    }

    fn refresh_type_symbol_paths(&self) {
        let mut effective_paths = self.file_paths.clone();
        effective_paths.extend(self.symbol_paths.clone());
        self.type_pool.set_symbol_paths(effective_paths);
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

    /// Get the relocation-stable symbol path for a given FileId.
    pub(crate) fn get_symbol_path(&self, file_id: FileId) -> Option<&str> {
        self.symbol_paths
            .get(&file_id)
            .or_else(|| self.file_paths.get(&file_id))
            .map(|s| s.as_str())
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
