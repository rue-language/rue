//! SemaContext builder implementation.
//!
//! This module contains methods for building a [`SemaContext`] from a [`Sema`]
//! for use in parallel function body analysis.

use std::collections::HashMap;

use lasso::Spur;
use rue_target::Target;

use crate::inference::{FunctionSig, MethodSig};
use crate::sema_context::{InferenceContext as SemaContextInferenceContext, SemaContext};
use crate::types::{StructId, Type};

use super::{KnownSymbols, Sema};

impl<'a> Sema<'a> {
    /// Build a SemaContext from the current Sema state.
    ///
    /// This creates an immutable context that can be shared across threads
    /// for parallel function body analysis. The SemaContext contains all
    /// type definitions, function signatures, and method signatures.
    ///
    /// The returned SemaContext borrows functions and methods from Sema,
    /// so Sema must outlive the SemaContext. This is reflected in the
    /// lifetime relationship: the returned context lives for `'s` (the
    /// borrow of self), not `'a` (the lifetime of the RIR/interner).
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let mut sema = Sema::new(rir, interner, preview);
    /// sema.inject_builtin_types();
    /// sema.register_type_names()?;
    /// sema.resolve_declarations()?;
    /// let ctx = sema.build_sema_context();
    /// // Now ctx can be shared across threads for parallel analysis
    /// // (while sema is borrowed)
    /// ```
    pub fn build_sema_context<'s>(&'s self) -> SemaContext<'s>
    where
        'a: 's,
    {
        // Build the inference context
        let inference_ctx = self.build_sema_context_inference();

        SemaContext {
            rir: self.rir,
            interner: self.interner,
            structs: self.structs.clone(),
            enums: self.enums.clone(),
            // Pass references to functions/methods/constants instead of cloning.
            // This is safe because after declaration gathering, these HashMaps are immutable.
            functions: &self.functions,
            methods: &self.methods,
            constants: &self.constants,
            preview_features: self.preview_features.clone(),
            builtin_string_id: self.builtin_string_id,
            builtin_arch_id: self.builtin_arch_id,
            builtin_os_id: self.builtin_os_id,
            target: Target::default(), // Will be overridden by caller if needed
            inference_ctx,
            known: KnownSymbols::new(self.interner),
            type_pool: self.type_pool.clone(),
            module_registry: crate::sema_context::ModuleRegistry::new(),
            source_file_path: None, // Will be set when analyzing specific files
            file_paths: self.file_paths.clone(), // Copy file paths for module resolution
            param_arena: &self.param_arena,
        }
    }

    /// Build the inference context portion of SemaContext.
    pub(crate) fn build_sema_context_inference(&self) -> SemaContextInferenceContext {
        // Build function signatures with InferType for constraint generation
        let func_sigs: HashMap<Spur, FunctionSig> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSig {
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                        is_generic: info.is_generic,
                        param_modes: self.param_arena.modes(info.params).to_vec(),
                        param_comptime: self.param_arena.comptime(info.params).to_vec(),
                        param_names: self.param_arena.names(info.params).to_vec(),
                        return_type_sym: info.return_type_sym,
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::Struct(id))
        let struct_types: HashMap<Spur, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::Struct(*id)))
            .collect();

        // Build enum types map (name -> Type::Enum(id))
        let enum_types: HashMap<Spur, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::Enum(*id)))
            .collect();

        // Build method signatures with InferType for constraint generation
        let method_sigs: HashMap<(StructId, Spur), MethodSig> = self
            .methods
            .iter()
            .map(|((struct_id, method_name), info)| {
                (
                    (*struct_id, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        SemaContextInferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
        }
    }
}
