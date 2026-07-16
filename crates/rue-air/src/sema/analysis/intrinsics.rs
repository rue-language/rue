//! Compiler intrinsic analysis (dbg/drop/cast/panic/assert/parse/import/... non-pointer intrinsics).
//!
//! This category owns non-pointer intrinsic analysis within the canonical
//! semantic-analysis implementation.

use super::*;

impl<'a> BodySema<'a> {
    /// Implementation for Intrinsic calls.
    pub(crate) fn analyze_intrinsic_impl(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        result_expected: Option<Type>,
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
        let known = &self.known;

        // Raw-pointer and heap intrinsics are unchecked operations: they may
        // only be used inside a `checked` block (spec 9.1:1, chapter 9). Gate
        // them before the per-intrinsic dispatch so every one shares a single
        // diagnostic. `@syscall` has its own gating; the set here is
        // @raw/@raw_mut/@field_ptr/@ptr_read/@ptr_write/@ptr_offset/
        // @ptr_to_int/@int_to_ptr plus the heap intrinsics
        // @alloc/@free/@realloc (RUE-1, RUE-301).
        if ctx.checked_depth == 0
            && (name == known.ptr_read
                || name == known.ptr_write
                || name == known.ptr_offset
                || name == known.ptr_to_int
                || name == known.int_to_ptr
                || name == known.raw
                || name == known.raw_mut
                || name == known.field_ptr
                || name == known.alloc
                || name == known.free
                || name == known.realloc
                || name == known.alloc_bytes
                || name == known.realloc_bytes
                || name == known.free_bytes
                || name == known.byte_read
                || name == known.byte_write)
        {
            let intrinsic_name_str = self.interner.resolve(&name);
            let kind = if name == known.alloc
                || name == known.free
                || name == known.realloc
                || name == known.alloc_bytes
                || name == known.realloc_bytes
                || name == known.free_bytes
            {
                "heap"
            } else {
                "raw-pointer"
            };
            return Err(CompileError::new(
                ErrorKind::UncheckedOpRequiresChecked {
                    what: format!("{kind} intrinsic `@{intrinsic_name_str}`"),
                },
                span,
            )
            .with_help("wrap the operation in a `checked { ... }` block"));
        }

        // Use pre-interned symbol comparison instead of string comparison
        if name == known.dbg {
            self.analyze_dbg_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.drop {
            self.analyze_drop_intrinsic(air, &args, span, ctx)
        } else if name == known.int_cast {
            self.analyze_intcast_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.test_preview_gate {
            self.analyze_test_preview_gate_intrinsic(air, &args, span)
        } else if name == known.read_line {
            self.analyze_read_line_intrinsic(air, name, inst_ref, &args, span, result_expected, ctx)
        } else if name == known.to_string {
            self.analyze_to_string_intrinsic(air, &args, span, ctx)
        } else if let Some(intrinsic_name_str) = known.get_parse_intrinsic_name(name) {
            self.analyze_parse_intrinsic(
                air,
                name,
                inst_ref,
                intrinsic_name_str,
                &args,
                span,
                result_expected,
                ctx,
            )
        } else if name == known.cast {
            self.analyze_cast_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.panic {
            self.analyze_panic_intrinsic(air, &args, span, ctx)
        } else if name == known.assert {
            self.analyze_assert_intrinsic(air, &args, span, ctx)
        } else if name == known.import {
            self.analyze_import_intrinsic(air, &args, span)
        } else if name == known.random_u32 {
            self.analyze_random_u32_intrinsic(air, name, &args, span)
        } else if name == known.random_u64 {
            self.analyze_random_u64_intrinsic(air, name, &args, span)
        } else if name == known.ptr_read {
            self.analyze_ptr_read_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.ptr_write {
            self.analyze_ptr_write_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_offset {
            self.analyze_ptr_offset_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_to_int {
            self.analyze_ptr_to_int_intrinsic(air, name, &args, span, ctx)
        } else if name == known.int_to_ptr {
            self.analyze_int_to_ptr_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.alloc {
            self.analyze_alloc_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.free {
            self.analyze_free_intrinsic(air, name, &args, span, ctx)
        } else if name == known.realloc {
            self.analyze_realloc_intrinsic(air, name, &args, span, ctx)
        } else if name == known.alloc_bytes {
            self.analyze_alloc_bytes_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.realloc_bytes {
            self.analyze_realloc_bytes_intrinsic(air, name, &args, span, ctx)
        } else if name == known.free_bytes {
            self.analyze_free_bytes_intrinsic(air, name, &args, span, ctx)
        } else if name == known.byte_read {
            self.analyze_byte_read_intrinsic(air, name, &args, span, ctx)
        } else if name == known.byte_write {
            self.analyze_byte_write_intrinsic(air, name, &args, span, ctx)
        } else if name == known.raw {
            let raw = self.known.raw;
            self.analyze_addr_of_intrinsic(air, &args, span, ctx, false, raw, "addr_of")
        } else if name == known.raw_mut {
            let raw_mut = self.known.raw_mut;
            self.analyze_addr_of_intrinsic(air, &args, span, ctx, true, raw_mut, "addr_of_mut")
        } else if name == known.field_ptr {
            self.analyze_field_ptr_intrinsic(air, &args, span, ctx)
        } else if name == known.syscall {
            self.analyze_syscall_intrinsic(air, name, &args, span, ctx)
        } else if name == known.target_arch {
            self.analyze_target_arch_intrinsic(air, &args, span)
        } else if name == known.target_os {
            self.analyze_target_os_intrinsic(air, &args, span)
        } else {
            Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(self.interner.resolve(&name).to_string()),
                span,
            ))
        }
    }

    pub(crate) fn analyze_internal_intrinsic_impl(
        &mut self,
        air: &mut Air,
        intrinsic: rue_rir::InternalIntrinsic,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args_len != intrinsic.arity() {
            let argument = if intrinsic.arity() == 1 {
                "argument"
            } else {
                "arguments"
            };
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "internal intrinsic `{}` expects {} {}, found {}",
                    intrinsic.as_str(),
                    intrinsic.arity(),
                    argument,
                    args_len
                )),
                span,
            ));
        }

        let args: Vec<RirCallArg> = self
            .rir
            .get_inst_refs(args_start, args_len)
            .into_iter()
            .map(|value| RirCallArg {
                value,
                mode: RirArgMode::Normal,
            })
            .collect();

        match intrinsic {
            rue_rir::InternalIntrinsic::IterLen => {
                self.analyze_iter_len_intrinsic(air, &args, span, ctx)
            }
            rue_rir::InternalIntrinsic::CharScalar => {
                self.analyze_string_char_op_intrinsic(air, &args, span, ctx, false, false)
            }
            rue_rir::InternalIntrinsic::CharNext => {
                self.analyze_string_char_op_intrinsic(air, &args, span, ctx, true, false)
            }
            rue_rir::InternalIntrinsic::CharScalarLossy => {
                self.analyze_string_char_op_intrinsic(air, &args, span, ctx, false, true)
            }
            rue_rir::InternalIntrinsic::CharNextLossy => {
                self.analyze_string_char_op_intrinsic(air, &args, span, ctx, true, true)
            }
        }
    }

    // ========================================================================
    // `for`-loop iteration intrinsics (RUE-220)
    //
    // These three intrinsics are synthesized ONLY by the `for`-loop desugaring
    // in AstGen (`gen_for`); they are the type-dependent leaves of the
    // desugaring, resolved here once the collection's type is known. Reading
    // the collection is a scoped borrow (ADR-0037): the move state is snapshot
    // and restored so iterating does not consume the source.
    // ========================================================================

    /// [`rue_rir::InternalIntrinsic::IterLen`] computes the loop bound
    /// (`usize`), dispatching the
    /// iterable kind by the collection's type: an array's length `N` (a
    /// compile-time constant), or a StrBuf's source-defined byte length. This
    /// is where the whole `for` loop is preview-gated (RUE-220):
    /// every for-loop emits exactly one `IterLen`.
    pub(super) fn analyze_iter_len_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let coll = args[0].value;
        let coll_result = self.analyze_borrowed_collection(air, coll, ctx)?;
        let coll_type = coll_result.ty;

        // Array: the bound is the compile-time length N.
        if let Some(array_id) = coll_type.as_array() {
            let (_elem, len) = self.type_pool.array_def(array_id);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Const(len),
                ty: Type::U64,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::U64));
        }

        // String byte view: the bound is the byte length `s.len()`.
        if self.is_strbuf(coll_type) {
            let source_method = coll_type.as_struct().and_then(|struct_id| {
                let method = self.interner.get_or_intern("len");
                (self.type_pool.struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
                    && self.method_info((struct_id, method)).is_some())
                .then_some((struct_id, method))
            });
            let Some((struct_id, method)) = source_method else {
                return Err(CompileError::new(
                    ErrorKind::InternalError(
                        "trusted std StrBuf is missing its source `len` method".to_string(),
                    ),
                    span,
                ));
            };
            ctx.referenced_methods.insert((struct_id, method));
            let (receiver, temp_scope) = self.materialize_borrow_argument(
                air,
                coll_result.air_ref,
                coll_result.ty,
                span,
                ctx,
            )?;
            let call_name = self
                .interner
                .get_or_intern(&self.method_symbol(struct_id, "len", true));
            let mode = AirArgMode::Borrow;
            let extra = [receiver.as_u32(), mode.as_u32()];
            let args_start = air.add_extra(&extra);
            let call_ref = air.add_inst(AirInst {
                data: AirInstData::Call {
                    runtime: None,
                    name: call_name,
                    args_start,
                    args_len: 1,
                },
                ty: Type::U64,
                span,
            });
            let call_ref =
                self.wrap_value_with_temp_scope(air, call_ref, Type::U64, span, temp_scope);
            return Ok(AnalysisResult::new(call_ref, Type::U64));
        }

        if coll_type.is_error() {
            return Ok(AnalysisResult::new(coll_result.air_ref, Type::U64));
        }

        Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "an array or a StrBuf".to_string(),
                found: coll_type.safe_name_with_pool(Some(&self.type_pool)),
            },
            span,
        )
        .with_help(
            "`for` can iterate an array, a StrBuf's bytes, or a StrBuf's \
             `.chars()` view",
        ))
    }

    /// `CharScalar` computes the Unicode scalar (`u32`) at byte `offset`, and
    /// `CharNext` computes the byte offset of the
    /// next character (`usize`). Both back `for c in s.chars()`; the receiver
    /// must be a String. When `lossy` is false these trap at runtime on invalid
    /// UTF-8; when true (`.chars_lossy()`) they substitute U+FFFD instead
    /// (ADR-0035).
    pub(super) fn analyze_string_char_op_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        is_next: bool,
        lossy: bool,
    ) -> CompileResult<AnalysisResult> {
        let coll = args[0].value;
        let pos = args[1].value;

        let coll_result = self.analyze_borrowed_collection(air, coll, ctx)?;
        if !self.is_strbuf(coll_result.ty) && !coll_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "StrBuf".to_string(),
                    found: coll_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                span,
            )
            .with_help("`.chars()` iteration is only available on a StrBuf"));
        }

        let pos_result = self.analyze_inst(air, pos, ctx)?;

        if coll_result.ty.is_error() {
            return Ok(AnalysisResult::new(
                coll_result.air_ref,
                if is_next { Type::U64 } else { Type::U32 },
            ));
        }
        let (fn_name, ret_ty) = match (is_next, lossy) {
            (true, false) => ("__rue_str_char_next", Type::U64),
            (true, true) => ("__rue_str_char_next_lossy", Type::U64),
            (false, false) => ("__rue_str_char_scalar", Type::U32),
            (false, true) => ("__rue_str_char_scalar_lossy", Type::U32),
        };
        let call_name = self.interner.get_or_intern(fn_name);
        let (ptr, len, temp_scope) =
            self.project_strbuf_text_fields(air, coll_result.air_ref, coll_result.ty, span, ctx)?;
        let extra = vec![
            ptr.as_u32(),
            AirArgMode::Normal.as_u32(),
            len.as_u32(),
            AirArgMode::Normal.as_u32(),
            pos_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
        ];
        let args_start = air.add_extra(&extra);
        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                runtime: Some(match (is_next, lossy) {
                    (true, false) => crate::RuntimeCallKind::StrCharNext,
                    (true, true) => crate::RuntimeCallKind::StrCharNextLossy,
                    (false, false) => crate::RuntimeCallKind::StrCharScalar,
                    (false, true) => crate::RuntimeCallKind::StrCharScalarLossy,
                }),
                name: call_name,
                args_start,
                args_len: 3,
            },
            ty: ret_ty,
            span,
        });
        let call_ref = self.wrap_value_with_temp_scope(air, call_ref, ret_ty, span, temp_scope);
        Ok(AnalysisResult::new(call_ref, ret_ty))
    }

    /// Analyze a for-loop's collection operand as a scoped borrow: the value is
    /// read but its move state is restored afterward, so referencing it each
    /// iteration (for the bound, the element, and the advance) does not consume
    /// the source and it remains usable after the loop.
    pub(super) fn analyze_borrowed_collection(
        &mut self,
        air: &mut Air,
        coll: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let root = self.extract_root_variable(coll);
        let move_before = root.and_then(|v| ctx.moved_vars.get(&v).cloned());
        let coll_result = self.analyze_inst(air, coll, ctx)?;
        if let Some(var) = root {
            match move_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(coll_result.air_ref);
        Ok(coll_result)
    }

    // Helper methods for intrinsic analysis (delegated from analyze_intrinsic_impl)

    pub(super) fn analyze_dbg_intrinsic(
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

        // @dbg borrows its argument — it prints the value without consuming it,
        // so a non-Copy argument (e.g. a String) remains usable afterwards and
        // is dropped by its owner at scope exit (RUE-21). When the argument is
        // a place rooted at a variable, set `byref_arg_root` so the var-ref /
        // place analyses treat the read as a borrow and skip move tracking,
        // exactly as they do for a `borrow`-mode call argument. A non-place
        // argument (literal, arithmetic, call result) has no owning variable to
        // preserve, so it is analyzed normally.
        let byref_root = root_variable_of(self.rir, args[0].value);
        let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
        let arg_result = self.analyze_inst(air, args[0].value, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let arg_result = arg_result?;
        let arg_type = arg_result.ty;

        // An `<error>`-typed argument reaching here via `Ok` means no
        // diagnostic was emitted for it (sema errors propagate as `Err`), so
        // type inference failed silently. Report a proper internal error
        // instead of letting codegen hit its `unreachable!` (RUE-149).
        if arg_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "@dbg argument type failed to resolve during inference".to_string(),
                ),
                span,
            ));
        }

        // Validate type: @dbg supports integers, bool, and String (spec 4.13:7).
        // Structs, enums, and arrays must be rejected HERE — codegen has no
        // lowering for them and would panic ("@dbg only supports scalars and
        // strings"), which the spec mandates as a compile error instead.
        if !arg_type.is_integer()
            && arg_type != Type::BOOL
            && !self.is_strbuf(arg_type)
            && !self.is_str_like(arg_type)
            && !arg_type.is_never()
        {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "dbg".to_string(),
                    expected: "integer, bool, or String".to_string(),
                    found: arg_type.safe_name_with_pool(Some(&self.type_pool)),
                })),
                span,
            ));
        }

        let source_strbuf = arg_type.as_struct().is_some_and(|struct_id| {
            self.type_pool.struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        let (arg_ref, temp_scope) = if source_strbuf {
            self.materialize_borrow_argument(air, arg_result.air_ref, arg_type, span, ctx)?
        } else {
            (arg_result.air_ref, Vec::new())
        };
        let args_start = air.add_extra(&[arg_ref.as_u32()]);
        let intrinsic_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(if arg_type == Type::BOOL {
                    crate::RuntimeCallKind::DebugBool
                } else if self.is_strbuf(arg_type) || self.is_str_like(arg_type) {
                    crate::RuntimeCallKind::DebugStr
                } else if arg_type.is_signed() {
                    crate::RuntimeCallKind::DebugI64
                } else {
                    crate::RuntimeCallKind::DebugU64
                }),
                name: self.known.dbg,
                args_start,
                args_len: 1,
            },
            ty: Type::UNIT,
            span,
        });
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, Type::UNIT, span, temp_scope);
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze the `@drop(x)` intentional-destroy intrinsic (RUE-187,
    /// ADR-0039).
    ///
    /// `@drop(x)` runs `x`'s full drop glue (destructor + recursive field
    /// drops) at this site AND discharges `x`'s consumption obligation:
    ///
    /// - *linear* — the operand is consumed like any move, so the
    ///   must-consume check (E0406) is satisfied and reusing `x` afterwards is
    ///   a use-after-move (E0205);
    /// - *affine with a destructor* — the glue runs EARLY (deterministic
    ///   cleanup before scope exit), and the suppressed scope-exit drop keeps
    ///   the "dropped exactly once" invariant;
    /// - *Copy* — no move, no glue: a no-op.
    ///
    /// The obligation-discharge rides on the ordinary move machinery: analyzing
    /// the operand as a by-value use marks its slot moved (so drop elaboration
    /// skips the scope-exit drop, RUE-61) and records the consumption. The
    /// emitted `Drop` reuses the exact glue path that scope-exit drops use, so
    /// both backends already lower it and no `unchecked` context is required
    /// (drop glue is memory-safe).
    pub(super) fn analyze_drop_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "drop".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the operand as an ordinary by-value use: for a variable this
        // emits a `MarkMoved`-wrapped load, which both records the move (later
        // use -> E0205, and the linear must-consume obligation is satisfied)
        // and tells drop elaboration to skip the slot's scope-exit drop.
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // An `<error>`-typed operand reaching here via `Ok` means inference
        // failed silently with no diagnostic; report it rather than letting a
        // later stage hit an `unreachable!` (mirrors @dbg, RUE-149).
        if arg_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "@drop operand type failed to resolve during inference".to_string(),
                ),
                span,
            ));
        }

        // Emit the drop glue at this site. The CFG builder elides the `Drop`
        // for trivially droppable types (so `@drop` of a Copy value or a
        // glue-free linear marker is a pure no-op beyond discharging the
        // obligation), and otherwise lowers it through the same destructor +
        // field-drop path as a scope-exit drop.
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Drop {
                value: arg_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    pub(super) fn analyze_cast_intrinsic(
        &mut self,
        air: &mut Air,
        _inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // `@cast` has no inference rule and is redundant with `@intCast`
        // (RUE-319). Reject it up front with a clear pointer to the working
        // intrinsic. Inference gives `@cast` a
        // fresh type variable (like `@intCast`) precisely so this diagnostic is
        // what the user sees, in every context, instead of a masking
        // type-mismatch error. Arguments are still analyzed so any error inside
        // them is reported too.
        for arg in args {
            let _ = self.analyze_inst(air, arg.value, ctx);
        }
        Err(
            CompileError::new(ErrorKind::UnknownIntrinsic("cast".to_string()), span)
                .with_help("use `@intCast` to cast between integer types"),
        )
    }

    pub(super) fn analyze_panic_intrinsic(
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
            // Panic with no message: still a real trap (RUE-319). Emitting an
            // Intrinsic with zero args (not a UnitConst) is what makes cfg_lower
            // lower it to the `__rue_panic_no_msg` abort call instead of a
            // silent no-op. `@panic` has type `!` (never): it diverges and never
            // returns, so it participates in never coercion just like a `-> !`
            // call, `return`, or `break` (spec 3.4:2, 4.13:5b; RUE-512).
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Intrinsic {
                    runtime: Some(crate::RuntimeCallKind::PanicNoMessage),
                    name: self.known.panic,
                    args_start: 0,
                    args_len: 0,
                },
                ty: Type::NEVER,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::NEVER));
        }

        // Panic borrows its message until the runtime call. Give a canonical
        // source StrBuf rvalue an addressable owner for that whole call.
        let arg_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        self.validate_abort_message_type("panic", arg_result.ty, self.rir.get(args[0].value).span)?;

        let source_strbuf = arg_result.ty.as_struct().is_some_and(|struct_id| {
            self.type_pool.struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        let (arg_ref, temp_scope) = if source_strbuf {
            self.materialize_borrow_argument(air, arg_result.air_ref, arg_result.ty, span, ctx)?
        } else {
            (arg_result.air_ref, Vec::new())
        };
        let args_start = air.add_extra(&[arg_ref.as_u32()]);
        let intrinsic_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::Panic),
                name: self.known.panic,
                args_start,
                args_len: 1,
            },
            ty: Type::NEVER,
            span,
        });
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, Type::NEVER, span, temp_scope);
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
    }

    pub(super) fn analyze_assert_intrinsic(
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
        let cond_span = self.rir.get(args[0].value).span;
        if cond_result.ty != Type::BOOL && !cond_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "assert".to_string(),
                    expected: "bool condition".to_string(),
                    found: cond_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                })),
                cond_span,
            ));
        }

        // Build args for AIR
        let mut extra_data = vec![cond_result.air_ref.as_u32()];
        let mut temp_scope = Vec::new();
        if args.len() > 1 {
            let msg_result = self.analyze_inst_for_projection(air, args[1].value, ctx)?;
            self.validate_abort_message_type(
                "assert",
                msg_result.ty,
                self.rir.get(args[1].value).span,
            )?;
            let source_strbuf = msg_result.ty.as_struct().is_some_and(|struct_id| {
                self.type_pool.struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
            });
            let msg_ref = if source_strbuf {
                let (msg_ref, scope) = self.materialize_borrow_argument(
                    air,
                    msg_result.air_ref,
                    msg_result.ty,
                    span,
                    ctx,
                )?;
                temp_scope = scope;
                msg_ref
            } else {
                msg_result.air_ref
            };
            extra_data.push(msg_ref.as_u32());
            if !temp_scope.is_empty() {
                // The message's hoisted owner scope must not jump ahead of the
                // condition. Lower it first; the intrinsic reuses the cached
                // value after the message has been evaluated.
                temp_scope.insert(0, cond_result.air_ref);
            }
        }

        let args_len = extra_data.len() as u32;
        let args_start = air.add_extra(&extra_data);
        let intrinsic_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(if args.len() > 1 {
                    crate::RuntimeCallKind::AssertWithMessage
                } else {
                    crate::RuntimeCallKind::AssertFailed
                }),
                name: self.known.assert,
                args_start,
                args_len,
            },
            ty: Type::UNIT,
            span,
        });
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, Type::UNIT, span, temp_scope);
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Validate the shared message contract of `@panic` and `@assert`.
    ///
    /// The runtime ABI consumes the canonical text family through its trusted
    /// pointer-and-length representation. Checking text identity here is
    /// important: accepting an arbitrary aggregate with a convenient slot
    /// count would let codegen reinterpret unrelated fields as text.
    fn validate_abort_message_type(
        &self,
        intrinsic_name: &str,
        message_ty: Type,
        message_span: Span,
    ) -> CompileResult<()> {
        if !self.is_strbuf(message_ty) && !self.is_str_like(message_ty) && !message_ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: intrinsic_name.to_string(),
                    expected: "text message".to_string(),
                    found: message_ty.safe_name_with_pool(Some(&self.type_pool)),
                })),
                message_span,
            ));
        }

        Ok(())
    }

    /// Analyze @intCast intrinsic.
    pub(super) fn analyze_intcast_intrinsic(
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
                    found: from_ty.safe_name_with_pool(Some(&self.type_pool)),
                })),
                span,
            ));
        }

        // Get the target type from HM inference
        let target_ty = match ctx.resolved_types.get(&inst_ref).copied() {
            Some(ty) if ty.is_integer() => ty,
            Some(Type::ERROR) => {
                // The target type variable decayed to `<error>` with no
                // constraint to fix it (e.g. the result flows only into a
                // type-polymorphic sink like `@dbg`). Report the actual problem
                // — the cast target can't be inferred — rather than an
                // array-specific "empty array" diagnostic (RUE-588).
                return Err(CompileError::new(
                    ErrorKind::CannotInferCastTarget(intrinsic_name.to_string()),
                    span,
                ));
            }
            Some(ty) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: intrinsic_name.to_string(),
                        expected: "integer".to_string(),
                        found: ty.safe_name_with_pool(Some(&self.type_pool)),
                    })),
                    span,
                ));
            }
            None => {
                // Type inference couldn't determine the target type (RUE-588).
                return Err(CompileError::new(
                    ErrorKind::CannotInferCastTarget(intrinsic_name.to_string()),
                    span,
                ));
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
    pub(super) fn analyze_test_preview_gate_intrinsic(
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
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Validate that inference resolved a fallible intrinsic's result to an
    /// `Option`-shaped enum whose `Some` variant carries `expected_payload` and
    /// whose `None` variant is empty, and return that enum type.
    ///
    /// This is how `@read_line` / `@parse_*` obtain the concrete library
    /// `Option` from context (RUE-6, ADR-0038): the intrinsic's return unifies
    /// with the in-scope `Option(T)` (a `let` annotation, or the match arms the
    /// result feeds), so no new "blessed builtin" tier is introduced — the
    /// ordinary comptime-generic `Option` enum is used. The runtime signals
    /// success/failure and codegen fills in this enum's discriminant + payload.
    pub(super) fn resolve_option_result_type(
        &mut self,
        ctx: &AnalysisContext,
        result_expected: Option<Type>,
        inst_ref: InstRef,
        expected_payload: Type,
        intrinsic_display: &str,
        payload_display: &str,
        span: Span,
    ) -> CompileResult<Type> {
        // Prefer the sema-supplied expected type (a let annotation or match
        // scrutinee pattern), which is how the concrete comptime-generic
        // `Option` reaches us. Without one, there is no context to infer the
        // Option type from — report that plainly rather than letting an
        // unresolved inference variable decay to `<error>` (E9000).
        let (ty, from_expected) = match result_expected {
            Some(ty) => (ty, true),
            None => (
                Self::get_resolved_type(ctx, inst_ref, span, intrinsic_display)?,
                false,
            ),
        };

        // A bad annotation/pattern already produced its own diagnostic and
        // poisoned the expected type to `error`; do not cascade a second one.
        if from_expected && ty.is_error() {
            return Ok(ty);
        }

        let valid = if let TypeKind::Enum(enum_id) = ty.kind() {
            let def = self.type_pool.enum_def(enum_id);
            match (def.find_variant("Some"), def.find_variant("None")) {
                (Some(si), Some(ni)) if def.variant_count() == 2 => {
                    let some_payload = def.variant_payload(si);
                    let none_payload = def.variant_payload(ni);
                    some_payload.len() == 1
                        && some_payload[0] == expected_payload
                        && none_payload.is_empty()
                }
                _ => false,
            }
        } else {
            false
        };

        if valid {
            return Ok(ty);
        }

        // As the operand of `?` on a bare fallible intrinsic, no valid `Option`
        // reaches us from context — the `?` site cannot supply one because the
        // enclosing function's `Option(U)` has the wrong payload (RUE-318). The
        // intrinsic knows its own payload, so instantiate the canonical library
        // `Option(payload)` directly: `find_or_create_anon_enum` returns the
        // very same `EnumId` an in-scope `Option(payload)` produces (ADR-0038),
        // so `?` then unwraps it exactly like any other typed `Option`.
        if ctx.try_operand {
            return Ok(self.find_or_create_anon_enum(
                &["Some".to_string(), "None".to_string()],
                &[vec![expected_payload], Vec::new()],
            ));
        }

        // Otherwise: with no context at all the resolved type is `<error>`; point
        // the user at the fix instead of printing the placeholder.
        let found = if ty.is_error() {
            "no expected type — annotate the binding or `match` on the result".to_string()
        } else {
            ty.safe_name_with_pool(Some(&self.type_pool))
        };
        Err(CompileError::new(
            ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                name: intrinsic_display.to_string(),
                expected: format!("Option({payload_display})"),
                found,
            })),
            span,
        ))
    }

    /// Analyze @read_line intrinsic.
    pub(super) fn analyze_read_line_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        result_expected: Option<Type>,
        ctx: &AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @read_line() - reads a line from stdin. Takes no arguments and
        // returns `Option(String)`: `Some(line)` normally, `None` at EOF
        // (RUE-6, ADR-0038). The concrete Option type is taken from context.
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

        let string_type = self
            .strbuf_type()
            .ok_or_compile_error(ErrorKind::UnknownType("StrBuf".to_string()), span)?;
        let option_ty = self.resolve_option_result_type(
            ctx,
            result_expected,
            inst_ref,
            string_type,
            "read_line",
            "StrBuf",
            span,
        )?;

        // The intrinsic lowers to a runtime call whose result codegen packs
        // into this `Option(StrBuf)` enum (discriminant + StrBuf payload).
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::ReadLine),
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: option_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, option_ty))
    }

    /// Analyze the `@to_string(n)` intrinsic (RUE-17 Phase 1, ADR-0035;
    /// RUE-314).
    ///
    /// Formats any integer (`i8`/`i16`/`i32`/`i64`/`u8`/`u16`/`u32`/`u64`) as
    /// its decimal representation in a fresh heap `String`. The runtime
    /// formatters operate on a 64-bit value, so a narrower argument is widened
    /// to 64 bits first. Signed operands select
    /// [`rue_builtins::TextBuiltinOperation::ToStringSigned`]; unsigned operands
    /// select [`rue_builtins::TextBuiltinOperation::ToStringUnsigned`] so a
    /// `u32`/`u64` with the high bit set prints as its unsigned value, not a
    /// negative number. The widening reuses the ordinary `IntCast` path, which
    /// the backends already sign/zero-extend correctly (RUE-88). The builtin
    /// mapping obtains the logical out-pointer signature from the canonical
    /// runtime manifest; AIR retains its existing external-call representation
    /// until the typed call migration.
    pub(super) fn analyze_to_string_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "to_string".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // The argument is consumed by value (every integer is Copy, so this is a
        // plain read). Any integer width is accepted (RUE-314).
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;
        if !arg_type.is_integer() && !arg_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "@to_string".to_string(),
                    expected: "integer".to_string(),
                    found: arg_type.safe_name_with_pool(Some(&self.type_pool)),
                })),
                span,
            ));
        }

        // Widen the argument to a 64-bit value and pick the runtime formatter by
        // signedness. Unsigned types must format their zero-extended value, so
        // route them through the unsigned formatter; an error-typed argument is
        // treated as signed (it never reaches codegen).
        let unsigned = arg_type.is_unsigned();
        let widen_to = if unsigned { Type::U64 } else { Type::I64 };
        let arg_air_ref = if arg_type.is_error() || arg_type == widen_to {
            arg_result.air_ref
        } else {
            air.add_inst(AirInst {
                data: AirInstData::IntCast {
                    value: arg_result.air_ref,
                    from_ty: arg_type,
                },
                ty: widen_to,
                span,
            })
        };

        let string_type = self
            .strbuf_type()
            .ok_or_compile_error(ErrorKind::UnknownType("StrBuf".to_string()), span)?;
        let operation = if unsigned {
            rue_builtins::TextBuiltinOperation::ToStringUnsigned
        } else {
            rue_builtins::TextBuiltinOperation::ToStringSigned
        };
        let runtime_helper = operation
            .runtime_helper()
            .expect("to_string builtin must map to a runtime helper");
        debug_assert_eq!(runtime_helper.parameters.len(), 2);
        let call_name = self.interner.get_or_intern(runtime_helper.symbol);

        // Encode the single by-value argument as a (value, mode) pair.
        let extra_data = [arg_air_ref.as_u32(), AirArgMode::Normal.as_u32()];
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                runtime: Some(if unsigned {
                    crate::RuntimeCallKind::ToStringUnsigned
                } else {
                    crate::RuntimeCallKind::ToString
                }),
                name: call_name,
                args_start,
                args_len: 1,
            },
            ty: string_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, string_type))
    }

    /// Analyze @parse_i32, @parse_i64, @parse_u32, @parse_u64 intrinsics.
    pub(super) fn analyze_parse_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        intrinsic_name_str: &str,
        args: &[RirCallArg],
        span: Span,
        result_expected: Option<Type>,
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

        // Analyze the argument - StrBuf borrows are handled by
        // analyze_inst_for_projection to avoid consuming the StrBuf
        let arg_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // Every text rung is consumed through its shared `{ptr, len}` view.
        if !self.is_strbuf(arg_type) && !self.is_str_like(arg_type) {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: format!("@{}", intrinsic_name_str),
                    expected: "text".to_string(),
                    found: arg_type.safe_name_with_pool(Some(&self.type_pool)),
                })),
                span,
            ));
        }

        // Determine the payload integer type based on the intrinsic name.
        let (payload_type, payload_display) = match intrinsic_name_str {
            "parse_i32" => (Type::I32, "i32"),
            "parse_i64" => (Type::I64, "i64"),
            "parse_u32" => (Type::U32, "u32"),
            "parse_u64" => (Type::U64, "u64"),
            _ => unreachable!(),
        };

        // The result is `Option(int)`: `Some(n)` on success, `None` on parse
        // failure (RUE-6, ADR-0038). The concrete Option type comes from
        // context and is validated to carry the matching integer payload.
        let option_ty = self.resolve_option_result_type(
            ctx,
            result_expected,
            inst_ref,
            payload_type,
            intrinsic_name_str,
            payload_display,
            span,
        )?;

        // Encode args into extra array
        let source_strbuf = arg_type.as_struct().is_some_and(|struct_id| {
            self.type_pool.struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        let (arg_ref, temp_scope) = if source_strbuf {
            self.materialize_borrow_argument(air, arg_result.air_ref, arg_type, span, ctx)?
        } else {
            (arg_result.air_ref, Vec::new())
        };
        let args_start = air.add_extra(&[arg_ref.as_u32()]);
        let intrinsic_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(match intrinsic_name_str {
                    "parse_i32" => crate::RuntimeCallKind::ParseI32,
                    "parse_i64" => crate::RuntimeCallKind::ParseI64,
                    "parse_u32" => crate::RuntimeCallKind::ParseU32,
                    "parse_u64" => crate::RuntimeCallKind::ParseU64,
                    _ => unreachable!(),
                }),
                name,
                args_start,
                args_len: 1,
            },
            ty: option_ty,
            span,
        });
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, option_ty, span, temp_scope);
        Ok(AnalysisResult::new(air_ref, option_ty))
    }

    /// Analyze @random_u32 intrinsic.
    pub(super) fn analyze_random_u32_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @random_u32() - takes no arguments, returns u32
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "random_u32".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Create the intrinsic instruction that returns u32
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::RandomU32),
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: Type::U32,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U32))
    }

    /// Analyze @random_u64 intrinsic.
    pub(super) fn analyze_random_u64_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @random_u64() - takes no arguments, returns u64
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "random_u64".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Create the intrinsic instruction that returns u64
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::RandomU64),
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze @import intrinsic.
    ///
    /// This requires the `modules` preview feature and takes a single string literal
    /// argument specifying the module path to import.
    pub(super) fn analyze_import_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let [arg] = args else {
            unreachable!("compiler import preflight rejects malformed @import calls")
        };

        // Compiler preflight guarantees a string literal and owns its
        // diagnostic. AIR only binds the already-resolved canonical record.
        let arg_inst = self.rir.get(arg.value);
        let import_path = match &arg_inst.data {
            rue_rir::InstData::StringConst(path_spur) => {
                self.interner.resolve(path_spur).to_string()
            }
            _ => unreachable!("compiler import preflight requires a string literal"),
        };

        // Bind this exact source site through AIR's compact lookup, which was
        // built from the compiler's accepted canonical discovery graph.
        let module_id = self.resolve_canonical_import(&import_path, span)?;

        // Return a module type
        // AIR doesn't have a ModuleConst instruction, so we use UnitConst as a placeholder
        // The type is what matters for subsequent member access resolution
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst, // Placeholder - module values are compile-time only
            ty: Type::new_module(module_id),
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::new_module(module_id)))
    }
}
