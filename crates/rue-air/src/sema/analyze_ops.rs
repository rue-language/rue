//! Instruction category analysis methods.
//!
//! This module contains the per-category analysis methods extracted from `analyze_inst`.
//! Each category method handles a specific group of related RIR instructions:
//!
//! - [`analyze_literal`] - Integer, boolean, string, and unit constants
//! - [`analyze_unary_op`] - Negation, logical NOT, bitwise NOT
//!
//! Control-flow expressions are owned by the sibling `control_flow` module.
//! Aggregate construction and member operations are owned by the sibling
//! `aggregates` module. Place and ownership behavior remains canonical in
//! `analysis::ownership`.
//!
//! - [`analyze_call_ops`] - Call and MethodCall
//! - [`analyze_intrinsic_ops`] - Intrinsic, TypeIntrinsic
//! - [`analyze_decl_noop`] - DropFnDecl (declarations that produce Unit)
//!
//! Binary operations (arithmetic, comparison, logical, bitwise) are handled
//! by helpers in `sema::analysis::builtin_ops`:
//! - `analyze_binary_arith` - Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
//! - `analyze_comparison` - Eq, Ne, Lt, Gt, Le, Ge
//! - Logical And/Or are simple enough to remain inline

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind, OptionExt};
use rue_rir::{InstData, InstRef, RirParamMode};

use crate::sema::context::ConstValue;
use rue_span::Span;

use super::context::{AnalysisContext, AnalysisResult};
use super::{BodySema, FunctionInfo};
use crate::inst::{Air, AirCallArg, AirInst, AirInstData, AirRef};
use crate::types::{Type, TypeKind};

// ============================================================================

impl<'a> BodySema<'a> {
    // ========================================================================
    // Literals: IntConst, BoolConst, StringConst, UnitConst
    // ========================================================================

