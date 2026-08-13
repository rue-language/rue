//! Type-inference glue: running inference, projection analysis, and resolved-type lookups.
//!
//! This category connects the inference engine to the canonical semantic
//! analysis and records resolved expression types.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;
use crate::inference::LazyInferenceFacts;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub(crate) struct InferenceBreakdown {
    pub(crate) precompute_ns: u64,
    pub(crate) constraint_generation_ns: u64,
    pub(crate) unification_resolution_ns: u64,
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    fn inference_function_is_selected(
        &self,
        function: Spur,
        span: Span,
        route: &crate::inference::InferenceCallRoute,
        checked: bool,
    ) -> Option<bool> {
        let info = self.function_info(function)?;
        if (info.is_unchecked || info.is_extern) && !checked {
            return Some(false);
        }
        match route {
            crate::inference::InferenceCallRoute::Unqualified { alias } => {
                if let Some(alias_name) = alias {
                    let alias = self.value_const(&(span.file_id, *alias_name))?;
                    if alias.value.as_function() != Some(function)
                        || self
                            .check_unqualified_visibility(
                                "constant",
                                self.body_interner().resolve(alias_name),
                                alias.span.file_id,
                                alias.is_pub,
                                span,
                            )
                            .is_err()
                    {
                        return Some(false);
                    }
                } else if self
                    .resolve_function_name_local(self.source_function_name(function), span.file_id)
                    .is_none_or(|selected| selected != function)
                {
                    return Some(false);
                }
                Some(
                    self.check_unqualified_visibility(
                        "function",
                        self.body_interner()
                            .resolve(&self.source_function_name(function)),
                        info.file_id,
                        info.is_pub,
                        span,
                    )
                    .is_ok(),
                )
            }
            crate::inference::InferenceCallRoute::Module {
                module,
                member,
                via_alias,
            } => {
                let module_file = self.module_def(*module).file_id;
                if *via_alias {
                    // The visible facade const is the module membership and
                    // visibility grant. Its selected function may be private
                    // or defined in another file, exactly as in the canonical
                    // module-call path; do not reapply direct-member policy to
                    // that hidden target.
                    let alias = self.value_const(&(module_file, *member))?;
                    return Some(
                        alias.value.as_function() == Some(function)
                            && self.is_accessible(span.file_id, module_file, alias.is_pub),
                    );
                }
                let selected = self
                    .resolve_function_name_local(*member, module_file)
                    .is_some_and(|selected| selected == function);
                Some(
                    selected
                        && info.file_id == module_file
                        && self.is_accessible(span.file_id, info.file_id, info.is_pub),
                )
            }
        }
    }

    pub(crate) fn record_inference_body_dependencies(
        &mut self,
        dependencies: &[crate::inference::InferenceBodyDependency],
        expr_types: &HashMap<InstRef, InferType>,
    ) -> bool {
        if !self.body_analysis_error_recovery() {
            return false;
        }

        let mut incomplete = false;
        for dependency in dependencies {
            match dependency {
                crate::inference::InferenceBodyDependency::Function {
                    function,
                    span,
                    route,
                    checked,
                } => match self.inference_function_is_selected(*function, *span, route, *checked) {
                    Some(true) => self.record_body_callable_dependency(*function),
                    Some(false) => {}
                    None => incomplete = true,
                },
                crate::inference::InferenceBodyDependency::Method {
                    structure,
                    method,
                    span,
                    associated,
                } => {
                    let Some(info) = self.method_info((*structure, *method)) else {
                        incomplete = true;
                        continue;
                    };
                    if info.has_self != *associated {
                        if *associated {
                            let def = self.body_type_pool().struct_def(*structure);
                            if self
                                .check_unqualified_visibility(
                                    "struct",
                                    &def.name,
                                    def.file_id,
                                    def.is_pub,
                                    *span,
                                )
                                .is_err()
                            {
                                continue;
                            }
                        }
                        self.record_body_method_dependency((*structure, *method));
                    }
                }
                crate::inference::InferenceBodyDependency::ModuleBinding(file, name) => {
                    self.record_body_named_dependency(
                        crate::NamedConstDependencyTargetEvent::ModuleBinding {
                            file: file.index(),
                            name: self.body_interner().resolve(name).to_owned(),
                        },
                    );
                }
                crate::inference::InferenceBodyDependency::ValueConst {
                    file,
                    name,
                    access_file,
                    qualified,
                } => {
                    let Some(info) = self.value_const(&(*file, *name)) else {
                        incomplete = true;
                        continue;
                    };
                    if *qualified && !self.is_accessible(*access_file, *file, info.is_pub) {
                        continue;
                    }
                    self.record_body_named_dependency(
                        crate::NamedConstDependencyTargetEvent::ValueConst {
                            file: file.index(),
                            name: self.body_interner().resolve(name).to_owned(),
                        },
                    );
                }
                crate::inference::InferenceBodyDependency::Specialization {
                    function,
                    type_arguments,
                    value_arguments,
                    span,
                    route,
                    checked,
                } => {
                    match self.inference_function_is_selected(*function, *span, route, *checked) {
                        Some(true) => {}
                        Some(false) => continue,
                        None => {
                            incomplete = true;
                            continue;
                        }
                    }
                    if self.body_dependency_observer().is_none() {
                        incomplete = true;
                        continue;
                    }
                    match self.canonical_specialization_instance(
                        *function,
                        type_arguments,
                        value_arguments,
                    ) {
                        Ok(identity) => self.record_specialization_dependency(identity),
                        Err(_) => incomplete = true,
                    }
                }
                crate::inference::InferenceBodyDependency::IncompleteCallable => {
                    // This path is reached only while publishing an inference
                    // error. An unresolved callable is therefore part of the
                    // rejected source construct, not evidence that a query
                    // dependency is absent. Treating it as retryable discards
                    // the user diagnostic and makes the uncanceled body query
                    // surface E9000 instead.
                }
            }
        }

        fn record_concrete<H: OrdinaryBodyAnalysisHost>(
            sema: &mut OrdinaryBodyEngine<'_, H>,
            ty: &InferType,
            access_file: FileId,
        ) -> bool {
            match ty {
                InferType::Concrete(ty) => {
                    let accessible = match ty.kind() {
                        TypeKind::Struct(id) => {
                            let def = sema.body_type_pool().struct_def(id);
                            sema.is_accessible(access_file, def.file_id, def.is_pub)
                        }
                        TypeKind::Enum(id) => {
                            let def = sema.body_type_pool().enum_def(id);
                            sema.is_accessible(access_file, def.file_id, def.is_pub)
                        }
                        _ => true,
                    };
                    if accessible {
                        sema.record_resolved_declaration_type(*ty);
                    }
                    !accessible
                }
                InferType::Array { element, .. } => record_concrete(sema, element, access_file),
                InferType::Var(_) | InferType::IntLiteral => false,
            }
        }
        for (inst_ref, ty) in expr_types {
            incomplete |=
                record_concrete(self, ty, self.body_rir_ref().get(*inst_ref).span.file_id);
        }
        incomplete
    }

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
    pub(crate) fn run_type_inference(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<(HashMap<InstRef, Type>, InferenceBreakdown)> {
        let precompute_started = Instant::now();
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
        let runtime_params: Vec<Spur> = params
            .iter()
            .filter_map(|(name, _, _, is_comptime)| (!is_comptime).then_some(*name))
            .collect();
        let comptime_local_bindings =
            self.precompute_comptime_type_locals(body, type_subst, value_subst, &runtime_params);

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
            .filter_map(
                |(inst_ref, ty)| match self.body_rir_ref().get(inst_ref).data {
                    rue_rir::InstData::Alloc {
                        name: Some(name), ..
                    } => Some((name, ty)),
                    _ => None,
                },
            )
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
        let precompute_ns = elapsed_ns(precompute_started);

        // Demand-population provider for the inference-context families
        // (RUE-1091 slice r5b). It materializes each consulted function/method
        // signature and struct/enum/const on first lookup from the frozen
        // declaration state, reading the live method tables — so a method call
        // on an anonymous-struct receiver whose signature was registered lazily
        // (during comptime evaluation, including the pre-pass above) resolves to
        // its declared type on first consult, subsuming the old per-body
        // `extra_method_sigs` reconciliation (RUE-164) with no behavior change.
        //
        // The provider holds an immutable borrow of `self`; it is dropped right
        // after Phase-1 constraint generation completes, before any `&mut self`
        // access resumes. This is the detached-context wiring: the shared
        // `InferenceContext` cache carries no `Sema` borrow (it outlives every
        // body's `&mut self`), while the fill source is threaded here per body,
        // sound because constraint generation reads `Sema` only immutably.
        // Phase 1 runs in its own scope so the provider's immutable borrow of
        // `self` ends before Phase 2 resumes `&mut self` access (RUE-1091 slice
        // r5b): the shared `InferenceContext` cache holds no `Sema` borrow, and
        // the fill source is threaded here per body.
        let constraint_generation_started = Instant::now();
        let (
            constraints,
            int_literal_vars,
            string_literal_vars,
            string_literal_default,
            expr_types,
            type_var_count,
            inference_body_dependencies,
        ) = {
            let facts = self.inference_facts(infer_ctx);

            // Create constraint generator driven by the demand-population provider.
            let mut cgen = ConstraintGenerator::with_lazy_facts(
                self.body_rir_ref(),
                self.body_interner(),
                self.body_type_pool(),
                type_subst,
                &facts,
            );
            cgen = cgen.with_strbuf_type(self.strbuf_type());
            let str_name = self
                .body_interner()
                .get("str")
                .expect("stable string default was registered before inference");
            let str_ty = facts
                .builtin_struct_type(str_name)
                .expect("canonical str type is present in the inference context");
            cgen = cgen.with_string_literal_default(str_ty);
            let mut cgen = cgen
                .with_comptime_local_bindings(&comptime_local_bindings)
                .with_inline_ctor_head_types(&inline_ctor_head_types)
                .with_comptime_values(value_subst);

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
                        // No comptime parameter has a string type, so a captured
                        // string value never occurs (RUE-957); skip rather than
                        // fabricate a type for it.
                        ConstValue::String(_) => continue,
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

            let inference_body_dependencies = cgen.body_dependencies().to_vec();

            // The function body's type must match the return type.
            // This handles implicit returns like `fn foo() -> i8 { 42 }`.
            // For arrays, we need to convert Type to InferType structurally.
            //
            // String literals use marked inference variables whose allowed nominal
            // targets include `str` and `Str(N)`, so this constraint retains the
            // implicit literal coercion while rejecting every other mismatched
            // tail before AIR/CFG construction (RUE-1652).
            cgen.add_constraint(Constraint::equal(
                body_info.ty,
                self.type_to_infer_type(return_type),
                body_info.span,
            ));

            // Consume the constraint generator to release borrows
            let (
                constraints,
                int_literal_vars,
                string_literal_vars,
                string_literal_default,
                expr_types,
                type_var_count,
            ) = cgen.into_parts();
            (
                constraints,
                int_literal_vars,
                string_literal_vars,
                string_literal_default,
                expr_types,
                type_var_count,
                inference_body_dependencies,
            )
        };
        let constraint_generation_ns = elapsed_ns(constraint_generation_started);

        // Phase 2: Solve constraints via unification
        let unification_resolution_started = Instant::now();
        // Pre-size the substitution for better performance on large functions
        let mut unifier = Unifier::with_capacity(type_var_count);
        unifier.mark_int_literal_vars(&int_literal_vars);
        // Literal contextualization is nominal. Admit only compiler-owned
        // identities: core `str`, the trusted std StrBuf language item when
        // imported, and synthetic fixed strings.
        let mut string_literal_types = vec![string_literal_default];
        string_literal_types.extend(self.strbuf_type());
        string_literal_types.extend(
            self.generated_structs()
                .values()
                .copied()
                .map(Type::new_struct)
                .filter(|&ty| self.is_str_fixed_struct(ty)),
        );
        string_literal_types.sort_unstable_by_key(Type::as_u32);
        string_literal_types.dedup();
        unifier.mark_string_literal_vars(&string_literal_vars, &string_literal_types);
        let equivalence_queries = std::cell::Cell::new(0usize);
        let errors = unifier.solve_constraints_with(&constraints, &|left, right| {
            if left == right {
                return true;
            }
            equivalence_queries.set(equivalence_queries.get() + 1);
            self.types_equivalent(left, right)
        });
        self.body_analysis_work_mut()
            .semantic_type_equivalence_queries += equivalence_queries.get();

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            {
                let inference_dependency_incomplete = self
                    .record_inference_body_dependencies(&inference_body_dependencies, &expr_types);
                self.set_body_analysis_inference_failure_incomplete(
                    inference_dependency_incomplete,
                );
            }
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.name_with_pool(self.body_type_pool()),
                    found: found.name_with_pool(self.body_type_pool()),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.safe_name_with_pool(Some(self.body_type_pool())),
                },
                UnifyResult::StringLiteralNonString { found } => ErrorKind::TypeMismatch {
                    expected: "string type".to_string(),
                    found: found.name_with_pool(self.body_type_pool()),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!(
                        "{var} = {} (infinite type)",
                        ty.name_with_pool(self.body_type_pool())
                    ),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegate(ty.safe_name_with_pool(Some(self.body_type_pool())))
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.safe_name_with_pool(Some(self.body_type_pool())),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.safe_name_with_pool(Some(self.body_type_pool())),
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

        {
            self.set_body_analysis_inference_failure_incomplete(false);
        }

        // Default any unconstrained integer literals to i32
        unifier.default_int_literal_vars(&int_literal_vars);
        unifier.default_unconstrained_vars(&string_literal_vars, string_literal_default);

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

        let unification_resolution_ns = elapsed_ns(unification_resolution_started);
        Ok((
            resolved_types,
            InferenceBreakdown {
                precompute_ns,
                constraint_generation_ns,
                unification_resolution_ns,
            },
        ))
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

        // For VarRef, we handle it specially: check for full moves but don't mark as moved
        if let InstData::VarRef { name, anchor } = &inst.data {
            // Check if it's a parameter — unless a `let` shadowed it with a
            // same-named local, which then wins for all later references
            // (spec 5.1:10, RUE-278); the local is resolved just below.
            if !ctx.locals.contains_key(name) {
                if let Some(param_info) = ctx.param(*name) {
                    let ty = param_info.ty;

                    // Check if this parameter has been fully moved
                    // (Partial moves are checked at the FieldGet level)
                    if let Some(move_state) = self.moved_state(ctx, name) {
                        if let Some(moved_span) = move_state.full_move {
                            let name_str = self.body_interner().resolve(&*name);
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
            let name_str = self.body_interner().resolve(&*name);
            let Some(local) = ctx.locals.get(name) else {
                // Not a param or local: fall back to the main VarRef path so
                // file-level constants (and comptime vars/type names) resolve
                // in projection positions too — e.g. `N == 1` routes its
                // operands through here (RUE-165). Constants inline a fresh
                // value, so there is no move state to preserve, and unknown
                // names still get E0201 from the fallback.
                let resolved_ty = ctx.resolved_type_of(inst_ref);
                return self.analyze_var_ref(
                    air,
                    *name,
                    anchor.clone(),
                    inst.span,
                    resolved_ty,
                    ctx,
                );
            };

            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been fully moved
            // (Partial moves are checked at the FieldGet level)
            if let Some(move_state) = self.moved_state(ctx, name) {
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
            if let InstData::VarRef { name, .. } = self.body_rir_ref().get(base).data
                && !self.is_runtime_value_binding(name, ctx)
                && let Some(result) =
                    self.try_analyze_dotted_enum_variant(air, name, field, field_span, ctx)?
            {
                return Ok(result);
            }
            if let InstData::FieldGet {
                base: module_ref,
                field: type_name,
            } = self.body_rir_ref().get(base).data
                && let Some(result) = self.try_analyze_module_dotted_enum_variant(
                    air, module_ref, type_name, field, field_span, ctx,
                )?
            {
                return Ok(result);
            }

            // Projection-mode reads borrow their source rather than moving it.
            // Keep addressable local/parameter chains as one canonical place;
            // only computed rvalues need the temporary spill below.
            if let Some(result) = self.try_read_traced_place(air, inst_ref, field_span, ctx)? {
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
                return self.analyze_module_type_member_access(
                    air,
                    module_id,
                    field,
                    super::const_use_anchor_of(self.body_rir_ref(), base),
                    field_span,
                    ctx,
                );
            }

            let struct_id = match base_type.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        field_span,
                    ));
                }
            };

            let struct_def = self.body_type_pool().struct_def(struct_id);
            let field_name = self.body_interner().resolve(&field);
            let Some((field_index, struct_field)) = struct_def.find_field(field_name) else {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.to_string(),
                        field_name: field_name.to_string(),
                    },
                    field_span,
                ));
            };

            let field_type = struct_field.ty;

            let air_ref = self.emit_projected_rvalue_read(
                air,
                base_result.air_ref,
                base_type,
                AirProjection::Field {
                    struct_id,
                    field_index: field_index as u32,
                },
                field_type,
                field_span,
                ctx,
            )?;
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // For index access in projection mode (e.g., `arr[i].field`), we allow the
        // indexing without checking if the element type is Copy. This enables
        // accessing Copy fields of non-Copy array elements.
        if let InstData::IndexGet { base, index } = &inst.data {
            // Snapshot the base root's move state before analysis, in case this
            // is a String/str/slice byte index in projection mode (RUE-700).
            let base_root = self.extract_root_variable(*base);
            let base_move_state_before = self.snapshot_move_state_value(base_root, ctx);

            // Recursively analyze the base in projection mode
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            // Binary-op operands are analyzed in projection mode (see
            // `analyze_builtin_binary_op`), so a String/str/slice byte index used
            // inside a condition — `if s[0] == 45`, `while s[i] != 0` — reaches
            // here with a non-array base. Mirror `analyze_index_get`'s non-array
            // delegation (`s[i] -> byte_at`) instead of rejecting it with E0900
            // (RUE-700); a let-bound `let c = s[0]` already goes through the
            // String-aware path.
            if self.is_strbuf(base_type) {
                return self.analyze_string_index_get(
                    air,
                    base_result,
                    base_root,
                    base_move_state_before,
                    *index,
                    inst.span,
                    ctx,
                );
            }
            if self.is_str_like(base_type) {
                return self.analyze_str_index_get(
                    air,
                    base_result,
                    base_root,
                    base_move_state_before,
                    *index,
                    inst.span,
                    ctx,
                );
            }
            if let Some(elem_ty) = self.slice_element_type(base_type) {
                return self.analyze_slice_index_get(
                    air,
                    base_result,
                    base_root,
                    base_move_state_before,
                    *index,
                    elem_ty,
                    inst.span,
                    ctx,
                );
            }

            let array_type_id = match base_type.kind() {
                TypeKind::Array(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        inst.span,
                    ));
                }
            };

            let (element_type, length) = self.body_type_pool().array_def(array_type_id);

            // Index must be an integer type (signed or unsigned) per spec
            // 7.1:7. A negative or out-of-range runtime index is not a type
            // error; it traps at runtime via the bounds check (RUE-81).
            let index_result = self.analyze_inst(air, *index, ctx)?;
            if !index_result.ty.is_integer() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer type".to_string(),
                        found: index_result
                            .ty
                            .safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    self.body_rir_ref().get(*index).span,
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
                        self.body_rir_ref().get(*index).span,
                    ));
                }
            }

            // NOTE: We do NOT check if element_type is Copy here.
            // In projection mode, we allow accessing elements for further projection
            // (e.g., arr[i].field where field is Copy).

            let air_ref = self.emit_projected_rvalue_read(
                air,
                base_result.air_ref,
                base_type,
                AirProjection::Index {
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                element_type,
                inst.span,
                ctx,
            )?;
            return Ok(AnalysisResult::new(air_ref, element_type));
        }

        // A `-> borrow T` accessor call is a place in projection position
        // (ADR-0062): read it through the traced place, so comparing or
        // projecting a drop-glue element borrows rather than copies it.
        if matches!(&inst.data, InstData::MethodCall { .. })
            && self.place_root_with_accessors(inst_ref, ctx).is_some()
            && let Some(result) = self.try_read_traced_place(air, inst_ref, inst.span, ctx)?
        {
            return Ok(result);
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
        ctx.resolved_type_of(inst_ref).ok_or_else(|| {
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
