//! Method, module-member, and associated-function call analysis.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

impl<'a> Sema<'a> {
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
        // (RUE-256). For a user-struct method, resolution needs the receiver
        // type — peeked WITHOUT emitting AIR or recording a move, since a
        // move-based analysis would hard-reject the read of any non-local
        // place (E0437/E0429/E0904) before the by-ref intent is known — and the
        // root is by-ref only when the method takes `self` by reference
        // (RUE-254). Module receivers keep their existing post-analysis handling.
        let receiver_byref_root = if is_builtin_mutation_method {
            receiver_var
        } else {
            receiver_var.and_then(|root| {
                let ty = self.peek_place_type(receiver, ctx)?;
                let struct_id = ty.as_struct()?;
                let info = self.methods.get(&(struct_id, method))?;
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

        // Check argument count (method_info.params excludes self)
        let method_param_types = self.param_arena.types(method_info.params);
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

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

        // Analyze arguments - receiver first, then remaining args
        let mut air_args = vec![AirCallArg {
            value: receiver_result.air_ref,
            mode: receiver_mode,
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

    /// Analyze a module member call: `module.function(args)` becomes a direct function call.
    ///
    /// In Phase 1 of the module system, modules are virtual namespaces. When you import
    /// a module with `@import("foo.rue")`, all of foo.rue's functions are already in the
    /// global function table (via multi-file compilation). The module provides a
    /// namespace at the source level; `check_module_member_call` enforces that only
    /// functions defined in the imported file resolve as members.
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
        // Look up the function in the global function table. Not finding it
        // there means the module certainly has no such member.
        let fn_name_str = self.interner.resolve(&function_name).to_string();
        let module_def = self.module_registry.get_def(module_id);
        let fn_info = self
            .functions
            .get(&function_name)
            .ok_or_compile_error(
                ErrorKind::UnknownModuleMember {
                    module_name: module_def.import_path.clone(),
                    member_name: fn_name_str.clone(),
                },
                span,
            )?
            .clone();

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(function_name);

        let param_types = self.param_arena.types(fn_info.params).to_vec();
        let param_modes = self.param_arena.modes(fn_info.params).to_vec();
        let args = self.rir.get_call_args(args_start, args_len);
        let accessible = self.is_accessible(span.file_id, fn_info.file_id, fn_info.is_pub);
        check_module_member_call(
            self.rir,
            &module_def.import_path,
            self.canonical_file_id(&module_def.file_path),
            fn_info.file_id,
            &fn_name_str,
            &param_types,
            &param_modes,
            &args,
            accessible,
            span,
        )?;

        // Functions with comptime parameters need specialization: a plain
        // Call to the base name would reference a body that is never
        // analyzed (generic bodies are only materialized per specialization,
        // RUE-166). Delegate to the unqualified call path, which emits
        // CallGeneric; module membership and accessibility were checked above.
        if fn_info.is_generic {
            return self.analyze_call(air, function_name, args_start, args_len, span, ctx);
        }

        // Check for exclusive access violation. (The old, pre-deduplication
        // copy of this function skipped this check entirely.)
        self.check_exclusive_access(&args, span)?;

        // Analyze arguments (the per-pipeline recursion seam)
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        Ok(emit_module_member_call(
            air,
            function_name,
            &air_args,
            fn_info.return_type,
            span,
        ))
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
        // First check if it's a comptime type variable (e.g., `let P = Point(); P::origin()`)
        //
        // `privacy_exempt` mirrors the enum-variant construction handler: a
        // comptime-bound type (`let P = Point(); P::origin()`) arrived through a
        // binding, not by naming the struct, so it is exempt from the
        // unqualified-privacy check. A bare `Point::origin()` names the struct
        // and must obey the same uniform-privacy rule (spec 10.3:7) as a struct
        // literal or type annotation would.
        let mut privacy_exempt = false;
        let struct_id = if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
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
        } else {
            *self
                .structs
                .get(&type_name)
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
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
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
        let method_param_types = self.param_arena.types(method_info.params);
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
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
        // Use the internal struct name (e.g., "__anon_struct_0") for anonymous structs,
        // not the user-visible type variable name (e.g., "P")
        let struct_def = self.type_pool.struct_def(struct_id);
        let internal_type_name = &struct_def.name;
        let call_name = format!("{}::{}", internal_type_name, function_name_str);
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
