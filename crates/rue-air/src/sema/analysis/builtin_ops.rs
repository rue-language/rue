//! Built-in arithmetic, comparison, string, and builtin method/assoc-fn analysis.
//!
//! This category owns builtin operations within the canonical semantic-analysis
//! implementation.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum IntegerSentinel {
    UnsignedMax,
    NegativeOne,
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
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
            .is_some_and(|ty| self.is_strbuf(*ty));
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
    /// dropped by its owner at statement end; neither is leaked. The canonical
    /// standard-library nominal calls its source-defined `concat_borrowed`
    /// method.
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
        self.cancel_recorded_move(air, lhs_result.air_ref);
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
        self.cancel_recorded_move(air, rhs_result.air_ref);

        // Defensive type check (HM inference already guarantees both are StrBuf
        // when we get here; this guards against error-recovery paths).
        for operand in [&lhs_result, &rhs_result] {
            if !self.is_strbuf(operand.ty) && !operand.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "StrBuf".to_string(),
                        found: operand.ty.safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    span,
                ));
            }
        }

        let string_type = self
            .strbuf_type()
            .ok_or_compile_error(ErrorKind::UnknownType("StrBuf".to_string()), span)?;
        let struct_id = string_type.as_struct().ok_or_compile_error(
            ErrorKind::InternalError("canonical StrBuf lang item is not a struct".to_string()),
            span,
        )?;
        let method = self.intern_body_symbol("concat_borrowed")?;
        if self
            .call_facts()
            .call_method_info(struct_id, method)
            .is_none()
        {
            return Err(CompileError::new(
                ErrorKind::InternalError("canonical StrBuf is missing concat_borrowed".to_string()),
                span,
            ));
        }
        ctx.referenced_methods.insert((struct_id, method));
        self.record_body_method_dependency((struct_id, method))?;
        let call_name =
            self.intern_body_symbol(&self.method_symbol(struct_id, "concat_borrowed", false))?;
        let arg_mode = AirArgMode::Borrow;

        let (lhs_arg, mut temp_scope) =
            self.materialize_borrow_argument(air, lhs_result.air_ref, lhs_result.ty, span, ctx)?;
        let (rhs_arg, rhs_scope) =
            self.materialize_borrow_argument(air, rhs_result.air_ref, rhs_result.ty, span, ctx)?;
        temp_scope.extend(rhs_scope);
        let call_ref = air.add_call(
            None,
            call_name,
            &[
                AirCallArg {
                    value: lhs_arg,
                    mode: arg_mode,
                },
                AirCallArg {
                    value: rhs_arg,
                    mode: arg_mode,
                },
            ],
            string_type,
            span,
        )?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, call_ref, string_type, span, temp_scope)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            string_type,
            lhs_result.continues && rhs_result.continues,
        ))
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
    /// free). The owned-buffer and shared-view cases select typed builtin
    /// mappings whose logical signatures come from the canonical runtime
    /// manifest. AIR retains its existing external-call representation until
    /// the typed call migration.
    pub(crate) fn analyze_print_builtin(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let fn_name = if name == self.known_symbols().println {
            "println"
        } else {
            "print"
        };

        let args = self.body_rir_ref().call_args(args).to_vec();
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 1,
                    found: args.len(),
                },
                span,
            )
            .with_help(format!("`{fn_name}` takes exactly one text argument")));
        }
        self.validate_explicit_call_modes(&args, std::iter::once(RirParamMode::Normal))?;
        let arg_value = args.first().unwrap().value;

        // Borrow the argument (like a `+` operand): analyze in projection mode
        // and cancel any move marker so a variable operand is not consumed and
        // a temporary is still dropped by its owner at statement end.
        let arg_result = self.analyze_inst_for_projection(air, arg_value, ctx)?;
        self.cancel_recorded_move(air, arg_result.air_ref);

        if !self.is_strbuf(arg_result.ty)
            && !self.is_str_like(arg_result.ty)
            && !arg_result.ty.is_error()
        {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "text".to_string(),
                    found: arg_result
                        .ty
                        .safe_name_with_pool(Some(self.body_type_pool())),
                },
                self.body_rir_ref().get(arg_value).span,
            )
            .with_help(format!("`{fn_name}` takes StrBuf, str, or Str(N) text")));
        }

        let source_strbuf = arg_result.ty.as_struct().is_some_and(|struct_id| {
            self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        let shared_text = source_strbuf || self.is_str_like(arg_result.ty);
        debug_assert!(shared_text || arg_result.ty.is_error());
        let operation = if name == self.known_symbols().println {
            rue_builtins::TextBuiltinOperation::PrintlnView
        } else {
            rue_builtins::TextBuiltinOperation::PrintView
        };
        let runtime_helper = operation
            .runtime_helper()
            .expect("print builtin must map to a runtime helper");
        let call_name = self.intern_body_symbol(runtime_helper.symbol)?;

        // A StrBuf source reads its `{ptr, len}` prefix through the trusted
        // accessors and passes them as separate scalars (the `*Projected`
        // helpers), so print never depends on StrBuf's field layout
        // (RUE-1066). Since `source_strbuf` already covers every StrBuf value,
        // the remaining case is a `str`/`Str(N)` view, forwarded by value to
        // the `*Aggregate` helpers.
        let extra_data = if source_strbuf {
            let (ptr, len, temp_scope) =
                self.project_strbuf_text_fields(air, arg_result.air_ref, arg_result.ty, span, ctx)?;
            let args = vec![
                AirCallArg {
                    value: ptr,
                    mode: AirArgMode::Normal,
                },
                AirCallArg {
                    value: len,
                    mode: AirArgMode::Normal,
                },
            ];
            (args, temp_scope)
        } else {
            (
                vec![AirCallArg {
                    value: arg_result.air_ref,
                    mode: AirArgMode::Normal,
                }],
                Vec::new(),
            )
        };
        let (extra_data, temp_scope) = extra_data;
        let call_ref = air.add_call(
            Some(if name == self.known_symbols().println {
                if source_strbuf {
                    crate::RuntimeCallKind::StrPrintlnProjected
                } else {
                    crate::RuntimeCallKind::StrPrintlnAggregate
                }
            } else if source_strbuf {
                crate::RuntimeCallKind::StrPrintProjected
            } else {
                crate::RuntimeCallKind::StrPrintAggregate
            }),
            call_name,
            &extra_data,
            Type::UNIT,
            span,
        )?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, call_ref, Type::UNIT, span, temp_scope)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            arg_result.continues,
        ))
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
        let reachable_edges_after_lhs = ctx.loop_break_stack.clone();
        let rhs_result = self.analyze_inst(air, rhs, ctx)?;
        if !lhs_result.continues {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_lhs);
        }

        if !lhs_result.continues || !rhs_result.continues {
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: lhs_result.ty,
                span,
            });
            return Ok(AnalysisResult::with_continues(
                air_ref,
                lhs_result.ty,
                false,
            ));
        }

        // Verify the type is integer (HM should have enforced this, but check anyway)
        if !lhs_result.ty.is_integer() && !lhs_result.ty.is_error() && !lhs_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: lhs_result
                        .ty
                        .safe_name_with_pool(Some(self.body_type_pool())),
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
        let lhs_is_literal = matches!(
            self.body_rir_ref().get(lhs).data,
            InstData::StringConst { .. }
        );
        let (lhs_result, rhs_result) = if lhs_is_literal {
            let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
            let reachable_edges_after_rhs = ctx.loop_break_stack.clone();
            let expected = (self.is_strbuf(rhs_result.ty) || self.is_str_like(rhs_result.ty))
                .then_some(rhs_result.ty);
            let lhs_result = ctx.with_expected_type(expected, |ctx| {
                self.analyze_inst_for_projection(air, lhs, ctx)
            })?;
            if !rhs_result.continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_rhs);
            }
            (lhs_result, rhs_result)
        } else {
            let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
            let reachable_edges_after_lhs = ctx.loop_break_stack.clone();
            let expected = (self.is_strbuf(lhs_result.ty) || self.is_str_like(lhs_result.ty))
                .then_some(lhs_result.ty);
            let rhs_result = ctx.with_expected_type(expected, |ctx| {
                self.analyze_inst_for_projection(air, rhs, ctx)
            })?;
            if !lhs_result.continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_lhs);
            }
            (lhs_result, rhs_result)
        };
        let lhs_type = lhs_result.ty;

        if !lhs_result.continues || !rhs_result.continues {
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::BOOL,
                span,
            });
            return Ok(AnalysisResult::with_continues(air_ref, Type::BOOL, false));
        }

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
            self.validate_equality_operand_type(lhs_type, self.body_rir_ref().get(lhs).span)?;
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.safe_name_with_pool(Some(self.body_type_pool())),
                },
                self.body_rir_ref().get(lhs).span,
            ));
        }

        if allow_bool && self.is_sentinel_lookup_test(lhs, rhs) {
            ctx.warnings.push(
                CompileWarning::new(WarningKind::SentinelLookup, span).with_help(
                    "preserve absence in the type system: use an Option-returning lookup \
                     such as `get`, `index_of`, or `find`, then match on `Some`/`None`",
                ),
            );
        }

        let comparison = make_data(lhs_result.air_ref, rhs_result.air_ref);
        if matches!(comparison, AirInstData::Eq(..) | AirInstData::Ne(..))
            && let Some(result) = self.try_prepare_aggregate_equality(
                air,
                &comparison,
                lhs_result,
                rhs_result,
                span,
                ctx,
            )?
        {
            return Ok(result);
        }

        let air_ref = air.add_inst(AirInst {
            data: comparison,
            ty: Type::BOOL,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    fn is_sentinel_lookup_test(&self, lhs: InstRef, rhs: InstRef) -> bool {
        self.integer_sentinel(rhs)
            .is_some_and(|sentinel| self.is_get_or_with_sentinel(lhs, sentinel))
            || self
                .integer_sentinel(lhs)
                .is_some_and(|sentinel| self.is_get_or_with_sentinel(rhs, sentinel))
    }

    fn is_get_or_with_sentinel(&self, inst: InstRef, sentinel: IntegerSentinel) -> bool {
        let InstData::MethodCall { method, args, .. } = &self.body_rir_ref().get(inst).data else {
            return false;
        };
        if self.body_interner().resolve(method) != "get_or" {
            return false;
        }
        let args = self.body_rir_ref().call_args(args).to_vec();
        let mut args = args.iter();
        let (Some(_index), Some(default), None) = (args.next(), args.next(), args.next()) else {
            return false;
        };
        self.integer_sentinel(default.value) == Some(sentinel)
    }

    fn integer_sentinel(&self, inst: InstRef) -> Option<IntegerSentinel> {
        match self.body_rir_ref().get(inst).data {
            InstData::IntConst(u64::MAX) => Some(IntegerSentinel::UnsignedMax),
            InstData::Sub { lhs, rhs }
                if matches!(self.body_rir_ref().get(lhs).data, InstData::IntConst(0))
                    && matches!(self.body_rir_ref().get(rhs).data, InstData::IntConst(1)) =>
            {
                Some(IntegerSentinel::NegativeOne)
            }
            _ => None,
        }
    }

    /// Check if an RIR instruction is a VarRef to a comptime type variable.
    ///
    /// This is used when validating comptime arguments to detect variables
    /// that hold comptime type values (e.g., `let P = Point(); ... Line(P)`).
    pub(crate) fn is_comptime_type_var(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> bool {
        if let InstData::VarRef { name, .. } = &self.body_rir_ref().get(inst_ref).data {
            ctx.comptime_type_vars.contains_key(name)
        } else {
            false
        }
    }

    /// Resolve the type of a place without emitting AIR or recording a move.
    pub(crate) fn peek_place_type(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> Option<Type> {
        match &self.body_rir_ref().get(inst_ref).data {
            InstData::VarRef { name, .. } => {
                // An accessor-inline place alias (`self` inside an inlined
                // accessor body, ADR-0062) shadows caller bindings.
                if let Some(alias) = ctx.place_aliases.get(name) {
                    return Some(
                        alias
                            .projections
                            .last()
                            .map(|p| p.result_type)
                            .unwrap_or(alias.base_type),
                    );
                }
                if let Some(local) = ctx.locals.get(name) {
                    return Some(local.ty);
                }
                ctx.param(*name).map(|param| param.ty)
            }
            // A `-> borrow T` or `-> inout T` accessor call is a place of its element type
            // (ADR-0062); any other method call is not a place.
            InstData::MethodCall {
                receiver, method, ..
            } => {
                let base_ty = self.peek_place_type(*receiver, ctx)?;
                let struct_id = base_ty.as_struct()?;
                let info = self.call_facts().call_method_info(struct_id, *method)?;
                (info.returns_borrow || info.returns_inout).then_some(info.return_type)
            }
            InstData::FieldGet { base, field } => {
                let base_ty = self.peek_place_type(*base, ctx)?;
                let struct_id = base_ty.as_struct()?;
                let struct_def = self.body_type_pool().struct_def(struct_id);
                let field_name_str = self.body_interner().resolve(field);
                let (_field_index, struct_field) = struct_def.find_field(field_name_str)?;
                Some(struct_field.ty)
            }
            InstData::IndexGet { base, .. } => {
                let base_ty = self.peek_place_type(*base, ctx)?;
                let array_id = base_ty.as_array()?;
                let (elem_type, _len) = self.body_type_pool().array_def(array_id);
                Some(elem_type)
            }
            _ => None,
        }
    }
}
