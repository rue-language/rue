//! Built-in arithmetic, comparison, string, and builtin method/assoc-fn analysis.
//!
//! This category owns builtin operations within the canonical semantic-analysis
//! implementation.

use super::*;

impl<'a> BodySema<'a> {
    /// Convert RIR argument mode to AIR argument mode.
    pub(super) fn convert_arg_mode(mode: RirArgMode) -> AirArgMode {
        match mode {
            RirArgMode::Normal => AirArgMode::Normal,
            RirArgMode::Inout => AirArgMode::Inout,
            RirArgMode::Borrow => AirArgMode::Borrow,
        }
    }
    /// Analyze the `+` operator.
    ///
    /// `+` is overloaded: on integers it is arithmetic addition, and on two
    /// `String`s it is concatenation (RUE-17 Phase 1, ADR-0035). HM inference
    /// has already resolved the result type, so we dispatch on it: a `String`
    /// result routes to [`analyze_string_concat`]; anything else is ordinary
    /// integer arithmetic. A mixed `String + int` never resolves to `String`
    /// (unification fails first with E0206), so it takes the arithmetic path and
    /// is rejected there — the user sees a clear type-mismatch error.
    pub(super) fn analyze_add(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        lhs: InstRef,
        rhs: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let is_concat = ctx
            .resolved_types
            .get(&inst_ref)
            .is_some_and(|ty| self.is_builtin_string(*ty));
        if is_concat {
            return self.analyze_string_concat(air, lhs, rhs, span, ctx);
        }
        self.analyze_binary_arith(air, lhs, rhs, AirInstData::Add, span, ctx)
    }

    /// Analyze `s1 + s2` where both operands are `String`: produce a NEW
    /// concatenated `String` (RUE-17 Phase 1, ADR-0035).
    ///
    /// Both operands are *borrowed* (read, not consumed) — like the operands of
    /// `==` — so a named operand remains usable afterwards and a temporary is
    /// dropped by its owner at statement end; neither is leaked. The operation
    /// lowers to an `extern "C"` sret call to `__rue_String_concat(out, ptr1,
    /// len1, cap1, ptr2, len2, cap2)`, reusing the ordinary aggregate-return and
    /// String-flattening call paths (no codegen change).
    pub(super) fn analyze_string_concat(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Analyze both operands in projection (borrow) mode so a named operand
        // is neither moved nor consumed — exactly like the operands of `==`. A
        // variable operand yields a plain Load (no move recorded, still dropped
        // by its owner); a temporary operand (e.g. `@to_string(7) + ...`) falls
        // through to analyze_inst, which marks it moved — cancel that marker so
        // the temporary is instead dropped normally at statement end (no leak,
        // no double free). cancel_move_marker is a no-op on the Load case.
        let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
        air.cancel_move_marker(lhs_result.air_ref);
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
        air.cancel_move_marker(rhs_result.air_ref);

        // Defensive type check (HM inference already guarantees both are StrBuf
        // when we get here; this guards against error-recovery paths).
        for operand in [&lhs_result, &rhs_result] {
            if !self.is_builtin_string(operand.ty) && !operand.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "StrBuf".to_string(),
                        found: operand.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }
        }

        let string_type = self.builtin_string_type();
        let call_name = self
            .interner
            .get_or_intern(rue_builtins::STRING_CONCAT_RUNTIME_FN);

