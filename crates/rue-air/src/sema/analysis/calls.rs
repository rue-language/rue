//! Free, method, module-member, and associated-function call analysis.
//!
//! This category owns source call dispatch, callee and receiver resolution,
//! generic specialization, argument/result coordination, and AIR call emission
//! within the canonical semantic-analysis implementation. Argument place,
//! loan, move, and representation coercion decisions delegate to
//! `analysis::ownership`.

use super::super::ordinary_engine::{
    OrdinaryBodyAnalysisHost, OrdinaryBodyEngine, ResolvedCalleeName,
};
use super::*;
use crate::sema::NamedConstDependencyTargetEvent;
use crate::sema::context::DivergenceKind;
use crate::sema::info::FunctionCallInfo;
use ahash::AHashMap;

/// Reject a runtime call whose result is a comptime-only value.
///
/// A `type` (or module) value cannot exist at runtime (spec 4.14:6), so a call
/// producing one must reduce at compile time: an all-`comptime` call to a
/// `-> type` function is folded to a `TypeConst` before emission (4.14:28). A
/// call reaching an emitter with such a result — the callee has a runtime
/// parameter, or its form is not reduced — has no call ABI to lower to, so it
/// is diagnosed at the call instead of reaching codegen (RUE-2417).
///
/// `has_runtime_param_or_receiver` distinguishes the two ways a call can
/// reach here: a genuine runtime parameter (or, for a method, its runtime
/// `self` receiver) makes the "every parameter comptime" help accurate, but a
/// zero-parameter or all-`comptime`-parameter callee (`fn m() -> type { .. }`,
/// a zero-parameter associated function) already meets that condition
/// vacuously — its body simply failed to reduce — so the help would point at
/// a condition the call already satisfies (RUE-2417 review).
fn reject_runtime_comptime_only_result(
    return_type: Type,
    has_runtime_param_or_receiver: bool,
    span: Span,
) -> CompileResult<()> {
    if return_type.is_comptime_type() || return_type.is_module() {
        let reason = if has_runtime_param_or_receiver {
            "this call produces a type value at runtime; a `type` result requires a \
             fully-comptime call"
        } else {
            "this function's body does not reduce to a type at compile time"
        };
        let mut err = CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: reason.to_string(),
            },
            span,
        );
        if has_runtime_param_or_receiver {
            err = err.with_help(
                "a call to a function returning `type` is evaluated at compile time only when \
                 every parameter of the function is `comptime` and every argument is \
                 compile-time known",
            );
        }
        return Err(err);
    }
    Ok(())
}

/// Validate membership for a module-member function call.
///
/// Membership (spec 4.13:90, RUE-140): a module contains only declarations
/// from the imported file. The callee is resolved by the receiver module's
/// canonical `FileId`, and `member_file_id` is checked as a defensive invariant.
/// Comparing by canonical FileId rather than raw path strings makes equivalent
/// import spellings — `helper.rue` vs
/// `./helper.rue` — resolve members identically (spec 10.2:4, RUE-240).
///
/// Visibility is a separate question with its own owner: the caller applies
/// [`OrdinaryBodyEngine::check_item_visibility`] once membership holds, so a
/// private member reports the same E0706 here as in every other position.
fn check_module_member_access(
    module_name: &str,
    module_file_id: Option<FileId>,
    member_file_id: FileId,
    fn_name_str: &str,
    via_reexport: bool,
    span: Span,
) -> CompileResult<()> {
    // Check membership: the function must be defined in the module's file
    // (canonical FileId equality) — UNLESS the call resolved through a
    // re-export const in the facade, whose presence is the membership grant
    // (`pub const f = @import("x").f;`, ADR-0026, RUE-592).
    if !via_reexport && module_file_id != Some(member_file_id) {
        return Err(crate::unknown_module_member(module_name, fn_name_str, span));
    }

    Ok(())
}

