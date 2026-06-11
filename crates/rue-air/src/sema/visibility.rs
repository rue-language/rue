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

    /// Check that an *unqualified* call may reach the callee (RUE-37,
    /// RUE-180).
    ///
    /// All loaded files share one flat global function namespace for *name
    /// resolution* (spec 10.5:2, transitional), but privacy is uniform in
    /// every multi-file compilation, imports or not (spec 10.3:7):
    ///
    /// - If the callee is accessible per [`Sema::is_accessible`] (it is
    ///   `pub`, or the caller is in the callee's directory — ADR-0026
    ///   intra-directory visibility), the call is fine.
    /// - Otherwise the call is an error (E0460), naming the callee's
    ///   defining file. Whether that file was loaded via `@import` or merely
    ///   listed on the command line makes no difference.
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

        // `is_accessible` is permissive when either file path is unknown
        // (single-file mode, synthetic items), so the callee's path is
        // always known here.
        let defining_file = self
            .get_file_path(callee_file_id)
            .unwrap_or("<unknown>")
            .to_string();

        Err(CompileError::new(
            ErrorKind::PrivateUnqualifiedAccess {
                name: fn_name.to_string(),
                defining_file,
            },
            span,
        )
        .with_help(format!(
            "`{fn_name}` is not marked `pub`; private items are only visible within their defining directory"
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
