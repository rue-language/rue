//! Core per-instruction analysis dispatch and field/index assignment analysis.
//!
//! This category is the canonical instruction dispatcher and owns assignment
//! analysis for projected places.

use super::*;

impl<'a> BodySema<'a> {
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
    /// - **Variables**: Alloc, VarRef, Assign
    /// - **Structs**: StructDecl, StructInit, FieldGet, FieldSet
    /// - **Arrays**: ArrayInit, IndexGet, IndexSet
    /// - **Enums**: EnumDecl, EnumVariant
    /// - **Calls**: Call, MethodCall
    /// - **Intrinsics**: Intrinsic, TypeIntrinsic, OffsetOf
    /// - **Declarations**: DropFnDecl, FnDecl
    pub(crate) fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // These expressions synthesize their result from their own operands or
        // declaration. A surrounding result expectation belongs to the whole
        // expression and must not contextually type those operands. Keep this
        // boundary at dispatch so every implementation path (including
        // String `+` and place/projection fast paths) gets identical cleanup.
        let clears_result_expectation = matches!(
            &self.rir.get(inst_ref).data,
            InstData::Add { .. }
                | InstData::Sub { .. }
                | InstData::Mul { .. }
                | InstData::Div { .. }
                | InstData::Mod { .. }
                | InstData::BitAnd { .. }
                | InstData::BitOr { .. }
                | InstData::BitXor { .. }
                | InstData::Shl { .. }
                | InstData::Shr { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
                | InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::And { .. }
                | InstData::Or { .. }
                | InstData::Neg { .. }
                | InstData::Not { .. }
                | InstData::BitNot { .. }
                | InstData::Loop { .. }
                | InstData::InfiniteLoop { .. }
                | InstData::FieldGet { .. }
                | InstData::FieldSet { .. }
                | InstData::IndexGet { .. }
                | InstData::IndexSet { .. }
        );
        if clears_result_expectation {
            return ctx
                .with_expected_type(None, |ctx| self.analyze_inst_dispatch(air, inst_ref, ctx));
        }
        self.analyze_inst_dispatch(air, inst_ref, ctx)
    }