    /// Analyze a literal constant instruction.
    ///
    /// Handles: IntConst, BoolConst, StringConst, UnitConst
    pub(crate) fn analyze_literal(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::IntConst(value) => {
                // Get the type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "integer literal")?;

                // Check if the literal value fits in the target type's range
                if !ty.literal_fits(*value) {
                    return Err(CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: *value,
                            ty: ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        inst.span,
                    ));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::BoolConst(value) => {
                let ty = Type::BOOL;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::StringConst(symbol) => {
                // A string literal is static-backed: its bytes live in `.rodata`
                // (the local string table), and the value is the fat pointer to
                // them. When a `str` is expected (ADR-0043 Phase 3, RUE-324) the
                // literal materializes as the 2-word `str` `{ptr, len}`; the same
                // `StringConst` AIR node lowers to only the ptr+len words there
                // (the cap word is dropped in codegen). Otherwise it is the
                // 3-word heap `String` as before.
                let ty = if let Some(expected) = ctx
                    .expected_type
                    .filter(|ty| self.is_str_like(*ty) || self.is_strbuf(*ty))
                {
                    expected
                } else {
                    // HM inference carries the preview-dependent default and
                    // any explicit `StrBuf` context. Use that resolved type as
                    // the fallback so AIR materialization cannot drift from
                    // the canonical inference path.
                    Self::get_resolved_type(ctx, inst_ref, inst.span, "string literal")?
                };
                // Add string to the local per-function string table.
                let string_content = self.interner.resolve(&*symbol).to_string();

                // Capacity-fits legality (ADR-0043 Phase 5, RUE-326): a string
                // literal materialized as a fixed `Str(N)` must fit — its UTF-8
                // byte length must be ≤ N — else it is a clean compile error
                // (E0492). `str` (no capacity) never triggers this.
                if let Some(capacity) = self.str_fixed_capacity(ty) {
                    let byte_len = string_content.len() as u64;
                    if byte_len > capacity {
                        return Err(CompileError::new(
                            ErrorKind::StrFixedCapacityExceeded { capacity, byte_len },
                            inst.span,
                        ));
                    }
                }

                let local_string_id = ctx.add_local_string(string_content);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::StringConst(local_string_id),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::UnitConst => {
                let ty = Type::UNIT;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_literal called with non-literal instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    // ========================================================================
    // Unary operations: Neg, Not, BitNot
    // ========================================================================

    /// Analyze a unary operator instruction.
    ///
    /// Handles: Neg, Not, BitNot
    pub(crate) fn analyze_unary_op(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Neg { operand } => {
                // Get the resolved type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "negation operator")?;

                // Unary `-` requires a signed integer operand (i8/i16/i32/i64/
                // isize). Reject unsigned integers (no negative range), bool, and
                // every other non-signed type. `<error>`/`never` pass through so a
                // prior error isn't masked by a spurious second diagnostic.
                if !ty.is_signed() && !ty.is_error() && !ty.is_never() {
                    let note = if ty.is_unsigned() {
                        "unsigned values cannot be negated"
                    } else {
                        "unary `-` requires a signed integer operand (i8, i16, i32, i64, isize)"
                    };
                    return Err(CompileError::new(
                        ErrorKind::CannotNegate(ty.safe_name_with_pool(Some(&self.type_pool))),
                        inst.span,
                    )
                    .with_note(note));
                }

                // Special case: negating a literal that equals |MIN| for signed types.
                let operand_inst = self.rir.get(*operand);
                if let InstData::IntConst(value) = &operand_inst.data {
                    // Check if this value, when negated, fits in the target signed type
                    if ty.negated_literal_fits(*value) && !ty.literal_fits(*value) {
                        // This is the MIN value case - store the MIN value directly.
                        let neg_value = match ty.kind() {
                            TypeKind::I8 => (i8::MIN as i64) as u64,
                            TypeKind::I16 => (i16::MIN as i64) as u64,
                            TypeKind::I32 => (i32::MIN as i64) as u64,
                            TypeKind::I64 => i64::MIN as u64,
                            _ => unreachable!(),
                        };
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(neg_value),
                            ty,
                            span: inst.span,
                        });
                        return Ok(AnalysisResult::new(air_ref, ty));
                    }
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Neg(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Not { operand } => {
                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Not(operand_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            InstData::BitNot { operand } => {
                // Get the resolved type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "bitwise NOT operator")?;

                // Bitwise NOT operates on integer types only
                if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "integer type".to_string(),
                            found: ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        inst.span,
                    ));
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BitNot(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_unary_op called with non-unary instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    // ========================================================================
    // Logical operations: And, Or
    // ========================================================================

    /// Analyze a logical operator instruction.
    ///
    /// Handles: And, Or
    pub(crate) fn analyze_logical_op(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::And { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            InstData::Or { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_logical_op called with non-logical instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    // ========================================================================
    // Call operations: Call, MethodCall
    // ========================================================================

    /// Analyze a call operation instruction.
    ///
    /// Handles: Call and MethodCall.
    pub(crate) fn analyze_call_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        // A call has a declared result type; an expectation on that result
        // must not become the context of its receiver or arguments. The
        // callee's parameter analyzer establishes a fresh context for each
        // operand instead. Keep the isolation at this shared dispatch so it
        // covers direct, module, method, associated, builtin, and enum calls.
        ctx.with_expected_type(None, |ctx| match &inst.data {
            InstData::Call { name, args } => self.analyze_call(air, *name, args, inst.span, ctx),

            InstData::MethodCall {
                receiver,
                method,
                args,
            } => self.analyze_method_call(air, *receiver, *method, args, inst.span, ctx),

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_call_ops called with non-call instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        })
    }

    /// Analyze a function call.
    ///
    /// Also used by the module-member-call path for callees with comptime
    /// parameters, which must go through generic specialization (RUE-166).
    pub(crate) fn analyze_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let source_name = name;
        let mut name = name;
        let mut resolved_alias = false;
        if let Some(const_info) = self.resolve_const_info_in_file(name, span.file_id).cloned()
            && let Some(callee) = const_info.value.as_function()
        {
            let alias_name = self.interner.resolve(&name).to_string();
            self.check_unqualified_visibility(
                "constant",
                &alias_name,
                const_info.span.file_id,
                const_info.is_pub,
                span,
            )?;
            self.record_body_named_dependency(super::NamedConstDependencyTargetEvent::ValueConst {
                file: const_info.span.file_id.index(),
                name: alias_name,
            });
            name = callee;
            resolved_alias = true;
        }

        let local_name = (!resolved_alias)
            .then(|| self.resolve_function_name_local(name, span.file_id))
            .flatten();
        if let Some(local_name) = local_name {
            name = local_name;
        }

        // `print(s)` / `println(s)` are builtin free functions (RUE-1), not
        // user-defined ones: intercept them here before the function lookup,
        // but only when the program hasn't shadowed the name with its own
        // `fn print`/`fn println` (a user definition wins, keeping these names
        // unreserved).
        if !resolved_alias
            && local_name.is_none()
            && (source_name == self.known.print || source_name == self.known.println)
        {
            return self.analyze_print_builtin(air, source_name, args, span, ctx);
        }

        if !resolved_alias && local_name.is_none() {
            let fn_name_str = self.interner.resolve(&source_name).to_string();
            return Err(CompileError::new(
                ErrorKind::UndefinedFunction(fn_name_str),
                span,
            ));
        }

        // Look up the function
        let source_name = self.source_function_name(name);
        let fn_name_str = self.interner.resolve(&source_name).to_string();
        let fn_info = self
            .functions
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;
        let fn_info = fn_info.clone();

        self.analyze_resolved_function_call(air, name, fn_info, args, span, ctx, true)
    }

    /// Analyze a call after the source-level callee has already been resolved
    /// to an internal function key.
    ///
    /// Unqualified source calls enter through [`Self::analyze_call`], which
    /// performs local alias resolution, module-local name canonicalization, and
    /// builtin interception before reaching this helper. Module-member calls
    /// such as `std.option.Option(i64)` resolve and validate their member in
    /// `analyze_module_member_call_impl`; generic members use this helper
    /// directly so module-qualified type constructors do not re-enter
    /// unqualified source-name lookup.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_resolved_function_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        fn_info: FunctionInfo,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
        check_unqualified_visibility: bool,
    ) -> CompileResult<AnalysisResult> {
        let source_name = self.source_function_name(name);
        let fn_name_str = self.interner.resolve(&source_name).to_string();

        // Visibility (E0460, RUE-37/RUE-180): an unqualified call must not
        // reach a private function defined in another directory — privacy is
        // uniform in every multi-file compilation (spec 10.3:7). The lookup
        // has already selected a declaration using the reference file.
        if check_unqualified_visibility {
            self.check_unqualified_visibility(
                "function",
                &fn_name_str,
                fn_info.file_id,
                fn_info.is_pub,
                span,
            )?;
        }

        // An `unchecked fn` may only be called inside a `checked` block
        // (spec 9.1:1). The callee's body is analyzed like any other function;
        // it is the *call site* that must be in an unchecked context.
        if fn_info.is_unchecked && ctx.checked_depth == 0 {
            return Err(CompileError::new(
                ErrorKind::UncheckedOpRequiresChecked {
                    what: format!("calling unchecked function `{fn_name_str}`"),
                },
                span,
            )
            .with_help("wrap the call in a `checked { ... }` block"));
        }

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(name);

        // Get parameter data from the arena
        let param_types = self.param_arena.types(fn_info.params);
        let param_modes = self.param_arena.modes(fn_info.params);
        let param_comptime = self.param_arena.comptime(fn_info.params);
        let param_names = self.param_arena.names(fn_info.params);

        let args = self.rir.call_args(args);
        // Check argument count
        if args.len() != param_types.len() {
            let expected = param_types.len();
            let found = args.len();
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount { expected, found },
                span,
            ));
        }

        // Source argument modes must match the declaration exactly before an
        // explicit by-ref marker is interpreted as a place/loan operation.
        self.validate_explicit_call_modes(&args, param_modes.iter().copied())?;

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Extract info before any mutable borrow
        let is_generic = fn_info.is_generic;
        let param_types = param_types.to_vec();
        let param_comptime = param_comptime.to_vec();
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let param_names = param_names.to_vec();
        let param_modes = param_modes.to_vec();
        let base_return_type = fn_info.return_type;
        let fn_body = fn_info.body;

        // `-> type` functions with no runtime parameters reduce immediately,
        // but their arguments still obey the ordinary comptime contract. Build
        // the maps through the propagating evaluator before reducing the body;
        // otherwise a constructor that ignores a wrong-kind/private argument
        // can accidentally accept it.
        let all_params_comptime = param_comptime.iter().all(|&flag| flag);
        if self.function_returns_type(&fn_info) && (args.is_empty() || all_params_comptime) {
            let mut type_subst = std::collections::HashMap::new();
            let mut value_subst = std::collections::HashMap::new();
            for (i, is_comptime) in param_comptime.iter().enumerate() {
                if !*is_comptime {
                    continue;
                }
                let value = self.evaluate_const_in_fn(args.get(i).unwrap().value, ctx)?;
                if param_comptime_type[i] {
                    match value {
                        Some(ConstValue::Type(ty)) => {
                            type_subst.insert(param_names[i], ty);
                        }
                        Some(ConstValue::Unit) => {
                            type_subst.insert(param_names[i], Type::UNIT);
                        }
                        Some(_) => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime type parameter must be a type literal"
                                        .to_string(),
                                },
                                self.rir.get(args.get(i).unwrap().value).span,
                            ));
                        }
                        None => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeArgNotConst {
                                    param_name: self.interner.resolve(&param_names[i]).to_string(),
                                },
                                self.rir.get(args.get(i).unwrap().value).span,
                            ));
                        }
                    }
                } else if let Some(value) = value {
                    value_subst.insert(param_names[i], value);
                } else {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeArgNotConst {
                            param_name: self.interner.resolve(&param_names[i]).to_string(),
                        },
                        self.rir.get(args.get(i).unwrap().value).span,
                    ));
                }
            }
            // Try to evaluate the function body at compile time. A hard error
            // raised while reducing the constructor (e.g. an unbounded
            // self-recursive `-> type` function exceeding the comptime depth
            // limit, RUE-261) must surface as its real diagnostic (E1200)
            // rather than being swallowed into a downstream link error, so use
            // the propagating reduction entry point.
            if let Some(ConstValue::Type(ty)) = self
                .reduce_type_ctor_body(name, &type_subst, &value_subst)
                .map_err(|e| Self::label_ctor_instantiation_site(e, span))?
            {
                // Success! Return a TypeConst instruction instead of a runtime call
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
            // If we can't evaluate at compile time, fall through to runtime call
            // (which will fail at link time, but gives a better error experience)
        }

