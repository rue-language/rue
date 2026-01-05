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
    /// Directory module membership includes:
    /// - Files directly in the directory (e.g., `utils/strings.rue` is in `utils`)
    /// - Facade files for the directory (e.g., `_utils.rue` is in `utils` module)
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
                // Get the "module identity" for each file.
                // For a regular file like `utils/strings.rue`, the module is `utils/`
                // For a facade file like `_utils.rue`, the module is `utils/` (the directory it represents)
                let acc_module = get_module_identity(Path::new(acc));
                let tgt_module = get_module_identity(Path::new(tgt));

                acc_module == tgt_module
            }
            // If either path is unknown, allow access (e.g., synthetic types, single-file mode)
            _ => true,
        }
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

/// Get the module identity for a file path.
///
/// - For regular files: returns the parent directory
/// - For facade files (`_foo.rue`): returns the corresponding directory (`foo/`)
///
/// This allows facade files to be treated as part of their corresponding directory module.
pub(crate) fn get_module_identity(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let file_stem = path.file_stem()?.to_str()?;

    // Check if this is a facade file (starts with underscore)
    if file_stem.starts_with('_') {
        // Facade file: _utils.rue -> parent/utils
        let module_name = &file_stem[1..]; // Strip the leading underscore
        Some(parent.join(module_name))
    } else {
        // Regular file: the module is just the parent directory
        Some(parent.to_path_buf())
    }
}
