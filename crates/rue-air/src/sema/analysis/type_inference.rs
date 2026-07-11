//! Type-inference glue: running inference, projection analysis, and resolved-type lookups.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

impl<'a> Sema<'a> {
    /// Run Hindley-Milner type inference on a function body.
    ///
    /// This is Phases 1-2 of the HM algorithm:
    /// 1. Generate constraints by walking the RIR
    /// 2. Solve constraints via unification
    ///
    /// The `infer_ctx` parameter provides pre-computed type information (function
    /// signatures, struct/enum types, method signatures) converted to InferType format.
    /// This avoids rebuilding these maps for each function, reducing O(n²) to O(n).
    ///
    /// Returns a map from RIR instruction refs to their resolved concrete types.
    pub(super) fn run_type_inference(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<HashMap<InstRef, Type>> {
        // Pre-resolve `let`-bound comptime type aliases (`let P = F();` where
        // `F` returns `type`) so inference can see the concrete anonymous
        // struct types behind them. Without this, `P { ... }`, `let p: P`,
        // and methods on `P`-typed receivers all fell through to `<error>`
        // or unconstrained variables (RUE-170, RUE-164). This may create the
        // anonymous structs (idempotently — analysis re-evaluates the same
        // initializers later and structural equality dedups them). Keyed by
        // binding site (the `let`'s Alloc); the generator brings each alias
        // into scope when its statement is reached and unwinds it with the
        // enclosing block (RUE-530).
        let comptime_local_bindings =
            self.precompute_comptime_type_locals(body, type_subst, value_subst);

        // The inline-head pre-reduction below evaluates head expressions
        // without walking the body, so it can't replay lexical scope; give it
        // the flattened name view. Same-named ties resolve to the later
        // binding site deterministically (instruction order, which follows
        // program order) — matching the old flat map for this opportunistic
        // path.
        let mut flat_bindings: Vec<(InstRef, Type)> = comptime_local_bindings
            .iter()
            .map(|(inst_ref, ty)| (*inst_ref, *ty))
            .collect();
        flat_bindings.sort_by_key(|(inst_ref, _)| inst_ref.as_u32());
        let comptime_local_types: HashMap<Spur, Type> = flat_bindings
            .into_iter()
            .filter_map(|(inst_ref, ty)| match self.rir.get(inst_ref).data {
                rue_rir::InstData::Alloc {
                    name: Some(name), ..
                } => Some((name, ty)),
                _ => None,
            })
            .collect();

        // Pre-reduce inline type-constructor heads (`F(args).Variant(..)`,
        // `F(args) { ... }`; RUE-596) to their concrete types, keyed by the
        // head's `InstRef` — the nameless analogue of the alias map above.
        // Without this, a construction argument on an inline head was never
        // constrained and an integer payload literal defaulted to `i32`
        // (RUE-599). Runs before the lazy-method collection below so methods
        // registered while reducing a head are included in it.
        let inline_ctor_head_types =
            self.precompute_inline_ctor_head_types(type_subst, value_subst, &comptime_local_types);

        // Anonymous-struct methods are registered lazily (during comptime
        // evaluation, including the pre-pass above), after the shared
        // `InferenceContext` was built — so collect the signatures it doesn't
        // know about. Without these, a method call on an anonymous-struct
        // receiver inferred to `<error>` and poisoned sibling constraints
        // (RUE-164).
        let extra_method_sigs: HashMap<(StructId, Spur), crate::inference::MethodSig> = self
            .methods
            .iter()
            .filter(|(key, _)| !infer_ctx.method_sigs.contains_key(*key))
            .map(|(key, info)| {
                (
                    *key,
                    crate::inference::MethodSig {
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

        // Create constraint generator using pre-computed inference context
        let mut cgen = ConstraintGenerator::with_type_subst(
            self.rir,
            self.interner,
            &infer_ctx.func_sigs,
            &infer_ctx.struct_types,
            &infer_ctx.enum_types,
            &infer_ctx.method_sigs,
            &self.type_pool,
            type_subst,
        )
        .with_const_types(&infer_ctx.const_types)
        .with_const_type_aliases(&infer_ctx.const_type_aliases)
        .with_const_values(&infer_ctx.const_values)
        .with_const_function_aliases(&infer_ctx.const_function_aliases)
        .with_structs_by_file_name(&infer_ctx.struct_types_by_file_name)
        .with_enums_by_file_name(&infer_ctx.enum_types_by_file_name)
        .with_module_binding_types(&infer_ctx.module_binding_types)
        .with_module_file_ids(&infer_ctx.module_file_ids)
        .with_functions_by_file_name(&infer_ctx.functions_by_file_name)
        .with_comptime_local_bindings(&comptime_local_bindings)
        .with_inline_ctor_head_types(&inline_ctor_head_types)
        .with_comptime_values(value_subst)
        .with_extra_method_sigs(&extra_method_sigs);

        // Build parameter map for constraint context.
        // Convert Type to InferType so arrays are represented structurally.
        let mut param_vars: HashMap<Spur, ParamVarInfo> = params
            .iter()
            .map(|(name, ty, mode, _is_comptime)| {
                (
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(*ty),
                        is_inout: *mode == RirParamMode::Inout,
                    },
                )
            })
            .collect();

        // Add comptime value variables as if they were parameters
        // This allows constraint generation to see captured comptime values
        // (anonymous-struct methods capturing `comptime N` from the enclosing
        // function). Real parameters keep their declared type: in a
        // value-specialized body (RUE-166) the comptime value parameter is
        // also a runtime parameter with a precise type (e.g. `comptime n:
        // i64`), inserted into `param_vars` above, which the gap-filling
        // `or_insert` here must not clobber.
        //
        // A *captured* integer value carries only its magnitude — its declared
        // width is not threaded through the capture — so it is typed as a
        // fresh integer-literal variable and takes its width from use (a
        // `comptime N: u8` read where u8 is expected unifies to u8), exactly
        // like the literal it stands in for. Emission then reads that resolved
        // width back out of `resolved_types` instead of hard-coding i32
        // (RUE-216).
        if let Some(values) = value_subst {
            for (name, const_val) in values {
                let ty = match const_val {
                    ConstValue::Integer(_) => {
                        param_vars.entry(*name).or_insert(ParamVarInfo {
                            ty: InferType::Var(cgen.fresh_int_literal_var()),
                            is_inout: false,
                        });
                        continue;
                    }
                    ConstValue::Bool(_) => Type::BOOL,
                    ConstValue::Type(t) => *t,
                    ConstValue::Function(_) => Type::COMPTIME_TYPE,
                    ConstValue::Unit => Type::UNIT,
                };
                param_vars.entry(*name).or_insert(ParamVarInfo {
                    ty: self.type_to_infer_type(ty),
                    is_inout: false,
                });
            }
        }

        // Create constraint context
        let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

        // Phase 1: Generate constraints
        let body_info = cgen.generate(body, &mut cgen_ctx);

        // The function body's type must match the return type.
        // This handles implicit returns like `fn foo() -> i8 { 42 }`.
        // For arrays, we need to convert Type to InferType structurally.
        //
        // A `str` return type (ADR-0043 Phase 3, RUE-324) accepts an
        // implicit-return string literal (HM type `String`) by coercion; skip
        // strict equality there and let sema materialize the static-backed
        // first-class `str` at the tail expression.
        if !self.is_str_like(return_type) {
            cgen.add_constraint(Constraint::equal(
                body_info.ty,
                self.type_to_infer_type(return_type),
                body_info.span,
            ));
        }

        // Consume the constraint generator to release borrows
        let (constraints, int_literal_vars, expr_types, type_var_count) = cgen.into_parts();

        // Phase 2: Solve constraints via unification
        // Pre-size the substitution for better performance on large functions
        let mut unifier = Unifier::with_capacity(type_var_count);
        unifier.mark_int_literal_vars(&int_literal_vars);
        let errors = unifier.solve_constraints(&constraints);

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.name_with_pool(&self.type_pool),
                    found: found.name_with_pool(&self.type_pool),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.safe_name_with_pool(Some(&self.type_pool)),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!(
                        "{var} = {} (infinite type)",
                        ty.name_with_pool(&self.type_pool)
                    ),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegate(ty.safe_name_with_pool(Some(&self.type_pool)))
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                UnifyResult::ArrayLengthMismatch { expected, found } => {
                    ErrorKind::ArrayLengthMismatch {
                        expected: *expected,
                        found: *found,
                    }
                }
            };

            let mut compile_error = CompileError::new(error_kind, err.span);

            // Add note for unsigned negation errors
            if matches!(err.kind, UnifyResult::NotSigned { .. }) {
                compile_error = compile_error.with_note("unsigned values cannot be negated");
            }

            return Err(compile_error);
        }

        // Default any unconstrained integer literals to i32
        unifier.default_int_literal_vars(&int_literal_vars);

        // Pre-collect all array types from resolved InferTypes before converting them.
        // This ensures all array types are created before the conversion loop, which
        // enables parallelization of function analysis (mutation happens here, not in
        // infer_type_to_type).
        for (_, infer_ty) in &expr_types {
            let resolved = unifier.resolve_infer_type(infer_ty);
            self.pre_create_array_types_from_infer_type(&resolved);
        }

        // Build the resolved types map, converting InferType to Type.
        // Since we pre-created all array types above, infer_type_to_type only
        // performs lookups (no mutation).
        let mut resolved_types = HashMap::new();
        for (inst_ref, infer_ty) in &expr_types {
            let resolved = unifier.resolve_infer_type(infer_ty);
            let concrete_ty = self.infer_type_to_type(&resolved);
            resolved_types.insert(*inst_ref, concrete_ty);
        }

        Ok(resolved_types)
    }
    /// Analyze an RIR instruction for projection (field access).
    ///
    /// This is like `analyze_inst` but does NOT mark non-Copy values as moved.
    /// Used for field access where we're reading from a struct without consuming it.
    /// We still check that the variable hasn't already been moved (fully moved).
    /// Field-level move checking is done at the FieldGet level, not here.
    pub(crate) fn analyze_inst_for_projection(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        // For VarRef, we handle it specially: check for full moves but don't mark as moved
        if let InstData::VarRef { name } = &inst.data {
            // Check if it's a parameter — unless a `let` shadowed it with a
            // same-named local, which then wins for all later references
            // (spec 5.1:10, RUE-278); the local is resolved just below.
            if !ctx.locals.contains_key(name) {
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    let ty = param_info.ty;

                    // Check if this parameter has been fully moved
                    // (Partial moves are checked at the FieldGet level)
                    if let Some(move_state) = ctx.moved_vars.get(name) {
                        if let Some(moved_span) = move_state.full_move {
                            let name_str = self.interner.resolve(&*name);
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                inst.span,
                            )
                            .with_label("value moved here", moved_span)
                            .with_help(super::borrow_instead_of_move_help(name_str)));
                        }
                    }

                    // NOTE: We do NOT mark as moved here - this is a projection

                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: param_info.abi_slot,
                        },
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
            }

            // Look up the variable in locals
            let name_str = self.interner.resolve(&*name);
            let Some(local) = ctx.locals.get(name) else {
                // Not a param or local: fall back to the main VarRef path so
                // file-level constants (and comptime vars/type names) resolve
                // in projection positions too — e.g. `N == 1` routes its
                // operands through here (RUE-165). Constants inline a fresh
                // value, so there is no move state to preserve, and unknown
                // names still get E0201 from the fallback.
                let resolved_ty = ctx.resolved_types.get(&inst_ref).copied();
                return self.analyze_var_ref(air, *name, inst.span, resolved_ty, ctx);
            };

            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been fully moved
            // (Partial moves are checked at the FieldGet level)
            if let Some(move_state) = ctx.moved_vars.get(name) {
                if let Some(moved_span) = move_state.full_move {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(name_str)));
                }
            }

            // NOTE: We do NOT mark as moved here - this is a projection

            // Mark variable as used
            ctx.used_locals.insert(*name);

            // Load the variable
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // For nested field access (e.g., a.b.c), recursively use projection mode
        if let InstData::FieldGet { base, field } = &inst.data {
            let (base, field, field_span) = (*base, *field, inst.span);

            // `Enum.Variant` (RUE-488): a field access on a bare enum type name
            // is an enum-variant value, even in a projection/read position such
            // as a comparison operand (`Color.Red == Color.Red`). Mirror the
            // reroute in `analyze_field_get`, including the module-qualified
            // form `module.Enum.Variant`.
            if let InstData::VarRef { name } = self.rir.get(base).data
                && !self.is_runtime_value_binding(name, ctx)
                && let Some(result) =
                    self.try_analyze_dotted_enum_variant(air, name, field, field_span, ctx)?
            {
                return Ok(result);
            }
            if let InstData::FieldGet {
                base: module_ref,
                field: type_name,
            } = self.rir.get(base).data
                && let Some(result) = self.try_analyze_module_dotted_enum_variant(
                    air, module_ref, type_name, field, field_span, ctx,
                )?
            {
                return Ok(result);
            }

            let base_result = self.analyze_inst_for_projection(air, base, ctx)?;
            let base_type = base_result.ty;

            // Module member access in a projection position: equality
            // operands are read through a shared borrow (4.3:3f), so a
            // module-qualified constant (`tag == m.CONST`) reaches this arm
            // with a module-typed base. Resolve it as a member access —
            // mirroring `analyze_field_get`'s fallback — instead of
            // rejecting it as field access on a non-struct (RUE-632). A
            // constant inlines a fresh value, so no move state applies.
            if let Some(module_id) = base_type.as_module() {
                return self.analyze_module_type_member_access(air, module_id, field, field_span);
            }

            let struct_id = match base_type.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        field_span,
                    ));
                }
            };

            let struct_def = self.type_pool.struct_def(struct_id);
            let field_name_str = self.interner.resolve(&field).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    field_span,
                )?;

            let field_type = struct_field.ty;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: base_result.air_ref,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span: field_span,
            });
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // For index access in projection mode (e.g., `arr[i].field`), we allow the
        // indexing without checking if the element type is Copy. This enables
        // accessing Copy fields of non-Copy array elements.
        if let InstData::IndexGet { base, index } = &inst.data {
            // Recursively analyze the base in projection mode
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let array_type_id = match base_type.kind() {
                TypeKind::Array(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        inst.span,
                    ));
                }
            };

            let (element_type, length) = self.type_pool.array_def(array_type_id);

            // Index must be an integer type (signed or unsigned) per spec
            // 7.1:7. A negative or out-of-range runtime index is not a type
            // error; it traps at runtime via the bounds check (RUE-81).
            let index_result = self.analyze_inst(air, *index, ctx)?;
            if !index_result.ty.is_integer() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer type".to_string(),
                        found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    self.rir.get(*index).span,
                ));
            }

            let array_length = length;

            // Compile-time bounds check for constant indices, evaluated at the
            // index's resolved operand types so an overflowing index expression
            // is a compile-time error, not a folded runtime panic (RUE-234).
            if let Some(const_index) = self.try_get_const_index_checked(*index, ctx)? {
                if const_index < 0 || const_index >= array_length as i128 {
                    return Err(CompileError::new(
                        ErrorKind::IndexOutOfBounds {
                            index: const_index,
                            length: array_length,
                        },
                        self.rir.get(*index).span,
                    ));
                }
            }

            // NOTE: We do NOT check if element_type is Copy here.
            // In projection mode, we allow accessing elements for further projection
            // (e.g., arr[i].field where field is Copy).

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::IndexGet {
                    base: base_result.air_ref,
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                ty: element_type,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, element_type));
        }

        // For other expressions, use the normal analyze_inst
        // (they will trigger move semantics as expected)
        self.analyze_inst(air, inst_ref, ctx)
    }

    /// Look up the resolved type for an instruction from HM inference.
    ///
    /// Returns an `InternalError` if the type was not resolved. This should
    /// never happen in normal operation, but provides a better error message
    /// than a panic if there's a bug in type inference.
    pub(crate) fn get_resolved_type(
        ctx: &AnalysisContext,
        inst_ref: InstRef,
        span: Span,
        context: &str,
    ) -> CompileResult<Type> {
        ctx.resolved_types.get(&inst_ref).copied().ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "type inference did not resolve type for {} (instruction {:?})",
                    context, inst_ref
                )),
                span,
            )
        })
    }
}