/// Record the conservative edge of an ordinary runtime call whose declared
/// result is `!`. This edge is reachable whenever the call's receiver and all
/// arguments have a continuing path; it remains generic divergence even when
/// another alternative operand path already contributed panic provenance.
fn record_reachable_never_call(
    ctx: &mut AnalysisContext,
    call_operands_continue: bool,
    return_type: Type,
) {
    if call_operands_continue && return_type.is_never() {
        ctx.divergence_kinds.insert(DivergenceKind::Other);
    }
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Validate the source-level contract shared by every ordinary call form.
    /// Receiver exclusivity is checked separately for methods because their
    /// implicit `self` access participates in the same loan set as the explicit
    /// arguments.
    pub(super) fn validate_call_contract(
        &self,
        args_range: &rue_rir::RirCallArgsRange,
        param_types: &[Type],
        param_modes: &[RirParamMode],
        span: Span,
        check_exclusive: bool,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        let args = self.body_rir_ref().call_args(args_range).to_vec();
        if args.len() != param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }
        debug_assert_eq!(param_types.len(), param_modes.len());
        self.validate_explicit_call_modes(&args, param_modes.iter().copied())?;
        if check_exclusive {
            self.check_exclusive_access(&args, span, ctx)?;
        }
        Ok(())
    }

    /// Lower explicit call operands through the canonical ownership/coercion
    /// authority, then require each operand's analyzed type to be its
    /// parameter's type ([`Self::require_slot_type`]). Inference cannot be
    /// trusted for this: an operand it has no fact for (an unreduced
    /// constructor head, typed `<error>`) unifies with any parameter
    /// (RUE-2438). Only a generic callee skips the check here, because its
    /// declared parameter types still mention type parameters; it checks each
    /// operand against the substituted type once the call's type arguments
    /// are known.
    pub(super) fn analyze_call_operands(
        &mut self,
        air: &mut Air,
        args_range: &rue_rir::RirCallArgsRange,
        param_types: &[Type],
        param_modes: &[RirParamMode],
        check_operand_types: bool,
        call_may_continue: bool,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<CallOperands> {
        let args = self.body_rir_ref().call_args(args_range).to_vec();
        let operands = self.analyze_call_args_coerced(
            air,
            args.iter().copied(),
            param_types,
            param_modes,
            call_may_continue,
            ctx,
        )?;
        // Operands of a runtime call analyzed as an inline constructor head
        // (`f(true) { .. }`) are recovered only on the way to that head's
        // "not a type" diagnostic (`recover_missing_ctor_head_arguments`);
        // such a head never reaches code generation, so its operands keep
        // that one stable diagnostic rather than an argument mismatch.
        if check_operand_types && !ctx.recover_missing_ctor_head_arguments {
            for (((arg, air_arg), expected), mode) in args
                .iter()
                .zip(&operands.args)
                .zip(param_types)
                .zip(param_modes)
            {
                // An `inout str` view keeps its source's own type as the
                // operand; `validate_inout_str_operand` is that position's
                // authority.
                if self.is_inout_str_param(*mode, *expected) {
                    continue;
                }
                self.require_slot_type(
                    *expected,
                    air.get(air_arg.value).ty,
                    self.body_rir_ref().get(arg.value).span,
                )?;
            }
        }
        Ok(operands)
    }

    /// Emit an ordinary resolved call and package its declared result.
    ///
    /// `temp_scope` carries the storage annotations of any borrow-operand
    /// temporary this call materialized (RUE-953). Wrapping the *call* in that
    /// block is what puts the temporaries' scope exit — and therefore their
    /// drop — after the callee has read through the loan.
    #[allow(clippy::too_many_arguments)]
    fn emit_call_result(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[AirCallArg],
        temp_scope: Vec<AirRef>,
        return_type: Type,
        has_runtime_param_or_receiver: bool,
        continues: bool,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        reject_runtime_comptime_only_result(return_type, has_runtime_param_or_receiver, span)?;
        let air_ref = air.add_call(None, name, args, return_type, span)?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, air_ref, return_type, span, temp_scope)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            return_type,
            continues,
        ))
    }

    /// Analyze an associated function call.
    ///
    /// Resolves and analyzes an associated-function call through the
    /// call-analysis category.
    fn analyze_assoc_fn_call(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args_range: &rue_rir::RirCallArgsRange,
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
            let variant_name = self.body_interner().resolve(&function).to_string();
            let def = self.body_type_pool().enum_def(enum_id);
            if let Some(variant_index) = def.find_variant(&variant_name) {
                return self.analyze_enum_variant_construction(
                    air,
                    enum_id,
                    variant_index as u32,
                    type_name,
                    via_comptime,
                    args_range,
                    span,
                    ctx,
                );
            }
        }

        self.analyze_assoc_fn_call_impl(air, type_name, function, args_range, span, ctx, None)
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

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
            } => {
                // A `-> borrow T` accessor call inlines to its guards plus
                // the yielded place (ADR-0062); in value position the place
                // is read. Intercepted before ordinary method dispatch so
                // the receiver is traced as a place, never read as a value.
                if let Some(struct_id) = self
                    .peek_place_type(*receiver, ctx)
                    .and_then(|ty| ty.as_struct())
                    && self
                        .call_facts()
                        .call_method_info(struct_id, *method)
                        .is_some_and(|info| info.returns_borrow || info.returns_inout)
                {
                    return self.analyze_accessor_call_value(
                        air, inst_ref, *receiver, struct_id, *method, args, inst.span, ctx,
                    );
                }
                self.analyze_method_call(air, *receiver, *method, args, inst.span, ctx)
            }

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
    fn analyze_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let source_name = name;
        // A callback parameter is called through its `fn` type (ADR-0096,
        // RUE-2194). The parameter binding wins over an item of the same
        // name exactly as it does for a value read (spec 5.1:11); a `let`
        // shadowing the parameter is a value, not a callable.
        if !ctx.locals.contains_key(&name)
            && let Some(param) = ctx.param(name)
            && param.ty.is_function()
        {
            let (slot, callback) = (param.abi_slot, param.ty);
            return self.analyze_callback_call(air, slot, callback, args_range, span, ctx);
        }
        // One resolution order for the callee name, shared with the staged
        // inference pre-pass and comptime call admission (RUE-2161).
        let Some(resolved) = self.resolve_callee_name(name, span.file_id) else {
            // `print(s)` / `println(s)` / `eprint(s)` / `eprintln(s)` are
            // builtin free functions, not user-defined ones: intercept them
            // here, but only when the program hasn't shadowed the name with
            // its own `fn print`/`fn println`/`fn eprint`/`fn eprintln` (a
            // user definition wins, keeping these names unreserved).
            if source_name == self.known_symbols().print
                || source_name == self.known_symbols().println
                || source_name == self.known_symbols().eprint
                || source_name == self.known_symbols().eprintln
            {
                return self.analyze_print_builtin(air, source_name, args_range, span, ctx);
            }
            // `Str(4)` in expression position spells a type, not a call to a
            // declared function (RUE-2266). It reaches here for the same
            // reason `print` does — the name heads no declaration — and, like
            // a reduced `-> type` constructor call, it analyzes to a
            // `TypeConst`.
            if let Some(result) =
                self.analyze_builtin_type_call(air, source_name, args_range, span, ctx)?
            {
                return Ok(result);
            }
            let fn_name_str = self.body_interner().resolve(&source_name).to_string();
            return Err(CompileError::new(
                ErrorKind::UndefinedFunction(fn_name_str),
                span,
            ));
        };
        let name = match resolved {
            ResolvedCalleeName::Local(local) => local,
            ResolvedCalleeName::Alias { callee, alias } => {
                let alias_name = self.body_interner().resolve(&source_name).to_string();
                self.check_item_visibility(
                    crate::PrivateItemKind::Const,
                    &alias_name,
                    alias.span.file_id,
                    alias.is_pub,
                    span,
                )?;
                self.record_body_named_dependency(NamedConstDependencyTargetEvent::ValueConst {
                    file: alias.span.file_id.index(),
                    name: alias_name,
                });
                callee
            }
        };

        // Look up the function
        let source_name = self.call_facts().call_source_function_name(name);
        let fn_name_str = self.body_interner().resolve(&source_name).to_string();
        let fn_info = self
            .call_facts()
            .call_function_info(name)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;

        self.analyze_resolved_function_call(air, name, fn_info, args_range, span, ctx, true)
    }

    /// Analyze a call whose callee names a *builtin type constructor* rather
    /// than a declaration — `Str(4)` (RUE-2266).
    ///
    /// The result is the type the identical annotation spelling produces, so
    /// `identity(Str(4), "hi")`, `Option(Str(4))` and `let T = Str(4);` all
    /// see the same `Str(4)` a `let s: Str(4)` annotation sees. Answers `None`
    /// for every other name, and for a builtin whose arguments are not
    /// compile-time integers, so the caller still reports the ordinary
    /// undefined-function error.
    fn analyze_builtin_type_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        if !crate::sema::typeck::is_builtin_type_constructor(self.body_interner().resolve(&name)) {
            return Ok(None);
        }
        let args = self.body_rir_ref().call_args(args_range).to_vec();
        let mut values = Vec::with_capacity(args.len());
        for arg in &args {
            let Some(value) = self.evaluate_const_in_fn(arg.value, ctx)? else {
                return Ok(None);
            };
            values.push(value);
        }
        let Some(ty) =
            self.resolve_builtin_type_call_in_file(name, &values, ctx.current_file_id, span)?
        else {
            return Ok(None);
        };
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::TypeConst(ty),
            ty: Type::COMPTIME_TYPE,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE)))
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
    fn analyze_resolved_function_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        fn_info: FunctionCallInfo,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
        check_visibility: bool,
    ) -> CompileResult<AnalysisResult> {
        let source_name = self.call_facts().call_source_function_name(name);
        let fn_name_str = self.body_interner().resolve(&source_name).to_string();

        // Visibility (E0706, RUE-37/RUE-180): a call must not reach a
        // private function defined in another directory — privacy is uniform
        // in every multi-file compilation (spec 10.3:7). The lookup has
        // already selected a declaration using the reference file.
        if check_visibility {
            self.check_item_visibility(
                crate::PrivateItemKind::Function,
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

        // A foreign `extern "C"` function may only be called inside a `checked`
        // block (ADR-0064 unchecked-only ruling): the boundary carries no Rue
        // exclusivity, aliasing, or lifetime guarantee, exactly as `@syscall`
        // requires a checked context (ADR-0028).
        if fn_info.is_extern && ctx.checked_depth == 0 {
            return Err(CompileError::new(
                ErrorKind::UncheckedOpRequiresChecked {
                    what: format!("calling foreign function `{fn_name_str}`"),
                },
                span,
            )
            .with_help("wrap the call in a `checked { ... }` block"));
        }

        // Copy the exact parameter point before any lazy fact can append.
        let param_data = self.body_param_data(fn_info.params);
        let param_types = param_data.types().to_vec();
        let param_modes = param_data.modes().to_vec();
        let param_comptime = param_data.comptime().to_vec();
        let param_names = param_data.names().to_vec();

        self.validate_call_contract(args_range, &param_types, &param_modes, span, true, ctx)?;
        // The declaration, visibility, checked-call policy, and explicit call
        // contract have all selected this exact callable. Record before
        // operand analysis so a later argument diagnostic retains the edge.
        #[cfg(test)]
        {
            self.record_body_named_dependency(NamedConstDependencyTargetEvent::FreeFunction {
                file: fn_info.file_id.index(),
                name: fn_name_str.clone(),
            });
            self.record_body_callable_dependency(name);
        }
        let args = self.body_rir_ref().call_args(args_range).to_vec();

        // Extract info before any mutable borrow
        let is_generic = fn_info.is_generic;
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let param_modes = param_modes.to_vec();
        let base_return_type = fn_info.return_type;

        // `-> type` functions with no runtime parameters reduce immediately,
        // but their arguments still obey the ordinary comptime contract. Build
        // the maps through the propagating evaluator before reducing the body;
        // otherwise a constructor that ignores a wrong-kind/private argument
        // can accidentally accept it.
        let all_params_comptime = param_comptime.iter().all(|&flag| flag);
        if self.function_returns_type(&fn_info) && (args.is_empty() || all_params_comptime) {
            let mut type_subst = AHashMap::new();
            let mut value_subst = AHashMap::new();
            for (i, is_comptime) in param_comptime.iter().enumerate() {
                if !*is_comptime {
                    continue;
                }
                let value = self.evaluate_const_in_fn(args.get(i).unwrap().value, ctx)?;
                let is_comptime_type = param_comptime_type
                    .get(i)
                    .copied()
                    .unwrap_or(matches!(value, Some(ConstValue::Type(_))));
                if is_comptime_type {
                    match value {
                        Some(ConstValue::Type(ty)) => {
                            type_subst.insert(param_names[i], ty);
                            self.check_interface_bounds(
                                &fn_info,
                                i,
                                param_names[i],
                                ty,
                                self.body_rir_ref().get(args.get(i).unwrap().value).span,
                                span,
                            )?;
                        }
                        Some(ConstValue::Unit) => {
                            type_subst.insert(param_names[i], Type::UNIT);
                            self.check_interface_bounds(
                                &fn_info,
                                i,
                                param_names[i],
                                Type::UNIT,
                                self.body_rir_ref().get(args.get(i).unwrap().value).span,
                                span,
                            )?;
                        }
                        Some(_) => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime type parameter must be a type literal"
                                        .to_string(),
                                },
                                self.body_rir_ref().get(args.get(i).unwrap().value).span,
                            ));
                        }
                        None => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeArgNotConst {
                                    param_name: self
                                        .body_interner()
                                        .resolve(&param_names[i])
                                        .to_string(),
                                },
                                self.body_rir_ref().get(args.get(i).unwrap().value).span,
                            ));
                        }
                    }
                } else if let Some(value) = value {
                    value_subst.insert(param_names[i], value);
                } else {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeArgNotConst {
                            param_name: self.body_interner().resolve(&param_names[i]).to_string(),
                        },
                        self.body_rir_ref().get(args.get(i).unwrap().value).span,
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
                .reduce_type_ctor_body(name, &type_subst, &value_subst, span)
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
            // A body that does not reduce falls through to the runtime call
            // emission below, which rejects its `type` result (RUE-2417).
        }

        // Only runtime calls contribute a lazy body edge. A successfully
        // reduced `-> type` producer is consumed above as a TypeConst and its
        // provider-owned result, not its executable body, is the dependency.
        ctx.referenced_functions.insert(name);

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
                        let param_name = self.body_interner().resolve(&param_names[i]).to_string();
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
                            self.body_rir_ref().get(arg.value).span,
                        )
                        .with_help(help));
                    }
                }
            }
        }

        // Analyze all arguments. Slice parameters (ADR-0043, RUE-322) coerce a
        // `borrow arr` argument into a by-value fat pointer here.
        let expression_ledgers_before_call = ctx.ownership.checkpoint_expression_ledgers();
        let CallOperands {
            args: air_args,
            temp_scope,
            continues,
        } = self.analyze_call_operands(
            air,
            args_range,
            &param_types,
            &param_modes,
            !fn_info.is_generic,
            !fn_info.return_type.is_never(),
            ctx,
        )?;

        // Handle generic function calls differently
        if is_generic {
            // Separate type arguments and comptime value arguments from
            // runtime arguments
            let mut type_args: Vec<Type> = Vec::new();
            let mut value_args: Vec<ConstValue> = Vec::new();
            let mut runtime_args: Vec<AirCallArg> = Vec::new();
            let mut type_subst: AHashMap<Spur, Type> = AHashMap::new();
            // Comptime VALUE parameters (`comptime N: i32`) map to their
            // captured constant so a runtime param type mentioning one — an
            // array length `arr: [i32; N]` — resolves at this call (RUE-16).
            let mut value_subst: AHashMap<Spur, ConstValue> = AHashMap::new();

            for (i, (air_arg, is_comptime)) in
                air_args.iter().zip(param_comptime.iter()).enumerate()
            {
                if *is_comptime {
                    // The source declaration distinguishes a type parameter
                    // from a value parameter whose semantic type is deferred.
                    let argument = air.get(air_arg.value);
                    let is_comptime_type = param_comptime_type
                        .get(i)
                        .copied()
                        .unwrap_or(matches!(argument.data, AirInstData::TypeConst(_)));
                    if is_comptime_type {
                        // This is a TYPE parameter - expect a TypeConst instruction
                        let inst = argument;
                        if let AirInstData::TypeConst(ty) = &inst.data {
                            let ty = *ty;
                            type_args.push(ty);
                            // Record the substitution: param_name -> concrete_type
                            type_subst.insert(param_names[i], ty);
                            // A bounded parameter (spec 6.8:14) accepts only a
                            // conforming type argument (spec 6.8:15).
                            self.check_interface_bounds(
                                &fn_info,
                                i,
                                param_names[i],
                                ty,
                                self.body_rir_ref().get(args.get(i).unwrap().value).span,
                                span,
                            )?;
                        } else if matches!(inst.data, AirInstData::UnitConst) {
                            // `()` in a `comptime T: type` position is the unit
                            // TYPE (RUE-565); the declared parameter kind
                            // disambiguates it from the unit value. Mirrors the
                            // ConstValue::Unit arm in the reduction path above.
                            type_args.push(Type::UNIT);
                            type_subst.insert(param_names[i], Type::UNIT);
                            self.check_interface_bounds(
                                &fn_info,
                                i,
                                param_names[i],
                                Type::UNIT,
                                self.body_rir_ref().get(args.get(i).unwrap().value).span,
                                span,
                            )?;
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
                                value_args.push(const_val.clone());
                                value_subst.insert(param_names[i], const_val);
                            }
                            None => {
                                let param_name =
                                    self.body_interner().resolve(&param_names[i]).to_string();
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
                                    self.body_rir_ref().get(arg_value).span,
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
                let argument_is_type =
                    matches!(air.get(air_arg.value).data, AirInstData::TypeConst(_));
                if is_comptime
                    && param_comptime_type
                        .get(i)
                        .copied()
                        .unwrap_or(argument_is_type)
                {
                    // The comptime type argument itself - already validated above.
                    continue;
                }
                let expected = self.resolve_substituted_param_type(
                    &fn_info,
                    i,
                    declared,
                    &type_subst,
                    &value_subst,
                    span,
                )?;
                if is_comptime {
                    // Generic/runtime lowering keeps a comptime value in the
                    // call ABI, but its source-level comptime contract still
                    // needs the canonical value validation. In particular,
                    // `Str(N)` shares a runtime layout with `str` while
                    // remaining a distinct fixed-capacity type.
                    let value = value_subst
                        .get(&param_names[i])
                        .cloned()
                        .expect("comptime value substitution was captured above");
                    self.validate_comptime_value_for_type(
                        name,
                        param_names[i],
                        value,
                        expected,
                        span,
                    )?;
                }
                let found = air.get(air_arg.value).ty;
                if !self.types_compatible(found, expected) && !expected.is_error() {
                    let arg_span = self.body_rir_ref().get(args.get(i).unwrap().value).span;
                    // A named function bound to a `fn` parameter whose type
                    // mentions a comptime type parameter is checked here,
                    // once the substitution is known (ADR-0096, RUE-2194).
                    if expected.is_function()
                        && let AirInstData::FnRef { name: callee } = air.get(air_arg.value).data
                    {
                        let source = self.call_facts().call_source_function_name(callee);
                        return Err(CompileError::new(
                            ErrorKind::CallbackSignatureMismatch(Box::new(
                                rue_error::CallbackSignatureMismatchError {
                                    function: self.body_interner().resolve(&source).to_string(),
                                    expected: self.format_type_name(expected),
                                    found: self.format_type_name(found),
                                },
                            )),
                            arg_span,
                        ));
                    }
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: self.format_type_name(expected),
                            found: self.format_type_name(found),
                        },
                        arg_span,
                    ));
                }
            }

            // Determine the actual return type by substituting type parameters.
            // Handles bare type parameters (`-> T`), composites mentioning one
            // (`-> [T; 3]`, RUE-172), and the literal `type` return (which
            // resolves back to COMPTIME_TYPE and is comptime-evaluated below).
            let return_type =
                self.resolve_substituted_return_type(&fn_info, &type_subst, &value_subst, span)?;

            if self.body_dependency_observer().is_some()
                && let Ok(identity) =
                    self.canonical_specialization_instance(name, &type_args, &value_args)
            {
                self.record_specialization_dependency(identity);
            }

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
                let mut value_subst: AHashMap<Spur, ConstValue> = AHashMap::new();
                for (i, is_comptime) in param_comptime.iter().enumerate() {
                    let argument_is_type = air_args.get(i).is_some_and(|argument| {
                        matches!(air.get(argument.value).data, AirInstData::TypeConst(_))
                    });
                    if *is_comptime
                        && !param_comptime_type
                            .get(i)
                            .copied()
                            .unwrap_or(argument_is_type)
                    {
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
                    self.reduce_type_ctor_body(name, &type_subst, &value_subst, span)?
                {
                    // Success! Return a TypeConst instruction instead of a
                    // runtime call. This arm is reached only when every
                    // parameter is `comptime`, and a `comptime` parameter takes
                    // an unmarked argument (4.10:3), so `temp_scope` is
                    // necessarily empty here — there is no call left to wrap.
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                // A body that does not reduce falls through to the emission
                // below, which rejects the runtime `type` result (RUE-2417).
            }

            let has_runtime_param = param_comptime.iter().any(|&is_comptime| !is_comptime);
            reject_runtime_comptime_only_result(return_type, has_runtime_param, span)?;
            let air_ref = air.add_call_generic(
                name,
                &type_args,
                &value_args,
                &runtime_args,
                return_type,
                span,
            )?;
            let air_ref =
                self.wrap_value_with_temp_scope(air, air_ref, return_type, span, temp_scope)?;
            record_reachable_never_call(ctx, continues, return_type);
            let result = AnalysisResult::with_continues(
                air_ref,
                return_type,
                continues && !return_type.is_never(),
            );
            if !result.continues {
                ctx.ownership
                    .rollback_expression_ledgers(expression_ledgers_before_call);
            }
            Ok(result)
        } else {
            // Regular non-generic call
            let return_type = base_return_type;
            let has_runtime_param = param_comptime.iter().any(|&is_comptime| !is_comptime);
            let result = self.emit_call_result(
                air,
                name,
                &air_args,
                temp_scope,
                return_type,
                has_runtime_param,
                continues && !return_type.is_never(),
                span,
            )?;
            record_reachable_never_call(ctx, continues, return_type);
            if !result.continues {
                ctx.ownership
                    .rollback_expression_ledgers(expression_ledgers_before_call);
            }
            Ok(result)
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

    /// Implementation for MethodCall.
    fn analyze_method_call_impl(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        method: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let args = self.body_rir_ref().call_args(args_range).to_vec();
        // An accessor-call receiver chain (`v.get_ref(i).len()`, ADR-0062)
        // roots at the accessor's own receiver root: the chain is a place.
        let receiver_var = self
            .extract_root_variable(receiver)
            .or_else(|| self.place_root_with_accessors(receiver, ctx));
        let method_name_str = self.body_interner().resolve(&method).to_string();

        // `Type.function(args)` is an associated-function call / enum
        // tuple-variant construction (RUE-196, RUE-488): `.` is the sole
        // member-access spelling. Preserve ordinary value-method dispatch when a
        // runtime local/parameter shadows the type name; reinterpret an unbound
        // identifier receiver as a type namespace when it names a struct/enum or
        // a comptime type-variable bound to one (`let O = Option(i32);
        // O.Some(1)`). The struct/enum lookups mirror what `analyze_assoc_fn_call`
        // itself resolves — MODULE-LOCAL names plus builtins (RUE-525): an
        // unqualified `Type.assoc()` naming another file's type is
        // name-not-found, exactly like a struct literal, type annotation, or
        // plain function reference; the module-qualified spelling
        // (`m.Type.assoc()`) is the supported form (ADR-0046).
        if let InstData::VarRef { name, .. } = self.body_rir_ref().get(receiver).data
            && !self.is_runtime_value_binding(name, ctx)
            && (self.resolve_struct_type_name(name, ctx).is_some()
                || self.resolve_enum_type_name(name, ctx).is_some()
                || ctx.comptime_type_vars.contains_key(&name))
        {
            return self.analyze_assoc_fn_call(air, name, method, args_range, span, ctx);
        }

        // Module-qualified associated-function call / tuple-variant construction:
        // `module.Type.function(args)` (RUE-488). The receiver is `module.Type`
        // (a field access) whose module member is a struct or enum type. The
        // receiver module's defining file is authoritative for the type name.
        if let InstData::FieldGet {
            base: module_ref,
            field: type_name,
        } = self.body_rir_ref().get(receiver).data
            && let Some(result) = self.try_analyze_module_qualified_type_call(
                air, module_ref, type_name, method, args_range, span, ctx,
            )?
        {
            return Ok(result);
        }

        // Slice methods (ADR-0043, RUE-322): `s.len()` reads the fat pointer's
        // runtime `len` word. Detected from the receiver's type. The `str`
        // string type (RUE-324) shares this path; a `str` local's HM-inferred
        // type is `String` (inference does not model `str`), so the sema place
        // type is consulted first (via `peek_place_type`) and only then the
        // inferred type — otherwise `str.len()` would miss this route.
        let receiver_slice_ty = receiver_var
            .and_then(|_| self.peek_place_type(receiver, ctx))
            .or_else(|| ctx.resolved_type_of(receiver));
        if receiver_slice_ty.is_some_and(|ty| self.slice_element_type(ty).is_some()) {
            return self.analyze_slice_method(
                air,
                receiver,
                receiver_var,
                &method_name_str,
                args.len(),
                span,
                ctx,
            );
        }

        // Decide up front whether this receiver is accessed by reference, so
        // it is analyzed as a BORROW — not a move — in every place position (a
        // field, an array element by const or dynamic index, a field of
        // `self`, or through an inout/borrow parameter), mirroring the by-ref
        // *argument* path (spec 6.4:25/6.4:29). For a builtin String mutation
        // method the receiver is always by-ref, so its root is the by-ref root
        // (RUE-256). Registry-declared builtin ByRef queries need the same
        // pre-analysis classification (RUE-584). For a user-struct method,
        // resolution needs the receiver type — peeked WITHOUT emitting AIR or
        // recording a move, since a move-based analysis would hard-reject the
        // read of any non-local place (E0437/E0429/E0904) before the by-ref
        // intent is known — and the root is by-ref only when the method takes
        // `self` by reference (RUE-254). Module receivers keep their existing
        // post-analysis handling.
        let receiver_byref_root = {
            // A method that exists on NEITHER the user-method table nor the
            // builtin registry is diagnosed here, before the receiver is
            // analyzed: the unknown call would otherwise default to a
            // by-value receiver, and a field-of-`inout self` receiver then
            // failed the MOVE check first — reporting E0437 "cannot move out
            // of inout parameter" for what is actually a typo'd method name
            // (RUE-640).
            if receiver_var.is_some()
                && let Some(struct_id) = self
                    .peek_place_type(receiver, ctx)
                    .and_then(|ty| ty.as_struct())
            {
                let known = self.has_method((struct_id, method));
                if !known {
                    let type_name = self.format_type_name(Type::new_struct(struct_id));
                    return Err(CompileError::new(
                        ErrorKind::UndefinedMethod {
                            type_name,
                            method_name: method_name_str.clone(),
                        },
                        span,
                    ));
                }
            }
            receiver_var.and_then(|root| {
                let ty = self.peek_place_type(receiver, ctx)?;
                let struct_id = ty.as_struct()?;

                let info = self.call_facts().call_method_info(struct_id, method)?;
                matches!(info.self_mode, RirParamMode::Inout | RirParamMode::Borrow).then_some(root)
            })
        };

        let receiver_move_state_before = self.snapshot_move_state(receiver_var, ctx);

        // Analyze the receiver expression. When it is a by-ref receiver,
        // `byref_arg_root` makes the var-ref / field / index reads borrow the
        // place instead of moving out of it (restored afterwards).
        let expression_ledgers_before_call = ctx.ownership.checkpoint_expression_ledgers();
        let mut receiver_result =
            self.analyze_with_borrow_root(air, receiver, receiver_byref_root, ctx)?;
        let receiver_continues = receiver_result.continues;
        let receiver_type = receiver_result.ty;

        // Handle module member access: module.function() becomes a direct function call
        if let Some(module_id) = receiver_type.as_module() {
            return self
                .analyze_module_member_call_impl(air, module_id, method, args_range, span, ctx);
        }

        // Inline type-constructor call head (RUE-596, spec 4.14:23):
        // `F(args).NAME(..)` where `F(args)` reduced to a concrete type at
        // comptime. Resolve `.NAME` as an enum-variant construction or
        // associated-function call on the reduced type — exactly as if it had
        // been bound with `let P = F(args); P.NAME(..)`. The receiver's stray
        // `TypeConst` is a comptime-only no-op (CFG build drops it). Only a
        // struct/enum reduced type takes this path; any other kind (e.g.
        // `const X = i32; X.foo()`) falls through to the ordinary
        // `MethodCallOnNonStruct` diagnostic, and the bound-name form
        // (`let P = F(args); P.NAME`) never reaches here. Elided args
        // (`Option(_)`) stay out of scope (RUE-401).
        if receiver_type == Type::COMPTIME_TYPE
            && let AirInstData::TypeConst(reduced_ty) = air.get(receiver_result.air_ref).data
        {
            match reduced_ty.kind() {
                TypeKind::Enum(enum_id) => {
                    let variant_name = self.body_interner().resolve(&method).to_string();
                    let def = self.body_type_pool().enum_def(enum_id);
                    if let Some(vidx) = def.find_variant(&variant_name) {
                        return self.analyze_enum_variant_construction(
                            air,
                            enum_id,
                            vidx as u32,
                            method,
                            true,
                            args_range,
                            span,
                            ctx,
                        );
                    }
                    return Err(CompileError::new(
                        ErrorKind::UndefinedAssocFn {
                            type_name: self.format_type_name(reduced_ty),
                            function_name: variant_name,
                        },
                        span,
                    ));
                }
                TypeKind::Struct(struct_id) => {
                    return self.analyze_assoc_fn_call_impl(
                        air,
                        method,
                        method,
                        args_range,
                        span,
                        ctx,
                        Some(struct_id),
                    );
                }
                _ => {}
            }
        }

        // Check that receiver is a struct type
        let struct_id = match receiver_type.kind() {
            TypeKind::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::MethodCallOnNonStruct {
                        found: self.format_type_name(receiver_type),
                        method_name: method_name_str,
                    },
                    span,
                ));
            }
        };

        // Look up the method using StructId directly
        let method_key = (struct_id, method);
        let Some(method_info) = self.call_facts().call_method_info(struct_id, method) else {
            return Err(CompileError::new(
                ErrorKind::UndefinedMethod {
                    type_name: self.format_type_name(Type::new_struct(struct_id)),
                    method_name: method_name_str.clone(),
                },
                span,
            ));
        };
        // Track this method as referenced (for lazy analysis). Anonymous
        // struct methods are often registered while reducing a comptime type
        // constructor; without this edge the lazy pipeline can emit a call to
        // `__anon_struct_N.method` without analyzing and emitting that method
        // body.
        ctx.referenced_methods.insert(method_key);

        // Check that this is a method (has self), not an associated function
        if !method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::AssocFnCalledAsMethod {
                    type_name: self.format_type_name(Type::new_struct(struct_id)),
                    function_name: method_name_str,
                },
                span,
            ));
        }

        let method_param_data = self.body_param_data(method_info.params);
        let method_param_types = method_param_data.types().to_vec();
        let method_param_modes = method_param_data.modes().to_vec();
        // The receiver's autoref is implicit and deliberately excluded from
        // the explicit contract. Receiver-aware exclusivity runs below.
        self.validate_call_contract(
            args_range,
            &method_param_types,
            &method_param_modes,
            span,
            false,
            ctx,
        )?;
        self.record_body_method_dependency(method_key)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;
        let self_mode = method_info.self_mode;

        // Receiver passing mode (autoref, RUE-15). `borrow self` / `inout
        // self` receivers are accessed by reference — the receiver is passed
        // by address, reusing the by-ref parameter calling convention — so the
        // call does NOT consume the receiver. Bare `self` stays by-value.
        let receiver_mode = match self_mode {
            RirParamMode::Inout => AirArgMode::Inout,
            RirParamMode::Borrow => AirArgMode::Borrow,
            _ => AirArgMode::Normal,
        };

        let mut receiver_temp_scope = Vec::new();
        if receiver_mode != AirArgMode::Normal {
            let receiver_is_source_strbuf = receiver_type.as_struct().is_some_and(|struct_id| {
                self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
            });
            if receiver_mode == AirArgMode::Borrow
                && receiver_var.is_none()
                && receiver_is_source_strbuf
            {
                let (borrowed, temp_scope) = self.materialize_borrow_argument(
                    air,
                    receiver_result.air_ref,
                    receiver_result.ty,
                    self.body_rir_ref().get(receiver).span,
                    ctx,
                )?;
                receiver_result = AnalysisResult::new(borrowed, receiver_result.ty);
                receiver_temp_scope = temp_scope;
            }
            // An inlined accessor receiver arrives as a guards block whose
            // tail is the place read (ADR-0062); peel it for the address
            // check, exactly like the `inout str` view materialization.
            let receiver_is_accessor_place =
                self.place_root_with_accessors(receiver, ctx).is_some();
            let receiver_addressable_probe =
                if receiver_is_accessor_place || ctx.accessor_call_insts.contains_key(&receiver) {
                    let (place, prefix) =
                        self.peel_projected_rvalue_scope(air, receiver_result.air_ref);
                    if !prefix.is_empty() {
                        receiver_result = AnalysisResult::new(place, receiver_result.ty);
                        receiver_temp_scope.extend(prefix);
                    }
                    place
                } else {
                    receiver_result.air_ref
                };
            self.require_addressable_read(
                air,
                receiver_addressable_probe,
                receiver_mode == AirArgMode::Inout,
                self.body_rir_ref().get(receiver).span,
            )?;

            if receiver_var.is_none()
                && !receiver_is_accessor_place
                && (receiver_mode == AirArgMode::Inout || !receiver_is_source_strbuf)
            {
                return Err(CompileError::new(
                    if receiver_mode == AirArgMode::Inout {
                        ErrorKind::InoutNonLvalue
                    } else {
                        ErrorKind::BorrowNonLvalue
                    },
                    self.body_rir_ref().get(receiver).span,
                ));
            }

            if let Some(receiver_root) = receiver_var {
                // Calling an `inout self` method on a collection an enclosing
                // `for` loop is iterating mutates a shared-borrowed value (spec
                // 4.8:26, RUE-257) — E0428. A `borrow self` method only reads it,
                // which coexists with the loop's shared borrow, so it is allowed.
                if receiver_mode == AirArgMode::Inout {
                    self.reject_mutate_iter_borrowed(receiver_root, span, ctx)?;
                    // An enclosing call may hold a loan of this root that the
                    // mutation would invalidate. Frame construction only walks
                    // the outer call's own `inout` arguments, so an argument
                    // that merely CONTAINS this call -- a block, an `if`, a
                    // `match` arm -- never reached that check (RUE-1786).
                    self.reject_exclusive_use_of_call_loaned_root(receiver_root, span, ctx)?;
                }

                // `inout self` requires a mutable receiver binding (spec 6, reuses
                // E0203), mirroring Rust. `borrow self` works on any binding.
                if receiver_mode == AirArgMode::Inout
                    && !self.receiver_root_is_mutable(receiver_root, ctx)
                {
                    let name = self.body_interner().resolve(&receiver_root).to_string();
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name.clone()),
                        span,
                    )
                    .with_help(format!(
                        "`inout self` needs a mutable receiver; make the binding \
                     mutable: `let mut {name} = ...`"
                    )));
                }

                // Access-point exclusivity (ADR-0037): the receiver's inout/borrow
                // access is scoped to this call. Reject genuine overlap — the
                // receiver root also passed as an inout/borrow argument
                // (`s.absorb(inout s)`). An argument that merely READS self
                // (`v.push(v.len())`) is fine: its read completes before the
                // receiver access begins, and it is not a by-ref argument so it
                // never enters the exclusivity sets.
                let mut excl_args: Vec<RirCallArg> = Vec::with_capacity(args.len() + 1);
                excl_args.push(RirCallArg {
                    value: receiver,
                    mode: if receiver_mode == AirArgMode::Inout {
                        RirArgMode::Inout
                    } else {
                        RirArgMode::Borrow
                    },
                });
                excl_args.extend(args.iter().map(|arg| *arg));
                self.check_exclusive_access(&excl_args, span, ctx)?;

                // By-ref receivers are borrows, not moves. The receiver was
                // already analyzed under `byref_arg_root` above (RUE-254), so no
                // move was recorded and no marker emitted; this restore of the
                // pre-receiver snapshot and marker cancellation are defensive
                // no-ops for a well-formed place receiver (and still cover the
                // now-vestigial path where the receiver root differs from the
                // byref root). Mirrors the builtin ByRef/ByMutRef handling.
                self.restore_move_state_and_cancel(
                    air,
                    receiver_result.air_ref,
                    receiver_move_state_before.clone(),
                    ctx,
                );
            }
        } else {
            // Check for exclusive access violation (by-value receiver)
            self.check_exclusive_access(&args, span, ctx)?;
        }

        // Analyze arguments - receiver first, then remaining args.
        //
        // A by-ref receiver's loan spans the whole call: while the remaining
        // arguments are analyzed, a by-value move of the receiver's root
        // (`s.absorb(s)`) must conflict exactly like `f(inout s, s)` does
        // (RUE-523), so the receiver contributes a loan frame of its own.
        let mut air_args = vec![AirCallArg {
            value: receiver_result.air_ref,
            mode: receiver_mode,
        }];
        // A receiver place reached through an accessor result
        // (`a.pmut().set()`, `h.acc.pmut().q.get()`) is not a fresh use of
        // the root: it uses the place that accessor's loan already granted,
        // exactly like `setp(inout a.pmut())` or `a.pmut().c = 5`. Record the
        // loan's kind -- `Some(true)` for an exclusive (`-> inout`) result --
        // so the root-granular conflict checks below key on the receiver
        // place. Mutating through a shared (`-> borrow`) result stays a
        // conflict with that loan (spec 6.6:8, 6.6:10, E0259).
        let receiver_accessor_loan = ctx
            .accessor_place_refs
            .get(&receiver)
            .map(|&(_, _, _, exclusive, _)| exclusive);
        let receiver_frame = match (receiver_mode, receiver_var) {
            (AirArgMode::Inout, Some(root)) => {
                // An `inout self` receiver on a root an accessor result
                // borrows in the same full expression violates exclusivity
                // (ADR-0062, E0259).
                self.reject_receiver_accessor_loan_conflict(
                    root,
                    receiver_mode,
                    receiver_accessor_loan,
                    span,
                    ctx,
                )?;
                Some(vec![(root, CallLoanKind::Inout, false)])
            }
            (AirArgMode::Borrow, Some(root)) => {
                self.reject_receiver_accessor_loan_conflict(
                    root,
                    receiver_mode,
                    receiver_accessor_loan,
                    span,
                    ctx,
                )?;
                Some(vec![(root, CallLoanKind::Borrow, false)])
            }
            _ => None,
        };
        let receiver_frame_pushed = receiver_frame.is_some();
        if let Some(frame) = receiver_frame {
            ctx.ownership.call_loaned_roots.push(frame);
        }
        let args_result = self.analyze_call_operands(
            air,
            args_range,
            &method_param_types,
            &method_param_modes,
            true,
            receiver_continues && !return_type.is_never(),
            ctx,
        );
        if receiver_frame_pushed {
            ctx.ownership.call_loaned_roots.pop();
        }
        let args_result = args_result?;
        // Re-check the receiver after the argument list is analyzed
        // (RUE-1593): an accessor expanded among this call's own arguments
        // (`p.bump_with(p.f())`) registered its loan only during argument
        // analysis, after the pre-frame receiver check above. The by-ref
        // receiver access spans the same full expression as that loan, so it
        // is rejected in either evaluation order (spec 6.6:10, 6.6:16,
        // E0259). A receiver reached through an accessor result needs no
        // re-check: an argument's own accessor call or by-ref use of the root
        // is checked against the receiver's loan when it is analyzed.
        if let Some(root) = receiver_var {
            self.reject_receiver_accessor_loan_conflict(
                root,
                receiver_mode,
                receiver_accessor_loan,
                span,
                ctx,
            )?;
        }
        air_args.extend(args_result.args);
        // The receiver's materialized owner is entered first: it is created
        // before any explicit operand, so its scope must be the outer one.
        let mut temp_scope = receiver_temp_scope;
        temp_scope.extend(args_result.temp_scope);

        // The method call symbol `Type.method`, module-qualified when the type
        // name spans files (RUE-571). The host renders and interns it, so the
        // call meets the definition it names.
        let call_name_sym = self.method_symbol_handle(struct_id, &method_name_str, true)?;

        // A method call always has a runtime `self` receiver — methods take
        // no `comptime self` — so the "make every parameter comptime" help
        // is always accurate here (RUE-2417 review, S2).
        let call = self.emit_call_result(
            air,
            call_name_sym,
            &air_args,
            temp_scope,
            return_type,
            true,
            receiver_continues && args_result.continues && !return_type.is_never(),
            span,
        )?;
        record_reachable_never_call(
            ctx,
            receiver_continues && args_result.continues,
            return_type,
        );
        if call.continues
            && receiver_mode == AirArgMode::Inout
            && let Some(root) = self.extract_root_variable(receiver)
        {
            self.record_completed_exclusive_use(root, span, ctx);
        }
        if !call.continues {
            ctx.ownership
                .rollback_expression_ledgers(expression_ledgers_before_call);
        }
        Ok(call)
    }

    /// Analyze a module member call: `module.function(args)` becomes a direct function call.
    ///
    /// Modules are virtual namespaces. A member resolves through the imported
    /// file's `(FileId, source name)` entry, or through an explicit public
    /// function-valued re-export in that file.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn analyze_module_member_call_impl(
        &mut self,
        air: &mut Air,
        module_id: ModuleId,
        function_name: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let fn_name_str = self.body_interner().resolve(&function_name).to_string();
        let module_def = self.call_facts().call_module_def(module_id);
        let module_file_id = Some(module_def.file_id);
        let mut function_key = module_file_id.and_then(|file_id| {
            self.call_facts()
                .call_resolve_function_name_local(function_name, file_id)
        });

        // Fallback: a re-exported function member — `pub const f = @import("x").f;`
        // in the facade binds `f` to a function value (ADR-0026, RUE-592). It is
        // not a free function *defined* in the facade, so resolve the call to the
        // underlying function the const points at. The re-export const's own
        // visibility gates access from here; its presence is the membership grant,
        // so the "defined in this file" and underlying-visibility checks are
        // bypassed below.
        let mut via_reexport = false;
        if function_key.is_none()
            && let Some(mfile) = module_file_id
        {
            let reexport = self
                .call_facts()
                .call_value_const(mfile, function_name)
                .and_then(|info| match info.value {
                    ConstValue::Function(fkey) => Some((fkey.spur(), info.is_pub)),
                    _ => None,
                });
            if let Some((fkey, is_pub)) = reexport {
                self.check_item_visibility(
                    crate::PrivateItemKind::Const,
                    &fn_name_str,
                    mfile,
                    is_pub,
                    span,
                )?;
                #[cfg(test)]
                self.record_body_named_dependency(NamedConstDependencyTargetEvent::ValueConst {
                    file: mfile.index(),
                    name: fn_name_str.clone(),
                });
                function_key = Some(fkey);
                via_reexport = true;
            }
        }

        let function_key = function_key.ok_or_else(|| {
            crate::unknown_module_member(
                &crate::module_display_name(&module_def.import_path),
                &fn_name_str,
                span,
            )
        })?;
        let fn_info = self
            .call_facts()
            .call_function_info(function_key)
            .ok_or_else(|| {
                crate::unknown_module_member(
                    &crate::module_display_name(&module_def.import_path),
                    &fn_name_str,
                    span,
                )
            })?;

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(function_key);

        let param_data = self.body_param_data(fn_info.params);
        let param_types = param_data.types().to_vec();
        let param_modes = param_data.modes().to_vec();
        check_module_member_access(
            &crate::module_display_name(&module_def.import_path),
            module_file_id,
            fn_info.file_id,
            &fn_name_str,
            via_reexport,
            span,
        )?;
        // A re-export was already visibility-checked against its facade const;
        // otherwise the callee's own `pub` and defining file govern, through
        // the one privacy decision every position shares.
        if !via_reexport {
            self.check_item_visibility(
                crate::PrivateItemKind::Function,
                &fn_name_str,
                fn_info.file_id,
                fn_info.is_pub,
                span,
            )?;
        }

        // Functions with comptime parameters need specialization: a plain
        // Call to the base name would reference a body that is never
        // analyzed (generic bodies are only materialized per specialization,
        // RUE-166). A `-> type` function reduces at compile time exactly as an
        // unqualified call to it does (spec 4.14:5a, 4.14:28), including a
        // zero-parameter one. Use the already-resolved call path so
        // module-qualified type constructors do not re-enter unqualified
        // source-name lookup; module membership and accessibility were checked
        // above.
        if fn_info.is_generic || self.function_returns_type(&fn_info) {
            return self.analyze_resolved_function_call(
                air,
                function_key,
                fn_info,
                args_range,
                span,
                ctx,
                false,
            );
        }

        self.validate_call_contract(args_range, &param_types, &param_modes, span, true, ctx)?;
        self.record_body_callable_dependency(function_key);

        // Analyze arguments (the per-pipeline recursion seam). Module-qualified
        // calls use the coercing path so slice and `borrow str` parameters
        // materialize their by-value fat-pointer views exactly like direct
        // calls do (RUE-559) — std functions taking `borrow s: str` are called
        // this way.
        let expression_ledgers_before_call = ctx.ownership.checkpoint_expression_ledgers();
        let CallOperands {
            args: air_args,
            temp_scope,
            continues,
        } = self.analyze_call_operands(
            air,
            args_range,
            &param_types,
            &param_modes,
            true,
            !fn_info.return_type.is_never(),
            ctx,
        )?;

        // Reached only when the callee is neither generic nor `-> type`
        // (the branch above redirects both cases to `analyze_resolved_function_call`),
        // so every declared parameter here is a runtime one (`is_generic ==
        // params.any(is_comptime)`); a `type`/module result would mean the
        // callee genuinely has a runtime parameter, or none at all.
        let has_runtime_param = !param_types.is_empty();
        let result = self.emit_call_result(
            air,
            function_key,
            &air_args,
            temp_scope,
            fn_info.return_type,
            has_runtime_param,
            continues && !fn_info.return_type.is_never(),
            span,
        )?;
        record_reachable_never_call(ctx, continues, fn_info.return_type);
        if !result.continues {
            ctx.ownership
                .rollback_expression_ledgers(expression_ledgers_before_call);
        }
        Ok(result)
    }

    /// Analyze a type-qualified associated-function call.
    ///
    /// `resolved` carries a struct already resolved (and visibility-checked,
    /// E0706) by the module-qualified path (`m.Type.assoc()`, RUE-525): the
    /// type lives in the RECEIVER MODULE's file, so re-resolving the bare
    /// name in the caller's file would miss it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_assoc_fn_call_impl(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
        resolved: Option<StructId>,
    ) -> CompileResult<AnalysisResult> {
        let type_name_str = self.body_interner().resolve(&type_name).to_string();
        let function_name_str = self.body_interner().resolve(&function).to_string();

        // Check that the type exists and is a struct
        // First check if it's a comptime type variable (e.g., `let P = Point(); P::origin()`)
        //
        // `privacy_exempt` mirrors the enum-variant construction handler: a
        // comptime-bound type (`let P = Point(); P::origin()`) arrived through a
        // binding, not by naming the struct, so it is exempt from the
        // unqualified-privacy check. A bare `Point::origin()` names the struct
        // and must obey the same uniform-privacy rule (spec 10.3:7) as a struct
        // literal or type annotation would.
        let mut privacy_exempt = false;
        let struct_id = if let Some(struct_id) = resolved {
            // Module-qualified path: visibility (E0706) was already checked
            // against the receiver module by the caller.
            privacy_exempt = true;
            struct_id
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            privacy_exempt = true;
            // Extract struct ID from the comptime type
            match ty.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "struct type".to_string(),
                            found: self.format_type_name(ty),
                        },
                        span,
                    ));
                }
            }
        } else if let Some(info) = self
            .call_facts()
            .call_value_const(ctx.current_file_id, type_name)
            && let ConstValue::Type(ty) = info.value
        {
            // Module-level `const C = Counter(i32); C.zero()` (RUE-595): the
            // specialization arrived through a `const` binding, mirroring the
            // comptime-type-variable branch above and the const arm of
            // `resolve_enum_type_name` — so it is likewise privacy-exempt.
            privacy_exempt = true;
            match ty.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "struct type".to_string(),
                            found: self.format_type_name(ty),
                        },
                        span,
                    ));
                }
            }
        } else {
            // Module-local first, then builtins (RUE-525) — never the global
            // by-name table: an unqualified reference to another file's type
            // is name-not-found, matching every other unqualified form.
            self.struct_in_file(ctx.current_file_id, type_name)
                .or_else(|| self.resolve_builtin_struct_name(type_name))
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.clone()), span)?
        };

        // Privacy (E0706, RUE-330): naming a private struct to call one of its
        // associated functions (`Secret::make()`) across a directory boundary is
        // rejected, matching struct-literal / type-annotation references. Privacy
        // is uniform across item kinds (spec 10.3:1, 10.3:7). Builtin structs
        // (String, ...) have no source path, so `is_accessible` is permissive and
        // this is a no-op for them.
        if !privacy_exempt {
            let struct_def = self.body_type_pool().struct_def(struct_id);
            self.check_item_visibility(
                crate::PrivateItemKind::Struct,
                &type_name_str,
                struct_def.file_id,
                struct_def.is_pub,
                span,
            )?;
        }

        // Look up the function using StructId
        let method_key = (struct_id, function);
        let method_info = self
            .call_facts()
            .call_method_info(struct_id, function)
            .ok_or_compile_error(
                ErrorKind::UndefinedAssocFn {
                    type_name: type_name_str.clone(),
                    function_name: function_name_str.clone(),
                },
                span,
            )?;
        // Track this associated function/method as referenced (for lazy analysis)
        ctx.referenced_methods.insert(method_key);

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

        let method_param_data = self.body_param_data(method_info.params);
        let method_param_types = method_param_data.types().to_vec();
        let method_param_modes = method_param_data.modes().to_vec();
        let method_param_comptime = method_param_data.comptime().to_vec();
        self.validate_call_contract(
            args_range,
            &method_param_types,
            &method_param_modes,
            span,
            true,
            ctx,
        )?;
        self.record_body_method_dependency(method_key)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze explicit arguments through the same representation-aware
        // path as free and module-member calls. In particular, `borrow str`
        // and `[T]` parameters are physical by-value views even though their
        // source modes remain Borrow (RUE-634).
        let expression_ledgers_before_call = ctx.ownership.checkpoint_expression_ledgers();
        let CallOperands {
            args: air_args,
            temp_scope,
            continues,
        } = self.analyze_call_operands(
            air,
            args_range,
            &method_param_types,
            &method_param_modes,
            true,
            !return_type.is_never(),
            ctx,
        )?;

        // The associated-function call symbol `Type::function`. The owner
        // component is the internal struct name (`__anon_struct_<digest>`) for
        // anonymous structs — not the user-visible type variable name — and the
        // module-qualified name when the type name spans files (RUE-571); the
        // host renders both, so the call meets the definition it names.
        let call_name_sym = self.method_symbol_handle(struct_id, &function_name_str, false)?;

        // This path does not attempt comptime reduction of the associated
        // function's body (unlike `analyze_resolved_function_call`), so a
        // zero-parameter or all-`comptime` associated function returning
        // `type` reaches here having already met the "every parameter
        // comptime" condition vacuously; only a genuine runtime parameter
        // makes that help text accurate (RUE-2417 review, S2).
        let has_runtime_param = method_param_comptime
            .iter()
            .any(|&is_comptime| !is_comptime);
        let result = self.emit_call_result(
            air,
            call_name_sym,
            &air_args,
            temp_scope,
            return_type,
            has_runtime_param,
            continues && !return_type.is_never(),
            span,
        )?;
        record_reachable_never_call(ctx, continues, return_type);
        if !result.continues {
            ctx.ownership
                .rollback_expression_ledgers(expression_ledgers_before_call);
        }
        Ok(result)
    }
}
