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

    /// Scan an anonymous struct's methods for a method that declares its *own*
    /// `comptime T: type` parameter (a type parameter owned by the method, not
    /// by the enclosing `-> type` constructor). Returns the offending method's
    /// span and name if found.
    ///
    /// Such a method would need to be monomorphized per call over its own type
    /// parameter — a generics feature that is not yet supported (RUE-284).
    /// Detecting it here lets the constructor's comptime reduction raise a clear
    /// diagnostic *at the method* instead of silently failing method
    /// registration, which used to poison the whole constructor and surface as a
    /// misleading E1200 mis-located at the `let` that instantiates it.
    pub(crate) fn find_method_own_comptime_type_param(
        &self,
        methods: &rue_rir::RirAnonStructMethodsRange,
    ) -> Option<(Span, String)> {
        let method_refs = self.rir.anon_struct_methods(methods);
        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params,
                ..
            } = &method_inst.data
            {
                let params = self.rir.params(params);
                for p in params {
                    // A method-owned `comptime T: type` parameter: `comptime`
                    // modifier plus the `type` type. (The enclosing
                    // constructor's own comptime params are not method params,
                    // so they are never seen here.)
                    if p.is_comptime && self.interner.resolve(&p.ty) == "type" {
                        return Some((
                            method_inst.span,
                            self.interner.resolve(method_name).to_string(),
                        ));
                    }
                }
            }
        }
        None
    }

    /// Register methods from an anonymous struct type with type substitution (comptime-safe).
    ///
    /// This variant supports comptime parameter capture by using `resolve_type_for_comptime_with_subst`
    /// to resolve type parameters like `T` to their concrete types from the enclosing function's
    /// comptime arguments.
    ///
    /// For example, in:
    /// ```rue
    /// fn Wrapper(comptime T: type) -> type {
    ///     struct { value: T, fn get(self) -> T { self.value } }
    /// }
    /// ```
    /// When `Wrapper(i32)` is called, the type_subst map will contain `T -> i32`, so the
    /// method's return type `T` is resolved to `i32`.
    pub(crate) fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        _span: Span,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &std::collections::HashMap<Spur, ConstValue>,
    ) -> Option<()> {
        let method_refs = self.rir.anon_struct_methods(methods);

        // Track method names in this registration batch to detect duplicates
        let mut seen_methods: std::collections::HashSet<Spur> = std::collections::HashSet::new();

        // Stage registrations and commit only if the whole batch validates.
        // Inserting one-by-one left earlier methods registered when a later
        // one failed (e.g. a duplicate name), so re-evaluating the same
        // AnonStructType — which happens since the RUE-170 inference pre-pass
        // evaluates type aliases before analysis does — saw the methods as
        // "already registered", skipped this check, and silently succeeded.
        let mut staged: Vec<((StructId, Spur), MethodInfo)> = Vec::new();

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

                // Check for duplicate methods within this struct definition
                if seen_methods.contains(method_name) {
                    return None; // Duplicate method in same struct - evaluation fails
                }
                seen_methods.insert(*method_name);

                // Check if method was already registered from a previous call
                if self.has_method(key) {
                    return None;
                }

                // Resolve parameter types using comptime-safe resolution with substitution
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
                        self.resolve_type_for_comptime_with_subst_and_values_at_span(
                            p.ty,
                            type_subst,
                            value_subst,
                            method_inst.span,
                        )?
                    };
                    param_types.push(resolved_ty);
                }

                // Resolve return type
                let ret_type_str = self.interner.resolve(return_type);
                let ret_type = if ret_type_str == "Self" {
                    struct_type
                } else {
                    self.resolve_type_for_comptime_with_subst_and_values_at_span(
                        *return_type,
                        type_subst,
                        value_subst,
                        method_inst.span,
                    )?
                };

                // Allocate method parameters in the arena
                let param_range = self.param_arena.alloc_method(
                    param_names,
                    param_types,
                    param_modes,
                    param_comptime,
                );

                staged.push((
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
                ));
            }
        }
        for ((struct_id, method_name), info) in staged {
            let has_self = info.has_self;
            self.anonymous_methods
                .insert((struct_id, method_name), info);
            self.index_anonymous_callable_method(struct_id, method_name, has_self);
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

    /// Extract method signatures from RIR for structural equality comparison.
    ///
    /// This extracts method signatures as type symbols (Spur), not resolved Types.
    /// This is intentional: for structural equality, we compare type symbols directly
    /// so that `Self` matches `Self` even before we know the concrete StructId.
    pub(crate) fn extract_anon_method_sigs(
        &mut self,
        methods: &rue_rir::RirAnonStructMethodsRange,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &std::collections::HashMap<Spur, ConstValue>,
    ) -> Vec<super::super::AnonMethodSig> {
        fn resolve<D: crate::sema::DeclarationPhase>(
            sema: &mut crate::sema::Sema<'_, D>,
            symbol: Spur,
            type_subst: &std::collections::HashMap<Spur, Type>,
            value_subst: &std::collections::HashMap<Spur, ConstValue>,
            span: Span,
        ) -> crate::sema::AnonMethodType {
            use crate::sema::AnonMethodType as T;
            let spelling = sema.interner.resolve(&symbol).to_owned();
            if spelling == "Self" {
                return T::SelfType;
            }
            if let Some((element, len)) = crate::types::parse_array_type_syntax(&spelling) {
                let element = sema.interner.get_or_intern(&element);
                let len = sema
                    .resolve_array_length(&len, span, Some(value_subst))
                    .ok();
                return len.map_or_else(
                    || T::Syntax(spelling.into()),
                    |len| T::Array {
                        element: Box::new(resolve(sema, element, type_subst, value_subst, span)),
                        len,
                    },
                );
            }
            if let Some(pointee) = spelling.strip_prefix("ptr const ") {
                let pointee = sema.interner.get_or_intern(pointee);
                return T::PtrConst(Box::new(resolve(
                    sema,
                    pointee,
                    type_subst,
                    value_subst,
                    span,
                )));
            }
            if let Some(pointee) = spelling.strip_prefix("ptr mut ") {
                let pointee = sema.interner.get_or_intern(pointee);
                return T::PtrMut(Box::new(resolve(
                    sema,
                    pointee,
                    type_subst,
                    value_subst,
                    span,
                )));
            }
            sema.resolve_type_for_comptime_with_subst_and_values_at_span(
                symbol,
                type_subst,
                value_subst,
                span,
            )
            .map_or_else(|| T::Syntax(spelling.into()), T::Concrete)
        }

        let method_refs = self.rir.anon_struct_methods(methods);
        let mut sigs = Vec::with_capacity(method_refs.len());

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name,
                params,
                return_type,
                has_self,
                self_mode,
                ..
            } = &method_inst.data
            {
                // Extract the complete explicit-parameter signature. Passing
                // modes and comptime flags affect the callable contract just as
                // types do, so anonymous types that differ in any of them must
                // not share one StructId/method body (RUE-634).
                let params = self.rir.params(params);
                let param_types = params
                    .iter()
                    .map(|p| resolve(self, p.ty, type_subst, value_subst, method_inst.span))
                    .collect();
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

                sigs.push(super::super::AnonMethodSig {
                    name: *name,
                    has_self: *has_self,
                    self_mode: *self_mode,
                    param_types,
                    param_modes,
                    param_comptime,
                    return_type: resolve(
                        self,
                        *return_type,
                        type_subst,
                        value_subst,
                        method_inst.span,
                    ),
                });
            }
        }

        sigs
    }

    // ========================================================================
    // Pointer intrinsics (require unchecked context)
    // ========================================================================
}
