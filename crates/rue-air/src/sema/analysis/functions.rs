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
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes
        let param_info: Vec<(Spur, Type, RirParamMode, bool)> = params
            .map(|p| {
                let ty = self.resolve_type(p.ty, span)?;
                // spec 4.14:5 — a parameter of type `type` must be marked
                // `comptime`. Without this gate a `type`-valued runtime
                // parameter flows into codegen and ICEs ("block has no
                // terminator", RUE-217) instead of a clean legality error.
                self.reject_runtime_type_value(ty, p.is_comptime, span)?;
                Ok((p.name, ty, p.mode, p.is_comptime))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        let function_symbol = self.interner.get_or_intern(fn_name);
        let producer = self
            .canonical_function_producer(function_symbol, &HashMap::new(), &HashMap::new())
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical producer for '{fn_name}': {failure:?}"
                    )),
                    span,
                )
            })?;
        let crate::StableProducerId::Function(identity) = &producer.0 else {
            unreachable!("a callable body producer is always a function")
        };
        let identity = (**identity).clone();
        let previous_producer = self.active_anonymous_producer.replace(producer);
        let analysis = self.analyze_function(
            infer_ctx,
            ret_type,
            &param_info,
            body,
            allow_unused_variable,
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
                name: fn_name.to_string(),
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
                allow_unreachable_code,
            },
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ))
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
            self.reject_runtime_type_value(ty, p.is_comptime, span)?;
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
    pub(super) fn analyze_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)], // (name, type, mode, is_comptime)
        body: InstRef,
        allow_unused_variable: bool,
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
        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            None,
            None,
            false,
            allow_unused_variable,
            // Free and associated functions have no receiver, so no binding
            // can be a mutable by-value `self`.
            false,
        )
    }

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
    pub(super) fn analyze_function_internal(
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
        let mut air = Air::new(return_type);

        // Preview gate (RUE-15 / ADR-0037): a `borrow self` / `inout self`
        // receiver lowers to a synthetic `self` parameter carrying a non-Normal
        // mode. `self` can never be a user-written parameter name (it is a
        // dedicated keyword), so this is a reliable single chokepoint for
        // every method body — named or anonymous struct — that actually
        // reaches analysis. By-value `self` (Normal) and destructors are
        // unaffected.
        let self_sym = self.interner.get_or_intern("self");
        if let Some((_, _, mode, _)) = params
            .iter()
            .find(|(name, _, mode, _)| *name == self_sym && *mode != RirParamMode::Normal)
        {
            debug_assert!(matches!(mode, RirParamMode::Inout | RirParamMode::Borrow));
        }

        // Classify and total every parameter before allocating per-slot mode
        // metadata. This makes a cumulatively oversized signature fail with
        // E0907 instead of first attempting a displacement-sized allocation
        // (RUE-780).
        let mut num_param_slots = 0_u32;
        let mut param_layouts = Vec::with_capacity(params.len());
        for (pname, ptype, mode, _) in params.iter() {
            // Only the synthetic `self` entry of a `mut self` method can be a
            // mutable by-value binding; ordinary parameters have no `mut`
            // form.
            let is_mut_binding = self_is_mut && *pname == self_sym && *mode == RirParamMode::Normal;
            // Inout and Borrow parameters are passed by reference.
            // Comptime parameters are VALUE params (like `comptime n: i32`), passed by value.
            // Normal parameters are passed by value.
            //
            // Exception (ADR-0043, RUE-322): a slice parameter `borrow s: [T]`
            // is itself a two-word fat pointer `{ptr, len}`. The `borrow`/`inout`
            // keyword marks shared-vs-exclusive access, not an extra level of
            // indirection, so the slice value is passed BY VALUE through the
            // existing multi-slot aggregate ABI (2 slots) rather than as a
            // pointer-to-slice. The call site materializes the fat pointer.
            //
            // `str` intentionally diverges for `inout`: it is first-class and
            // reassignable, so assigning to an `inout str` parameter must
            // rebind the caller's `{ptr, len}` fat pointer. That requires a
            // normal by-reference parameter slot; otherwise AIR emits
            // ParamStore but codegen sees a by-value param slot and panics
            // (RUE-385).
            let is_slice = self.slice_element_type(*ptype).is_some();
            let is_inout_str = *mode == RirParamMode::Inout && self.is_str_struct(*ptype);
            // Exact `borrow Str(N)` / `inout Str(N)` parameters retain their
            // nominal fixed-capacity type and use the ordinary one-pointer
            // by-reference ABI. Only actual slice views (including bare
            // `borrow str`) use the materialized two-slot by-value ABI.
            let is_exact_str_fixed_ref = matches!(mode, RirParamMode::Borrow | RirParamMode::Inout)
                && self.is_str_fixed_struct(*ptype);
            let is_slice_by_value = is_slice && !is_inout_str && !is_exact_str_fixed_ref;
            let is_by_ref = (*mode == RirParamMode::Inout || *mode == RirParamMode::Borrow)
                && !is_slice_by_value;
            let slot_count = if is_by_ref {
                // By-ref parameters are always 1 slot (pointer)
                1
            } else {
                // A by-value parameter materializes the whole object in the
                // frame: reject an oversized type (E0906, RUE-561).
                self.require_layout_slots(*ptype, self.rir.get(body).span)?
            };
            self.reserve_frame_slots(&mut num_param_slots, slot_count, self.rir.get(body).span)?;
            param_layouts.push((is_by_ref, is_mut_binding, slot_count));
        }

        let mut param_vec: Vec<ParamInfo> = Vec::with_capacity(params.len());
        let mut param_by_ref: Vec<bool> = Vec::with_capacity(num_param_slots as usize);
        let mut param_writable: Vec<bool> = Vec::with_capacity(num_param_slots as usize);

        // Publish the already-validated offsets and per-slot modes.
        let mut next_abi_slot = 0_u32;
        for ((pname, ptype, mode, is_comptime), (is_by_ref, is_mut_binding, slot_count)) in
            params.iter().zip(param_layouts)
        {
            param_vec.push(ParamInfo {
                name: *pname,
                abi_slot: next_abi_slot,
                ty: *ptype,
                mode: *mode,
                is_comptime: *is_comptime,
                is_mut: is_mut_binding,
            });
            param_by_ref.resize(param_by_ref.len() + slot_count as usize, is_by_ref);
            // "Writable" means the body may store to the slot: `inout`
            // (by-ref, written back to the caller) or a `mut self` receiver
            // (by-value, callee-local).
            param_writable.resize(
                param_writable.len() + slot_count as usize,
                *mode == RirParamMode::Inout || is_mut_binding,
            );
            next_abi_slot += slot_count;
        }
        debug_assert_eq!(next_abi_slot, num_param_slots);
        let param_modes = ParamSlotModes::new(param_by_ref, param_writable);

        // The callee owns its pass-by-value (Normal) parameters and must drop
        // them at exit unless they are moved out (RUE-61). Inout/borrow params
        // stay owned by the caller; comptime params are substituted away.
        // Destructors clear this list after analysis (see the destructor path).
        air.set_param_drops(
            param_vec
                .iter()
                .filter(|p| {
                    p.mode == RirParamMode::Normal && !p.is_comptime && p.abi_slot < num_param_slots
                })
                .map(|p| (p.abi_slot, p.ty))
                .collect(),
        );

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let resolved_types = self.run_type_inference(
            infer_ctx,
            return_type,
            params,
            body,
            type_subst,
            value_subst,
        )?;

        // Create analysis context with resolved types
        // If type_subst is provided, initialize comptime_type_vars with the substitutions
        // so that type parameters can be resolved during struct initialization.
        let comptime_type_vars = type_subst.map(|s| s.clone()).unwrap_or_else(HashMap::new);
        let comptime_value_vars = value_subst.map(|s| s.clone()).unwrap_or_else(HashMap::new);
        let (canonical_producer, canonical_producer_arguments) =
            self.active_anonymous_producer.clone().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(
                        "body analysis started without a canonical producer identity".into(),
                    ),
                    self.rir.get(body).span,
                )
            })?;
        let crate::StableProducerId::Function(canonical_function_identity) = &canonical_producer
        else {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "callable body has a non-function canonical producer".into(),
                ),
                self.rir.get(body).span,
            ));
        };
        let canonical_function_identity = (**canonical_function_identity).clone();
        let mut ctx = AnalysisContext {
            producer: body,
            canonical_producer,
            canonical_producer_arguments,
            canonical_function_identity,
            current_file_id: self.rir.get(body).span.file_id,
            locals: HashMap::new(),
            params: &param_vec,
            next_slot: 0,
            loop_depth: 0,
            checked_depth: 0,
            loop_break_stack: Vec::new(),
            used_locals: HashSet::new(),
            return_type,
            scope_stack: Vec::new(),
            moved_scope_stack: Vec::new(),
            comptime_type_scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: HashMap::new(),
            warnings: Vec::new(),
            allow_unused_variables: allow_unused_variable,
            local_string_table: HashMap::new(),
            local_strings: Vec::new(),
            local_atoms: Vec::new(),
            comptime_type_vars,
            comptime_value_vars,
            referenced_functions: HashSet::new(),
            referenced_methods: HashSet::new(),
            byref_arg_root: None,
            call_loaned_roots: Vec::new(),
            in_loop_move_recheck: false,
            iter_borrows: Vec::new(),
            expected_type: None,
            try_operand: false,
        };

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types. A
        // `str`-returning function (ADR-0043 Phase 3, RUE-324) supplies `str` as
        // the expected type so an implicit-return string literal (the block's
        // tail expression) materializes as a static-backed first-class `str`.
        // Inner `let`s clear `expected_type` for their own initializers (they
        // `take()` it), so this only reaches the tail value.
        if self.is_str_like(return_type) {
            ctx.expected_type = Some(return_type);
        }
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;
        ctx.expected_type = None;

        // Linear parameters: the callee owns its pass-by-value parameters and
        // drops them at exit unless moved out (RUE-61), so a by-value
        // parameter carrying a linear value must be consumed by the body on
        // every path — exactly like a linear local (RUE-176). Inout/borrow
        // parameters stay owned by the caller and comptime parameters are
        // substituted away; destructors are exempt (see the doc comment).
        if !is_destructor {
            for p in &param_vec {
                if p.mode != RirParamMode::Normal || p.is_comptime {
                    continue;
                }
                if !self.type_requires_consumption(p.ty) {
                    continue;
                }
                let state = self.moved_state(&ctx, &p.name);
                if !state.is_some_and(|s| s.full_move_on_all_paths) {
                    // Element-wise consumption of a linear array parameter
                    // (RUE-186) satisfies the obligation like a whole move.
                    match self.check_array_elementwise_consumption(
                        p.ty,
                        state,
                        p.name,
                        self.rir.get(body).span,
                    )? {
                        ElementwiseConsumption::Complete => continue,
                        ElementwiseConsumption::NotElementwise => {}
                    }
                    let name = self.interner.resolve(&p.name);
                    let err = linear_not_consumed_error(
                        name,
                        self.rir.get(body).span,
                        state.and_then(|s| s.full_move),
                    )
                    .with_note(format!(
                        "parameter '{name}' is passed by value, so this function owns it \
                         and must consume it (pass it on, return it, or destructure it)"
                    ));
                    return Err(self.attach_infectious_linear_note(err, p.ty));
                }
            }
        }

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::NEVER {
            // Two-types model (ADR-0043, RUE-386): a `str`-returning function's
            // implicit-return (tail) value must be a first-class `str`. A buffer
            // (`StrBuf`/`Str(N)`) or a borrowed `str` view escaping here dangles
            // once its backing storage is dropped. (Explicit `return x;` is
            // checked in `analyze_return`.)
            if self.is_str_struct(return_type) {
                let tail = self.rir_block_tail_expr(body);
                self.reject_non_first_class_str(
                    tail,
                    body_result.ty,
                    FirstClassStrSite::Return,
                    self.rir.get(tail).span,
                    &ctx,
                )?;
            }
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(body_result.air_ref)),
                ty: return_type,
                span: self.rir.get(body).span,
            });
        }

        // AIR emission can select a nominal type through paths which already
        // carry a resolved `Type` (match patterns, inferred expressions, and
        // comptime type values) without calling the textual type resolver.
        // Observe those types while the current body owner is still installed;
        // this walks produced AIR values, never RIR, and retains no AIR.
        if self
            .declaration_type_observer
            .as_ref()
            .is_some_and(|observer| observer.4 == crate::DeclarationTypeDependencyKind::Body)
        {
            let mut observed_types =
                Vec::with_capacity(1 + param_vec.len() + air.instructions().len());
            observed_types.push(return_type);
            observed_types.extend(param_vec.iter().map(|param| param.ty));
            observed_types.extend(air.instructions().iter().map(|inst| inst.ty));
            self.body_analysis_work
                .body_dependency_air_instructions_observed += air.instructions().len();
            for ty in observed_types {
                self.record_resolved_declaration_type(ty);
            }
        }

        if self.one_body_error_recovery && !self.one_body_recovered_errors.is_empty() {
            return Err(self.one_body_recovered_errors[0].clone());
        }

        Ok((
            air,
            ctx.next_slot,
            num_param_slots,
            param_modes,
            ctx.warnings,
            ctx.local_strings,
            ctx.local_atoms,
            ctx.referenced_functions,
            ctx.referenced_methods,
        ))
    }

    /// Analyze a specialized function body.
    ///
    /// This is similar to `analyze_function` but for generic function specialization.
    /// The `type_subst` map provides substitutions for type parameters to their
    /// concrete types; the `value_subst` map provides the concrete values of the
    /// comptime value parameters (RUE-166).
    ///
    /// For example, when specializing `fn identity<T>(x: T) -> T { x }` with `T = i32`,
    /// the `params` will be `[(x, i32, Normal)]` and `return_type` will be `i32`.
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
