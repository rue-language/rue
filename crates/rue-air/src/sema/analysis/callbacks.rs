//! Second-class callback semantics (ADR-0096, RUE-2194).
//!
//! A `fn` parameter binds a named function whose signature is exactly the
//! parameter's (spec 6.1:50), is called with ordinary call rules, and is
//! forwarded to another `fn` parameter of the same type; every other read of
//! it is an escape (spec 6.1:51). This category owns the three sites that
//! implement those rules: argument binding, the indirect call, and the escape
//! rejection. Argument place, loan, and coercion decisions stay with
//! `analysis::ownership`, which delegates a callback operand here.

use super::super::ordinary_engine::{
    OrdinaryBodyAnalysisHost, OrdinaryBodyEngine, ResolvedCalleeName,
};
use super::*;
use crate::sema::NamedConstDependencyTargetEvent;
use crate::sema::info::FunctionCallInfo;
use crate::types::{FunctionParamMode, FunctionTypeDef, FunctionTypeParam};

/// The reason an operand that names no function cannot bind a `fn` parameter.
const NOT_A_FUNCTION_NAME: &str = "a `fn` parameter binds a named function: a free function, a module-qualified function, a compile-time alias to one, or a receiverless associated function";

/// The reason a generic function cannot bind a `fn` parameter.
const GENERIC_CALLBACK: &str = "a callback is monomorphic; write a named function that supplies the comptime arguments and pass that";