        // Both String operands are flattened into (ptr, len, cap) by codegen;
        // Normal mode with the move cancelled gives flatten-without-consume.
        let extra_data = [
            lhs_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
            rhs_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
        ];
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: 2,
            },
            ty: string_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, string_type))
    }

    /// Analyze the `print(s)` / `println(s)` builtin free functions (RUE-1).
    ///
    /// Both take a single `String` and return unit: `print` writes its raw
    /// bytes to stdout with nothing added, `println` appends a single `\n`.
    /// Formatting and interpolation are deliberately out of scope — callers
    /// compose with `@to_string` and `+` (e.g. `println("n=" + @to_string(n))`).
    ///
    /// The `String` argument is *borrowed* (read, not consumed), exactly like
    /// the operands of `s1 + s2`: a named argument stays usable afterwards and
    /// a temporary is dropped by its owner at statement end (no leak, no double
    /// free). This lowers to an ordinary `extern "C"` call to the runtime
    /// `__rue_print` / `__rue_println`, reusing the String-flattening call path
    /// (the String passes as its three fields ptr/len/cap; the runtime ignores
    /// cap) — so no codegen change is needed.
    pub(crate) fn analyze_print_builtin(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let fn_name = if name == self.known.println {
            "println"
        } else {
            "print"
        };

        let args = self.rir.get_call_args(args_start, args_len);
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 1,
                    found: args.len(),
                },
                span,
            )
            .with_help(format!("`{fn_name}` takes exactly one StrBuf argument")));
        }
        self.validate_explicit_call_modes(&args, std::iter::once(RirParamMode::Normal))?;
        let arg_value = args[0].value;

        // Borrow the argument (like a `+` operand): analyze in projection mode
        // and cancel any move marker so a variable operand is not consumed and
        // a temporary is still dropped by its owner at statement end.
        let arg_result = self.analyze_inst_for_projection(air, arg_value, ctx)?;
        air.cancel_move_marker(arg_result.air_ref);

        if !self.is_builtin_string(arg_result.ty) && !arg_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "StrBuf".to_string(),
                    found: arg_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                self.rir.get(arg_value).span,
            )
            .with_help(format!(
                "`{fn_name}` takes a StrBuf; build one with `@to_string`, `+`, or StrBuf methods"
            )));
        }

        let runtime_fn = if name == self.known.println {
            rue_builtins::PRINTLN_RUNTIME_FN
        } else {
            rue_builtins::PRINT_RUNTIME_FN
        };
        let call_name = self.interner.get_or_intern(runtime_fn);

        // The String is flattened into (ptr, len, cap) by codegen; Normal mode
        // with the move cancelled gives flatten-without-consume (same as the
        // operands of `__rue_String_concat`).
        let extra_data = [arg_result.air_ref.as_u32(), AirArgMode::Normal.as_u32()];
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: 1,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze a binary arithmetic operator (+, -, *, /, %).
    ///
    /// Follows Rust's type inference rules:
    /// Types are determined by HM inference. Both operands must have the same type.
    pub(super) fn analyze_binary_arith<F>(
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
                    found: lhs_result.ty.safe_name_with_pool(Some(&self.type_pool)),
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
    pub(super) fn analyze_comparison<F>(
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
        // Chained comparisons (`a < b < c`) are rejected at PARSE time
        // (`rue-parser/src/validate.rs`), where parentheses are still
        // visible. RIR erases parens, so an RIR-level check here could not
        // tell `1 < 2 == true` (a chain) from `(1 < 2) == true` (an ordinary
        // boolean equality) and wrongly rejected the latter (RUE-528).

        // Comparisons read values without consuming them (like projections).
        // This matches Rust's PartialEq trait which takes references.
        let lhs_is_literal = matches!(self.rir.get(lhs).data, InstData::StringConst(_));
        let (lhs_result, rhs_result) = if lhs_is_literal {
            let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
            let expected = (self.is_builtin_string(rhs_result.ty)
                || self.is_str_like(rhs_result.ty))
            .then_some(rhs_result.ty);
            let lhs_result = ctx.with_expected_type(expected, |ctx| {
                self.analyze_inst_for_projection(air, lhs, ctx)
            })?;
            (lhs_result, rhs_result)
        } else {
            let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
            let expected = (self.is_builtin_string(lhs_result.ty)
                || self.is_str_like(lhs_result.ty))
            .then_some(lhs_result.ty);
            let rhs_result = ctx.with_expected_type(expected, |ctx| {
                self.analyze_inst_for_projection(air, rhs, ctx)
            })?;
            (lhs_result, rhs_result)
        };
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors
        if lhs_type.is_never() || lhs_type.is_error() {
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::BOOL,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::BOOL));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) are structural over aggregates
            // (RUE-285): they work on integers, booleans, strings, unit, and
            // any struct, array, or enum (whose leaves bottom out at these).
            // Note: String is now a struct, so is_struct() covers it.
            if !lhs_type.is_integer()
                && lhs_type != Type::BOOL
                && lhs_type != Type::UNIT
                && !lhs_type.is_struct()
                && !lhs_type.is_array()
                && !lhs_type.is_enum()
                && !self.is_builtin_string(lhs_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer, bool, string, unit, struct, array, or enum".to_string(),
                        found: lhs_type.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.safe_name_with_pool(Some(&self.type_pool)),
                },
                self.rir.get(lhs).span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::BOOL,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    /// Check if an RIR instruction is a VarRef to a comptime type variable.
    ///
    /// This is used when validating comptime arguments to detect variables
    /// that hold comptime type values (e.g., `let P = Point(); ... Line(P)`).
    pub(crate) fn is_comptime_type_var(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> bool {
        if let InstData::VarRef { name } = &self.rir.get(inst_ref).data {
            ctx.comptime_type_vars.contains_key(name)
        } else {
            false
        }
    }

    /// Analyze a builtin type associated function call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    pub(super) fn analyze_builtin_assoc_fn(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        struct_id: StructId,
        builtin_def: &'static BuiltinTypeDef,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        use rue_builtins::BuiltinParamType;

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

        // Builtin registry parameters have types but no source mode marker, so
        // every explicit argument is an ordinary unmarked value.
        self.validate_explicit_call_modes(
            args,
            std::iter::repeat_n(RirParamMode::Normal, args.len()),
        )?;

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match assoc_fn.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::BOOL,
                BuiltinParamType::SelfType => Type::new_struct(struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty && !arg_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.safe_name_with_pool(Some(&self.type_pool)),
                        found: arg_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match assoc_fn.return_ty {
            BuiltinReturnType::Unit => Type::UNIT,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::BOOL,
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
    pub(super) fn analyze_builtin_method(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        method_ctx: &BuiltinMethodContext<'_>,
        receiver: ReceiverInfo,
        args: &[RirCallArg],
    ) -> CompileResult<AnalysisResult> {
        use rue_builtins::{BuiltinParamType, ReceiverMode};

        // Look up the method in the registry
        let method = method_ctx
            .builtin_def
            .find_method(method_ctx.method_name)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: method_ctx.builtin_def.name.to_string(),
                        method_name: method_ctx.method_name.to_string(),
                    },
                    method_ctx.span,
                )
            })?;

        // Handle receiver mode (borrow vs mutation vs consume)
        match method.receiver_mode {
            ReceiverMode::ByRef | ReceiverMode::ByMutRef => {
                // Borrow (ByRef) / mutation (ByMutRef) semantics - "unmove"
                // the receiver since it's not consumed, and cancel the move
                // marker the receiver analysis emitted so drop elaboration
                // doesn't treat this borrow as a move.
                //
                // Restore the pre-receiver snapshot instead of removing the
                // whole entry: earlier moves of sibling paths (`consume(w.s);
                // w.t.len()`) must stay recorded, or a later use of the moved
                // sibling compiles and double-frees (RUE-33).
                if let Some(var_symbol) = receiver.var {
                    match receiver.move_state_before.clone() {
                        Some(state) => {
                            ctx.moved_vars.insert(var_symbol, state);
                        }
                        None => {
                            ctx.moved_vars.remove(&var_symbol);
                        }
                    }
                }
                air.cancel_move_marker(receiver.result.air_ref);
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
                method_ctx.span,
            ));
        }

        // ReceiverMode governs only the implicit receiver. Every explicit
        // builtin-method parameter is unmarked at the source level.
        self.validate_explicit_call_modes(
            args,
            std::iter::repeat_n(RirParamMode::Normal, args.len()),
        )?;

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len() + 1);

        // Add receiver as first argument
        air_args.push((receiver.result.air_ref, AirArgMode::Normal));

        // A by-ref receiver's loan spans the whole call, so a by-value move of
        // the receiver's root in an argument (`s.contains(s)` — the needle is
        // a String, moved) must conflict exactly like `f(borrow s, s)` does
        // (RUE-523). Push the receiver's loan frame while the arguments are
        // analyzed; move-record sites consult it.
        let receiver_loan = match (method.receiver_mode, receiver.var) {
            (ReceiverMode::ByMutRef, Some(root)) => Some(vec![(root, CallLoanKind::Inout)]),
            (ReceiverMode::ByRef, Some(root)) => Some(vec![(root, CallLoanKind::Borrow)]),
            _ => None,
        };
        let receiver_loan_pushed = receiver_loan.is_some();
        if let Some(frame) = receiver_loan {
            ctx.call_loaned_roots.push(frame);
        }
        let args_result = (|| -> CompileResult<()> {
            // Analyze and add other arguments
            for (i, arg) in args.iter().enumerate() {
                let arg_result = self.analyze_inst(air, arg.value, ctx)?;

                // Get expected type from param
                let expected_ty = match method.params[i].ty {
                    BuiltinParamType::U64 => Type::U64,
                    BuiltinParamType::U8 => Type::U8,
                    BuiltinParamType::Bool => Type::BOOL,
                    BuiltinParamType::SelfType => Type::new_struct(method_ctx.struct_id),
                };

                // Type check
                if arg_result.ty != expected_ty
                    && !arg_result.ty.is_error()
                    && !(self.is_builtin_string(arg_result.ty)
                        && matches!(method.params[i].ty, BuiltinParamType::SelfType))
                {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: expected_ty.safe_name_with_pool(Some(&self.type_pool)),
                            found: arg_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        method_ctx.span,
                    ));
                }

                air_args.push((arg_result.air_ref, AirArgMode::Normal));
            }
            Ok(())
        })();
        if receiver_loan_pushed {
            ctx.call_loaned_roots.pop();
        }
        args_result?;

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match method.return_ty {
            BuiltinReturnType::Unit => Type::UNIT,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::BOOL,
            BuiltinReturnType::SelfType => self.builtin_air_type(method_ctx.struct_id),
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
            span: method_ctx.span,
        });

        // For mutation methods, store the result back to the receiver
        if method.receiver_mode == ReceiverMode::ByMutRef {
            // This is the only ParamStore producer besides whole inout
            // assignment. The builtin registry requires every ByMutRef method
            // to return SelfType, so its writeback is representation-identical
            // to the receiver by construction (RUE-641).
            debug_assert_eq!(
                return_ty, receiver.result.ty,
                "builtin mutation writeback must preserve the receiver type"
            );
            let storage = receiver.storage.ok_or_else(|| {
                CompileError::new(ErrorKind::InvalidAssignmentTarget, method_ctx.span)
            })?;
            return self.store_string_result(air, call_ref, storage, method_ctx.span);
        }

        Ok(AnalysisResult::new(call_ref, return_ty))
    }

    /// Validate that a String mutation-method receiver names a MUTABLE place.
    ///
    /// A mutation method (`push_str`, `push`, `clear`, `reserve`) is `inout
    /// self`: it writes the updated String back through the receiver place, so
    /// the place's root binding must be mutable. The receiver value itself is
    /// read as a borrow (via `byref_arg_root`), so use-after-move and the
    /// projection reads are checked by the normal receiver analysis; this only
    /// enforces mutability of the write target, mirroring assignment.
    ///
    /// Resolve the TYPE of a place expression without emitting any AIR or
    /// recording a move (RUE-254).
    ///
    /// Used to learn a method receiver's type — and thus, via method
    /// resolution, whether the method takes `self` by reference (`inout
    /// self` / `borrow self`) — *before* the receiver expression is analyzed.
    /// A by-ref receiver must be analyzed as a borrow rather than a move
    /// (spec 6.4:25, 6.4:29), and that decision needs the type; a move-based
    /// analysis would hard-reject the read of any non-local place (an inout
    /// parameter, an indexed element, a field of `self`, ...) before the
    /// by-ref intent could be recovered. The index expression of an
    /// `IndexGet` is intentionally NOT visited — the element type does not
    /// depend on the index value, and visiting it here would double-analyze
    /// its side effects. Returns `None` for anything that is not a statically
    /// typed place chain rooted at a variable (call results, literals, ...).
    pub(super) fn peek_place_type(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> Option<Type> {
        match &self.rir.get(inst_ref).data {
            InstData::VarRef { name } => {
                if let Some(local) = ctx.locals.get(name) {
                    return Some(local.ty);
                }
                ctx.params.iter().find(|p| p.name == *name).map(|p| p.ty)
            }
            InstData::FieldGet { base, field } => {
                let base_ty = self.peek_place_type(*base, ctx)?;
                let struct_id = base_ty.as_struct()?;
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_name_str = self.interner.resolve(field);
                let (_field_index, struct_field) = struct_def.find_field(field_name_str)?;
                Some(struct_field.ty)
            }
            InstData::IndexGet { base, .. } => {
                let base_ty = self.peek_place_type(*base, ctx)?;
                let array_id = base_ty.as_array()?;
                let (elem_type, _len) = self.type_pool.array_def(array_id);
                Some(elem_type)
            }
            _ => None,
        }
    }

    /// Errors when the receiver is:
    /// - not a place rooted at a variable (`String::new().push(..)`) → E0424
    /// - an immutable `let` binding or a normal/comptime parameter → E0203
    /// - a `borrow` parameter (can't mutate borrowed memory) → E0428
    pub(super) fn check_string_receiver_mutable(
        &self,
        receiver_var: Option<Spur>,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<()> {
        let Some(root) = receiver_var else {
            return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
        };

        // A builtin mutation method takes the receiver by `inout`, so calling
        // one on a collection an enclosing `for` loop is iterating mutates a
        // shared-borrowed value (spec 4.8:26, RUE-257) — E0428, exactly like a
        // direct `a[i] = …` or reassignment inside the body.
        self.reject_mutate_iter_borrowed(root, span, ctx)?;

        // Root local: must be `let mut`. Checked before parameters because a
        // `let` that shadows a param name rebinds the receiver to that local
        // (spec 5.1:10, RUE-278).
        if let Some(local) = ctx.locals.get(&root) {
            if !local.is_mut {
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(self.interner.resolve(&root).to_string()),
                    span,
                ));
            }
            return Ok(());
        }

        // Root parameter: only `inout` names mutable caller storage.
        if let Some(param) = ctx.params.iter().find(|p| p.name == root) {
            return match param.mode {
                RirParamMode::Inout => Ok(()),
                RirParamMode::Borrow => Err(CompileError::new(
                    ErrorKind::MutateBorrowedValue {
                        variable: self.interner.resolve(&root).to_string(),
                    },
                    span,
                )),
                RirParamMode::Normal => Err(CompileError::new(
                    ErrorKind::AssignToImmutable(self.interner.resolve(&root).to_string()),
                    span,
                )),
            };
        }

        Err(CompileError::new(
            ErrorKind::UndefinedVariable(self.interner.resolve(&root).to_string()),
            span,
        ))
    }

    /// Derive the write-back storage for a String mutation method from the AIR
    /// the receiver read produced. The receiver was read as a borrow, so its
    /// value instruction is a plain `Load` (local), `Param` (inout parameter),
    /// or `PlaceRead` (struct field / array element / projection chain); each
    /// maps directly to the matching store target. Reusing the already-built
    /// place means any index expression is evaluated exactly once (RUE-256).
    pub(super) fn string_receiver_storage_from_read(
        &self,
        air: &Air,
        receiver_ref: AirRef,
        span: Span,
    ) -> CompileResult<StringReceiverStorage> {
        match &air.get(receiver_ref).data {
            AirInstData::Load { slot } => Ok(StringReceiverStorage::Local { slot: *slot }),
            AirInstData::Param { index } => Ok(StringReceiverStorage::Param { abi_slot: *index }),
            AirInstData::PlaceRead { place } => Ok(StringReceiverStorage::Place { place: *place }),
            // Any other value (a literal, a call result, …) has no caller-visible
            // storage to write back to.
            _ => Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span)),
        }
    }

    /// Store the result of a String mutation method back to the receiver's storage.
    ///
    /// Returns a Unit-typed result since mutation methods don't return a value.
    pub(super) fn store_string_result(
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
                ty: Type::UNIT,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::UNIT,
                span,
            }),
            StringReceiverStorage::Place { place } => air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place,
                    value: call_ref,
                },
                ty: Type::UNIT,
                span,
            }),
        };

        // A bare `Store`/`ParamStore` produces no CFG value, so returning it as
        // the expression's value leaves an argument position with no operand —
        // `lower_value` returns `None`, the CFG block ends unterminated, and
        // codegen aborts ("block has no terminator", RUE-224). Wrap the store as
        // a side-effect statement inside a Block whose value is a genuine
        // `UnitConst`. In statement position (`s.clear();`) the block runs the
        // store and discards the unit; in value position (`take(s.clear())`) the
        // expression is now a real Unit value, so the argument type check rejects
        // it with a clean E0206 (Unit vs the expected argument type) instead of
        // ICEing.
        let unit_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span,
        });
        let stmts_start = air.add_extra(&[store_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 1,
                value: unit_ref,
            },
            ty: Type::UNIT,
            span,
        });

        Ok(AnalysisResult::new(block_ref, Type::UNIT))
    }
}