    fn analyze_inst_dispatch(
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

            // Binary arithmetic operations (Add also covers String + String
            // concatenation — see analyze_add).
            InstData::Add { lhs, rhs } => {
                self.analyze_add(air, inst_ref, *lhs, *rhs, inst.span, ctx)
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
            | InstData::Try { .. }
            | InstData::Break { .. }
            | InstData::Continue
            | InstData::Ret(_)
            | InstData::Block { .. } => self.analyze_control_flow(air, inst_ref, ctx),

            // Variable operations
            InstData::Alloc { .. } | InstData::VarRef { .. } | InstData::Assign { .. } => {
                self.analyze_variable_ops(air, inst_ref, ctx)
            }

            // Struct operations
            InstData::StructDecl { .. }
            | InstData::StructInit { .. }
            | InstData::FieldGet { .. }
            | InstData::FieldSet { .. } => self.analyze_struct_ops(air, inst_ref, ctx),

            // Array operations
            InstData::ArrayInit { .. }
            | InstData::ArrayRepeat { .. }
            | InstData::IndexGet { .. }
            | InstData::IndexSet { .. } => self.analyze_array_ops(air, inst_ref, ctx),

            // Enum operations
            InstData::EnumDecl { .. } | InstData::EnumVariant { .. } => {
                self.analyze_enum_ops(air, inst_ref, ctx)
            }

            // Call operations
            InstData::Call { .. } | InstData::MethodCall { .. } => {
                self.analyze_call_ops(air, inst_ref, ctx)
            }

            // Intrinsic operations
            InstData::Intrinsic { .. }
            | InstData::InternalIntrinsic { .. }
            | InstData::TypeIntrinsic { .. }
            | InstData::OffsetOf { .. } => self.analyze_intrinsic_ops(air, inst_ref, ctx),

            // Declaration no-ops (produce Unit in expression context)
            InstData::DropFnDecl { .. } | InstData::FnDecl { .. } | InstData::ConstDecl { .. } => {
                self.analyze_decl_noop(air, inst_ref, ctx)
            }

            // Comptime block expression
            InstData::Comptime { expr } => {
                // Evaluate the inner expression at compile time. The
                // environment carries the comptime parameters in scope and
                // the HM-resolved types, so arithmetic is checked at the
                // operand type (spec 8.1 / 4.14:4) and comptime parameters
                // are usable as constants (spec 4.14:5). A would-panic
                // operation (overflow, division by zero) propagates as a
                // compile error here.
                let result = {
                    let mut env = super::super::comptime_eval::ComptimeEnv::for_analysis(ctx);
                    self.eval_const_expr(*expr, &mut env)?
                };
                match result {
                    Some(ConstValue::Integer(value)) => {
                        // Get the expected type from resolved types
                        let ty =
                            Self::get_resolved_type(ctx, inst_ref, inst.span, "comptime block")?;

                        // Backstop range check: negative results are legal
                        // for signed targets (RUE-71); the value just has to
                        // be representable in the target type.
                        if !super::super::comptime_eval::const_int_fits(value, ty) {
                            return if value >= 0 {
                                Err(CompileError::new(
                                    ErrorKind::LiteralOutOfRange {
                                        value: value as u64,
                                        ty: ty.safe_name_with_pool(Some(&self.type_pool)),
                                    },
                                    inst.span,
                                ))
                            } else {
                                Err(CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "value {} is out of range for type {}",
                                            value,
                                            ty.safe_name_with_pool(Some(&self.type_pool))
                                        ),
                                    },
                                    inst.span,
                                ))
                            };
                        }

                        // Two's-complement encoding: negative values are
                        // sign-extended into the u64 payload, matching how
                        // negative literals are emitted elsewhere.
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(value as u64),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Bool(value)) => {
                        let ty = Type::BOOL;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::BoolConst(value),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Type(_type_val)) => {
                        // Type values can only exist at comptime - they cannot be returned
                        // from a comptime block since they can't exist at runtime.
                        Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "type values cannot exist at runtime".to_string(),
                            },
                            inst.span,
                        ))
                    }
                    Some(ConstValue::Function(_)) => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "function references cannot exist at runtime".to_string(),
                        },
                        inst.span,
                    )),
                    Some(ConstValue::Unit) => {
                        let ty = Type::UNIT;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    None => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason:
                                "expression contains values that cannot be known at compile time"
                                    .to_string(),
                        },
                        inst.span,
                    )),
                }
            }

            // Type constant: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                // Resolve the type name to a concrete type
                let ty = self.resolve_type(*type_name, inst.span)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous struct type: a struct type constructed at comptime
            // (e.g., `struct { first: T, second: T, fn get(self) -> T { ... } }` in a comptime function)
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Empty structs are not allowed (unless they have methods)
                if field_decls.is_empty() && *methods_len == 0 {
                    return Err(CompileError::new(ErrorKind::EmptyStruct, inst.span));
                }

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    let field_ty = self.resolve_type(type_sym, inst.span)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                // (uses type symbols, not resolved Types, so Self matches Self)
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Check if an equivalent anonymous struct already exists (structural equality)
                // This now compares fields, method signatures, AND captured comptime values
                let (struct_ty, _is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, &HashMap::new());

                // DON'T register methods here - they should be registered during const evaluation
                // (the comptime evaluator's AnonStructType arm in sema::comptime_eval).
                // If we register here, we create a struct without captured comptime values, which is incorrect.
                //
                // if is_new && *methods_len > 0 {
                //     let struct_id = struct_ty
                //         .as_struct()
                //         .expect("anon struct should have StructId");
                //     self.register_anon_struct_methods(
                //         struct_id,
                //         struct_ty,
                //         *methods_start,
                //         *methods_len,
                //         inst.span,
                //     )?;
                // }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(struct_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous enum type: an enum (sum) type constructed at comptime
            // (e.g., `enum { Some(T), None }` in a comptime type function). The
            // enum analog of the AnonStructType arm above. Generic anon enums
            // (payloads mentioning a `comptime T`) are comptime-evaluated, not
            // analyzed here — this path resolves a concrete anon enum, exactly
            // as the struct arm does (ADR-0038, RUE-6 phase 2).
            InstData::AnonEnumType {
                variants_start,
                variants_len,
                payloads_start,
                payloads_len,
            } => {
                let variant_syms = self
                    .rir
                    .get_symbols(*variants_start, *variants_len)
                    .to_vec();
                let payload_words = self.rir.get_extra(*payloads_start, *payloads_len).to_vec();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<Type>> = Vec::with_capacity(variant_syms.len());
                let mut pi = 0usize;
                for vsym in &variant_syms {
                    variant_names.push(self.interner.resolve(vsym).to_string());
                    let k = if payload_words.is_empty() {
                        0
                    } else {
                        let k = payload_words[pi] as usize;
                        pi += 1;
                        k
                    };
                    let mut tys: Vec<Type> = Vec::with_capacity(k);
                    for _ in 0..k {
                        let ty_sym = Spur::try_from_usize(payload_words[pi] as usize)
                            .expect("valid interned type symbol in payload region");
                        pi += 1;
                        let field_ty = self.resolve_type(ty_sym, inst.span)?;
                        // A payload of type `type` cannot exist at runtime
                        // (spec 4.14:6); reject it like struct fields / enum
                        // declarations do.
                        if field_ty.is_comptime_type() {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "type values cannot exist at runtime".to_string(),
                                },
                                inst.span,
                            ));
                        }
                        tys.push(field_ty);
                    }
                    variant_payloads.push(tys);
                }

                let enum_ty = self.find_or_create_anon_enum(&variant_names, &variant_payloads);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(enum_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Checked block: evaluate the inner expression within an unchecked
            // context. Raw-pointer intrinsics and calls to `unchecked fn`s are
            // only legal while `checked_depth > 0` (spec 9.1:1, chapter 9).
            InstData::Checked { expr } => {
                ctx.checked_depth += 1;
                let result = self.analyze_inst(air, *expr, ctx);
                ctx.checked_depth -= 1;
                result
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
        use crate::sema::analyze_ops::ProjectionInfo;

        // Try to trace the base to a place
        if let Some(mut trace) = self.try_trace_place(base, air, ctx)? {
            // Check if the root variable was fully moved
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.interner.resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(root_name)));
                }
            }

            // Writing through a field of a collection an enclosing `for` loop
            // is iterating mutates a shared-borrowed value (spec 4.8:26,
            // RUE-233) — E0428, like an explicit `borrow` parameter.
            self.reject_mutate_iter_borrowed(trace.root_var, span, ctx)?;

            // Check mutability
            let root_name = self.interner.resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                // Provide more specific error based on whether it's a param or local
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            root_name,
                            root_name,
                            root_type.safe_name_with_pool(Some(&self.type_pool))
                        )));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                }
            }

            // Add the final field projection
            let base_type = trace.result_type();
            let struct_id = match base_type.as_struct() {
                Some(id) => id,
                None => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
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
                    span,
                )?;

            let field_type = struct_field.ty;

            // Add the field projection to the trace
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Field {
                    struct_id,
                    field_index: field_index as u32,
                },
                result_type: field_type,
                field_name: Some(field),
                const_index: None,
                index_segment: None,
            });

            // A write through an element of a partially moved array
            // (`xs[0].f = ...` after an element of `xs` moved out) is
            // rejected (RUE-186, E0480), like a direct element write.
            self.reject_write_into_partially_moved_array(&trace, ctx, span)?;

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // RUE-387: writing a live linear value's field would silently drop
            // the old field value. Legal only when that exact field path was
            // proven moved-out on every path (`consume(o.f); o.f = ...`); a
            // dynamic index anywhere in the chain can never prove it.
            let discharged = !trace.has_untrackable_index()
                && self.place_linear_discharged(
                    field_type,
                    trace.root_var,
                    &trace.field_path(),
                    span,
                    ctx,
                );
            self.check_linear_overwrite(field_type, discharged, false, span)?;

            // The write reinitializes its destination: the assigned path
            // (and any moved sub-paths under it) is no longer moved, so
            // `o.f = ...` after `consume(o.f)` makes `o.f` usable again.
            // Writes through an index projection are skipped: a partially
            // moved array is already rejected outright above (E0480), and
            // per-element write re-arm is unsupported (the sema path and the
            // runtime drop flags would disagree), so the whole array must be
            // reinitialized instead — never a single `arr[0].f` write.
            if !trace
                .projections
                .iter()
                .any(|p| matches!(p.proj, AirProjection::Index { .. }))
            {
                let assigned_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get_mut(&trace.root_var) {
                    state.mark_path_reinitialized(&assigned_path);
                    if state.is_empty() {
                        ctx.moved_vars.remove(&trace.root_var);
                    }
                }
            }

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Fallback: base is not a place (e.g., function call result)
        // This shouldn't normally happen for valid assignment targets
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
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
        use crate::sema::analyze_ops::ProjectionInfo;

        // Try to trace the base to a place
        if let Some(mut trace) = self.try_trace_place(base, air, ctx)? {
            // Check if the root variable was fully moved
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.interner.resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(root_name)));
                }
            }

            // Writing an element of a collection an enclosing `for` loop is
            // iterating mutates a shared-borrowed value (spec 4.8:26,
            // RUE-233) — E0428, like an explicit `borrow` parameter.
            self.reject_mutate_iter_borrowed(trace.root_var, span, ctx)?;

            // Check mutability
            let root_name = self.interner.resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            root_name,
                            root_name,
                            root_type.safe_name_with_pool(Some(&self.type_pool))
                        )));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                }
            }

            // Get array type info from the trace
            let base_type = trace.result_type();
            let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
                Some(id) => {
                    let (elem, len) = self.type_pool.array_def(id);
                    (id, elem, len)
                }
                None => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            };

            // Analyze index. Index must be an integer type (signed or
            // unsigned) per spec 7.1:7; negative/out-of-range runtime indices
            // trap at runtime via the bounds check (RUE-81).
            let index_result = self.analyze_inst(air, index, ctx)?;
            if !index_result.ty.is_integer() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer type".to_string(),
                        found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    self.rir.get(index).span,
                ));
            }

            // Compile-time bounds check for constant indices, evaluated at the
            // index's resolved operand types so an overflowing index expression
            // is a compile-time error, not a folded runtime panic (RUE-234).
            if let Some(const_index) = self.try_get_const_index_checked(index, ctx)? {
                if const_index < 0 || const_index >= array_len as i128 {
                    return Err(CompileError::new(
                        ErrorKind::IndexOutOfBounds {
                            index: const_index,
                            length: array_len,
                        },
                        self.rir.get(index).span,
                    ));
                }
            }

            // Add the index projection. A non-negative constant index carries
            // its element path segment so field_path nests through it (RUE-279).
            let const_index = self.try_get_const_index(index);
            let index_segment = match const_index {
                Some(k) if k >= 0 => Some(index_path_segment(self.interner, k as u64)),
                _ => None,
            };
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Index {
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                result_type: elem_type,
                field_name: None,
                const_index,
                index_segment,
            });

            // Writing into an array with moved-out elements is rejected
            // (RUE-186, E0480): the write can't re-arm per-element ownership.
            self.reject_write_into_partially_moved_array(&trace, ctx, span)?;

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // RUE-387: writing a live linear value into an array element would
            // silently drop the old element. Legal only when that exact
            // constant-index element was proven moved-out on every path
            // (`arr[0] = f(arr[0])`); a runtime index can never prove which
            // element is live, so it is always rejected (repro 3).
            let discharged = !trace.has_untrackable_index()
                && self.place_linear_discharged(
                    elem_type,
                    trace.root_var,
                    &trace.field_path(),
                    span,
                    ctx,
                );
            self.check_linear_overwrite(elem_type, discharged, false, span)?;

            // The write reinitializes its destination element: the assigned
            // path is no longer moved, so `arr[0] = arr[0]` un-marks the
            // move-out the RHS `arr[0]` just recorded and leaves the element
            // usable again (spec 3.8:55). This mirrors `analyze_field_set_impl`,
            // which reinitializes an assigned field path. Only a direct,
            // constant-index element of a root array is reinitialized: that is
            // exactly the shape `record_element_move_out` tracks per element
            // (`projections == [Index]` with a known index), so a nested or
            // dynamic index — which was never recorded as a per-element move —
            // is conservatively left alone (RUE-228).
            if let [
                ProjectionInfo {
                    const_index: Some(k),
                    ..
                },
            ] = trace.projections.as_slice()
            {
                if *k >= 0 {
                    let elem_path = vec![index_path_segment(self.interner, *k as u64)];
                    if let Some(state) = ctx.moved_vars.get_mut(&trace.root_var) {
                        state.mark_path_reinitialized(&elem_path);
                        if state.is_empty() {
                            ctx.moved_vars.remove(&trace.root_var);
                        }
                    }
                }
            }

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Fallback: base is not a place
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
    }
}
