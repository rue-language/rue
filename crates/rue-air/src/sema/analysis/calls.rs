//! Method, module-member, and associated-function call analysis.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

impl<'a> BodySema<'a> {
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
        if let InstData::VarRef { name } = self.rir.get(receiver).data
            && !self.is_runtime_value_binding(name, ctx)
            && (self.resolve_struct_type_name(name, ctx).is_some()
                || self.resolve_enum_type_name(name, ctx).is_some()
                || ctx.comptime_type_vars.contains_key(&name))
        {
            return self.analyze_assoc_fn_call(air, name, method, args_start, args_len, span, ctx);
        }

        // Module-qualified associated-function call / tuple-variant construction:
        // `module.Type.function(args)` (RUE-488). The receiver is `module.Type`
        // (a field access) whose module member is a struct or enum type. The
        // receiver module's defining file is authoritative for the type name.
        if let InstData::FieldGet {
            base: module_ref,
            field: type_name,
        } = self.rir.get(receiver).data
            && let Some(result) = self.try_analyze_module_qualified_type_call(
                air, module_ref, type_name, method, args_start, args_len, span, ctx,
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
            .or_else(|| ctx.resolved_types.get(&receiver).copied());
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

        // Check if this is a builtin (String) mutation method. Gate the
        // name-only match on the receiver's resolved type actually being the
        // builtin: a user struct method that merely shares a name with a String
        // mutation method (`push`/`push_str`/`clear`/`reserve`) must not be
        // misclassified as a `ByMutRef` mutation and wrongly demand a `mut`
        // receiver (RUE-223). The type is read from HM inference without
        // analyzing the receiver, so ANY place receiver (a local, a struct
        // field, an array element, an inout parameter, or a chain rooted at
        // one) is recognized, not just a bare local (RUE-256).
        let receiver_is_builtin_string = ctx
            .resolved_types
            .get(&receiver)
            .copied()
            .is_some_and(|ty| self.is_builtin_string(ty));
        let is_builtin_mutation_method =
            receiver_is_builtin_string && self.is_builtin_mutation_method(&method_name_str);

        // A String mutation method is `inout self` (spec 3.10:15-19): it
        // accesses the receiver by reference and writes the result back in
        // place, so the receiver must name a MUTABLE place. Reject an immutable
        // binding up front (reusing the assignment-target diagnostics), before
        // the receiver is analyzed as a borrow below.
        if is_builtin_mutation_method {
            self.check_string_receiver_mutable(receiver_var, ctx, span)?;
        }

        // Snapshot the receiver root's move state before analyzing the
        // receiver expression: builtin ByRef/ByMutRef methods restore it to
        // undo the move the receiver analysis records (see ReceiverInfo).
        let receiver_move_state_before = receiver_var.and_then(|v| ctx.moved_vars.get(&v).cloned());

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
        let receiver_byref_root = if is_builtin_mutation_method {
            receiver_var
        } else {
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
                let known = self.has_method((struct_id, method))
                    || self
                        .get_builtin_type_def(struct_id)
                        .is_some_and(|def| def.find_method(&method_name_str).is_some());
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

                let builtin_borrow_receiver = self
                    .get_builtin_type_def(struct_id)
                    .and_then(|def| def.find_method(&method_name_str))
                    .is_some_and(|info| info.receiver_mode == rue_builtins::ReceiverMode::ByRef);
                if builtin_borrow_receiver {
                    return Some(root);
                }

                let info = self.method_info((struct_id, method))?;
                matches!(info.self_mode, RirParamMode::Inout | RirParamMode::Borrow).then_some(root)
            })
        };

        // Analyze the receiver expression. When it is a by-ref receiver,
        // `byref_arg_root` makes the var-ref / field / index reads borrow the
        // place instead of moving out of it (restored afterwards).
        let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, receiver_byref_root);
        let receiver_result = self.analyze_inst(air, receiver, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let receiver_result = receiver_result?;
        let receiver_type = receiver_result.ty;

        // The write-back target for a String mutation method is the place the
        // receiver read just produced (a `PlaceRead` for a field/element, a
        // `Load` for a local, or a `Param` for an inout parameter). Reusing
        // that place — rather than re-tracing the receiver — evaluates any
        // index expression exactly once (RUE-256).
        let receiver_storage = if is_builtin_mutation_method {
            Some(self.string_receiver_storage_from_read(air, receiver_result.air_ref, span)?)
        } else {
            None
        };

        // Handle module member access: module.function() becomes a direct function call
        if let Some(module_id) = receiver_type.as_module() {
            return self.analyze_module_member_call_impl(
                air, module_id, method, args_start, args_len, span, ctx,
            );
        }

        // Inline type-constructor call head (RUE-596, preview
        // `inline_type_ctor_paths`, relaxing spec 4.14:23): `F(args).NAME(..)`
        // where `F(args)` reduced to a concrete type at comptime. Resolve
        // `.NAME` as an enum-variant construction or associated-function call on
        // the reduced type — exactly as if it had been bound with
        // `let P = F(args); P.NAME(..)`. The receiver's stray `TypeConst` is a
        // comptime-only no-op (CFG build drops it). Only a struct/enum reduced
        // type takes this path; any other kind (e.g. `const X = i32; X.foo()`)
        // falls through to the ordinary `MethodCallOnNonStruct` diagnostic, and
        // the bound-name form (`let P = F(args); P.NAME`) never reaches here.
        // Elided args (`Option(_)`) stay out of scope (RUE-401).
        if receiver_type == Type::COMPTIME_TYPE
            && let AirInstData::TypeConst(reduced_ty) = air.get(receiver_result.air_ref).data
        {
            // The preview gate covers exactly what RUE-596 added: a CALL as
            // the path head (`F(args).NAME(..)`). A bare qualified member
            // that merely evaluates to a type (`m.Alias.new()`) is ordinary
            // module-member access and must not trip the gate — it used to,
            // with a message naming a call that doesn't exist (RUE-631).
            let receiver_is_call_head = matches!(
                self.rir.get(receiver).data,
                rue_rir::InstData::Call { .. } | rue_rir::InstData::MethodCall { .. }
            );
            match reduced_ty.kind() {
                TypeKind::Enum(enum_id) => {
                    if receiver_is_call_head {
                        self.require_preview(
                            PreviewFeature::InlineTypeCtorPath,
                            "an inline type-constructor call as a path head",
                            span,
                        )?;
                    }
                    let variant_name = self.interner.resolve(&method).to_string();
                    let def = self.type_pool.enum_def(enum_id);
                    if let Some(vidx) = def.find_variant(&variant_name) {
                        return self.analyze_enum_variant_construction(
                            air,
                            enum_id,
                            vidx as u32,
                            method,
                            true,
                            args_start,
                            args_len,
                            span,
                            ctx,
                        );
                    }
                    return Err(CompileError::new(
                        ErrorKind::UndefinedAssocFn {
                            type_name: reduced_ty.safe_name_with_pool(Some(&self.type_pool)),
                            function_name: variant_name,
                        },
                        span,
                    ));
                }
                TypeKind::Struct(struct_id) => {
                    if receiver_is_call_head {
                        self.require_preview(
                            PreviewFeature::InlineTypeCtorPath,
                            "an inline type-constructor call as a path head",
                            span,
                        )?;
                    }
                    return self.analyze_assoc_fn_call_impl(
                        air,
                        method,
                        method,
                        args_start,
                        args_len,
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
                        found: receiver_type.safe_name_with_pool(Some(&self.type_pool)),
                        method_name: method_name_str,
                    },
                    span,
                ));
            }
        };

        // Check if this is a builtin type and handle its methods
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            let method_ctx = BuiltinMethodContext {
                struct_id,
                builtin_def,
                method_name: &method_name_str,
                span,
            };
            let receiver_info = ReceiverInfo {
                result: receiver_result,
                var: receiver_var,
                move_state_before: receiver_move_state_before,
                storage: receiver_storage,
            };
            return self.analyze_builtin_method(air, ctx, &method_ctx, receiver_info, &args);
        }

        // Look up the struct name by its ID (for error messages)
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name_str = struct_def.name.clone();

        // Look up the method using StructId directly
        let method_key = (struct_id, method);
        let method_info = self.method_info(method_key).ok_or_compile_error(
            ErrorKind::UndefinedMethod {
                type_name: struct_name_str.clone(),
                method_name: method_name_str.clone(),
            },
            span,
        )?;

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
                    type_name: struct_name_str,
                    function_name: method_name_str,
                },
                span,
            ));
        }

        // Check argument count (method_info.params excludes self)
        let method_param_types = self.param_arena.types(method_info.params).to_vec();
        let method_param_modes = self.param_arena.modes(method_info.params).to_vec();
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // The receiver's autoref is implicit and deliberately excluded; only
        // explicit arguments must spell the method declaration's exact modes.
        self.validate_explicit_call_modes(&args, method_param_modes.iter().copied())?;

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

        if receiver_mode != AirArgMode::Normal {
            require_air_byref_place(
                air,
                receiver_result.air_ref,
                receiver_mode == AirArgMode::Inout,
                self.rir.get(receiver).span,
            )?;

            // The receiver must be a place (a variable or a field/index chain
            // rooted at one): codegen forms its address. A temporary (call
            // result, literal, …) has no caller-visible storage to borrow.
            let Some(receiver_root) = receiver_var else {
                return Err(CompileError::new(
                    if receiver_mode == AirArgMode::Inout {
                        ErrorKind::InoutNonLvalue
                    } else {
                        ErrorKind::BorrowNonLvalue
                    },
                    self.rir.get(receiver).span,
                ));
            };

            // Calling an `inout self` method on a collection an enclosing
            // `for` loop is iterating mutates a shared-borrowed value (spec
            // 4.8:26, RUE-257) — E0428. A `borrow self` method only reads it,
            // which coexists with the loop's shared borrow, so it is allowed.
            if receiver_mode == AirArgMode::Inout {
                self.reject_mutate_iter_borrowed(receiver_root, span, ctx)?;
            }

            // `inout self` requires a mutable receiver binding (spec 6, reuses
            // E0203), mirroring Rust. `borrow self` works on any binding.
            if receiver_mode == AirArgMode::Inout
                && !self.receiver_root_is_mutable(receiver_root, ctx)
            {
                let name = self.interner.resolve(&receiver_root).to_string();
                return Err(
                    CompileError::new(ErrorKind::AssignToImmutable(name.clone()), span).with_help(
                        format!(
                            "`inout self` needs a mutable receiver; make the binding \
                     mutable: `let mut {name} = ...`"
                        ),
                    ),
                );
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
            excl_args.extend(args.iter().cloned());
            self.check_exclusive_access(&excl_args, span)?;

            // By-ref receivers are borrows, not moves. The receiver was
            // already analyzed under `byref_arg_root` above (RUE-254), so no
            // move was recorded and no marker emitted; this restore of the
            // pre-receiver snapshot and marker cancellation are defensive
            // no-ops for a well-formed place receiver (and still cover the
            // now-vestigial path where the receiver root differs from the
            // byref root). Mirrors the builtin ByRef/ByMutRef handling.
            match receiver_move_state_before.clone() {
                Some(state) => {
                    ctx.moved_vars.insert(receiver_root, state);
                }
                None => {
                    ctx.moved_vars.remove(&receiver_root);
                }
            }
            air.cancel_move_marker(receiver_result.air_ref);
        } else {
            // Check for exclusive access violation (by-value receiver)
            self.check_exclusive_access(&args, span)?;
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
        let receiver_frame = match (receiver_mode, receiver_var) {
            (AirArgMode::Inout, Some(root)) => Some(vec![(root, CallLoanKind::Inout)]),
            (AirArgMode::Borrow, Some(root)) => Some(vec![(root, CallLoanKind::Borrow)]),
            _ => None,
        };
        let receiver_frame_pushed = receiver_frame.is_some();
        if let Some(frame) = receiver_frame {
            ctx.call_loaned_roots.push(frame);
        }
        let args_result = self.analyze_call_args_coerced(
            air,
            &args,
            &method_param_types,
            &method_param_modes,
            ctx,
        );
        if receiver_frame_pushed {
            ctx.call_loaned_roots.pop();
        }
        air_args.extend(args_result?);

        // Generate the method call symbol: `Type.method`, file-qualified when
        // the type name spans files (RUE-571) — must match the definition
        // side, which builds its name through the same helper.
        let call_name = self.method_symbol(struct_id, &method_name_str, true);
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
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let fn_name_str = self.interner.resolve(&function_name).to_string();
        let module_def = self.module_registry.get_def(module_id);
        let module_file_id = self.canonical_file_id(&module_def.file_path);
        let mut function_key = module_file_id
            .and_then(|file_id| self.resolve_function_name_local(function_name, file_id));

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
                .constants_by_file_name
                .get(&(mfile, function_name))
                .and_then(|info| match info.value {
                    ConstValue::Function(fkey) => Some((fkey, info.is_pub)),
                    _ => None,
                });
            if let Some((fkey, is_pub)) = reexport {
                if !self.is_accessible(span.file_id, mfile, is_pub) {
                    return Err(CompileError::new(
                        ErrorKind::PrivateMemberAccess {
                            item_kind: "const".to_string(),
                            name: fn_name_str.clone(),
                        },
                        span,
                    ));
                }
                function_key = Some(fkey);
                via_reexport = true;
            }
        }

        let function_key = function_key.ok_or_else(|| {
            CompileError::new(
                ErrorKind::UnknownModuleMember {
                    module_name: module_def.import_path.clone(),
                    member_name: fn_name_str.clone(),
                },
                span,
            )
        })?;
        let fn_info = self
            .functions
            .get(&function_key)
            .ok_or_compile_error(
                ErrorKind::UnknownModuleMember {
                    module_name: module_def.import_path.clone(),
                    member_name: fn_name_str.clone(),
                },
                span,
            )?
            .clone();

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(function_key);

        let param_types = self.param_arena.types(fn_info.params).to_vec();
        let param_modes = self.param_arena.modes(fn_info.params).to_vec();
        let args = self.rir.get_call_args(args_start, args_len);
        // A re-export was already visibility-checked against its facade const.
        let accessible =
            via_reexport || self.is_accessible(span.file_id, fn_info.file_id, fn_info.is_pub);
        check_module_member_call(
            &module_def.import_path,
            module_file_id,
            fn_info.file_id,
            &fn_name_str,
            &param_types,
            &args,
            accessible,
            via_reexport,
            span,
        )?;

        // Functions with comptime parameters need specialization: a plain
        // Call to the base name would reference a body that is never
        // analyzed (generic bodies are only materialized per specialization,
        // RUE-166). Use the already-resolved call path so module-qualified
        // type constructors do not re-enter unqualified source-name lookup;
        // module membership and accessibility were checked above.
        if fn_info.is_generic {
            return self.analyze_resolved_function_call(
                air,
                function_key,
                fn_info,
                args_start,
                args_len,
                span,
                ctx,
                false,
            );
        }

        // Generic members delegate to the resolved-call path above, which
        // performs the same exact source-mode validation. Non-generic members
        // validate here before treating any explicit marker as a loan/place.
        self.validate_explicit_call_modes(&args, param_modes.iter().copied())?;

        // Check for exclusive access violation. (The old, pre-deduplication
        // copy of this function skipped this check entirely.)
        self.check_exclusive_access(&args, span)?;

        // Analyze arguments (the per-pipeline recursion seam). Module-qualified
        // calls use the coercing path so slice and `borrow str` parameters
        // materialize their by-value fat-pointer views exactly like direct
        // calls do (RUE-559) — std functions taking `borrow s: str` are called
        // this way.
        let air_args =
            self.analyze_call_args_coerced(air, &args, &param_types, &param_modes, ctx)?;

        // Inference cannot recover the defining file for a function-local
        // `let m = @import(...)`: the intrinsic deliberately carries an
        // unresolved module sentinel until semantic analysis resolves the
        // path. We have the exact member and its parameter types here, so make
        // this path authoritative too instead of silently accepting a bad
        // argument when inference could not add the constraint.
        for ((arg, air_arg), expected) in args.iter().zip(&air_args).zip(&param_types) {
            let found = air.get(air_arg.value).ty;
            if !found.can_coerce_to(expected) {
                return Err(self.type_mismatch_error(
                    *expected,
                    found,
                    self.rir.get(arg.value).span,
                ));
            }
        }

        Ok(emit_module_member_call(
            air,
            function_key,
            &air_args,
            fn_info.return_type,
            span,
        ))
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
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
        resolved: Option<StructId>,
    ) -> CompileResult<AnalysisResult> {
        let args = self.rir.get_call_args(args_start, args_len);
        let type_name_str = self.interner.resolve(&type_name).to_string();
        let function_name_str = self.interner.resolve(&function).to_string();

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
                            found: ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            }
        } else if let Some(info) = self
            .constants_by_file_name
            .get(&(ctx.current_file_id, type_name))
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
                            found: ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            }
        } else {
            // Module-local first, then builtins (RUE-525) — never the global
            // by-name table: an unqualified reference to another file's type
            // is name-not-found, matching every other unqualified form.
            self.structs_by_file_name
                .get(&(ctx.current_file_id, type_name))
                .copied()
                .or_else(|| self.resolve_builtin_struct_name(type_name))
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.clone()), span)?
        };

        // Privacy (E0460, RUE-330): naming a private struct to call one of its
        // associated functions (`Secret::make()`) across a directory boundary is
        // rejected, matching struct-literal / type-annotation references. Privacy
        // is uniform across item kinds (spec 10.3:1, 10.3:7). Builtin structs
        // (String, ...) have no source path, so `is_accessible` is permissive and
        // this is a no-op for them.
        if !privacy_exempt {
            let struct_def = self.type_pool.struct_def(struct_id);
            self.check_unqualified_visibility(
                "struct",
                &type_name_str,
                struct_def.file_id,
                struct_def.is_pub,
                span,
            )?;
        }

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

        // Look up the function using StructId
        let method_key = (struct_id, function);
        let method_info = self.method_info(method_key).ok_or_compile_error(
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

        // Check argument count
        let method_param_types = self.param_arena.types(method_info.params).to_vec();
        let method_param_modes = self.param_arena.modes(method_info.params).to_vec();
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        self.validate_explicit_call_modes(&args, method_param_modes.iter().copied())?;

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze explicit arguments through the same representation-aware
        // path as free and module-member calls. In particular, `borrow str`
        // and `[T]` parameters are physical by-value views even though their
        // source modes remain Borrow (RUE-634).
        let air_args = self.analyze_call_args_coerced(
            air,
            &args,
            &method_param_types,
            &method_param_modes,
            ctx,
        )?;

        // Generate the associated-function call symbol: `Type::function`.
        // Uses the internal struct name (e.g. "__anon_struct_0") for anonymous
        // structs — not the user-visible type variable name — and the
        // file-qualified name when the type name spans files (RUE-571),
        // matching the definition side.
        let call_name = self.method_symbol(struct_id, &function_name_str, false);
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
}
