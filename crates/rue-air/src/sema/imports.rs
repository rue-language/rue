//! Import resolution and const initializer evaluation.
//!
//! This module handles:
//! - Evaluating const initializers (e.g., `const x = @import(...)`)
//! - Resolving import paths to actual file paths
//!
//! Import path resolution uses the structured [`ModulePath`] type for clear,
//! testable resolution logic with explicit priority order. See the module_path
//! module for details on how different import forms are resolved.

use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::Span;

use crate::types::Type;

use super::Sema;
use super::module_path::ModulePath;

impl Sema<'_> {
    /// Evaluate const initializers to determine their types.
    ///
    /// This is Phase 2.5 of semantic analysis, called after declaration gathering
    /// but before function body analysis. It handles:
    ///
    /// - `const x = @import("module")` - evaluates to Type::Module
    /// - Other const initializers are left with placeholder types for now
    ///
    /// This enables module re-exports where a const holds an imported module
    /// that can be accessed via dot notation.
    pub fn evaluate_const_initializers(&mut self) -> CompileResult<()> {
        // Collect const names to iterate (avoid borrowing issues)
        let const_names: Vec<lasso::Spur> = self.constants.keys().copied().collect();

        for name in const_names {
            let const_info = self.constants.get(&name).unwrap();
            let init_ref = const_info.init;
            let span = const_info.span;

            // Check if the init expression is an @import intrinsic
            let inst = self.rir.get(init_ref);
            if let rue_rir::InstData::Intrinsic {
                name: intrinsic_name,
                args_start,
                args_len,
            } = &inst.data
            {
                let intrinsic_name_str = self.interner.resolve(intrinsic_name);
                if intrinsic_name_str == "import" {
                    // This is an @import - evaluate it at compile time
                    let result = self.evaluate_import_intrinsic(*args_start, *args_len, span)?;

                    // Update the const type to the module type
                    if let Some(const_info_mut) = self.constants.get_mut(&name) {
                        const_info_mut.ty = result;
                    }
                }
            }
        }

        Ok(())
    }

    /// Evaluate an @import intrinsic call at compile time.
    ///
    /// This is used during const initializer evaluation to resolve module imports.
    pub(crate) fn evaluate_import_intrinsic(
        &mut self,
        args_start: u32,
        args_len: u32,
        span: Span,
    ) -> CompileResult<Type> {
        // @import takes exactly one argument
        if args_len != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "import".to_string(),
                    expected: 1,
                    found: args_len as usize,
                },
                span,
            ));
        }

        // Get the argument from extra data (intrinsics use inst_refs, not call_args)
        let arg_refs = self.rir.get_inst_refs(args_start, args_len);
        let arg_inst = self.rir.get(arg_refs[0]);

        // The argument must be a string literal
        let import_path = match &arg_inst.data {
            rue_rir::InstData::StringConst(path_spur) => {
                self.interner.resolve(path_spur).to_string()
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ImportRequiresStringLiteral,
                    arg_inst.span,
                ));
            }
        };

        // Resolve the import path
        let resolved_path = self.resolve_import_path_for_const(&import_path, span)?;

        // Register the module
        let (module_id, _is_new) = self
            .module_registry
            .get_or_create(import_path, resolved_path);

        Ok(Type::new_module(module_id))
    }

    /// Resolve an import path for const evaluation.
    ///
    /// This uses the structured `ModulePath` type for clear resolution logic.
    /// See the module_path module for the resolution order and rules.
    ///
    /// # Resolution Order
    ///
    /// 1. Standard library (`"std"`) - currently not supported
    /// 2. For explicit `.rue` paths - exact match, then suffix match
    /// 3. For simple paths - `{path}.rue`, then suffix match, then basename match
    /// 4. Facade files (`_foo.rue`) for directory modules
    pub(crate) fn resolve_import_path_for_const(
        &self,
        import_path: &str,
        span: Span,
    ) -> CompileResult<String> {
        let module_path = ModulePath::parse(import_path);

        // Try to resolve against loaded file paths
        let loaded_paths = self.file_paths.values();
        if let Some(resolved) = module_path.resolve(loaded_paths) {
            return Ok(resolved);
        }

        // Module not found - collect candidates for error message
        let candidates: Vec<String> = self.file_paths.values().cloned().collect();
        Err(CompileError::new(
            ErrorKind::ModuleNotFound {
                path: import_path.to_string(),
                candidates,
            },
            span,
        ))
    }
}
