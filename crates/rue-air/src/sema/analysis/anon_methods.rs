//! Anonymous-struct method registration and comptime method-signature extraction.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

impl<'a> Sema<'a> {
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
        methods_start: u32,
        methods_len: u32,
        _span: Span,
    ) -> CompileResult<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                self_mode,
                ..
            } = &method_inst.data
            {
                let key = (struct_id, *method_name);

                // Check for duplicate methods
                if self.methods.contains_key(&key) {
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
                let params = self.rir.get_params(*params_start, *params_len);
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

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        self_mode: *self_mode,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
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
        methods_start: u32,
        methods_len: u32,
        _span: Span,
    ) -> Option<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                self_mode,
                ..
            } = &method_inst.data
            {
                let key = (struct_id, *method_name);

                // Check for duplicate methods - return None in comptime context
                if self.methods.contains_key(&key) {
                    return None;
                }

                // Resolve parameter types using comptime-safe resolution
                let params = self.rir.get_params(*params_start, *params_len);
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

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        self_mode: *self_mode,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
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
        methods_start: u32,
        methods_len: u32,
    ) -> Option<(Span, String)> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);
        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params_start,
                params_len,
                ..
            } = &method_inst.data
            {
                let params = self.rir.get_params(*params_start, *params_len);
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
        methods_start: u32,
        methods_len: u32,
        _span: Span,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &std::collections::HashMap<Spur, ConstValue>,
    ) -> Option<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

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
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                self_mode,
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
                if self.methods.contains_key(&key) {
                    return None;
                }

                // Resolve parameter types using comptime-safe resolution with substitution
                let params = self.rir.get_params(*params_start, *params_len);
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
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                ));
            }
        }
        self.methods.extend(staged);
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
        &self,
        methods_start: u32,
        methods_len: u32,
    ) -> Vec<super::super::AnonMethodSig> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);
        let mut sigs = Vec::with_capacity(method_refs.len());

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name,
                params_start,
                params_len,
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
                let params = self.rir.get_params(*params_start, *params_len);
                let param_types: Vec<Spur> = params.iter().map(|p| p.ty).collect();
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

                sigs.push(super::super::AnonMethodSig {
                    name: *name,
                    has_self: *has_self,
                    self_mode: *self_mode,
                    param_types,
                    param_modes,
                    param_comptime,
                    return_type: *return_type,
                });
            }
        }

        sigs
    }

    // ========================================================================
    // Pointer intrinsics (require unchecked context)
    // ========================================================================
}
