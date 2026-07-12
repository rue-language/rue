//! Function-level analysis orchestration: single/method/destructor/specialized function entry points and their bodies.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

impl<'a> Sema<'a> {
    pub(super) fn analyze_single_function(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
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
    )> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes
        let param_info: Vec<(Spur, Type, RirParamMode, bool)> = params
            .iter()
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

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(
            infer_ctx,
            ret_type,
            &param_info,
            body,
            allow_unused_variable,
        )?;

        Ok((
            AnalyzedFunction {
                name: fn_name.to_string(),
                implicit_drop_source: None,
                air,
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
    pub(super) fn analyze_method_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
        self_mode: RirParamMode,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
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
        for p in params.iter() {
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

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function_internal(
            infer_ctx,
            ret_type,
            &param_info,
            body,
            Some(&type_subst),
            None,
            false,
            false,
        )?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                implicit_drop_source: None,
                air,
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
    pub(super) fn analyze_destructor_function(
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

        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function_internal(
            infer_ctx,
            Type::UNIT,
            &param_info,
            body,
            None,
            None,
            /* is_destructor */ true,
            false,
        )?;

        reject_self_move_in_destructor(&air, full_name)?;

        // The destructor consumes `self`; the drop glue (not the destructor
        // itself) drops the fields afterwards, so the destructor must not
        // re-drop its own parameter — that would recurse forever.
        air.clear_param_drops();

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                implicit_drop_source: None,
                air,
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
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
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

        let mut param_vec: Vec<ParamInfo> = Vec::new();
        let mut param_by_ref: Vec<bool> = Vec::new();
        let mut param_writable: Vec<bool> = Vec::new();

        // Add parameters to the param vec, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype, mode, is_comptime) in params.iter() {
            param_vec.push(ParamInfo {
                name: *pname,
                abi_slot: next_abi_slot,
                ty: *ptype,
                mode: *mode,
                is_comptime: *is_comptime,
            });
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
            for _ in 0..slot_count {
                param_by_ref.push(is_by_ref);
                param_writable.push(*mode == RirParamMode::Inout);
            }
            next_abi_slot += slot_count;
        }
        let num_param_slots = next_abi_slot;
        let param_modes = ParamSlotModes::new(param_by_ref, param_writable);

        // The callee owns its pass-by-value (Normal) parameters and must drop
        // them at exit unless they are moved out (RUE-61). Inout/borrow params
        // stay owned by the caller; comptime params are substituted away.
        // Destructors clear this list after analysis (see the destructor path).
        air.set_param_drops(
            param_vec
                .iter()
                .filter(|p| p.mode == RirParamMode::Normal)
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
        let mut ctx = AnalysisContext {
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
                let state = ctx.moved_vars.get(&p.name);
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

        Ok((
            air,
            ctx.next_slot,
            num_param_slots,
            param_modes,
            ctx.warnings,
            ctx.local_strings,
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
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
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
        )
    }

    /// Analyze a method body with `Self` type resolution.
    ///
    /// This is used for anonymous struct methods where `Self` should resolve to the
    /// struct type. The `self_type` is added to the type substitution map under the
    /// symbol "Self", allowing `Self { ... }` struct literals to work correctly.
    pub(super) fn analyze_method_body(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        self_type: Type,
        captured_comptime_values: &std::collections::HashMap<Spur, ConstValue>,
        enclosing_type_subst: &std::collections::HashMap<Spur, Type>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
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

        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            Some(&type_subst),
            Some(captured_comptime_values),
            false,
            false,
        )
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
    pub(super) fn analyze_anon_destructor_body(
        &mut self,
        infer_ctx: &InferenceContext,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        self_type: Type,
        captured_comptime_values: &std::collections::HashMap<Spur, ConstValue>,
        full_name: &str,
        enclosing_type_subst: &std::collections::HashMap<Spur, Type>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // Resolve `Self` to the concrete struct type.
        let self_sym = self.interner.get_or_intern("Self");
        // Seed with the enclosing `-> type` constructor's params (`T -> i32`)
        // so a generic destructor body can name `T` (RUE-313), then `Self` last.
        let mut type_subst = enclosing_type_subst.clone();
        type_subst.insert(self_sym, self_type);

        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function_internal(
            infer_ctx,
            Type::UNIT,
            params,
            body,
            Some(&type_subst),
            Some(captured_comptime_values),
            /* is_destructor */ true,
            false,
        )?;

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
            ref_fns,
            ref_meths,
        ))
    }
}
