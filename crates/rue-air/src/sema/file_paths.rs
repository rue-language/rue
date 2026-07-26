//! File path management for multi-file compilation.
//!
//! This module handles request-local source paths used in diagnostics and
//! relocation-stable identities used in generated symbols. Import resolution
//! is consumed separately through a borrowing canonical compiler view.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

use super::Sema;

impl<'a, D: super::DeclarationPhase> Sema<'a, D> {
    /// Install the complete resolved-import input for this semantic epoch.
    pub fn set_canonical_imports(
        &mut self,
        view: &dyn crate::CanonicalImportView,
    ) -> CompileResult<()> {
        let (context, registry) = crate::canonical_imports::CanonicalImportContext::build(view)?;
        self.canonical_imports = Some(Arc::new(context));
        self.module_registry = Arc::new(registry);
        Ok(())
    }

    pub(crate) fn resolve_canonical_import(
        &self,
        import_path: &str,
        span: Span,
    ) -> CompileResult<crate::ModuleId> {
        self.canonical_imports
            .as_ref()
            .ok_or_else(|| {
                CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "semantic analysis of imports requires accepted canonical import records"
                        .to_owned(),
                ))
            })?
            .resolve(import_path, span)
    }

    /// Set the designated semantic root module.
    ///
    /// FileIds are diagnostic handles, not semantic ranks. Multi-source
    /// callers provide the root explicitly so permuting otherwise arbitrary
    /// FileId assignments cannot change root-relative import behavior.
    pub fn set_root_file_id(&mut self, root_file_id: FileId) {
        self.root_file_id = Some(root_file_id);
    }

    /// Set source paths for diagnostics in multi-file compilation.
    ///
    /// This maps FileIds to their corresponding request-local source paths.
    pub fn set_file_paths(&mut self, file_paths: HashMap<FileId, String>) {
        self.file_paths = Arc::new(file_paths);
        self.refresh_type_symbol_paths();
    }

    /// Set stable source identities used when generated symbols need a module
    /// component. These are deliberately separate from physical file paths so
    /// relocating a source tree cannot change machine-level names.
    pub fn set_symbol_paths(&mut self, symbol_paths: HashMap<FileId, String>) {
        self.symbol_paths = Arc::new(symbol_paths);
        self.refresh_type_symbol_paths();
    }

    /// Install standard-library provenance established by import discovery.
    /// Path spelling alone never grants this authority.
    pub fn set_trusted_standard_library_files(&mut self, file_ids: HashSet<FileId>) {
        self.trusted_standard_library_files = Arc::new(file_ids);
    }

    fn refresh_type_symbol_paths(&self) {
        let mut effective_paths = (*self.file_paths).clone();
        effective_paths.extend(self.symbol_paths.iter().map(|(k, v)| (*k, v.clone())));
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
}
