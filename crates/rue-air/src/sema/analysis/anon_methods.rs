//! Anonymous-struct method registration and comptime method-signature extraction.
//!
//! These helpers extend the canonical semantic-analysis implementation for
//! anonymous nominal types.

use super::*;

impl<'a, D: crate::sema::DeclarationPhase> crate::sema::Sema<'a, D> {
    /// Register methods from an anonymous struct type.
    ///
    /// This is called when an anonymous struct with methods is encountered during
    /// comptime evaluation. The methods are registered with the anonymous struct's
    /// StructId as the key, enabling method lookup via the standard method resolution
    /// mechanism.
    ///
    /// Note: Self type in method signatures is resolved to the anonymous struct's
    /// StructId during parameter type resolution.
    #[allow(dead_code)] // Currently unused; kept for reference. Methods are registered via _for_comptime variants.
    pub(super) fn register_anon_struct_methods(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        _span: Span,
    ) -> CompileResult<()> {
        let method_refs = self.rir.anon_struct_methods(methods);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params,
                return_type,
                body,
                has_self,
                self_mode,
                self_is_mut,
                ..
            } = &method_inst.data
            {
                let key = (struct_id, *method_name);

                // Check for duplicate methods
                if self.has_method(key) {
                    let struct_def = self.type_pool.struct_def(struct_id);
                    let method_name_str = self.interner.resolve(method_name).to_string();
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: struct_def.name.clone(),
                            method_name: method_name_str,
                        },
                        method_inst.span,
                    ));
                }

                // Resolve parameter types (Self -> this anonymous struct's type)
                let params = self.rir.params(params);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        // Resolve type, with Self mapping to this struct
                        self.resolve_type_with_self(p.ty, struct_type, method_inst.span)
                    })
                    .collect::<CompileResult<Vec<_>>>()?;
                let ret_type =
                    self.resolve_type_with_self(*return_type, struct_type, method_inst.span)?;

                // Allocate method parameters in the arena
                let param_range = self.param_arena.alloc_method(
                    param_names,
                    param_types,
                    param_modes,
                    param_comptime,
                );

                self.anonymous_methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        self_mode: *self_mode,
                        self_is_mut: *self_is_mut,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
                self.index_anonymous_callable_method(struct_id, *method_name, *has_self);
            }
        }
        Ok(())
    }

    /// Register methods from an anonymous struct type (comptime-safe version).
    ///
    /// This is the comptime-safe version of `register_anon_struct_methods`.
    /// It returns `Option<()>` instead of `CompileResult<()>`, allowing
    /// `try_evaluate_const` to gracefully fall back when method registration
    /// encounters issues that would be errors at compile time.
    ///
    /// Key differences from `register_anon_struct_methods`:
    /// - Uses `resolve_type_for_comptime` instead of `resolve_type`
    /// - Returns `None` on any failure instead of an error
    /// - Silently skips duplicate methods (returns None)
    #[allow(dead_code)] // Currently unused; methods registered via analyze_inst or _with_subst variant
    pub(super) fn register_anon_struct_methods_for_comptime(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        _span: Span,
    ) -> Option<()> {
        let method_refs = self.rir.anon_struct_methods(methods);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params,
                return_type,
                body,
                has_self,
                self_mode,
                self_is_mut,
                ..
            } = &method_inst.data
            {
                let key = (struct_id, *method_name);

                // Check for duplicate methods - return None in comptime context
                if self.has_method(key) {
                    return None;
                }

                // Resolve parameter types using comptime-safe resolution
                let params = self.rir.params(params);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();
                let mut param_types: Vec<Type> = Vec::with_capacity(params.len());

                for p in params {
                    // Resolve type, with Self mapping to this struct
                    let type_str = self.interner.resolve(&p.ty);
                    let resolved_ty = if type_str == "Self" {
                        struct_type
                    } else {
                        self.resolve_type_for_comptime(p.ty)?
                    };
                    param_types.push(resolved_ty);
                }

                // Resolve return type
                let ret_type_str = self.interner.resolve(return_type);
                let ret_type = if ret_type_str == "Self" {
                    struct_type
                } else {
                    self.resolve_type_for_comptime(*return_type)?
                };

                // Allocate method parameters in the arena
                let param_range = self.param_arena.alloc_method(
                    param_names,
                    param_types,
                    param_modes,
                    param_comptime,
                );

                self.anonymous_methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        self_mode: *self_mode,
                        self_is_mut: *self_is_mut,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
                self.index_anonymous_callable_method(struct_id, *method_name, *has_self);
            }
        }
        Some(())
    }

    /// Install query-resolved anonymous method signatures while using RIR only
    /// for request-local parameter names and body instruction locators.
    pub(crate) fn register_projected_anon_struct_methods(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        signatures: &[super::super::AnonMethodSig],
    ) -> Option<()> {
        fn materialize(ty: &super::super::AnonMethodType, self_type: Type) -> Option<Type> {
            match ty {
                super::super::AnonMethodType::SelfType => Some(self_type),
                super::super::AnonMethodType::Concrete(ty) => Some(*ty),
                // Query projections currently publish composites as concrete
                // canonical types. These variants remain valid for the cold
                // AIR evaluator but must not appear at this install seam.
                super::super::AnonMethodType::Array { .. }
                | super::super::AnonMethodType::PtrConst(_)
                | super::super::AnonMethodType::PtrMut(_)
                | super::super::AnonMethodType::Syntax(_) => None,
            }
        }

        let refs = self.rir.anon_struct_methods(methods);
        if refs.len() != signatures.len() {
            return None;
        }
        let mut seen = std::collections::HashSet::new();
        let mut staged = Vec::with_capacity(signatures.len());
        for (method_ref, signature) in refs.iter().zip(signatures) {
            let instruction = self.rir.get(*method_ref);
            let InstData::FnDecl {
                name,
                params,
                body,
                has_self,
                self_mode,
                self_is_mut,
                ..
            } = &instruction.data
            else {
                return None;
            };
            let parameters = self.rir.params(params);
            if *name != signature.name
                || *has_self != signature.has_self
                || *self_mode != signature.self_mode
                || parameters.len() != signature.param_types.len()
                || !parameters
                    .iter()
                    .zip(signature.param_modes.iter())
                    .all(|(parameter, mode)| parameter.mode == *mode)
                || !parameters
                    .iter()
                    .zip(signature.param_comptime.iter())
                    .all(|(parameter, comptime)| parameter.is_comptime == *comptime)
                || !seen.insert(*name)
                || self.has_method((struct_id, *name))
            {
                return None;
            }
            let types = signature
                .param_types
                .iter()
                .map(|ty| materialize(ty, struct_type))
                .collect::<Option<Vec<_>>>()?;
            let range = self.param_arena.alloc_method(
                parameters.iter().map(|parameter| parameter.name),
                types,
                signature.param_modes.clone(),
                signature.param_comptime.clone(),
            );
            staged.push((
                (struct_id, *name),
                MethodInfo {
                    struct_type,
                    has_self: *has_self,
                    self_mode: *self_mode,
                    self_is_mut: *self_is_mut,
                    params: range,
                    return_type: materialize(&signature.return_type, struct_type)?,
                    body: *body,
                    span: instruction.span,
                },
            ));
        }
        for ((owner, name), info) in staged {
            let has_self = info.has_self;
            self.anonymous_methods.insert((owner, name), info);
            self.index_anonymous_callable_method(owner, name, has_self);
        }
        Some(())
    }

    /// Resolve a type symbol, with special handling for Self.
    ///
    /// If the type symbol is "Self", it resolves to the provided self_type.
    /// Otherwise, it delegates to the standard resolve_type method.
    pub(crate) fn resolve_type_with_self(
        &mut self,
        type_sym: Spur,
        self_type: Type,
        span: Span,
    ) -> CompileResult<Type> {
        let type_str = self.interner.resolve(&type_sym);
        if type_str == "Self" {
            Ok(self_type)
        } else {
            self.resolve_type(type_sym, span)
        }
    }

    // ========================================================================
    // Pointer intrinsics (require unchecked context)
    // ========================================================================
}