        // Check that comptime parameters receive compile-time constant values
        let has_comptime_params = param_comptime.iter().any(|&c| c);
        if has_comptime_params {
            // Validate each comptime parameter receives a compile-time constant
            for (i, (&is_comptime, arg)) in param_comptime.iter().zip(args.iter()).enumerate() {
                if is_comptime {
                    // Try to evaluate the argument at compile time. A direct
                    // reference to a comptime parameter of the *current*
                    // function also counts: its value is compile-time known
                    // at every call site, so it may be forwarded (spec 4.14:5).
                    let is_comptime_known = self.evaluate_const_in_fn(arg.value, ctx)?.is_some()
                        || self.is_comptime_type_var(arg.value, ctx)
                        || self.is_comptime_param_forward(arg.value, ctx);
                    if !is_comptime_known {
                        let param_name = self.interner.resolve(&param_names[i]).to_string();
                        // A module-qualified member-access value path is
                        // compile-time known but not yet folded in argument
                        // position (RUE-948): name that limitation and the
                        // file-level `const` workaround instead of the generic
                        // "requires a compile-time known value" wording.
                        let help = self
                            .comptime_arg_member_access_help(arg.value, ctx)
                            .unwrap_or_else(|| {
                                format!(
                                    "parameter '{}' is declared as 'comptime' and requires a compile-time known value",
                                    param_name
                                )
                            });
                        return Err(CompileError::new(
                            ErrorKind::ComptimeArgNotConst {
                                param_name: param_name.clone(),
                            },
                            self.rir.get(arg.value).span,
                        )
                        .with_help(help));
                    }
                }
            }
        }

