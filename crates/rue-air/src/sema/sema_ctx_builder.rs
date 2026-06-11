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
                        param_type_syms: self
                            .rir
                            .get_params(info.rir_params_start, info.rir_params_len)
                            .iter()
                            .map(|p| p.ty)
                            .collect(),
                        return_type_sym: info.return_type_sym,
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::new_struct(id))
        let struct_types: HashMap<Spur, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::new_struct(*id)))
            .collect();

        // Build enum types map (name -> Type::new_enum(id))
        let enum_types: HashMap<Spur, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::new_enum(*id)))
            .collect();

        // Build method signatures with InferType for constraint generation
        let mut method_sigs: HashMap<(StructId, Spur), MethodSig> = self
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

        // Register builtin-type method signatures (String::len, etc.) so the
        // inference pass can resolve their return types. Builtin methods are NOT
        // in `self.methods` — they have no RIR body and are analyzed via a separate
        // path in `analysis.rs` — so without this, a builtin method-call result
        // used in a binop (e.g. `s.len() == 2`) was left unconstrained and resolved
        // to `<error>`, poisoning the other operand's literal-range check. (RUE-95)
        self.register_builtin_method_sigs(&mut method_sigs);

        // Constant types (resolved during declaration gathering) so a const
        // reference in a function body infers to its declared type instead of
        // `<error>` (RUE-142).
        let const_types: HashMap<Spur, Type> = self
            .constants
            .iter()
            .map(|(name, info)| (*name, info.ty))
            .collect();

        SemaContextInferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
            const_types,
        }
    }

    /// Register inference signatures for builtin-type methods (e.g. `String::len`).
    ///
    /// Builtin methods live in the `rue-builtins` registry, not in `self.methods`
    /// (they have no RIR body), so the constraint generator can't find them by the
    /// `(StructId, method_name)` key it uses for user methods. We add an equivalent
    /// `MethodSig` for each builtin method whose name is interned (i.e. referenced
    /// somewhere in the program — uncalled methods need no signature). Names are
    /// looked up, never interned, so this stays compatible with `&self`. (RUE-95)
    pub(crate) fn register_builtin_method_sigs(
        &self,
        method_sigs: &mut HashMap<(StructId, Spur), MethodSig>,
    ) {
        use rue_builtins::{BUILTIN_TYPES, BuiltinParamType, BuiltinReturnType};

        for builtin in BUILTIN_TYPES {
            let Some(name_spur) = self.interner.get(builtin.name) else {
                continue;
            };
            let Some(&struct_id) = self.structs.get(&name_spur) else {
                continue;
            };
            let struct_type = Type::new_struct(struct_id);

            for method in builtin.methods {
                let Some(method_spur) = self.interner.get(method.name) else {
                    continue; // method never referenced in this program
                };
                let param_types = method
                    .params
                    .iter()
                    .map(|p| {
                        let ty = match p.ty {
                            BuiltinParamType::U64 => Type::U64,
                            BuiltinParamType::U8 => Type::U8,
                            BuiltinParamType::Bool => Type::BOOL,
                            BuiltinParamType::SelfType => struct_type,
                        };
                        self.type_to_infer_type(ty)
                    })
                    .collect();
                let return_ty = match method.return_ty {
                    BuiltinReturnType::Unit => Type::UNIT,
                    BuiltinReturnType::U64 => Type::U64,
                    BuiltinReturnType::U8 => Type::U8,
                    BuiltinReturnType::Bool => Type::BOOL,
                    BuiltinReturnType::SelfType => struct_type,
                };
                method_sigs.insert(
                    (struct_id, method_spur),
                    MethodSig {
                        struct_type,
                        has_self: true,
                        param_types,
                        return_type: self.type_to_infer_type(return_ty),
                    },
                );
            }

            // Associated functions (e.g. `String::new`, `String::with_capacity`) are
            // resolved by inference through the same `(StructId, name)` method map (see
            // `InstData::AssocFnCall` in inference/generate.rs). Like methods, they live
            // only in the rue-builtins registry, so without registering them an untyped
            // literal arg (`String::with_capacity(8)`) isn't constrained to the param's
            // `u64` and a result feeding a literal resolves to `<error>`. (RUE-95 sibling)
            for assoc_fn in builtin.associated_fns {
                let Some(fn_spur) = self.interner.get(assoc_fn.name) else {
                    continue; // never referenced in this program
                };
                let param_types = assoc_fn
                    .params
                    .iter()
                    .map(|p| {
                        let ty = match p.ty {
                            BuiltinParamType::U64 => Type::U64,
                            BuiltinParamType::U8 => Type::U8,
                            BuiltinParamType::Bool => Type::BOOL,
                            BuiltinParamType::SelfType => struct_type,
                        };
                        self.type_to_infer_type(ty)
                    })
                    .collect();
                let return_ty = match assoc_fn.return_ty {
                    BuiltinReturnType::Unit => Type::UNIT,
                    BuiltinReturnType::U64 => Type::U64,
                    BuiltinReturnType::U8 => Type::U8,
                    BuiltinReturnType::Bool => Type::BOOL,
                    BuiltinReturnType::SelfType => struct_type,
                };
                method_sigs.insert(
                    (struct_id, fn_spur),
                    MethodSig {
                        struct_type,
                        has_self: false,
                        param_types,
                        return_type: self.type_to_infer_type(return_ty),
                    },
                );
            }
        }
    }
}