/// A callable the argument to a `fn` parameter named, before its signature is
/// compared with the parameter's.
struct NamedCallable {
    /// The symbol a `FnRef` carries: the callee key of a free function or
    /// the member symbol of an associated function.
    symbol: Spur,
    /// The spelling diagnostics use.
    display: String,
    signature: FunctionTypeDef,
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Bind the argument written for a `fn` parameter (spec 6.1:50).
    ///
    /// The operand is one of exactly two things: a callback parameter of the
    /// current function, forwarded by name, or a named function. Both yield
    /// a value of the parameter's `fn` type: a `Param` read for the forward, a
    /// `FnRef` for the named function. Anything else is E0216; a named
    /// function whose signature differs is E0215; a forwarded callback of a
    /// different `fn` type is an ordinary type mismatch.
    ///
    /// `expected` is the parameter's `fn` type, or `None` when the callee is
    /// generic and the parameter's type is still a placeholder mentioning a
    /// comptime type parameter: the operand is then bound with the named
    /// function's own signature, and the generic call's substituted-type check
    /// compares it once the type arguments are known.
    pub(super) fn bind_callback_argument(
        &mut self,
        air: &mut Air,
        arg: &RirCallArg,
        expected: Option<Type>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AirRef> {
        let span = self.body_rir_ref().get(arg.value).span;
        match self.body_rir_ref().get(arg.value).data {
            InstData::VarRef { name, .. } => {
                if let Some(local) = ctx.locals.get(&name) {
                    // A local can never hold a callback (6.1:47), so a local
                    // shadowing a function or parameter name is a value, not
                    // a callable.
                    let found = format!(
                        "the local `{}` of type {}",
                        self.body_interner().resolve(&name),
                        self.format_type_name(local.ty)
                    );
                    return Err(self.ineligible_callback(found, NOT_A_FUNCTION_NAME, span));
                }
                if let Some(param) = ctx.param(name) {
                    let (param_ty, slot) = (param.ty, param.abi_slot);
                    if !param_ty.is_function() {
                        let found = format!(
                            "the parameter `{}` of type {}",
                            self.body_interner().resolve(&name),
                            self.format_type_name(param_ty)
                        );
                        return Err(self.ineligible_callback(found, NOT_A_FUNCTION_NAME, span));
                    }
                    // Forwarding (6.1:51): the callback is passed on by
                    // value, exactly as it arrived. Its type must be the
                    // receiving parameter's type; no `fn` type converts to
                    // another (6.1:48).
                    if let Some(expected) = expected
                        && !self.types_equivalent(param_ty, expected)
                    {
                        return Err(self.type_mismatch_error(expected, param_ty, span));
                    }
                    return Ok(air.add_inst(AirInst {
                        data: AirInstData::Param { index: slot },
                        ty: param_ty,
                        span,
                    }));
                }
                let callable = self.named_free_function_callback(name, span)?;
                self.bind_named_callback(air, callable, expected, span, ctx)
            }
            InstData::FieldGet { base, field } => {
                let callable = self.named_member_callback(base, field, span, ctx)?;
                self.bind_named_callback(air, callable, expected, span, ctx)
            }
            InstData::Call { .. } | InstData::MethodCall { .. } => Err(self.ineligible_callback(
                "a call expression".to_string(),
                NOT_A_FUNCTION_NAME,
                span,
            )),
            _ => Err(self.ineligible_callback(
                "an expression".to_string(),
                NOT_A_FUNCTION_NAME,
                span,
            )),
        }
    }

    /// Resolve an unqualified name written as a callback argument to a free
    /// function: one declared in the naming file, or a compile-time alias to
    /// one (`const CB = double;`).
    fn named_free_function_callback(
        &mut self,
        name: Spur,
        span: Span,
    ) -> CompileResult<NamedCallable> {
        let name_str = self.body_interner().resolve(&name).to_string();
        let Some(resolved) = self.resolve_callee_name(name, span.file_id) else {
            let known = *self.known_symbols();
            if name == known.print
                || name == known.println
                || name == known.eprint
                || name == known.eprintln
            {
                return Err(self.ineligible_callback(
                    format!("the builtin `{name_str}`"),
                    &format!(
                        "a builtin has no callable address; write a named function that calls `{name_str}` and pass that"
                    ),
                    span,
                ));
            }
            return Err(CompileError::new(
                ErrorKind::UndefinedFunction(name_str),
                span,
            ));
        };
        let callee = match resolved {
            ResolvedCalleeName::Local(callee) => callee,
            ResolvedCalleeName::Alias { callee, alias } => {
                self.check_item_visibility(
                    crate::PrivateItemKind::Const,
                    &name_str,
                    alias.span.file_id,
                    alias.is_pub,
                    span,
                )?;
                self.record_body_named_dependency(NamedConstDependencyTargetEvent::ValueConst {
                    file: alias.span.file_id.index(),
                    name: name_str.clone(),
                });
                callee
            }
        };
        self.free_function_callback(callee, span, true)
    }

    /// The dotted spelling of a callback argument's base (`m`, `m.S`,
    /// `outer.inner`), for diagnostics and the callable's display.
    fn member_base_display(&self, base: InstRef) -> Option<String> {
        let spine = crate::sema::decode_module_spine(self.body_rir_ref(), base)?;
        let interner = self.body_interner();
        Some(
            std::iter::once(spine.root)
                .chain(spine.fields.iter().copied())
                .map(|name| interner.resolve(&name).to_string())
                .collect::<Vec<_>>()
                .join("."),
        )
    }

    /// Resolve `module.function`, `outer.inner.function`, `Type.function`
    /// or `module.Type.function` written as a callback argument. A module
    /// member is a free function of the imported file or a public
    /// function-valued re-export (ADR-0026); a type member is a receiverless
    /// associated function of a concrete struct. The module prefix is a
    /// dotted spine resolved by the one module-path walker, so a re-export
    /// chain and a module-qualified type bind exactly as they call
    /// (RUE-1964, RUE-2197).
    fn named_member_callback(
        &mut self,
        base: InstRef,
        member: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<NamedCallable> {
        let member_str = self.body_interner().resolve(&member).to_string();
        let Some(base_str) = self.member_base_display(base) else {
            return Err(self.ineligible_callback(
                "a field access".to_string(),
                NOT_A_FUNCTION_NAME,
                span,
            ));
        };

        // `module.function` / `outer.inner.function`: the base names a
        // module — a body-local binding of module type, the file's own
        // `const m = @import(..)` binding (RUE-113), or a re-export chain.
        if let Some(module_id) = self.try_module_id_of(base, span, ctx)? {
            let module_def = self.call_facts().call_module_def(module_id);
            let module_file = module_def.file_id;
            let module_name = crate::module_display_name(&module_def.import_path);
            let mut via_reexport = false;
            let mut callee = self
                .call_facts()
                .call_resolve_function_name_local(member, module_file);
            if callee.is_none()
                && let Some(info) = self.call_facts().call_value_const(module_file, member)
                && let ConstValue::Function(key) = info.value
            {
                self.check_item_visibility(
                    crate::PrivateItemKind::Const,
                    &member_str,
                    module_file,
                    info.is_pub,
                    span,
                )?;
                callee = Some(key.spur());
                via_reexport = true;
            }
            let callee = callee
                .ok_or_else(|| crate::unknown_module_member(&module_name, &member_str, span))?;
            let fn_info = self
                .call_facts()
                .call_function_info(callee)
                .ok_or_else(|| crate::unknown_module_member(&module_name, &member_str, span))?;
            if !via_reexport && fn_info.file_id != module_file {
                return Err(crate::unknown_module_member(
                    &module_name,
                    &member_str,
                    span,
                ));
            }
            let mut callable = self.free_function_callback(callee, span, !via_reexport)?;
            callable.display = format!("{base_str}.{member_str}");
            return Ok(callable);
        }

        let display = format!("{base_str}.{member_str}");

        // `module.Type.function`: a receiverless associated function of a
        // struct the module's file defines or re-exports, under module-
        // qualified visibility (E0706) exactly as the call form (RUE-488).
        if let InstData::FieldGet {
            base: module_ref,
            field: type_name,
        } = self.body_rir_ref().get(base).data
            && let Some(module_id) = self.try_module_id_of(module_ref, span, ctx)?
        {
            let module_def = self.call_facts().call_module_def(module_id);
            let module_file = module_def.file_id;
            let module_name = crate::module_display_name(&module_def.import_path);
            let type_str = self.body_interner().resolve(&type_name).to_string();
            let selected = {
                let facts = self.aggregate_facts();
                crate::sema::select_module_type_member(facts, module_file, type_name)
            };
            let Some(nominal) = selected.as_struct() else {
                if matches!(selected, crate::sema::ModuleTypeMember::Absent) {
                    return Err(crate::unknown_module_member(&module_name, &type_str, span));
                }
                return Err(self.ineligible_callback(
                    format!("the member `{display}`"),
                    NOT_A_FUNCTION_NAME,
                    span,
                ));
            };
            let struct_id = nominal.id;
            let struct_def = self.body_type_pool().struct_def(struct_id);
            self.check_module_qualified_visibility(
                nominal.alias,
                module_file,
                (struct_def.file_id, struct_def.is_pub),
                crate::PrivateItemKind::Struct,
                &type_str,
                span,
            )?;
            return self.assoc_function_callback(struct_id, member, type_str, display, span, ctx);
        }

        // `Type.function`: a receiverless associated function of a concrete
        // struct named in this file (ADR-0046).
        if let InstData::VarRef {
            name: base_name, ..
        } = self.body_rir_ref().get(base).data
            && !self.is_runtime_value_binding(base_name, ctx)
            && let Some((struct_id, _)) = self.resolve_struct_type_name(base_name, ctx)
        {
            let struct_def = self.body_type_pool().struct_def(struct_id);
            self.check_item_visibility(
                crate::PrivateItemKind::Struct,
                &base_str,
                struct_def.file_id,
                struct_def.is_pub,
                span,
            )?;
            return self.assoc_function_callback(struct_id, member, base_str, display, span, ctx);
        }

        Err(self.ineligible_callback("a field access".to_string(), NOT_A_FUNCTION_NAME, span))
    }

    /// Classify the associated function `member` of `struct_id` as a
    /// callback: a receiverless, non-accessor, monomorphic member has a
    /// plain callable address (ADR-0096 §4).
    fn assoc_function_callback(
        &mut self,
        struct_id: crate::types::StructId,
        member: Spur,
        type_display: String,
        display: String,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<NamedCallable> {
        let member_str = self.body_interner().resolve(&member).to_string();
        let method = self
            .call_facts()
            .call_method_info(struct_id, member)
            .ok_or_compile_error(
                ErrorKind::UndefinedAssocFn {
                    type_name: type_display,
                    function_name: member_str.clone(),
                },
                span,
            )?;
        if method.has_self {
            return Err(self.ineligible_callback(
                format!("the method `{display}`"),
                "a method takes a receiver; write a named function that calls it on an explicit argument and pass that",
                span,
            ));
        }
        if method.returns_borrow || method.returns_inout {
            return Err(self.ineligible_callback(
                format!("the accessor `{display}`"),
                "an accessor is inlined at each call and has no callable address",
                span,
            ));
        }
        let params = self.body_param_data(method.params);
        if params.comptime().iter().any(|&comptime| comptime) {
            return Err(self.ineligible_callback(
                format!("the generic function `{display}`"),
                GENERIC_CALLBACK,
                span,
            ));
        }
        let signature = signature_of(params.types(), params.modes(), method.return_type);
        let symbol = self.method_symbol_handle(struct_id, &member_str, false)?;
        ctx_referenced_method(ctx, struct_id, member);
        self.record_body_method_dependency((struct_id, member))?;
        Ok(NamedCallable {
            symbol,
            display,
            signature,
        })
    }

    /// Classify a resolved free function as a callback: only an ordinary
    /// monomorphic Rue function has a plain callable address (ADR-0096 §4).
    fn free_function_callback(
        &mut self,
        callee: Spur,
        span: Span,
        check_visibility: bool,
    ) -> CompileResult<NamedCallable> {
        let source_name = self.call_facts().call_source_function_name(callee);
        let display = self.body_interner().resolve(&source_name).to_string();
        let fn_info: FunctionCallInfo = self
            .call_facts()
            .call_function_info(callee)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(display.clone()), span)?;
        if check_visibility {
            self.check_item_visibility(
                crate::PrivateItemKind::Function,
                &display,
                fn_info.file_id,
                fn_info.is_pub,
                span,
            )?;
        }
        if fn_info.is_generic {
            return Err(self.ineligible_callback(
                format!("the generic function `{display}`"),
                GENERIC_CALLBACK,
                span,
            ));
        }
        if fn_info.is_extern {
            return Err(self.ineligible_callback(
                format!("the foreign function `{display}`"),
                "an `extern \"C\"` function is called under the C convention; write a named Rue function that calls it inside a `checked` block and pass that",
                span,
            ));
        }
        if fn_info.is_unchecked {
            return Err(self.ineligible_callback(
                format!("the unchecked function `{display}`"),
                "an `unchecked` function may only be called inside a `checked` block; write a named function that does so and pass that",
                span,
            ));
        }
        if fn_info.returns_type {
            return Err(self.ineligible_callback(
                format!("the type constructor `{display}`"),
                "a `-> type` function is reduced at compile time and has no runtime body",
                span,
            ));
        }
        let params = self.body_param_data(fn_info.params);
        let signature = signature_of(params.types(), params.modes(), fn_info.return_type);
        Ok(NamedCallable {
            symbol: callee,
            display,
            signature,
        })
    }

    /// Whether a call operand names something a `fn` parameter could bind:
    /// a callback parameter of the current function or a named function. A
    /// generic callee's placeholder parameter type cannot say whether it is a
    /// `fn` type until the type arguments are known, so this decides whether
    /// the operand is bound by name or analyzed as a value.
    pub(super) fn operand_names_callable(
        &mut self,
        arg: &RirCallArg,
        ctx: &AnalysisContext,
    ) -> bool {
        let span = self.body_rir_ref().get(arg.value).span;
        match self.body_rir_ref().get(arg.value).data {
            InstData::VarRef { name, .. } => {
                if ctx.locals.contains_key(&name) {
                    return false;
                }
                if let Some(param) = ctx.param(name) {
                    return param.ty.is_function();
                }
                self.resolve_callee_name(name, span.file_id).is_some()
            }
            InstData::FieldGet { base, field } => {
                // The same three shapes `named_member_callback` binds, decided
                // without emitting: a module's function or function-valued
                // re-export, a module-qualified struct's receiverless
                // associated function, or a local struct's (RUE-2197).
                if let Ok(Some(module_id)) = self.try_module_id_of(base, span, ctx) {
                    let module_file = self.call_facts().call_module_def(module_id).file_id;
                    return self
                        .call_facts()
                        .call_resolve_function_name_local(field, module_file)
                        .is_some()
                        || self
                            .call_facts()
                            .call_value_const(module_file, field)
                            .is_some_and(|info| matches!(info.value, ConstValue::Function(_)));
                }
                if let InstData::FieldGet {
                    base: module_ref,
                    field: type_name,
                } = self.body_rir_ref().get(base).data
                    && let Ok(Some(module_id)) = self.try_module_id_of(module_ref, span, ctx)
                {
                    let module_file = self.call_facts().call_module_def(module_id).file_id;
                    let selected = {
                        let facts = self.aggregate_facts();
                        crate::sema::select_module_type_member(facts, module_file, type_name)
                    };
                    return selected
                        .as_struct()
                        .and_then(|nominal| self.call_facts().call_method_info(nominal.id, field))
                        .is_some_and(|method| !method.has_self);
                }
                let InstData::VarRef {
                    name: base_name, ..
                } = self.body_rir_ref().get(base).data
                else {
                    return false;
                };
                !self.is_runtime_value_binding(base_name, ctx)
                    && self
                        .resolve_struct_type_name(base_name, ctx)
                        .and_then(|(struct_id, _)| {
                            self.call_facts().call_method_info(struct_id, field)
                        })
                        .is_some_and(|method| !method.has_self)
            }
            _ => false,
        }
    }

    /// Check a named callable against the parameter's `fn` type and emit the
    /// `FnRef` that binds it. Naming the function makes its body reachable.
    fn bind_named_callback(
        &mut self,
        air: &mut Air,
        callable: NamedCallable,
        expected: Option<Type>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AirRef> {
        let ty = match expected {
            Some(expected) => {
                let expected_def = self.body_type_pool().function_def(
                    expected
                        .as_function()
                        .expect("callback binding is entered only for a `fn` parameter"),
                );
                if !self.callback_signature_matches(&callable.signature, &expected_def) {
                    return Err(CompileError::new(
                        ErrorKind::CallbackSignatureMismatch(Box::new(
                            rue_error::CallbackSignatureMismatchError {
                                function: callable.display,
                                expected: self.format_type_name(expected),
                                found: self.render_signature(&callable.signature),
                            },
                        )),
                        span,
                    ));
                }
                expected
            }
            None => self
                .body_type_pool()
                .try_intern_function(callable.signature.clone())
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "could not intern the signature of callback `{}`: {error:?}",
                            callable.display
                        )),
                        span,
                    )
                })?,
        };
        // The callee body is reachable through the callback exactly as
        // through a direct call (ADR-0096 §4).
        ctx.referenced_functions.insert(callable.symbol);
        #[cfg(test)]
        self.record_body_callable_dependency(callable.symbol);
        Ok(air.add_inst(AirInst {
            data: AirInstData::FnRef {
                name: callable.symbol,
            },
            ty,
            span,
        }))
    }

    /// Two signatures name the same `fn` type when they agree on arity, on
    /// the mode and type at every position, and on the result (6.1:48).
    fn callback_signature_matches(
        &self,
        found: &FunctionTypeDef,
        expected: &FunctionTypeDef,
    ) -> bool {
        found.params.len() == expected.params.len()
            && found
                .params
                .iter()
                .zip(expected.params.iter())
                .all(|(found, expected)| {
                    found.mode == expected.mode && self.types_equivalent(found.ty, expected.ty)
                })
            && self.types_equivalent(found.result, expected.result)
    }

    /// Spell a signature the way the matching `fn` type would be spelled.
    fn render_signature(&self, signature: &FunctionTypeDef) -> String {
        crate::types::function_type_name(
            signature
                .params
                .iter()
                .map(|param| (param.mode, self.format_type_name(param.ty))),
            (signature.result != Type::UNIT).then(|| self.format_type_name(signature.result)),
        )
    }

    fn ineligible_callback(&self, found: String, reason: &str, span: Span) -> CompileError {
        CompileError::new(
            ErrorKind::IneligibleCallback {
                found,
                reason: reason.to_string(),
            },
            span,
        )
    }

    /// Analyze `cb(args)` where `cb` is a callback parameter of the current
    /// function (spec 6.1:51). The call follows the parameter's `fn` type
    /// exactly as a direct call follows its declaration: the same argument
    /// count, the same explicit modes, the same exclusivity rules, and the
    /// same operand coercions. The callee value is the parameter read itself.
    pub(super) fn analyze_callback_call(
        &mut self,
        air: &mut Air,
        slot: u32,
        callback: Type,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let def = self.body_type_pool().function_def(
            callback
                .as_function()
                .expect("callback call is entered only for a `fn` parameter"),
        );
        let param_types: Vec<Type> = def.params.iter().map(|param| param.ty).collect();
        let param_modes: Vec<RirParamMode> =
            def.params.iter().map(|param| param.mode.to_rir()).collect();
        let result = def.result;
        self.validate_call_contract(args_range, &param_types, &param_modes, span, true, ctx)?;

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
            false,
            !result.is_never(),
            ctx,
        )?;
        let callee = air.add_inst(AirInst {
            data: AirInstData::Param { index: slot },
            ty: callback,
            span,
        });
        let air_ref = air.add_call_indirect(callee, &air_args, result, span)?;
        let air_ref = self.wrap_value_with_temp_scope(air, air_ref, result, span, temp_scope)?;
        if continues && result.is_never() {
            ctx.divergence_kinds
                .insert(crate::sema::context::DivergenceKind::Other);
        }
        let analysis =
            AnalysisResult::with_continues(air_ref, result, continues && !result.is_never());
        if !analysis.continues {
            ctx.ownership
                .rollback_expression_ledgers(expression_ledgers_before_call);
        }
        Ok(analysis)
    }
}

fn signature_of(types: &[Type], modes: &[RirParamMode], result: Type) -> FunctionTypeDef {
    FunctionTypeDef {
        params: types
            .iter()
            .zip(modes)
            .map(|(&ty, &mode)| FunctionTypeParam {
                mode: FunctionParamMode::from_rir(mode),
                ty,
            })
            .collect(),
        result,
    }
}

fn ctx_referenced_method(ctx: &mut AnalysisContext, struct_id: StructId, member: Spur) {
    ctx.referenced_methods.insert((struct_id, member));
}