        // Analyze all arguments. Slice parameters (ADR-0043, RUE-322) coerce a
        // `borrow arr` argument into a by-value fat pointer here.
        let air_args =
            self.analyze_call_args_coerced(air, args.values(), &param_types, &param_modes, ctx)?;

        // Handle generic function calls differently
        if is_generic {
            // Separate type arguments and comptime value arguments from
            // runtime arguments
            let mut type_args: Vec<Type> = Vec::new();
            let mut value_args: Vec<ConstValue> = Vec::new();
            let mut runtime_args: Vec<AirCallArg> = Vec::new();
            let mut type_subst: std::collections::HashMap<Spur, Type> =
                std::collections::HashMap::new();
            // Comptime VALUE parameters (`comptime N: i32`) map to their
            // captured constant so a runtime param type mentioning one — an
            // array length `arr: [i32; N]` — resolves at this call (RUE-16).
            let mut value_subst: std::collections::HashMap<Spur, ConstValue> =
                std::collections::HashMap::new();

            for (i, (air_arg, is_comptime)) in
                air_args.iter().zip(param_comptime.iter()).enumerate()
            {
                if *is_comptime {
                    // The source declaration distinguishes a type parameter
                    // from a value parameter whose semantic type is deferred.
                    if param_comptime_type[i] {
                        // This is a TYPE parameter - expect a TypeConst instruction
                        let inst = air.get(air_arg.value);
                        if let AirInstData::TypeConst(ty) = &inst.data {
                            type_args.push(*ty);
                            // Record the substitution: param_name -> concrete_type
                            type_subst.insert(param_names[i], *ty);
                        } else if matches!(inst.data, AirInstData::UnitConst) {
                            // `()` in a `comptime T: type` position is the unit
                            // TYPE (RUE-565); the declared parameter kind
                            // disambiguates it from the unit value. Mirrors the
                            // ConstValue::Unit arm in the reduction path above.
                            type_args.push(Type::UNIT);
                            type_subst.insert(param_names[i], Type::UNIT);
                        } else {
                            // Not a type - this is an error for type parameters
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime type parameter must be a type literal"
                                        .to_string(),
                                },
                                span,
                            ));
                        }
                    } else {
                        // This is a VALUE parameter (e.g., comptime n: i32).
                        // Capture its concrete value: the callee is
                        // specialized per value so its body sees the value as
                        // a compile-time constant (RUE-166). The argument is
                        // still also passed at runtime (value parameters are
                        // not erased from the signature).
                        match self.try_evaluate_const_in_fn(args.get(i).unwrap().value, ctx) {
                            Some(const_val) => {
                                value_args.push(const_val);
                                value_subst.insert(param_names[i], const_val);
                            }
                            None => {
                                let param_name = self.interner.resolve(&param_names[i]).to_string();
                                let arg_value = args.get(i).unwrap().value;
                                // RUE-948: a module-member value path is
                                // compile-time known but unfolded here; point
                                // at the file-level `const` workaround.
                                let help = self
                                    .comptime_arg_member_access_help(arg_value, ctx)
                                    .unwrap_or_else(|| {
                                        format!(
                                            "parameter '{}' is declared as 'comptime' and requires \
                                             a compile-time known value",
                                            param_name
                                        )
                                    });
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeArgNotConst {
                                        param_name: param_name.clone(),
                                    },
                                    self.rir.get(arg_value).span,
                                )
                                .with_help(help));
                            }
                        }
                        runtime_args.push(air_arg.clone());
                    }
                } else {
                    runtime_args.push(air_arg.clone());
                }
            }

            // Type-check the runtime arguments against their (substituted)
            // parameter types. Generic calls bypass the inference-based argument
            // checking when the type parameter isn't resolvable during constraint
            // generation, so this is the check that rejects e.g. passing a `B`
            // where `T == A` - without it the callee would read B-shaped fields
            // out of an A-sized allocation (RUE-99, RUE-73).
            for (i, (air_arg, &is_comptime)) in
                air_args.iter().zip(param_comptime.iter()).enumerate()
            {
                let declared = param_types[i];
                if is_comptime && param_comptime_type[i] {
                    // The comptime type argument itself - already validated above.
                    continue;
                }
                let expected = self.resolve_substituted_param_type(
                    &fn_info,
                    i,
                    declared,
                    &type_subst,
                    &value_subst,
                )?;
                let found = air.get(air_arg.value).ty;
                if found != expected
                    && !found.is_error()
                    && !found.is_never()
                    && !expected.is_error()
                {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: expected.safe_name_with_pool(Some(&self.type_pool)),
                            found: found.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        self.rir.get(args.get(i).unwrap().value).span,
                    ));
                }
            }

            // Determine the actual return type by substituting type parameters.
            // Handles bare type parameters (`-> T`), composites mentioning one
            // (`-> [T; 3]`, RUE-172), and the literal `type` return (which
            // resolves back to COMPTIME_TYPE and is comptime-evaluated below).
            let return_type =
                self.resolve_substituted_return_type(&fn_info, &type_subst, &value_subst)?;

            // Special case: functions that return `type` (not a type parameter) with only comptime args
            // can be fully evaluated at compile time to produce a concrete anonymous struct type.
            // This handles cases like:
            //   - `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`
            //   - `fn FixedBuffer(comptime N: i32) -> type { struct { fn capacity(self) -> i32 { N } } }`
            let all_params_comptime = param_comptime.iter().all(|&c| c);
            if return_type == Type::COMPTIME_TYPE && all_params_comptime {
                // The return type is literally `type`, not a type parameter that was substituted.
                // Try to evaluate the function body at compile time with type substitutions.
                // Also build value_subst from comptime VALUE parameters (e.g., comptime N: i32)
                let mut value_subst: std::collections::HashMap<Spur, ConstValue> =
                    std::collections::HashMap::new();
                for (i, is_comptime) in param_comptime.iter().enumerate() {
                    if *is_comptime && !param_comptime_type[i] {
                        // This is a comptime VALUE parameter - extract its const value
                        // (evaluated in the calling function's context)
                        if let Some(const_val) =
                            self.try_evaluate_const_in_fn(args.get(i).unwrap().value, ctx)
                        {
                            value_subst.insert(param_names[i], const_val);
                        }
                    }
                }
                if let Some(ConstValue::Type(ty)) =
                    self.try_evaluate_const_with_subst(fn_body, &type_subst, &value_subst)
                {
                    // Success! Return a TypeConst instruction instead of a runtime call
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                // If we can't evaluate at compile time, fall through to the error below
                // (we can't have a runtime call that returns `type`)
            }

            let air_ref = air.add_call_generic(
                name,
                &type_args,
                &value_args,
                &runtime_args,
                return_type,
                span,
            )?;
            Ok(AnalysisResult::new(air_ref, return_type))
        } else {
            // Regular non-generic call
            let return_type = base_return_type;

            // Encode call args into extra array
            let air_ref = air.add_call(None, name, &air_args, return_type, span)?;
            Ok(AnalysisResult::new(air_ref, return_type))
        }
    }

    /// Analyze a method call.
    ///
    /// Handles user-defined and builtin methods through the call-analysis
    /// category.
    fn analyze_method_call(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        method: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.analyze_method_call_impl(air, receiver, method, args, span, ctx)
    }

    /// Resolve a path/pattern enum type name that may be a comptime
    /// type-variable binding (`let O = Option(i32); O::Some(..)`), falling
    /// back to the named-enum table. Returns `(enum_id, via_comptime_binding)`,
    /// or `None` if the name is not an enum. When `via_comptime_binding` is
    /// true the enum arrived through a `let` binding (an anonymous enum from a
    /// comptime type function), so privacy does not apply — mirroring how the
    /// struct-literal / annotation paths treat comptime type variables as
    /// privacy-exempt (RUE-6 phase 2).
    pub(crate) fn resolve_enum_type_name(
        &self,
        type_name: Spur,
        ctx: &AnalysisContext,
    ) -> Option<(crate::types::EnumId, bool)> {
        if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            return ty.as_enum().map(|id| (id, true));
        }
        if let Some(info) = self
            .constants_by_file_name
            .get(&(ctx.current_file_id, type_name))
            && let ConstValue::Type(ty) = info.value
        {
            return ty.as_enum().map(|id| (id, true));
        }
        self.enums_by_file_name
            .get(&(ctx.current_file_id, type_name))
            .copied()
            .or_else(|| self.resolve_builtin_enum_name(type_name))
            .map(|id| (id, false))
    }

    /// Resolve a `Type.assoc()` / `Type { .. }` struct type name that may be a
    /// comptime type-variable binding (`let P = Point(i32)`) or a module-level
    /// `const` binding (`const P = Point(i32)`), falling back to the named-struct
    /// table and builtins. Returns `(struct_id, via_binding)`, or `None` if the
    /// name is not a struct. `via_binding` is true when the struct arrived
    /// through a `let`/`const` binding (an anonymous struct from a comptime type
    /// function), so privacy does not apply — the exact mirror of
    /// `resolve_enum_type_name` for the struct side (RUE-595). Without the
    /// `constants_by_file_name` arm a module-`const`-bound struct type resolved
    /// as a type namespace nowhere, so `const C = Counter(i32); C.zero()` failed
    /// (E0413) and `const P = Point(i32); P { .. }` failed (E0204) while the
    /// enum-bound and local-`let`-bound forms worked.
    pub(crate) fn resolve_struct_type_name(
        &self,
        type_name: Spur,
        ctx: &AnalysisContext,
    ) -> Option<(crate::types::StructId, bool)> {
        if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            return ty.as_struct().map(|id| (id, true));
        }
        if let Some(info) = self
            .constants_by_file_name
            .get(&(ctx.current_file_id, type_name))
            && let ConstValue::Type(ty) = info.value
        {
            return ty.as_struct().map(|id| (id, true));
        }
        self.structs_by_file_name
            .get(&(ctx.current_file_id, type_name))
            .copied()
            .or_else(|| self.resolve_builtin_struct_name(type_name))
            .map(|id| (id, false))
    }

    /// Analyze an associated function call.
    ///
    /// Resolves and analyzes an associated-function call through the
    /// call-analysis category.
    pub(crate) fn analyze_assoc_fn_call(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Enum tuple-variant construction: `Shape::Circle(5)` (RUE-221), and
        // its generic form `O::Some(5)` where `O` is a comptime type-variable
        // bound to `Option(i32)` (RUE-6 phase 2). If `type_name` resolves to an
        // enum whose variant is `function`, build an `EnumVariant` value
        // carrying the analyzed payload operands rather than dispatching to
        // associated-function resolution.
        if let Some((enum_id, via_comptime)) = self.resolve_enum_type_name(type_name, ctx) {
            let variant_name = self.interner.resolve(&function).to_string();
            let def = self.type_pool.enum_def(enum_id);
            if let Some(variant_index) = def.find_variant(&variant_name) {
                return self.analyze_enum_variant_construction(
                    air,
                    enum_id,
                    variant_index as u32,
                    type_name,
                    via_comptime,
                    args,
                    span,
                    ctx,
                );
            }
        }

        self.analyze_assoc_fn_call_impl(air, type_name, function, args, span, ctx, None)
    }

    /// Analyze construction of an enum tuple variant with a payload
    /// (`Shape.Circle(5)`), producing an `EnumVariant` AIR value (RUE-221).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_enum_variant_construction(
        &mut self,
        air: &mut Air,
        enum_id: crate::types::EnumId,
        variant_index: u32,
        type_name: Spur,
        privacy_exempt: bool,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let def = self.type_pool.enum_def(enum_id);
        let payload_types = def.variant_payload(variant_index as usize).to_vec();
        let variant_name = def.variants[variant_index as usize].clone();
        let enum_name = def.name.clone();

        // Visibility check, mirroring the bare-path `EnumVariant` handler
        // (E0460, privacy is uniform across item kinds). A comptime-bound enum
        // (`let O = Option(i32); O::Some(..)`) is exempt: the type value
        // arrived through a binding, not by naming the enum (privacy_exempt).
        if !privacy_exempt {
            self.check_unqualified_visibility(
                "enum",
                self.interner.resolve(&type_name),
                def.file_id,
                def.is_pub,
                span,
            )?;
        }

        let args = self.rir.call_args(args);

        // Arity check.
        if args.len() != payload_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: payload_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Enum payload fields are ordinary unmarked values. Reject explicit
        // `borrow`/`inout` before analyzing them or erasing their source modes.
        self.validate_explicit_call_modes(
            &args,
            std::iter::repeat_n(RirParamMode::Normal, args.len()),
        )?;

        // Analyze each payload argument and type-check against the declared
        // payload type (inference already constrained them; this is the final
        // legality check).
        let mut payload_refs: Vec<AirRef> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let expected = payload_types[i];
            let arg_result = ctx
                .with_expected_type(Some(expected), |ctx| self.analyze_inst(air, arg.value, ctx))?;
            let actual = arg_result.ty;
            if actual != expected && !actual.can_coerce_to(&expected) && actual != Type::ERROR {
                return Err(self.type_mismatch_error(
                    expected,
                    actual,
                    self.rir.get(arg.value).span,
                ));
            }
            payload_refs.push(arg_result.air_ref);
        }

        let ty = Type::new_enum(enum_id);

        // Suppress unused-variable warnings for names only used in messages.
        let _ = (&variant_name, &enum_name);

        let air_ref = air.add_enum_variant(enum_id, variant_index, &payload_refs, ty, span)?;
        Ok(AnalysisResult::new(air_ref, ty))
    }

    // ========================================================================
    // Intrinsic operations: Intrinsic, TypeIntrinsic
    // ========================================================================

    /// Analyze an intrinsic operation instruction.
    ///
    /// Handles: Intrinsic, TypeIntrinsic
    pub(crate) fn analyze_intrinsic_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);
        let result_expected = ctx.expected_type;

        match &inst.data {
            InstData::Intrinsic { name, args } => ctx.with_expected_type(None, |ctx| {
                self.analyze_intrinsic(air, inst_ref, *name, args, inst.span, result_expected, ctx)
            }),

            InstData::InternalIntrinsic { intrinsic, args } => ctx
                .with_expected_type(None, |ctx| {
                    self.analyze_internal_intrinsic_impl(air, *intrinsic, args, inst.span, ctx)
                }),

            InstData::TypeIntrinsic { name, type_arg } => {
                self.analyze_type_intrinsic(air, *name, *type_arg, inst.span, ctx)
            }

            InstData::OffsetOf { type_arg, field } => {
                self.analyze_offset_of(air, *type_arg, *field, inst.span)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_intrinsic_ops called with non-intrinsic instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a type intrinsic (@size_of, @align_of, @require_droppable,
    /// @require_trivially_droppable). Resolves the type argument through the
    /// current analysis context so a type parameter (`T` in a monomorphized
    /// generic method body, e.g. `ArrayBuf(T)::get`) binds to its concrete
    /// element type via `ctx.comptime_type_vars` (RUE-651).
    fn analyze_type_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        type_arg: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = self.interner.resolve(&name).to_string();
        let ty = self.resolve_type_with_ctx(type_arg, span, ctx)?;

        // `@require_droppable(T)` is the owning-container well-formedness gate
        // (RUE-388): it has no runtime value and evaluates to unit. It is
        // normally consumed at comptime while reducing a `-> type` constructor
        // body (see `Sema::check_require_droppable`), but handle it here too so
        // that if it ever reaches runtime analysis it performs the same
        // linear/destructor rejection instead of falling to E0700.
        if intrinsic_name == "require_droppable" {
            self.check_require_droppable(ty, span)?;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Const(0),
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // `@require_trivially_droppable(T)` is the by-copy-read gate (RUE-651).
        // Unlike `@require_droppable`, this one normally *does* reach runtime
        // analysis: it lives in `ArrayBuf(T)`'s `get`/`get_or` method bodies, and
        // demand-driven analysis (ADR-0045) monomorphizes those bodies with the
        // concrete element type only when a program actually calls a by-copy read.
        // If that `T` has drop glue, reading it by copy would alias its owned
        // resources (double-free), so reject it (E0711) and point the caller at
        // `pop`. It has no runtime value and evaluates to unit.
        if intrinsic_name == "require_trivially_droppable" {
            self.check_trivially_droppable(ty, span)?;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Const(0),
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Calculate the value through the checked layout query. Oversized
        // types produce E0906 rather than overflowing or truncating the slot
        // count (RUE-561).
        let value: u64 = match intrinsic_name.as_str() {
            "size_of" => {
                // Reject oversized layouts (E0906) before observing the
                // canonical layout authority, which owns the bytes-per-slot
                // conversion.
                self.require_layout_slots(ty, span)?;
                self.type_pool.provisional_layout(ty).size
            }
            "align_of" => {
                self.require_layout_slots(ty, span)?;
                self.type_pool.provisional_layout(ty).alignment
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
                    span,
                ));
            }
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Const(value),
            ty: Type::I32,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::I32))
    }

    /// Analyze `@offset_of(T, field)` (RUE-301): the compile-time byte offset of
    /// `field` within struct type `T`.
    ///
    /// The offset comes from the canonical layout authority
    /// (`struct_field_offset`, spec 3.6), the same query code generation
    /// addresses fields through, so `@offset_of(T, f)`, `@field_ptr(s.f)`, and
    /// direct `s.f` access agree by construction. The result is a comptime-known
    /// `u64`, mirroring Rust's `core::mem::offset_of!` (return type) and
    /// `@size_of`/`@align_of` (which likewise fold to a `Const` at analysis
    /// time).
    fn analyze_offset_of(
        &mut self,
        air: &mut Air,
        type_arg: Spur,
        field: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let ty = self.resolve_type(type_arg, span)?;

        // `@offset_of` is only meaningful for a struct type: only structs have
        // named fields. A non-struct operand is the same error class as `.f`
        // on a non-struct (E0428).
        let struct_id = match ty.as_struct() {
            Some(id) => id,
            None => {
                if ty.is_error() {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(0),
                        ty: Type::U64,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::U64));
                }
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: self.format_type_name(ty),
                    },
                    span,
                ));
            }
        };

        let struct_def = self.type_pool.struct_def(struct_id);
        let field_name_str = self.interner.resolve(&field);
        let field_index = match struct_def.find_field(field_name_str) {
            Some((index, _)) => index,
            None => {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.to_string(),
                    },
                    span,
                ));
            }
        };

        let byte_offset = self
            .type_pool
            .provisional_struct_field_offset(struct_id, field_index as u32);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Const(byte_offset),
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze an intrinsic call.
    ///
    /// Dispatches the intrinsic to the corresponding analysis category.
    fn analyze_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args: &rue_rir::RirIntrinsicArgsRange,
        span: Span,
        result_expected: Option<Type>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.analyze_intrinsic_impl(air, inst_ref, name, args, span, result_expected, ctx)
    }

    // ========================================================================
    // Declaration no-ops: DropFnDecl, FnDecl
    // ========================================================================

    /// Analyze a declaration that produces Unit in expression context.
    ///
    /// Handles: DropFnDecl
    pub(crate) fn analyze_decl_noop(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        _ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::DropFnDecl { .. } => {
                // These are processed during collection phase, just return Unit
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }

            InstData::FnDecl { .. } => {
                // Function declarations are errors in expression context
                Err(CompileError::new(
                    ErrorKind::InternalError(
                        "FnDecl should not appear in expression context".to_string(),
                    ),
                    inst.span,
                ))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_decl_noop called with non-declaration instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }
}
