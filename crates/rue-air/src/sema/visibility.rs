//! Visibility checking for module system.
//!
//! This module implements the visibility rules defined in ADR-0026:
//! - `pub` items are always accessible
//! - Private items are accessible if the files are in the same directory module

use std::path::{Path, PathBuf};

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::FileId;

use crate::types::EnumId;

use super::Sema;

impl Sema<'_> {
    /// Check if the accessing file can see a private item from the target file.
    ///
    /// Visibility rules (per ADR-0026):
    /// - `pub` items are always accessible
    /// - Private items are accessible if the files are in the same directory module
    ///
    /// Directory module membership is simply "files in the same directory" —
    /// including the facade, which lives inside its directory
    /// (`utils/_utils.rue` is in the `utils` module, RUE-137).
    ///
    /// Returns true if the item is accessible.
    pub(crate) fn is_accessible(
        &self,
        accessing_file_id: FileId,
        target_file_id: FileId,
        is_pub: bool,
    ) -> bool {
        // Public items are always accessible
        if is_pub {
            return true;
        }

        // Get paths for both files
        let accessing_path = self.get_file_path(accessing_file_id);
        let target_path = self.get_file_path(target_file_id);

        // If we can't determine the paths, be permissive (for single-file mode or tests)
        match (accessing_path, target_path) {
            (Some(acc), Some(tgt)) => {
                // Get the "module identity" for each file: its parent
                // directory (the facade included — it lives in-directory).
                let acc_module = get_module_identity(Path::new(acc));
                let tgt_module = get_module_identity(Path::new(tgt));

                acc_module == tgt_module
            }
            // If either path is unknown, allow access (e.g., synthetic types, single-file mode)
            _ => true,
        }
    }

    /// Check that an *unqualified* call may reach the callee (RUE-37).
    ///
    /// All loaded files share one flat global function namespace (spec
    /// 10.5:2, transitional), so a plain `secret()` call would otherwise
    /// resolve a private function from any file — bypassing the E0706 check
    /// that qualified `module.secret()` access gets. The rule (spec 10.3:7):
    ///
    /// - If the callee is accessible per [`Sema::is_accessible`] (it is
    ///   `pub`, or the caller is in the callee's directory), the call is fine.
    /// - Otherwise, if the callee's file was loaded *as a module* (it is the
    ///   target of a resolved `@import` in the program), the call is an error
    ///   (E0460): module privacy must not be escapable by dropping the
    ///   qualifier.
    /// - Files that are never imported (only listed explicitly on the
    ///   command line) keep the flat namespace: no error.
    ///
    /// Module-ness is read from the module registry, which is populated for
    /// top-level `const m = @import(...)` bindings during Phase 2.5, before
    /// any function body is analyzed. (A file imported *only* by a
    /// function-local `let m = @import(...)` is registered when that body is
    /// analyzed, so calls analyzed earlier may not yet see it as a module —
    /// an accepted gap of the transitional flat namespace.)
    pub(crate) fn check_unqualified_call_visibility(
        &self,
        fn_name: &str,
        callee_file_id: FileId,
        is_pub: bool,
        span: rue_span::Span,
    ) -> CompileResult<()> {
        if self.is_accessible(span.file_id, callee_file_id, is_pub) {
            return Ok(());
        }

        // Inaccessible — but only enforce if the callee's file is a module.
        let Some(callee_path) = self.get_file_path(callee_file_id) else {
            return Ok(());
        };
        let Some(import_path) = self.module_registry.import_path_for_file(callee_path) else {
            return Ok(());
        };

        Err(CompileError::new(
            ErrorKind::PrivateUnqualifiedAccess {
                name: fn_name.to_string(),
                module_path: import_path,
            },
            span,
        )
        .with_help(format!(
            "`{fn_name}` is not marked `pub`; private items are only visible within their own directory"
        )))
    }

    /// Resolve an enum type through a module reference.
    ///
    /// Used for qualified enum paths like `module.EnumName::Variant` in match patterns.
    /// Checks visibility: private enums are only accessible from the same directory.
    pub fn resolve_enum_through_module(
        &self,
        _module_ref: rue_rir::InstRef,
        type_name: lasso::Spur,
        span: rue_span::Span,
    ) -> CompileResult<EnumId> {
        let type_name_str = self.interner.resolve(&type_name);

        // Try to find the enum globally
        let enum_id = self.enums.get(&type_name).copied().ok_or_else(|| {
            CompileError::new(ErrorKind::UnknownEnumType(type_name_str.to_string()), span)
        })?;

        // Check visibility
        let enum_def = self.type_pool.enum_def(enum_id);
        let accessing_file_id = span.file_id;
        let target_file_id = enum_def.file_id;

        if !self.is_accessible(accessing_file_id, target_file_id, enum_def.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "enum".to_string(),
                    name: type_name_str.to_string(),
                },
                span,
            ));
        }

        Ok(enum_id)
    }
}

/// Get the module identity for a file path: its parent directory.
///
/// Since RUE-137 the directory-module facade lives INSIDE its directory
/// (`utils/_utils.rue`), so the facade's module is simply its parent — the
/// same rule as every other file. (The old sibling layout needed a special
/// case mapping `_utils.rue` to `utils/`; that layout no longer exists.)
pub(crate) fn get_module_identity(path: &Path) -> Option<PathBuf> {
    path.parent().map(Path::to_path_buf)
}
