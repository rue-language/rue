//! Function body analysis and AIR generation.
//!
//! This module contains the core semantic analysis functionality:
//! - Function analysis (analyze_single_function, analyze_method_function, analyze_destructor_function)
//! - Hindley-Milner type inference (run_type_inference)
//! - RIR to AIR instruction lowering (analyze_inst)
//! - Helper functions for expression analysis

use std::collections::{HashMap, HashSet};

use lasso::Spur;
use rue_builtins::BuiltinTypeDef;
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind,
    IntrinsicTypeMismatchError, MultiErrorResult, OptionExt, PreviewFeature, WarningKind,
};
use rue_rir::{InstData, InstRef, RirArgMode, RirCallArg, RirDirective, RirParamMode};
use rue_span::Span;

use super::context::{
    AnalysisContext, AnalysisResult, ConstValue, FieldPath, ParamInfo, StringReceiverStorage,
};
use super::{AnalyzedFunction, InferenceContext, Sema, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, InferType, ParamVarInfo, Unifier,
    UnifyResult,
};
use crate::inst::{Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirRef};
use crate::types::{StructId, Type};

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Collect analyzed functions with their local strings.
    // Each function collects strings independently (enabling future parallelization),
    // then we merge and deduplicate globally, remapping string IDs in the AIR.
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

    // Collect method refs from impl blocks
    let mut method_refs: HashSet<InstRef> = HashSet::new();
    for (_, inst) in sema.rir.iter() {
        if let InstData::ImplDecl {
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                method_refs.insert(method_ref);
            }
        }
    }

    // Analyze regular functions
    for (inst_ref, inst) in sema.rir.iter() {
        if let InstData::FnDecl {
            name,
            params_start,
            params_len,
            return_type,
            body,
            has_self: _,
            ..
        } = &inst.data
        {
            if method_refs.contains(&inst_ref) {
                continue;
            }

            let fn_name = sema.interner.resolve(&*name).to_string();
            let params = sema.rir.get_params(*params_start, *params_len);

            match sema.analyze_single_function(
                &infer_ctx,
                &fn_name,
                *return_type,
                &params,
                *body,
                inst.span,
            ) {
                Ok((analyzed, warnings, local_strings)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Analyze method bodies from impl blocks
    for (_, inst) in sema.rir.iter() {
        if let InstData::ImplDecl {
            type_name,
            methods_start,
            methods_len,
        } = &inst.data
        {
            let type_name_str = sema.interner.resolve(&*type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "impl block for undefined type '{}' survived validation",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::Struct(struct_id);

            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                let method_inst = sema.rir.get(method_ref);
                if let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    ..
                } = &method_inst.data
                {
                    let method_name_str = sema.interner.resolve(&*method_name).to_string();
                    let params = sema.rir.get_params(*params_start, *params_len);

                    let full_name = if *has_self {
                        format!("{}.{}", type_name_str, method_name_str)
                    } else {
                        format!("{}::{}", type_name_str, method_name_str)
                    };

                    match sema.analyze_method_function(
                        &infer_ctx,
                        &full_name,
                        *return_type,
                        &params,
                        *body,
                        method_inst.span,
                        struct_type,
                        *has_self,
                    ) {
                        Ok((analyzed, warnings, local_strings)) => {
                            functions_with_strings.push((analyzed, local_strings));
                            all_warnings.extend(warnings);
                        }
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
    }

    // Analyze destructor bodies
    for (_, inst) in sema.rir.iter() {
        if let InstData::DropFnDecl { type_name, body } = &inst.data {
            let type_name_str = sema.interner.resolve(&*type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "destructor for undefined type '{}' survived validation",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::Struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Merge strings from all functions into a global table with deduplication.
    // Build a mapping from (local_strings_vec, local_id) -> global_id for remapping.
    let mut global_string_table: HashMap<String, u32> = HashMap::new();
    let mut global_strings: Vec<String> = Vec::new();

    // For each function, build a mapping from local string ID to global string ID
    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings) in functions_with_strings {
        if !local_strings.is_empty() {
            // Build mapping from local IDs to global IDs
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|s| {
                    *global_string_table.entry(s.clone()).or_insert_with(|| {
                        let id = global_strings.len() as u32;
                        global_strings.push(s);
                        id
                    })
                })
                .collect();

            // Remap string IDs in the AIR
            analyzed
                .air
                .remap_string_ids(|local_id| local_to_global[local_id as usize]);
        }
        functions.push(analyzed);
    }

    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    errors.into_result_with(SemaOutput {
        functions,
        struct_defs: sema.struct_defs,
        enum_defs: sema.enum_defs,
        array_types: sema.array_type_defs,
        strings: global_strings,
        warnings: all_warnings,
    })
}

impl<'a> Sema<'a> {
    /// Check that a preview feature is enabled.
    ///
    /// This is used to gate experimental features behind the `--preview` flag.
    /// Returns an error with a helpful message if the feature is not enabled.
    ///
    /// # Arguments
    /// - `feature`: The preview feature to check
    /// - `what`: Human-readable description of what requires this feature
    /// - `span`: The source location where the feature is used
    ///
    /// # Returns
    /// - `Ok(())` if the feature is enabled
    /// - `Err(CompileError)` with a helpful message if not enabled
    pub(crate) fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    fn analyze_single_function(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
    ) -> CompileResult<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>)> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes
        let param_info: Vec<(Spur, Type, RirParamMode)> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_type(p.ty, span)?;
                Ok((p.name, ty, p.mode))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        let (air, num_locals, num_param_slots, param_modes, warnings, local_strings) =
            self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: fn_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
            },
            warnings,
            local_strings,
        ))
    }

    /// Analyze a method function from an impl block.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    fn analyze_method_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
    ) -> CompileResult<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>)> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Build parameter list, adding self as first parameter for methods
        let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

        if has_self {
            // Add self parameter (Normal mode - passed by value)
            let self_sym = self.interner.get_or_intern("self");
            param_info.push((self_sym, struct_type, RirParamMode::Normal));
        }

        // Add regular parameters with their modes
        for p in params.iter() {
            let ty = self.resolve_type(p.ty, span)?;
            param_info.push((p.name, ty, p.mode));
        }

        let (air, num_locals, num_param_slots, param_modes, warnings, local_strings) =
            self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
            },
            warnings,
            local_strings,
        ))
    }

    /// Analyze a destructor function.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    fn analyze_destructor_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        body: InstRef,
        _span: Span,
        struct_type: Type,
    ) -> CompileResult<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>)> {
        // Destructors take self parameter and return unit
        let self_sym = self.interner.get_or_intern("self");
        let param_info: Vec<(Spur, Type, RirParamMode)> =
            vec![(self_sym, struct_type, RirParamMode::Normal)];

        let (air, num_locals, num_param_slots, param_modes, warnings, local_strings) =
            self.analyze_function(infer_ctx, Type::Unit, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
            },
            warnings,
            local_strings,
        ))
    }
    /// Analyze a single function, producing AIR.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation,
    /// avoiding the cost of rebuilding maps for each function.
    ///
    /// Returns (air, num_locals, num_param_slots, param_modes, warnings).
    /// Warnings are collected per-function to enable future parallel analysis.
    fn analyze_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)], // (name, type, mode)
        body: InstRef,
    ) -> CompileResult<(Air, u32, u32, Vec<bool>, Vec<CompileWarning>, Vec<String>)> {
        let mut air = Air::new(return_type);
        let mut param_map: HashMap<Spur, ParamInfo> = HashMap::new();
        let mut param_modes: Vec<bool> = Vec::new();

        // Add parameters to the param map, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype, mode) in params.iter() {
            param_map.insert(
                *pname,
                ParamInfo {
                    abi_slot: next_abi_slot,
                    ty: *ptype,
                    mode: *mode,
                },
            );
            // Both inout and borrow are passed by reference (as a pointer = 1 slot)
            let is_by_ref = *mode != RirParamMode::Normal;
            let slot_count = if is_by_ref {
                // By-ref parameters are always 1 slot (pointer)
                1
            } else {
                self.abi_slot_count(*ptype)
            };
            for _ in 0..slot_count {
                param_modes.push(is_by_ref);
            }
            next_abi_slot += slot_count;
        }
        let num_param_slots = next_abi_slot;

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let resolved_types = self.run_type_inference(infer_ctx, return_type, params, body)?;

        // Create analysis context with resolved types
        let mut ctx = AnalysisContext {
            locals: HashMap::new(),
            params: &param_map,
            next_slot: 0,
            loop_depth: 0,
            used_locals: HashSet::new(),
            return_type,
            scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: HashMap::new(),
            warnings: Vec::new(),
            local_string_table: HashMap::new(),
            local_strings: Vec::new(),
        };

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::Never {
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
        ))
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
    fn run_type_inference(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
    ) -> CompileResult<HashMap<InstRef, Type>> {
        // Create constraint generator using pre-computed inference context
        let mut cgen = ConstraintGenerator::new(
            self.rir,
            self.interner,
            &infer_ctx.func_sigs,
            &infer_ctx.struct_types,
            &infer_ctx.enum_types,
            &infer_ctx.method_sigs,
        );

        // Build parameter map for constraint context.
        // Convert Type to InferType so arrays are represented structurally.
        let param_vars: HashMap<Spur, ParamVarInfo> = params
            .iter()
            .map(|(name, ty, _mode)| {
                (
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(*ty),
                    },
                )
            })
            .collect();

        // Create constraint context
        let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

        // Phase 1: Generate constraints
        let body_info = cgen.generate(body, &mut cgen_ctx);

        // The function body's type must match the return type.
        // This handles implicit returns like `fn foo() -> i8 { 42 }`.
        cgen.add_constraint(Constraint::equal(
            body_info.ty,
            InferType::Concrete(return_type),
            body_info.span,
        ));

        // Consume the constraint generator to release borrows
        let (constraints, int_literal_vars, expr_types, type_var_count) = cgen.into_parts();

        // Phase 2: Solve constraints via unification
        // Pre-size the substitution for better performance on large functions
        let mut unifier = Unifier::with_capacity(type_var_count);
        let errors = unifier.solve_constraints(&constraints);

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.to_string(),
                    found: found.to_string(),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.name().to_string(),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!("{var} = {ty} (infinite type)"),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegateUnsigned(ty.name().to_string())
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.name().to_string(),
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
            // First check if it's a parameter
            if let Some(param_info) = ctx.params.get(name) {
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
                        .with_label("value moved here", moved_span));
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

            // Look up the variable in locals
            let name_str = self.interner.resolve(&*name);
            let local = ctx.locals.get(name).ok_or_compile_error(
                ErrorKind::UndefinedVariable(name_str.to_string()),
                inst.span,
            )?;

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
                    .with_label("value moved here", moved_span));
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
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let struct_id = match base_type {
                Type::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let struct_def = &self.struct_defs[struct_id.0 as usize];
            let field_name_str = self.interner.resolve(&*field).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    inst.span,
                )?;

            let field_type = struct_field.ty;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: base_result.air_ref,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span: inst.span,
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

            let array_type_id = match base_type {
                Type::Array(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            // Index must be an unsigned integer
            let index_result = self.analyze_inst(air, *index, ctx)?;
            if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "unsigned integer type".to_string(),
                        found: index_result.ty.name().to_string(),
                    },
                    self.rir.get(*index).span,
                ));
            }

            let array_def = &self.array_type_defs[array_type_id.0 as usize];
            let element_type = array_def.element_type;
            let array_length = array_def.length;

            // Compile-time bounds check for constant indices
            if let Some(const_index) = self.try_get_const_index(*index) {
                if const_index < 0 || const_index as u64 >= array_length {
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
                    array_type_id,
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

    /// Analyze an RIR instruction, producing AIR instructions.
    ///
    /// Types are determined by Hindley-Milner inference (stored in `resolved_types`).
    /// Returns both the AIR reference and the synthesized type.
    /// Analyze a single RIR instruction and produce the corresponding AIR instruction.
    ///
    /// This method dispatches to category-specific methods in `analyze_ops.rs` for
    /// maintainability. Each category handles related instruction types together.
    ///
    /// # Categories
    ///
    /// - **Literals**: IntConst, BoolConst, StringConst, UnitConst
    /// - **Binary arithmetic**: Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
    /// - **Comparison**: Eq, Ne, Lt, Gt, Le, Ge
    /// - **Logical**: And, Or
    /// - **Unary**: Neg, Not, BitNot
    /// - **Control flow**: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    /// - **Variables**: Alloc, VarRef, ParamRef, Assign
    /// - **Structs**: StructDecl, StructInit, FieldGet, FieldSet
    /// - **Arrays**: ArrayInit, IndexGet, IndexSet
    /// - **Enums**: EnumDecl, EnumVariant
    /// - **Calls**: Call, MethodCall, AssocFnCall
    /// - **Intrinsics**: Intrinsic, TypeIntrinsic
    /// - **Declarations**: ImplDecl, DropFnDecl, FnDecl
    pub(crate) fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            // Literals
            InstData::IntConst(_)
            | InstData::BoolConst(_)
            | InstData::StringConst(_)
            | InstData::UnitConst => self.analyze_literal(air, inst_ref, ctx),

            // Binary arithmetic operations
            InstData::Add { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Add, inst.span, ctx)
            }
            InstData::Sub { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Sub, inst.span, ctx)
            }
            InstData::Mul { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mul, inst.span, ctx)
            }
            InstData::Div { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Div, inst.span, ctx)
            }
            InstData::Mod { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mod, inst.span, ctx)
            }

            // Bitwise binary operations
            InstData::BitAnd { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitAnd, inst.span, ctx)
            }
            InstData::BitOr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitOr, inst.span, ctx)
            }
            InstData::BitXor { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitXor, inst.span, ctx)
            }
            InstData::Shl { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shl, inst.span, ctx)
            }
            InstData::Shr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shr, inst.span, ctx)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Eq, inst.span, ctx)
            }
            InstData::Ne { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Ne, inst.span, ctx)
            }
            InstData::Lt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Lt, inst.span, ctx)
            }
            InstData::Gt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Gt, inst.span, ctx)
            }
            InstData::Le { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Le, inst.span, ctx)
            }
            InstData::Ge { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Ge, inst.span, ctx)
            }

            // Logical operations
            InstData::And { .. } | InstData::Or { .. } => {
                self.analyze_logical_op(air, inst_ref, ctx)
            }

            // Unary operations
            InstData::Neg { .. } | InstData::Not { .. } | InstData::BitNot { .. } => {
                self.analyze_unary_op(air, inst_ref, ctx)
            }

            // Control flow
            InstData::Branch { .. }
            | InstData::Loop { .. }
            | InstData::InfiniteLoop { .. }
            | InstData::Match { .. }
            | InstData::Break
            | InstData::Continue
            | InstData::Ret(_)
            | InstData::Block { .. } => self.analyze_control_flow(air, inst_ref, ctx),

            // Variable operations
            InstData::Alloc { .. }
            | InstData::VarRef { .. }
            | InstData::ParamRef { .. }
            | InstData::Assign { .. } => self.analyze_variable_ops(air, inst_ref, ctx),

            // Struct operations
            InstData::StructDecl { .. }
            | InstData::StructInit { .. }
            | InstData::FieldGet { .. }
            | InstData::FieldSet { .. } => self.analyze_struct_ops(air, inst_ref, ctx),

            // Array operations
            InstData::ArrayInit { .. } | InstData::IndexGet { .. } | InstData::IndexSet { .. } => {
                self.analyze_array_ops(air, inst_ref, ctx)
            }

            // Enum operations
            InstData::EnumDecl { .. } | InstData::EnumVariant { .. } => {
                self.analyze_enum_ops(air, inst_ref, ctx)
            }

            // Call operations
            InstData::Call { .. } | InstData::MethodCall { .. } | InstData::AssocFnCall { .. } => {
                self.analyze_call_ops(air, inst_ref, ctx)
            }

            // Intrinsic operations
            InstData::Intrinsic { .. } | InstData::TypeIntrinsic { .. } => {
                self.analyze_intrinsic_ops(air, inst_ref, ctx)
            }

            // Declaration no-ops (produce Unit in expression context)
            InstData::ImplDecl { .. } | InstData::DropFnDecl { .. } | InstData::FnDecl { .. } => {
                self.analyze_decl_noop(air, inst_ref, ctx)
            }
        }
    }

    // ========================================================================
    // Implementation methods for complex operations
    // These are called by the category methods in analyze_ops.rs
    // ========================================================================

    /// Implementation for FieldSet - handles both local and parameter field assignment.
    pub(crate) fn analyze_field_set_impl(
        &mut self,
        air: &mut Air,
        base: InstRef,
        field: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // For field assignment, we need to walk up the chain of field accesses
        // to find the root variable. We accumulate the slot offset as we go.

        // Walk up to find the root variable, collecting field symbols
        let mut current_base = base;
        let mut field_symbols: Vec<Spur> = Vec::new();

        // Result is either (Local, slot, type, is_mut, name) or (Param, abi_slot, type, mode, name)
        enum RootKind {
            Local { slot: u32, is_mut: bool },
            Param { abi_slot: u32, mode: RirParamMode },
        }

        let (var_name, root_kind, root_type, root_symbol) = loop {
            let current_inst = self.rir.get(current_base);
            match &current_inst.data {
                InstData::VarRef { name } => {
                    let name_str = self.interner.resolve(&*name);

                    // Check if this variable has been moved
                    if let Some(move_state) = ctx.moved_vars.get(name) {
                        if let Some(moved_span) = move_state.is_any_part_moved() {
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                span,
                            )
                            .with_label("value moved here", moved_span));
                        }
                    }

                    // First check if it's a parameter
                    if let Some(param_info) = ctx.params.get(name) {
                        break (
                            name_str.to_string(),
                            RootKind::Param {
                                abi_slot: param_info.abi_slot,
                                mode: param_info.mode,
                            },
                            param_info.ty,
                            *name,
                        );
                    }

                    // Then check locals
                    let local = ctx.locals.get(name).ok_or_compile_error(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        span,
                    )?;

                    break (
                        name_str.to_string(),
                        RootKind::Local {
                            slot: local.slot,
                            is_mut: local.is_mut,
                        },
                        local.ty,
                        *name,
                    );
                }
                InstData::ParamRef { name, .. } => {
                    let name_str = self.interner.resolve(&*name);
                    let param_info = ctx.params.get(name).ok_or_compile_error(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        span,
                    )?;

                    // Check if this parameter has been moved
                    if let Some(move_state) = ctx.moved_vars.get(name) {
                        if let Some(moved_span) = move_state.is_any_part_moved() {
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                span,
                            )
                            .with_label("value moved here", moved_span));
                        }
                    }

                    break (
                        name_str.to_string(),
                        RootKind::Param {
                            abi_slot: param_info.abi_slot,
                            mode: param_info.mode,
                        },
                        param_info.ty,
                        *name,
                    );
                }
                InstData::FieldGet {
                    base: inner_base,
                    field: inner_field,
                } => {
                    field_symbols.push(*inner_field);
                    current_base = *inner_base;
                }
                _ => {
                    return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
                }
            }
        };

        // Check mutability based on root kind
        let root_slot = match root_kind {
            RootKind::Local { slot, is_mut } => {
                if !is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(var_name),
                        span,
                    ));
                }
                slot
            }
            RootKind::Param { abi_slot, mode } => {
                match mode {
                    RirParamMode::Normal => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(var_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            var_name,
                            var_name,
                            root_type.name()
                        )));
                    }
                    RirParamMode::Inout => {
                        // Inout parameters can be mutated
                    }
                    RirParamMode::Borrow => {
                        return Err(CompileError::new(
                            ErrorKind::MutateBorrowedValue { variable: var_name },
                            span,
                        ));
                    }
                }
                abi_slot
            }
        };

        // Suppress unused variable warning
        let _ = root_symbol;

        // Walk through the field chain to compute the slot offset
        let mut current_type = root_type;
        let mut slot_offset: u32 = 0;

        for field_sym in field_symbols.iter().rev() {
            let struct_id = match current_type {
                Type::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: current_type.name().to_string(),
                        },
                        span,
                    ));
                }
            };

            let struct_def = &self.struct_defs[struct_id.0 as usize];
            let field_name_str = self.interner.resolve(&*field_sym).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    span,
                )?;

            slot_offset += self.field_slot_offset(struct_id, field_index);
            current_type = struct_field.ty;
        }

        // Now handle the final field being assigned
        let struct_id = match current_type {
            Type::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: current_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        let struct_def = &self.struct_defs[struct_id.0 as usize];
        let field_name_str = self.interner.resolve(&field).to_string();

        let (field_index, _struct_field) =
            struct_def.find_field(&field_name_str).ok_or_compile_error(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.clone(),
                    field_name: field_name_str.clone(),
                },
                span,
            )?;

        // Analyze the value
        let value_result = self.analyze_inst(air, value, ctx)?;

        // Emit the appropriate instruction based on whether root is a local or param
        let air_ref = match root_kind {
            RootKind::Local { slot, .. } => {
                let base_slot = slot + slot_offset;
                air.add_inst(AirInst {
                    data: AirInstData::FieldSet {
                        slot: base_slot,
                        struct_id,
                        field_index: field_index as u32,
                        value: value_result.air_ref,
                    },
                    ty: Type::Unit,
                    span,
                })
            }
            RootKind::Param { abi_slot, .. } => air.add_inst(AirInst {
                data: AirInstData::ParamFieldSet {
                    param_slot: abi_slot,
                    inner_offset: slot_offset,
                    struct_id,
                    field_index: field_index as u32,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            }),
        };
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Implementation for IndexSet - handles both local and parameter array index assignment.
    pub(crate) fn analyze_index_set_impl(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let base_inst = self.rir.get(base);

        enum IndexSetRootKind {
            Local { slot: u32, is_mut: bool },
            Param { abi_slot: u32, mode: RirParamMode },
        }

        let (var_name, root_kind, base_type) = match &base_inst.data {
            InstData::VarRef { name } => {
                let name_str = self.interner.resolve(&*name);

                // Check if this variable has been moved
                if let Some(move_state) = ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                // First check if it's a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    (
                        name_str.to_string(),
                        IndexSetRootKind::Param {
                            abi_slot: param_info.abi_slot,
                            mode: param_info.mode,
                        },
                        param_info.ty,
                    )
                } else {
                    // Then check locals
                    let local = ctx.locals.get(name).ok_or_compile_error(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        span,
                    )?;

                    (
                        name_str.to_string(),
                        IndexSetRootKind::Local {
                            slot: local.slot,
                            is_mut: local.is_mut,
                        },
                        local.ty,
                    )
                }
            }
            InstData::ParamRef { name, .. } => {
                let name_str = self.interner.resolve(&*name);
                let param_info = ctx.params.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                )?;

                // Check if this parameter has been moved
                if let Some(move_state) = ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                (
                    name_str.to_string(),
                    IndexSetRootKind::Param {
                        abi_slot: param_info.abi_slot,
                        mode: param_info.mode,
                    },
                    param_info.ty,
                )
            }
            _ => {
                return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
            }
        };

        // Check mutability based on root kind
        let (is_inout_param, slot) = match root_kind {
            IndexSetRootKind::Local { slot, is_mut } => {
                if !is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(var_name),
                        span,
                    ));
                }
                (false, slot)
            }
            IndexSetRootKind::Param { abi_slot, mode } => {
                let is_inout = match mode {
                    RirParamMode::Normal => false,
                    RirParamMode::Inout => true,
                    RirParamMode::Borrow => {
                        return Err(CompileError::new(
                            ErrorKind::MutateBorrowedValue { variable: var_name },
                            span,
                        ));
                    }
                };
                (is_inout, abi_slot)
            }
        };

        let array_type_id = match base_type {
            Type::Array(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: base_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        // Index must be an unsigned integer
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: index_result.ty.name().to_string(),
                },
                self.rir.get(index).span,
            ));
        }

        let array_def = &self.array_type_defs[array_type_id.0 as usize];
        let element_type = array_def.element_type;
        let array_length = array_def.length;

        // Compile-time bounds check for constant indices
        if let Some(const_index) = self.try_get_const_index(index) {
            if const_index < 0 || const_index as u64 >= array_length {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_index,
                        length: array_length,
                    },
                    self.rir.get(index).span,
                ));
            }
        }

        // Analyze the value
        let value_result = self.analyze_inst(air, value, ctx)?;

        // Emit appropriate instruction
        let air_ref = if is_inout_param {
            air.add_inst(AirInst {
                data: AirInstData::ParamIndexSet {
                    param_slot: slot,
                    array_type_id,
                    index: index_result.air_ref,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            })
        } else {
            air.add_inst(AirInst {
                data: AirInstData::IndexSet {
                    slot,
                    array_type_id,
                    index: index_result.air_ref,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            })
        };
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Implementation for MethodCall.
    pub(crate) fn analyze_method_call_impl(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        method: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let args = self.rir.get_call_args(args_start, args_len);
        let receiver_var = self.extract_root_variable(receiver);
        let method_name_str = self.interner.resolve(&method).to_string();

        // Check if this is a builtin mutation method
        let is_builtin_mutation_method = self.is_builtin_mutation_method(&method_name_str);

        // Get storage location for mutation methods before analyzing receiver
        let receiver_storage = if is_builtin_mutation_method {
            self.get_string_receiver_storage(receiver, ctx, span)?
        } else {
            None
        };

        // Analyze the receiver expression
        let receiver_result = self.analyze_inst(air, receiver, ctx)?;
        let receiver_type = receiver_result.ty;

        // Check that receiver is a struct type
        let struct_id = match receiver_type {
            Type::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::MethodCallOnNonStruct {
                        found: receiver_type.name().to_string(),
                        method_name: method_name_str,
                    },
                    span,
                ));
            }
        };

        // Check if this is a builtin type and handle its methods
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            return self.analyze_builtin_method(
                air,
                ctx,
                struct_id,
                builtin_def,
                receiver_result,
                receiver_var,
                receiver_storage,
                &method_name_str,
                &args,
                span,
            );
        }

        // Look up the struct name by its ID
        let struct_def = &self.struct_defs[struct_id.0 as usize];
        let struct_name_str = struct_def.name.clone();

        // Find the struct name symbol for method lookup
        let struct_name_sym = self.interner.get_or_intern(&struct_name_str);

        // Look up the method
        let method_key = (struct_name_sym, method);
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedMethod {
                type_name: struct_name_str.clone(),
                method_name: method_name_str.clone(),
            },
            span,
        )?;

        // Check that this is a method (has self), not an associated function
        if !method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::AssocFnCalledAsMethod {
                    type_name: struct_name_str,
                    function_name: method_name_str,
                },
                span,
            ));
        }

        // Check argument count (method_info.param_types excludes self)
        if args.len() != method_info.param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_info.param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze arguments - receiver first, then remaining args
        let mut air_args = vec![AirCallArg {
            value: receiver_result.air_ref,
            mode: AirArgMode::Normal,
        }];
        air_args.extend(self.analyze_call_args(air, &args, ctx)?);

        // Generate a method call name: Type.method
        let call_name = format!("{}.{}", struct_name_str, method_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

        // Encode call args into extra array
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Implementation for AssocFnCall.
    pub(crate) fn analyze_assoc_fn_call_impl(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let args = self.rir.get_call_args(args_start, args_len);
        let type_name_str = self.interner.resolve(&type_name).to_string();
        let function_name_str = self.interner.resolve(&function).to_string();

        // Check that the type exists and is a struct
        let struct_id = *self
            .structs
            .get(&type_name)
            .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.clone()), span)?;

        // Handle builtin type associated functions
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            return self.analyze_builtin_assoc_fn(
                air,
                ctx,
                struct_id,
                builtin_def,
                &function_name_str,
                &args,
                span,
            );
        }

        // Look up the function
        let method_key = (type_name, function);
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedAssocFn {
                type_name: type_name_str.clone(),
                function_name: function_name_str.clone(),
            },
            span,
        )?;

        // Check that this is an associated function (no self), not a method
        if method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::MethodCalledAsAssocFn {
                    type_name: type_name_str,
                    method_name: function_name_str,
                },
                span,
            ));
        }

        // Check argument count
        if args.len() != method_info.param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_info.param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze arguments
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        // Generate a function call name: Type::function
        let call_name = format!("{}::{}", type_name_str, function_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

        // Encode call args into extra array
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Implementation for Intrinsic calls.
    pub(crate) fn analyze_intrinsic_impl(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Intrinsic arguments are stored as plain InstRefs
        let arg_refs = self.rir.get_inst_refs(args_start, args_len);
        let args: Vec<RirCallArg> = arg_refs
            .into_iter()
            .map(|value| RirCallArg {
                value,
                mode: RirArgMode::Normal,
            })
            .collect();
        let intrinsic_name_str = self.interner.resolve(&name);

        match intrinsic_name_str {
            "dbg" => self.analyze_dbg_intrinsic(air, inst_ref, &args, span, ctx),
            "intCast" => self.analyze_intcast_intrinsic(air, inst_ref, &args, span, ctx),
            "test_preview_gate" => self.analyze_test_preview_gate_intrinsic(air, &args, span),
            "read_line" => self.analyze_read_line_intrinsic(air, name, &args, span),
            "parse_i32" | "parse_i64" | "parse_u32" | "parse_u64" => {
                self.analyze_parse_intrinsic(air, name, intrinsic_name_str, &args, span, ctx)
            }
            "cast" => self.analyze_cast_intrinsic(air, inst_ref, &args, span, ctx),
            "panic" => self.analyze_panic_intrinsic(air, &args, span, ctx),
            "assert" => self.analyze_assert_intrinsic(air, &args, span, ctx),
            _ => Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(intrinsic_name_str.to_string()),
                span,
            )),
        }
    }

    // Helper methods for intrinsic analysis (delegated from analyze_intrinsic_impl)

    fn analyze_dbg_intrinsic(
        &mut self,
        air: &mut Air,
        _inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "dbg".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // Validate type
        if !arg_type.is_integer()
            && arg_type != Type::Bool
            && !arg_type.is_struct()
            && !arg_type.is_enum()
            && !arg_type.is_array()
            && !arg_type.is_error()
            && !arg_type.is_never()
        {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "dbg".to_string(),
                    expected: "integer, bool, struct, enum, or array".to_string(),
                    found: arg_type.name().to_string(),
                })),
                span,
            ));
        }

        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.interner.get_or_intern("dbg"),
                args_start,
                args_len: 1,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    fn analyze_cast_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "cast".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get target type from HM inference
        let target_type = Self::get_resolved_type(ctx, inst_ref, span, "@cast intrinsic")?;

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let source_type = arg_result.ty;

        // Validate types
        if !source_type.is_integer() && !source_type.is_error() && !source_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer type".to_string(),
                    found: source_type.name().to_string(),
                })),
                span,
            ));
        }
        if !target_type.is_integer() && !target_type.is_error() && !target_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer target type".to_string(),
                    found: target_type.name().to_string(),
                })),
                span,
            ));
        }

        // Skip cast if types are the same
        if source_type == target_type || source_type.is_error() || source_type.is_never() {
            return Ok(arg_result);
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::IntCast {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
            ty: target_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, target_type))
    }

    fn analyze_panic_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @panic takes an optional string message
        if args.len() > 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "panic".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        if args.is_empty() {
            // Panic with no message
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::Never,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Never));
        }

        // Analyze the message argument
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;

        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.interner.get_or_intern("panic"),
                args_start,
                args_len: 1,
            },
            ty: Type::Never,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Never))
    }

    fn analyze_assert_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @assert takes a bool condition and optional message
        if args.is_empty() || args.len() > 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "assert".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let cond_result = self.analyze_inst(air, args[0].value, ctx)?;

        // Build args for AIR
        let mut extra_data = vec![cond_result.air_ref.as_u32()];
        if args.len() > 1 {
            let msg_result = self.analyze_inst(air, args[1].value, ctx)?;
            extra_data.push(msg_result.air_ref.as_u32());
        }

        let args_len = extra_data.len() as u32;
        let args_start = air.add_extra(&extra_data);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.interner.get_or_intern("assert"),
                args_start,
                args_len,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Analyze @intCast intrinsic.
    fn analyze_intcast_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = "intCast";

        // @intCast expects exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic_name.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the argument
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let from_ty = arg_result.ty;

        // Argument must be an integer type
        if !from_ty.is_integer() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: intrinsic_name.to_string(),
                    expected: "integer".to_string(),
                    found: from_ty.name().to_string(),
                })),
                span,
            ));
        }

        // Get the target type from HM inference
        let target_ty = match ctx.resolved_types.get(&inst_ref).copied() {
            Some(ty) if ty.is_integer() => ty,
            Some(Type::Error) => {
                // Error already reported during type inference
                return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
            }
            Some(ty) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: intrinsic_name.to_string(),
                        expected: "integer".to_string(),
                        found: ty.name().to_string(),
                    })),
                    span,
                ));
            }
            None => {
                // Type inference couldn't determine the target type
                return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
            }
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::IntCast {
                value: arg_result.air_ref,
                from_ty,
            },
            ty: target_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, target_ty))
    }

    /// Analyze @test_preview_gate intrinsic.
    fn analyze_test_preview_gate_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @test_preview_gate() - no-op intrinsic gated by test_infra preview feature.
        self.require_preview(
            PreviewFeature::TestInfra,
            "@test_preview_gate() intrinsic",
            span,
        )?;

        // Takes no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "test_preview_gate".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // No-op: just return a unit constant
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Analyze @read_line intrinsic.
    fn analyze_read_line_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @read_line() - reads a line from stdin and returns it as a String.
        // Takes no arguments, returns String.
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "read_line".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get the String type
        let string_type = self.builtin_string_type();

        // Create the intrinsic instruction that returns String
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: string_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, string_type))
    }

    /// Analyze @parse_i32, @parse_i64, @parse_u32, @parse_u64 intrinsics.
    fn analyze_parse_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        intrinsic_name_str: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Expects exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic_name_str.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the argument - String borrows are handled by
        // analyze_inst_for_projection to avoid consuming the String
        let arg_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // Argument must be a String
        if !self.is_builtin_string(arg_type) {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: format!("@{}", intrinsic_name_str),
                    expected: "String".to_string(),
                    found: arg_type.name().to_string(),
                })),
                span,
            ));
        }

        // Determine the return type based on the intrinsic name
        let return_type = match intrinsic_name_str {
            "parse_i32" => Type::I32,
            "parse_i64" => Type::I64,
            "parse_u32" => Type::U32,
            "parse_u64" => Type::U64,
            _ => unreachable!(),
        };

        // Encode args into extra array
        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    // Note: The old analyze_inst body from here onwards is now handled by the
    // dispatcher above and the category methods in analyze_ops.rs

    // ========================================================================
    // Helper methods for analysis
    // ========================================================================

    /// Convert RIR argument mode to AIR argument mode.
    fn convert_arg_mode(mode: RirArgMode) -> AirArgMode {
        match mode {
            RirArgMode::Normal => AirArgMode::Normal,
            RirArgMode::Inout => AirArgMode::Inout,
            RirArgMode::Borrow => AirArgMode::Borrow,
        }
    }
    /// Analyze a binary arithmetic operator (+, -, *, /, %).
    ///
    /// Follows Rust's type inference rules:
    /// Types are determined by HM inference. Both operands must have the same type.
    fn analyze_binary_arith<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        let lhs_result = self.analyze_inst(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst(air, rhs, ctx)?;

        // Verify the type is integer (HM should have enforced this, but check anyway)
        if !lhs_result.ty.is_integer() && !lhs_result.ty.is_error() && !lhs_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: lhs_result.ty.name().to_string(),
                },
                span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: lhs_result.ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, lhs_result.ty))
    }

    /// Analyze a comparison operator.
    ///
    /// Types are determined by HM inference. Both operands must have the same type.
    ///
    /// For equality operators (`==`, `!=`), both integers and booleans are allowed.
    /// For ordering operators (`<`, `>`, `<=`, `>=`), only integers are allowed.
    fn analyze_comparison<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        allow_bool: bool,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        // Check for chained comparisons (e.g., `a < b < c`)
        // Since the parser is left-associative, `a < b < c` parses as `(a < b) < c`,
        // so we only need to check if the LHS is a comparison.
        if self.is_comparison(lhs) {
            return Err(CompileError::new(ErrorKind::ChainedComparison, span)
                .with_help("use `&&` to combine comparisons: `a < b && b < c`"));
        }

        // Comparisons read values without consuming them (like projections).
        // This matches Rust's PartialEq trait which takes references.
        let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors
        if lhs_type.is_never() || lhs_type.is_error() {
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::Bool,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Bool));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) work on integers, booleans, strings, unit, and structs
            // Note: String is now a struct, so is_struct() covers it
            if !lhs_type.is_integer()
                && lhs_type != Type::Bool
                && lhs_type != Type::Unit
                && !lhs_type.is_struct()
                && !self.is_builtin_string(lhs_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer, bool, string, unit, or struct".to_string(),
                        found: lhs_type.name().to_string(),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.name().to_string(),
                },
                self.rir.get(lhs).span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::Bool,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Bool))
    }

    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls) or would cause overflow/panic.
    ///
    /// This is the foundation for compile-time bounds checking and can be extended
    /// for future `comptime` features.
    fn try_evaluate_const(&self, inst_ref: InstRef) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Integer(n) => n.checked_neg().map(ConstValue::Integer),
                    ConstValue::Bool(_) => None, // Can't negate a boolean
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                    ConstValue::Integer(_) => None, // Can't logical-NOT an integer
                }
            }

            // Binary arithmetic operations
            InstData::Add { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_add(r).map(ConstValue::Integer)
            }
            InstData::Sub { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_sub(r).map(ConstValue::Integer)
            }
            InstData::Mul { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_mul(r).map(ConstValue::Integer)
            }
            InstData::Div { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                if r == 0 {
                    None // Division by zero - defer to runtime
                } else {
                    l.checked_div(r).map(ConstValue::Integer)
                }
            }
            InstData::Mod { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                if r == 0 {
                    None // Modulo by zero - defer to runtime
                } else {
                    l.checked_rem(r).map(ConstValue::Integer)
                }
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a == b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a == b)),
                    _ => None, // Mixed types
                }
            }
            InstData::Ne { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a != b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a != b)),
                    _ => None,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l >= r))
            }

            // Logical operations
            InstData::And { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_bool()?;
                let r = self.try_evaluate_const(*rhs)?.as_bool()?;
                Some(ConstValue::Bool(l && r))
            }
            InstData::Or { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_bool()?;
                let r = self.try_evaluate_const(*rhs)?.as_bool()?;
                Some(ConstValue::Bool(l || r))
            }

            // Bitwise operations
            InstData::BitAnd { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l & r))
            }
            InstData::BitOr { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l | r))
            }
            InstData::BitXor { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l ^ r))
            }
            InstData::Shl { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                // Only constant-fold small shift amounts to avoid type-width issues.
                // For shifts >= 8, defer to runtime where hardware handles masking correctly.
                // This is conservative but safe - we don't know the operand type here.
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l << r))
            }
            InstData::Shr { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                // Only constant-fold small shift amounts to avoid type-width issues.
                // For shifts >= 8, defer to runtime where hardware handles masking correctly.
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l >> r))
            }
            InstData::BitNot { operand } => {
                let n = self.try_evaluate_const(*operand)?.as_integer()?;
                Some(ConstValue::Integer(!n))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    pub(crate) fn try_get_const_index(&self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Check if an RIR instruction is an integer literal.
    ///
    /// This is used for bidirectional type inference to detect when the LHS
    /// of a binary operator is a literal that can adopt its type from the RHS.
    fn is_integer_literal(&self, inst_ref: InstRef) -> bool {
        matches!(self.rir.get(inst_ref).data, InstData::IntConst(_))
    }

    /// Check if an RIR instruction is a comparison operation.
    ///
    /// This is used to detect chained comparisons (e.g., `a < b < c`) which are
    /// not allowed in Rue.
    fn is_comparison(&self, inst_ref: InstRef) -> bool {
        matches!(
            self.rir.get(inst_ref).data,
            InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
        )
    }

    /// Analyze a builtin type associated function call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    fn analyze_builtin_assoc_fn(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        struct_id: StructId,
        builtin_def: &'static BuiltinTypeDef,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        use rue_builtins::{BuiltinParamType, BuiltinReturnType};

        // Look up the associated function in the registry
        let assoc_fn = builtin_def
            .find_associated_fn(function_name)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedAssocFn {
                        type_name: builtin_def.name.to_string(),
                        function_name: function_name.to_string(),
                    },
                    span,
                )
            })?;

        // Check argument count
        if args.len() != assoc_fn.params.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: assoc_fn.params.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match assoc_fn.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::Bool,
                BuiltinParamType::SelfType => Type::Struct(struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty && !arg_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.name().to_string(),
                        found: arg_result.ty.name().to_string(),
                    },
                    span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match assoc_fn.return_ty {
            BuiltinReturnType::Unit => Type::Unit,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::Bool,
            BuiltinReturnType::SelfType => self.builtin_air_type(struct_id),
        };

        // Generate runtime function call
        let call_name = self.interner.get_or_intern(assoc_fn.runtime_fn);

        // Encode args into extra array
        let mut extra_data: Vec<u32> = Vec::with_capacity(air_args.len() * 2);
        for (air_ref, mode) in &air_args {
            extra_data.push(air_ref.as_u32());
            extra_data.push(mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: air_args.len() as u32,
            },
            ty: return_ty,
            span,
        });

        Ok(AnalysisResult::new(air_ref, return_ty))
    }

    /// Analyze a builtin type method call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    /// Handles borrow semantics (for query methods) and mutation semantics (for
    /// methods that modify the receiver).
    #[allow(clippy::too_many_arguments)]
    fn analyze_builtin_method(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        struct_id: StructId,
        builtin_def: &'static BuiltinTypeDef,
        receiver: AnalysisResult,
        receiver_var: Option<Spur>,
        receiver_storage: Option<StringReceiverStorage>,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        use rue_builtins::{BuiltinParamType, BuiltinReturnType, ReceiverMode};

        // Look up the method in the registry
        let method = builtin_def.find_method(method_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::UndefinedMethod {
                    type_name: builtin_def.name.to_string(),
                    method_name: method_name.to_string(),
                },
                span,
            )
        })?;

        // Handle receiver mode (borrow vs mutation vs consume)
        match method.receiver_mode {
            ReceiverMode::ByRef => {
                // Borrow semantics - "unmove" the variable since it's not consumed
                if let Some(var_symbol) = receiver_var {
                    ctx.moved_vars.remove(&var_symbol);
                }
            }
            ReceiverMode::ByMutRef => {
                // Mutation semantics - variable remains valid after
                if let Some(var_symbol) = receiver_var {
                    ctx.moved_vars.remove(&var_symbol);
                }
            }
            ReceiverMode::ByValue => {
                // Consume semantics - variable is moved (already handled by analyze_inst)
            }
        }

        // Check argument count
        if args.len() != method.params.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method.params.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len() + 1);

        // Add receiver as first argument
        air_args.push((receiver.air_ref, AirArgMode::Normal));

        // Analyze and add other arguments
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match method.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::Bool,
                BuiltinParamType::SelfType => Type::Struct(struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty
                && !arg_result.ty.is_error()
                && !(self.is_builtin_string(arg_result.ty)
                    && matches!(method.params[i].ty, BuiltinParamType::SelfType))
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.name().to_string(),
                        found: arg_result.ty.name().to_string(),
                    },
                    span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match method.return_ty {
            BuiltinReturnType::Unit => Type::Unit,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::Bool,
            BuiltinReturnType::SelfType => self.builtin_air_type(struct_id),
        };

        // Generate runtime function call
        let call_name = self.interner.get_or_intern(method.runtime_fn);

        // Encode args into extra array
        let mut extra_data: Vec<u32> = Vec::with_capacity(air_args.len() * 2);
        for (air_ref, mode) in &air_args {
            extra_data.push(air_ref.as_u32());
            extra_data.push(mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: air_args.len() as u32,
            },
            ty: return_ty,
            span,
        });

        // For mutation methods, store the result back to the receiver
        if method.receiver_mode == ReceiverMode::ByMutRef {
            let storage = receiver_storage
                .ok_or_else(|| CompileError::new(ErrorKind::InvalidAssignmentTarget, span))?;
            return self.store_string_result(air, call_ref, storage, span);
        }

        Ok(AnalysisResult::new(call_ref, return_ty))
    }

    /// Get the storage location for a String receiver in a mutation method call.
    ///
    /// For mutation methods like `push_str`, `push`, `clear`, `reserve`, we need
    /// to know where to store the updated String after the runtime function returns.
    ///
    /// Returns `Some(storage)` if the receiver is a mutable local or inout parameter.
    /// Returns an error if the receiver is:
    /// - An immutable binding (`let` instead of `var`)
    /// - A borrow parameter (can't mutate borrowed values)
    /// - Not an lvalue (e.g., a function call result)
    fn get_string_receiver_storage(
        &self,
        receiver_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<Option<StringReceiverStorage>> {
        let receiver_inst = self.rir.get(receiver_ref);

        match &receiver_inst.data {
            InstData::VarRef { name } => {
                // Check if this is a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    // Check parameter mode
                    match param_info.mode {
                        RirParamMode::Inout => {
                            return Ok(Some(StringReceiverStorage::Param {
                                abi_slot: param_info.abi_slot,
                            }));
                        }
                        RirParamMode::Borrow => {
                            let name_str = self.interner.resolve(&*name);
                            return Err(CompileError::new(
                                ErrorKind::MutateBorrowedValue {
                                    variable: name_str.to_string(),
                                },
                                span,
                            ));
                        }
                        RirParamMode::Normal => {
                            // Normal parameters can be mutated if declared as `var`
                            // For now, we don't allow mutation of normal params
                            let name_str = self.interner.resolve(&*name);
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(name_str.to_string()),
                                span,
                            ));
                        }
                    }
                }

                // Check if it's a local variable
                if let Some(local) = ctx.locals.get(name) {
                    if !local.is_mut {
                        let name_str = self.interner.resolve(&*name);
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(name_str.to_string()),
                            span,
                        ));
                    }
                    return Ok(Some(StringReceiverStorage::Local { slot: local.slot }));
                }

                // Variable not found
                let name_str = self.interner.resolve(&*name);
                Err(CompileError::new(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                ))
            }

            // For other receiver types (field access, function calls, etc.),
            // we don't support mutation for now
            _ => Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span)),
        }
    }

    /// Store the result of a String mutation method back to the receiver's storage.
    ///
    /// Returns a Unit-typed result since mutation methods don't return a value.
    fn store_string_result(
        &self,
        air: &mut Air,
        call_ref: AirRef,
        storage: StringReceiverStorage,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let store_ref = match storage {
            StringReceiverStorage::Local { slot } => air.add_inst(AirInst {
                data: AirInstData::Store {
                    slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
        };

        Ok(AnalysisResult::new(store_ref, Type::Unit))
    }

    /// Check if directives contain @allow for a specific warning name.
    pub(crate) fn has_allow_directive(
        &self,
        directives: &[RirDirective],
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.interner.get("allow");
        let warning_sym = self.interner.get(warning_name);

        for directive in directives {
            if Some(directive.name) == allow_sym {
                for arg in &directive.args {
                    if Some(*arg) == warning_sym {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check for unused local variables in the current scope (before popping it).
    /// Uses the scope stack to determine which variables were added in the current scope.
    pub(crate) fn check_unused_locals_in_current_scope(&self, ctx: &mut AnalysisContext) {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return;
        };

        for (symbol, _old_value) in current_scope {
            // Skip if variable was used
            if ctx.used_locals.contains(symbol) {
                continue;
            }

            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Get variable name
            let name = self.interner.resolve(&*symbol);

            // Skip variables starting with underscore (convention for intentionally unused)
            if name.starts_with('_') {
                continue;
            }

            // Skip if @allow(unused_variable) was applied
            if local.allow_unused {
                continue;
            }

            // Emit warning with help suggestion (to ctx.warnings for parallel safety)
            ctx.warnings.push(
                CompileWarning::new(WarningKind::UnusedVariable(name.to_string()), local.span)
                    .with_help(format!(
                        "if this is intentional, prefix it with an underscore: `_{}`",
                        name
                    )),
            );
        }
    }

    /// Check for unconsumed linear values in the current scope (before popping it).
    /// Linear values MUST be consumed (moved) - it's an error to let them drop implicitly.
    /// Returns an error if any linear value was not consumed.
    pub(crate) fn check_unconsumed_linear_values(
        &self,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return Ok(());
        };

        for (symbol, _old_value) in current_scope {
            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Only check linear types
            if !self.is_type_linear(local.ty) {
                continue;
            }

            // Check if this variable was moved (consumed)
            let was_consumed = ctx
                .moved_vars
                .get(symbol)
                .is_some_and(|state| state.full_move.is_some());

            if !was_consumed {
                let name = self.interner.resolve(&*symbol);
                return Err(CompileError::new(
                    ErrorKind::LinearValueNotConsumed(name.to_string()),
                    local.span,
                ));
            }
        }

        Ok(())
    }

    /// Extract the root variable symbol from an expression, if it refers to a variable.
    ///
    /// For inout arguments, we need to track which variable is being passed to detect
    /// when the same variable is passed to multiple inout parameters.
    ///
    /// Returns Some(symbol) for:
    /// - VarRef { name } -> the variable symbol
    /// - ParamRef { name, .. } -> the parameter symbol
    /// - FieldGet { base, .. } -> recursively extract from base
    /// - IndexGet { base, .. } -> recursively extract from base
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    pub(crate) fn extract_root_variable(&self, inst_ref: InstRef) -> Option<Spur> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            InstData::VarRef { name } => Some(*name),
            InstData::ParamRef { name, .. } => Some(*name),
            InstData::FieldGet { base, .. } => self.extract_root_variable(*base),
            InstData::IndexGet { base, .. } => self.extract_root_variable(*base),
            _ => None,
        }
    }

    /// Extract the root variable symbol and field path from an expression.
    ///
    /// For expressions like `s.a.b`, returns (sym("s"), [sym("a"), sym("b")]).
    /// For `s`, returns (sym("s"), []).
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    pub(crate) fn extract_field_path(&self, inst_ref: InstRef) -> Option<(Spur, FieldPath)> {
        let mut path = Vec::new();
        let root = self.extract_field_path_inner(inst_ref, &mut path)?;
        // Path is built in reverse order, so reverse it
        path.reverse();
        Some((root, path))
    }

    /// Helper for extract_field_path that builds the path in reverse order.
    fn extract_field_path_inner(&self, inst_ref: InstRef, path: &mut FieldPath) -> Option<Spur> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            InstData::VarRef { name } => Some(*name),
            InstData::ParamRef { name, .. } => Some(*name),
            InstData::FieldGet { base, field } => {
                path.push(*field);
                self.extract_field_path_inner(*base, path)
            }
            // For index expressions, we stop tracking the field path
            // (index-based moves are more complex and not addressed here)
            InstData::IndexGet { .. } => None,
            _ => None,
        }
    }

    /// Check exclusivity rules for inout and borrow parameters in a call.
    ///
    /// This enforces two rules:
    /// 1. Same variable cannot be passed to multiple inout parameters (prevents aliasing)
    /// 2. Same variable cannot be passed to both inout and borrow (law of exclusivity)
    ///
    /// The law of exclusivity: either one mutable (inout) access OR any number of
    /// immutable (borrow) accesses, never both simultaneously.
    pub(crate) fn check_exclusive_access(
        &self,
        args: &[RirCallArg],
        call_span: Span,
    ) -> CompileResult<()> {
        use std::collections::HashSet;
        let mut inout_vars: HashSet<Spur> = HashSet::new();
        let mut borrow_vars: HashSet<Spur> = HashSet::new();

        for arg in args {
            let maybe_var_symbol = self.extract_root_variable(arg.value);

            // Check that inout/borrow arguments are lvalues
            if arg.is_inout() && maybe_var_symbol.is_none() {
                return Err(CompileError::new(
                    ErrorKind::InoutNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }
            if arg.is_borrow() && maybe_var_symbol.is_none() {
                return Err(CompileError::new(
                    ErrorKind::BorrowNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }

            if let Some(var_symbol) = maybe_var_symbol {
                if arg.is_inout() {
                    // Check for duplicate inout access
                    if !inout_vars.insert(var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::InoutExclusiveAccess { variable: var_name },
                            call_span,
                        ));
                    }
                    // Check for borrow/inout conflict
                    if borrow_vars.contains(&var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                } else if arg.is_borrow() {
                    borrow_vars.insert(var_symbol);
                    // Check for borrow/inout conflict
                    if inout_vars.contains(&var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Analyze a list of call arguments, handling inout unmove logic.
    ///
    /// For inout arguments, the variable is "unmoving" after analysis - this is because
    /// inout is a mutable borrow, not a move. The value stays valid after the call.
    pub(crate) fn analyze_call_args(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::new();
        for arg in args.iter() {
            // For inout/borrow arguments, extract the variable name before analysis
            // so we can "unmove" it after - these are borrows, not moves
            let borrowed_var = if arg.is_inout() || arg.is_borrow() {
                self.extract_root_variable(arg.value)
            } else {
                None
            };

            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // If this was an inout/borrow argument, the variable shouldn't be marked as moved
            // because these are borrows - the value stays valid after the call
            if let Some(var_symbol) = borrowed_var {
                ctx.moved_vars.remove(&var_symbol);
            }

            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(air_args)
    }
}
