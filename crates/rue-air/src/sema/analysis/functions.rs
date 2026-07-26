//! Function-level analysis orchestration: single/method/destructor/specialized function entry points and their bodies.
//!
//! This category owns body setup, analysis, and finalization for each kind of
//! callable.

use super::*;

impl<'a> BodySema<'a> {
    pub(in crate::sema) fn analyze_single_function<P>(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        return_type: Spur,
        params: P,
        body: InstRef,
        span: Span,
        allow_unused_variable: bool,
        allow_unreachable_code: bool,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )>
    where
        P: ExactSizeIterator<Item = rue_rir::RirParam> + Clone,
    {
        super::super::ordinary_engine::OrdinaryBodyEngine::new(self).analyze_single_function(
            infer_ctx,
            fn_name,
            return_type,
            params,
            body,
            span,
            allow_unused_variable,
            allow_unreachable_code,
        )
    }

    /// Analyze a method function from an impl block.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    pub(in crate::sema) fn analyze_method_function<P>(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        return_type: Spur,
        params: P,
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
        self_mode: RirParamMode,
        self_is_mut: bool,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )>
    where
        P: ExactSizeIterator<Item = rue_rir::RirParam> + Clone,
    {
        // `Self` in a method signature (return or parameter position) resolves
        // to the enclosing struct's type, just like the receiver (RUE-123).
        let ret_type = self.resolve_type_with_self(return_type, struct_type, span)?;

        // Build parameter list, adding self as first parameter for methods
        let mut param_info: Vec<(Spur, Type, RirParamMode, bool)> = Vec::new();

        if has_self {
            // Add self parameter in the receiver's declared mode (by-value
            // `self`, or by-ref `borrow`/`inout self`; RUE-15).
            let self_sym = self.interner.get_or_intern("self");
            param_info.push((self_sym, struct_type, self_mode, false));
        }

        // Add regular parameters with their modes
        for p in params {
            let ty = self.resolve_type_with_self(p.ty, struct_type, span)?;
            // spec 4.14:5 — a parameter of type `type` must be marked
            // `comptime` (RUE-217); reject the runtime-`type` case cleanly
            // rather than letting it ICE in codegen.
            super::super::ordinary_engine::reject_runtime_type_value(ty, p.is_comptime, span)?;
            param_info.push((p.name, ty, p.mode, p.is_comptime));
        }

        // Bind `Self` to the enclosing struct type so that `Self { ... }`
        // literals and `Self`-typed locals resolve in the method body, exactly
        // as they do for anonymous-struct methods (RUE-123).
        let self_sym = self.interner.get_or_intern("Self");
        let mut type_subst = HashMap::new();
        type_subst.insert(self_sym, struct_type);

        let method_symbol = self.interner.get_or_intern(full_name);
        let identity = crate::FunctionInstanceKey::Definition(
            self.function_identity(method_symbol).map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical producer for method '{full_name}': {failure:?}"
                    )),
                    span,
                )
            })?,
        );
        let producer = (
            crate::StableProducerId::Function(Box::new(identity.clone())),
            crate::CanonicalArguments::default(),
        );
        let previous_producer = self.active_anonymous_producer.replace(producer);
        let analysis = self.analyze_function_internal(
            infer_ctx,
            ret_type,
            &param_info,
            body,
            Some(&type_subst),
            None,
            false,
            false,
            self_is_mut,
        );
        self.active_anonymous_producer = previous_producer;
        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            local_atoms,
            ref_fns,
            ref_meths,
        ) = analysis?;

        Ok((
            AnalyzedFunction {
                identity,
                callable_kind: crate::AnalyzedCallableKind::Ordinary,
                ordinary_owner: None,
                name: full_name.to_string(),
                implicit_drop_source: None,
                air: crate::ValidatedAir::from_semantic_air_with_symbols(
                    air,
                    &self.type_pool,
                    self.interner,
                )?,
                local_atoms,
                num_locals,
                num_param_slots,
                param_modes,
                allow_unreachable_code: false,
            },
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ))
    }

    /// Analyze a destructor function.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    pub(in crate::sema) fn analyze_destructor_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        body: InstRef,
        _span: Span,
        struct_type: Type,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // Destructors take self parameter and return unit
        let self_sym = self.interner.get_or_intern("self");
        let param_info: Vec<(Spur, Type, RirParamMode, bool)> =
            vec![(self_sym, struct_type, RirParamMode::Normal, false)];

        let owner_name = self
            .type_pool
            .struct_def(
                struct_type
                    .as_struct()
                    .expect("a named destructor owner must be a struct"),
            )
            .name
            .clone();
        let destructor_identity = self
            .stable_definition_token(
                _span.file_id.index(),
                &owner_name,
                Some(&owner_name),
                crate::StableDefinitionKind::Destructor,
            )
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical producer for destructor '{full_name}': {failure:?}"
                    )),
                    _span,
                )
            })?;
        let producer = (
            crate::StableProducerId::Function(Box::new(crate::FunctionInstanceKey::Definition(
                destructor_identity,
            ))),
            crate::CanonicalArguments::default(),
        );
        let previous_producer = self.active_anonymous_producer.replace(producer);
        let analysis = self.analyze_function_internal(
            infer_ctx,
            Type::UNIT,
            &param_info,
            body,
            None,
            None,
            /* is_destructor */ true,
            false,
            false,
        );
        self.active_anonymous_producer = previous_producer;
        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            local_atoms,
            ref_fns,
            ref_meths,
        ) = analysis?;

        reject_self_move_in_destructor(&air, full_name)?;

        // The destructor consumes `self`; the drop glue (not the destructor
        // itself) drops the fields afterwards, so the destructor must not
        // re-drop its own parameter — that would recurse forever.
        air.clear_param_drops();

        Ok((
            AnalyzedFunction {
                identity: crate::FunctionInstanceKey::Definition(destructor_identity),
                callable_kind: crate::AnalyzedCallableKind::Destructor,
                ordinary_owner: None,
                name: full_name.to_string(),
                implicit_drop_source: None,
                air: crate::ValidatedAir::from_semantic_air_with_symbols(
                    air,
                    &self.type_pool,
                    self.interner,
                )?,
                local_atoms,
                num_locals,
                num_param_slots,
                param_modes,
                allow_unreachable_code: false,
            },
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ))
    }
    /// Analyze a single function, producing AIR.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation,
    /// avoiding the cost of rebuilding maps for each function.
    ///
    /// Returns (air, num_locals, num_param_slots, param_modes, warnings).
    /// Warnings are collected per-function and merged during finalization.

    /// Internal function analysis with optional type substitutions.
    ///
    /// When `type_subst` is provided (for specialized generic functions), it populates
    /// `comptime_type_vars` so that type parameters can be resolved in struct initialization
    /// (e.g., `P { x: 1, y: 2 }` where `P` is a type parameter).
    ///
    /// `is_destructor` exempts the function from the linear-parameter
    /// must-consume check: a destructor's `self` is disposed of by the drop
    /// glue after the body runs, and moving it out is rejected anyway
    /// (RUE-139), so requiring consumption would make destructors on linear
    /// types impossible to write.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_function_internal(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        type_subst: Option<&std::collections::HashMap<Spur, Type>>,
        value_subst: Option<&std::collections::HashMap<Spur, ConstValue>>,
        is_destructor: bool,
        allow_unused_variable: bool,
        self_is_mut: bool,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        super::super::ordinary_engine::OrdinaryBodyEngine::new(self).analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            type_subst,
            value_subst,
            is_destructor,
            allow_unused_variable,
            self_is_mut,
        )
    }

    pub fn analyze_specialized_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &std::collections::HashMap<Spur, ConstValue>,
        self_is_mut: bool,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // For specialized functions, we need to populate comptime_type_vars with the
        // type substitutions so that references to type parameters (like `P { ... }`)
        // can be resolved in the function body, and comptime_value_vars with the
        // value substitutions so comptime contexts (comptime blocks, arguments to
        // further comptime parameters, comptime-known branch conditions) see the
        // concrete values.
        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            Some(type_subst),
            Some(value_subst),
            false,
            false,
            self_is_mut,
        )
    }

    /// Analyze a method body with `Self` type resolution.
    ///
    /// This is used for anonymous struct methods where `Self` should resolve to the
    /// struct type. The `self_type` is added to the type substitution map under the
    /// symbol "Self", allowing `Self { ... }` struct literals to work correctly.
    pub(in crate::sema) fn analyze_method_body(
        &mut self,
        infer_ctx: &InferenceContext,
        method_name: Spur,
        has_self: bool,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        self_type: Type,
        captured_comptime_values: &std::collections::HashMap<Spur, ConstValue>,
        enclosing_type_subst: &std::collections::HashMap<Spur, Type>,
        self_is_mut: bool,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // Create a type substitution map with Self -> the struct type, plus the
        // enclosing `-> type` constructor's type parameters (e.g. `T -> i32`
        // for `Vec(i32)`), so those parameters resolve throughout the method
        // body — `let x: T`, `Option(T)`, etc. (RUE-313). `Self` is inserted
        // last so it always wins over any same-named constructor parameter.
        let self_sym = self.interner.get_or_intern("Self");
        let mut type_subst = enclosing_type_subst.clone();
        type_subst.insert(self_sym, self_type);

        let canonical_producer = self
            .canonical_anonymous_member_producer(
                self_type,
                method_name,
                if has_self {
                    crate::AnonymousMemberKind::Method
                } else {
                    crate::AnonymousMemberKind::AssociatedFunction
                },
            )
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue anonymous method producer: {failure:?}"
                    )),
                    self.rir.get(body).span,
                )
            })?;
        let expected_kind = if has_self {
            crate::AnonymousMemberKind::Method
        } else {
            crate::AnonymousMemberKind::AssociatedFunction
        };
        let producer = self
            .one_body_requested_producer
            .as_ref()
            .filter(|requested| {
                matches!(
                    requested,
                    crate::StableProducerId::Function(requested)
                        if matches!(
                            requested.as_ref(),
                            crate::FunctionInstanceKey::AnonymousMember { member, .. }
                                if member.kind == expected_kind
                                    && member.name.as_ref() == self.interner.resolve(&method_name)
                        )
                )
            })
            .cloned()
            .unwrap_or(canonical_producer);
        let producer = (producer, crate::CanonicalArguments::default());
        let previous_producer = self.active_anonymous_producer.replace(producer);
        let analysis = self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            Some(&type_subst),
            Some(captured_comptime_values),
            false,
            false,
            self_is_mut,
        );
        self.active_anonymous_producer = previous_producer;
        analysis
    }

    /// Analyze the body of a `drop fn(self)` destructor declared inside an
    /// anonymous struct (RUE-312).
    ///
    /// This is the anon-struct analog of [`Self::analyze_destructor_function`]
    /// (which handles named-struct `drop fn Name(self)`): it resolves `Self` to
    /// the monomorphized struct type (like [`Self::analyze_method_body`]) *and*
    /// applies destructor semantics — `is_destructor = true` exempts the
    /// linear-parameter must-consume check, a self-move out of the body is
    /// rejected (`reject_self_move_in_destructor`, RUE-139), and the owned
    /// parameter drop list is cleared so the drop glue (not the destructor)
    /// disposes of `self`'s fields, avoiding infinite recursion. The return
    /// type is always unit.
    #[allow(clippy::type_complexity)]
    pub(in crate::sema) fn analyze_anon_destructor_body(
        &mut self,
        infer_ctx: &InferenceContext,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        self_type: Type,
        captured_comptime_values: &std::collections::HashMap<Spur, ConstValue>,
        method_name: Spur,
        full_name: &str,
        enclosing_type_subst: &std::collections::HashMap<Spur, Type>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // Resolve `Self` to the concrete struct type.
        let self_sym = self.interner.get_or_intern("Self");
        // Seed with the enclosing `-> type` constructor's params (`T -> i32`)
        // so a generic destructor body can name `T` (RUE-313), then `Self` last.
        let mut type_subst = enclosing_type_subst.clone();
        type_subst.insert(self_sym, self_type);

        let canonical_producer = self
            .canonical_anonymous_member_producer(
                self_type,
                method_name,
                crate::AnonymousMemberKind::Destructor,
            )
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue anonymous destructor producer: {failure:?}"
                    )),
                    self.rir.get(body).span,
                )
            })?;
        let producer = self
            .one_body_requested_producer
            .as_ref()
            .filter(|requested| {
                matches!(
                    requested,
                    crate::StableProducerId::Function(requested)
                        if matches!(
                            requested.as_ref(),
                            crate::FunctionInstanceKey::AnonymousMember { member, .. }
                                if member.kind == crate::AnonymousMemberKind::Destructor
                                    && member.name.as_ref() == self.interner.resolve(&method_name)
                        )
                )
            })
            .cloned()
            .unwrap_or(canonical_producer);
        let producer = (producer, crate::CanonicalArguments::default());
        let previous_producer = self.active_anonymous_producer.replace(producer);
        let analysis = self.analyze_function_internal(
            infer_ctx,
            Type::UNIT,
            params,
            body,
            Some(&type_subst),
            Some(captured_comptime_values),
            /* is_destructor */ true,
            false,
            false,
        );
        self.active_anonymous_producer = previous_producer;
        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            local_atoms,
            ref_fns,
            ref_meths,
        ) = analysis?;

        reject_self_move_in_destructor(&air, full_name)?;

        // The destructor consumes `self`; the drop glue drops the fields
        // afterwards, so the destructor must not re-drop its own parameter.
        air.clear_param_drops();

        Ok((
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            local_atoms,
            ref_fns,
            ref_meths,
        ))
    }
}
