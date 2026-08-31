//! Compiler intrinsic dispatch and analysis.
//!
//! This category owns source, internal, type, and layout intrinsic dispatch and
//! non-pointer implementations within canonical semantic analysis. Pointer and
//! raw-memory operations delegate their place and pointer semantics to the
//! sibling `pointers` and `ownership` modules.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;
use crate::sema::ownership_state::FieldPath;

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };
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
        type_arg: rue_rir::RirTypeSyntaxRef,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = self.body_interner().resolve(&name).to_string();
        let ty = self.resolve_rir_type_with_ctx(type_arg, span, ctx)?;

        // `@require_droppable(T)` is the owning-container well-formedness gate
        // (RUE-388): it has no runtime value and evaluates to unit. It is
        // normally consumed at comptime while reducing a `-> type` constructor
        // body (see the engine's `check_require_droppable`), but handle it here so
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

        // `@int_max(T)` / `@int_min(T)` (RUE-694): the largest/smallest value
        // representable in integer type `T`, typed as `T` itself — the only
        // result type that never truncates (`u64::MAX` doesn't fit any signed
        // type; `i64::MIN` doesn't fit `u64`). The value folds to a `Const`
        // here like `@size_of`; the u64 payload carries narrow signed minima
        // sign-extended, matching how negative literals are emitted.
        if intrinsic_name == "int_max" || intrinsic_name == "int_min" {
            if ty.is_error() {
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(0),
                    ty: Type::ERROR,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::ERROR));
            }
            let bound = if intrinsic_name == "int_max" {
                ty.int_max()
            } else {
                ty.int_min()
            };
            let Some(bound) = bound else {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: intrinsic_name,
                        expected: "an integer type".to_string(),
                        found: self.format_type_name(ty),
                    })),
                    span,
                ));
            };
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Const(bound as u64),
                ty,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
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
                self.body_type_pool().provisional_layout(ty).size
            }
            "align_of" => {
                self.require_layout_slots(ty, span)?;
                self.body_type_pool().provisional_layout(ty).alignment
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
        type_arg: rue_rir::RirTypeSyntaxRef,
        field: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let ty = self.resolve_rir_type(type_arg, span)?;

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

        let struct_def = self.body_type_pool().struct_def(struct_id);
        let field_name_str = self.body_interner().resolve(&field);
        let field_index = match struct_def.find_field(field_name_str) {
            Some((index, _)) => index,
            None => {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: self.format_type_name(Type::new_struct(struct_id)),
                        field_name: field_name_str.to_string(),
                    },
                    span,
                ));
            }
        };

        let byte_offset = self
            .body_type_pool()
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

    /// Implementation for Intrinsic calls.
    fn analyze_intrinsic_impl(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args: &rue_rir::RirIntrinsicArgsRange,
        span: Span,
        result_expected: Option<Type>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Intrinsic arguments are stored as plain InstRefs
        let arg_refs = self.body_rir_ref().intrinsic_args(args);
        let args: Vec<RirCallArg> = arg_refs
            .into_iter()
            .map(|value| RirCallArg {
                value,
                mode: RirArgMode::Normal,
            })
            .collect();
        let known = &self.known_symbols();

        // Raw-pointer, heap, and syscall intrinsics are unchecked operations:
        // they may only be used inside a `checked` block (spec 9.1:1, chapter
        // 9). Gate them before the per-intrinsic dispatch so every one shares a
        // single diagnostic. The set is @raw/@raw_mut/@field_ptr/@ptr_read/
        // @ptr_write/@ptr_offset/@ptr_to_int/@int_to_ptr, the unified byte
        // allocation family @alloc/@alloc_zeroed/@free/@realloc/@resize, and
        // @syscall (RUE-1, RUE-301, RUE-1369, ADR-0059 Phase 3).
        if ctx.checked_depth == 0
            && (name == known.ptr_read
                || name == known.ptr_write
                || name == known.ptr_read_unaligned
                || name == known.ptr_write_unaligned
                || name == known.ptr_offset
                || name == known.ptr_to_int
                || name == known.int_to_ptr
                || name == known.raw
                || name == known.raw_mut
                || name == known.field_ptr
                || name == known.place
                || name == known.alloc
                || name == known.alloc_zeroed
                || name == known.free
                || name == known.realloc
                || name == known.resize
                || name == known.byte_copy
                || name == known.byte_move
                || name == known.byte_set
                || name == known.arg_ptr
                || name == known.env_ptr
                || name == known.syscall)
        {
            let intrinsic_name_str = self.body_interner().resolve(&name);
            let kind = if name == known.alloc
                || name == known.alloc_zeroed
                || name == known.free
                || name == known.realloc
                || name == known.resize
            {
                "heap"
            } else if name == known.syscall {
                "syscall"
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
        } else if name == known.bit_cast {
            self.analyze_bitcast_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.test_preview_gate {
            self.analyze_test_preview_gate_intrinsic(air, &args, span)
        } else if name == known.read_line {
            self.analyze_read_line_intrinsic(air, name, inst_ref, &args, span, result_expected, ctx)
        } else if name == known.to_string {
            self.analyze_to_string_intrinsic(air, &args, span, ctx)
        } else if let Some(operation) = known.get_parse_intrinsic_operation(name) {
            let intrinsic_name_str = operation.expected_spelling();
            self.analyze_parse_intrinsic(
                air,
                name,
                inst_ref,
                operation,
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
        } else if name == known.arg_count {
            self.analyze_process_count_intrinsic(
                air,
                name,
                "arg_count",
                crate::IntrinsicOperation::ArgCount,
                &args,
                span,
            )
        } else if name == known.env_count {
            self.analyze_process_count_intrinsic(
                air,
                name,
                "env_count",
                crate::IntrinsicOperation::EnvCount,
                &args,
                span,
            )
        } else if name == known.arg_len {
            self.analyze_process_len_intrinsic(
                air,
                name,
                "arg_len",
                crate::IntrinsicOperation::ArgLen,
                &args,
                span,
                ctx,
            )
        } else if name == known.env_len {
            self.analyze_process_len_intrinsic(
                air,
                name,
                "env_len",
                crate::IntrinsicOperation::EnvLen,
                &args,
                span,
                ctx,
            )
        } else if name == known.arg_ptr {
            self.analyze_process_ptr_intrinsic(
                air,
                name,
                "arg_ptr",
                crate::IntrinsicOperation::ArgPtr,
                inst_ref,
                &args,
                span,
                ctx,
            )
        } else if name == known.env_ptr {
            self.analyze_process_ptr_intrinsic(
                air,
                name,
                "env_ptr",
                crate::IntrinsicOperation::EnvPtr,
                inst_ref,
                &args,
                span,
                ctx,
            )
        } else if name == known.wrapping_add
            || name == known.wrapping_sub
            || name == known.wrapping_mul
        {
            self.analyze_wrapping_arith_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_read {
            self.analyze_ptr_read_intrinsic(air, name, inst_ref, &args, span, ctx, false)
        } else if name == known.ptr_write {
            self.analyze_ptr_write_intrinsic(air, name, &args, span, ctx, false)
        } else if name == known.ptr_read_unaligned {
            self.analyze_ptr_read_intrinsic(air, name, inst_ref, &args, span, ctx, true)
        } else if name == known.ptr_write_unaligned {
            self.analyze_ptr_write_intrinsic(air, name, &args, span, ctx, true)
        } else if name == known.ptr_offset {
            self.analyze_ptr_offset_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_to_int {
            self.analyze_ptr_to_int_intrinsic(air, name, &args, span, ctx)
        } else if name == known.int_to_ptr {
            self.analyze_int_to_ptr_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.alloc || name == known.alloc_zeroed {
            self.analyze_alloc_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.realloc {
            self.analyze_realloc_intrinsic(air, name, &args, span, ctx)
        } else if name == known.resize {
            self.analyze_resize_intrinsic(air, name, &args, span, ctx)
        } else if name == known.free {
            self.analyze_free_intrinsic(air, name, &args, span, ctx)
        } else if name == known.byte_copy || name == known.byte_move {
            self.analyze_byte_copy_intrinsic(air, name, &args, span, ctx)
        } else if name == known.byte_set {
            self.analyze_byte_set_intrinsic(air, name, &args, span, ctx)
        } else if name == known.raw {
            let raw = self.known_symbols().raw;
            self.analyze_addr_of_intrinsic(air, &args, span, ctx, false, raw, "addr_of")
        } else if name == known.raw_mut {
            let raw_mut = self.known_symbols().raw_mut;
            self.analyze_addr_of_intrinsic(air, &args, span, ctx, true, raw_mut, "addr_of_mut")
        } else if name == known.field_ptr {
            self.analyze_field_ptr_intrinsic(air, &args, span, ctx)
        } else if name == known.place {
            self.analyze_place_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.syscall {
            self.analyze_syscall_intrinsic(air, name, &args, span, ctx)
        } else if name == known.target_arch {
            self.analyze_target_arch_intrinsic(air, &args, span)
        } else if name == known.target_os {
            self.analyze_target_os_intrinsic(air, &args, span)
        } else if name == known.target_data_model {
            self.analyze_target_data_model_intrinsic(air, &args, span)
        } else {
            Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(self.body_interner().resolve(&name).to_string()),
                span,
            ))
        }
    }

    /// Validate the trusted std pointer-to-place bridge. The enclosing yield
    /// converts this pointer-shaped AIR value into `AirPlaceBase::Indirect`;
    /// keeping the intrinsic itself value-shaped means it can be spliced and
    /// materialized with the ordinary pointer lowering path.
    fn analyze_place_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let Some(trailing) = ctx.accessor_trailing_yield else {
            return Err(CompileError::new(
                ErrorKind::UnknownIntrinsic("place".to_owned()),
                span,
            ));
        };
        let legal_trailing_bridge = match self.body_rir_ref().get(trailing).data {
            InstData::Yield(operand) => match self.body_rir_ref().get(operand).data {
                InstData::Checked { expr } => match self.body_rir_ref().get(expr).data {
                    InstData::Intrinsic { name, .. } => {
                        name == self.known_symbols().place && expr == inst_ref
                    }
                    _ => false,
                },
                _ => false,
            },
            _ => false,
        };
        if !legal_trailing_bridge
            || !self.file_module_is_trusted_standard_library(ctx.current_file_id)
        {
            return Err(CompileError::new(
                ErrorKind::UnknownIntrinsic("place".to_owned()),
                span,
            ));
        }
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "place".to_owned(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }
        // The bridge is representation-level, but the yielded place must
        // still be rooted in the accessor receiver. Admit exactly
        // `@place(@ptr_offset(<self field chain>, ...))`; this keeps the
        // trusted escape hatch from manufacturing a loan into unrelated
        // storage while leaving the index expression to the reviewed std
        // accessor's bounds guard.
        let ptr_offset_base = match &self.body_rir_ref().get(args[0].value).data {
            InstData::Intrinsic { name, args } if *name == self.known_symbols().ptr_offset => {
                let pointer_args = self.body_rir_ref().intrinsic_args(args);
                (pointer_args.len() == 2)
                    .then(|| pointer_args.values().next())
                    .flatten()
            }
            _ => None,
        };
        let self_symbol = self.intern_body_symbol("self")?;
        let mut root = ptr_offset_base;
        let receiver_rooted = loop {
            let Some(current) = root else {
                break false;
            };
            match &self.body_rir_ref().get(current).data {
                InstData::VarRef { name, .. } => break *name == self_symbol,
                InstData::FieldGet { base, .. } => root = Some(*base),
                _ => break false,
            }
        };
        if !receiver_rooted {
            return Err(CompileError::new(
                crate::declaration_validation::accessor_yield_root_error(
                    &crate::declaration_validation::AccessorYieldRootForm::Value,
                ),
                span,
            ));
        }
        let operand = self.analyze_inst(air, args[0].value, ctx)?;
        if !operand.ty.is_ptr() && !operand.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(rue_error::IntrinsicTypeMismatchError {
                    name: "place".to_owned(),
                    expected: "a raw pointer".to_owned(),
                    found: self.format_type_name(operand.ty),
                })),
                span,
            ));
        }
        // `@place` is a checked-language bridge, not a runtime operation.  The
        // pointer remains the ordinary value used by the indirect place that
        // the trailing-yield analysis constructs; emitting an intrinsic here
        // would incorrectly expose a call-shaped operation to codegen.
        Ok(AnalysisResult::new(operand.air_ref, operand.ty))
    }

    fn analyze_internal_intrinsic_impl(
        &mut self,
        air: &mut Air,
        intrinsic: rue_rir::InternalIntrinsic,
        args: &rue_rir::RirInternalIntrinsicArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let args_len = self.body_rir_ref().internal_intrinsic_args(args).len() as u32;
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
            .body_rir_ref()
            .internal_intrinsic_args(args)
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
            let (_elem, len) = self.body_type_pool().array_def(array_id);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Const(len),
                ty: Type::U64,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::U64));
        }

        // String byte view: the bound is the byte length `s.len()`.
        if self.is_strbuf(coll_type) {
            let source_method = if let Some(struct_id) = coll_type.as_struct() {
                let method = self.intern_body_symbol("len")?;
                (self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
                    && self.method_info((struct_id, method)).is_some())
                .then_some((struct_id, method))
            } else {
                None
            };
            let Some((struct_id, method)) = source_method else {
                return Err(CompileError::new(
                    ErrorKind::InternalError(
                        "trusted std StrBuf is missing its source `len` method".to_string(),
                    ),
                    span,
                ));
            };
            ctx.referenced_methods.insert((struct_id, method));
            self.record_body_method_dependency((struct_id, method))?;
            let (receiver, temp_scope) = self.materialize_borrow_argument(
                air,
                coll_result.air_ref,
                coll_result.ty,
                span,
                ctx,
            )?;
            let call_name = self.intern_body_symbol(&self.method_symbol(struct_id, "len", true))?;
            let call_ref = air.add_call(
                None,
                call_name,
                &[AirCallArg {
                    value: receiver,
                    mode: AirArgMode::Borrow,
                }],
                Type::U64,
                span,
            )?;
            let call_ref =
                self.wrap_value_with_temp_scope(air, call_ref, Type::U64, span, temp_scope)?;
            return Ok(AnalysisResult::new(call_ref, Type::U64));
        }

        if coll_type.is_error() {
            return Ok(AnalysisResult::new(coll_result.air_ref, Type::U64));
        }

        Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "an array or a StrBuf".to_string(),
                found: self.format_type_name(coll_type),
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
                    found: self.format_type_name(coll_result.ty),
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
        let (runtime, ret_ty) = match (is_next, lossy) {
            (true, false) => (crate::RuntimeCallKind::StrCharNext, Type::U64),
            (true, true) => (crate::RuntimeCallKind::StrCharNextLossy, Type::U64),
            (false, false) => (crate::RuntimeCallKind::StrCharScalar, Type::U32),
            (false, true) => (crate::RuntimeCallKind::StrCharScalarLossy, Type::U32),
        };
        let call_name = self.intern_body_symbol(runtime.helper().symbol())?;
        let (ptr, len, temp_scope) =
            self.project_strbuf_text_fields(air, coll_result.air_ref, coll_result.ty, span, ctx)?;
        let call_ref = air.add_call(
            Some(runtime),
            call_name,
            &[
                AirCallArg {
                    value: ptr,
                    mode: AirArgMode::Normal,
                },
                AirCallArg {
                    value: len,
                    mode: AirArgMode::Normal,
                },
                AirCallArg {
                    value: pos_result.air_ref,
                    mode: AirArgMode::Normal,
                },
            ],
            ret_ty,
            span,
        )?;
        let call_ref = self.wrap_value_with_temp_scope(air, call_ref, ret_ty, span, temp_scope)?;
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
        let move_before = self.snapshot_move_state(root, ctx);
        let coll_result = self.analyze_inst(air, coll, ctx)?;
        self.restore_move_state_and_cancel(air, coll_result.air_ref, move_before, ctx);
        Ok(coll_result)
    }

    // Helper methods for intrinsic analysis (delegated from analyze_intrinsic_impl)

    fn analyze_sequenced_intrinsic_operand(
        &mut self,
        air: &mut Air,
        operand: InstRef,
        reachable: bool,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let reachable_edges = ctx.ownership.loop_break_stack.clone();
        let divergence_before = ctx.divergence_kinds;
        let result = self.analyze_inst(air, operand, ctx)?;
        if !reachable {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges);
            ctx.divergence_kinds = divergence_before;
        }
        Ok(result)
    }

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
        let byref_root = root_variable_of(self.body_rir_ref(), args[0].value);
        let arg_result = self.analyze_with_borrow_root(air, args[0].value, byref_root, ctx)?;
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
                    found: self.format_type_name(arg_type),
                })),
                span,
            ));
        }

        let source_strbuf = arg_type.as_struct().is_some_and(|struct_id| {
            self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        // A StrBuf argument is passed as a layout-stable `{ptr, len}` str view
        // rather than the buffer header, so `@dbg`'s `DebugStr` text lowering
        // never depends on StrBuf's field layout (RUE-1066). A `str`/`Str(N)`
        // argument already has that shape and is forwarded by value.
        let (arg_ref, temp_scope) = if source_strbuf {
            self.strbuf_text_view(air, arg_result.air_ref, arg_type, span, ctx)?
        } else {
            (arg_result.air_ref, Vec::new())
        };
        let intrinsic_ref = air.add_intrinsic(
            if arg_type == Type::BOOL {
                crate::IntrinsicOperation::DebugBool
            } else if self.is_strbuf(arg_type) || self.is_str_like(arg_type) {
                crate::IntrinsicOperation::DebugStr
            } else if arg_type.is_signed() {
                crate::IntrinsicOperation::DebugI64
            } else {
                crate::IntrinsicOperation::DebugU64
            },
            self.known_symbols().dbg,
            &[arg_ref],
            Type::UNIT,
            span,
        )?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, Type::UNIT, span, temp_scope)?;
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze the `@drop(x)` intentional-destroy intrinsic (RUE-187,
    /// ADR-0039).
    ///
    /// `@drop(x)` runs `x`'s drop glue (or the path-granular residue walk for a
    /// partially moved place) at this site AND discharges `x`'s consumption
    /// obligation:
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

        // Analyze the operand as an ordinary by-value use. The drop operand is
        // the one exception to the usual whole-value-use rule: when a place
        // already has moved-out subplaces, its move marker is still accepted
        // and CFG drop elaboration destroys only the owned residue. Capture
        // the pre-use state so the linear residue check below observes the
        // state before the whole-value marker clears it.
        let drop_path = self.drop_operand_path(args[0].value)?;
        let root = root_variable_of(self.body_rir_ref(), args[0].value);
        let before_state = root.and_then(|root| self.drop_operand_move_state(ctx, root));
        let previous_drop_operand = ctx.ownership.drop_intrinsic_operand;
        // Only a statically recognized place gets the partial-residue
        // exception. A non-place operand (for example `@drop(make(x))`) is
        // an ordinary expression: nested by-value uses must still diagnose a
        // moved value, and there is no direct MarkMoved place for CFG to
        // elaborate as residue.
        ctx.ownership.drop_intrinsic_operand = drop_path.as_ref().and(root);
        let arg_result = self.analyze_inst(air, args[0].value, ctx);
        ctx.ownership.drop_intrinsic_operand = previous_drop_operand;
        let arg_result = arg_result?;
        let arg_type = arg_result.ty;

        if let Some(drop_path) = drop_path.as_deref()
            && let Some(root) = root
            && let Some(state) = before_state.as_ref()
            && state
                .partial_moves
                .iter()
                .any(|(path, _)| path.len() > drop_path.len() && path.starts_with(drop_path))
        {
            self.check_drop_partial_linear_residue(root, arg_type, state, drop_path, span)?;
        }

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

    /// Return the statically tracked move path named by a drop operand. A
    /// dynamic index cannot name a residual path, and is rejected by the
    /// ordinary projection rules for non-Copy values before it can reach
    /// residual-drop elaboration.
    fn drop_operand_path(&mut self, inst_ref: InstRef) -> CompileResult<Option<FieldPath>> {
        let data = self.body_rir_ref().get(inst_ref).data.clone();
        match data {
            InstData::VarRef { .. } => Ok(Some(Vec::new())),
            InstData::FieldGet { base, field } => {
                let Some(mut path) = self.drop_operand_path(base)? else {
                    return Ok(None);
                };
                path.push(field);
                Ok(Some(path))
            }
            InstData::IndexGet { base, index } => {
                let Some(mut path) = self.drop_operand_path(base)? else {
                    return Ok(None);
                };
                let Some(index) = self.try_get_const_index(index) else {
                    return Ok(None);
                };
                if index < 0 {
                    return Ok(None);
                }
                path.push(self.intern_index_path_segment(index as u64)?);
                Ok(Some(path))
            }
            _ => Ok(None),
        }
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
            let air_ref = air.add_intrinsic(
                crate::IntrinsicOperation::PanicNoMessage,
                self.known_symbols().panic,
                &[],
                Type::NEVER,
                span,
            )?;
            ctx.divergence_kinds
                .insert(super::super::context::DivergenceKind::Panic);
            return Ok(AnalysisResult::diverged(air_ref, Type::NEVER));
        }

        // Panic borrows its message until the runtime call. Give a canonical
        // source StrBuf rvalue an addressable owner for that whole call.
        let arg_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        self.validate_abort_message_type(
            "panic",
            arg_result.ty,
            self.body_rir_ref().get(args[0].value).span,
        )?;

        let source_strbuf = arg_result.ty.as_struct().is_some_and(|struct_id| {
            self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        // Narrow a StrBuf message to a layout-stable `{ptr, len}` str view so
        // the `Panic` text lowering reads a `str`, not the StrBuf header
        // (RUE-1066). `str`/`Str(N)` messages already have that shape.
        let (arg_ref, temp_scope) = if source_strbuf {
            self.strbuf_text_view(air, arg_result.air_ref, arg_result.ty, span, ctx)?
        } else {
            (arg_result.air_ref, Vec::new())
        };
        let intrinsic_ref = air.add_intrinsic(
            crate::IntrinsicOperation::Panic,
            self.known_symbols().panic,
            &[arg_ref],
            Type::NEVER,
            span,
        )?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, Type::NEVER, span, temp_scope)?;
        // An operand that diverges prevents the explicit panic call from
        // being reached; the context already records that operand's edge.
        // Only a normally evaluated message establishes the panic exemption.
        if arg_result.continues {
            ctx.divergence_kinds
                .insert(super::super::context::DivergenceKind::Panic);
        }
        Ok(AnalysisResult::diverged(air_ref, Type::NEVER))
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
        let cond_span = self.body_rir_ref().get(args[0].value).span;
        if cond_result.ty != Type::BOOL && !cond_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "assert".to_string(),
                    expected: "bool condition".to_string(),
                    found: self.format_type_name(cond_result.ty),
                })),
                cond_span,
            ));
        }

        // Build args for AIR
        let mut extra_data = vec![cond_result.air_ref];
        let mut temp_scope = Vec::new();
        if args.len() > 1 {
            let reachable_edges_before_message = ctx.ownership.loop_break_stack.clone();
            let divergence_before_message = ctx.divergence_kinds;
            let msg_result = self.analyze_inst_for_projection(air, args[1].value, ctx)?;
            if !cond_result.continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_message);
                ctx.divergence_kinds = divergence_before_message;
            }
            self.validate_abort_message_type(
                "assert",
                msg_result.ty,
                self.body_rir_ref().get(args[1].value).span,
            )?;
            let source_strbuf = msg_result.ty.as_struct().is_some_and(|struct_id| {
                self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
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
            extra_data.push(msg_ref);
            if !temp_scope.is_empty() {
                // The message's hoisted owner scope must not jump ahead of the
                // condition. Lower it first; the intrinsic reuses the cached
                // value after the message has been evaluated.
                temp_scope.insert(0, cond_result.air_ref);
            }
        }

        let intrinsic_ref = air.add_intrinsic(
            if args.len() > 1 {
                crate::IntrinsicOperation::AssertWithMessage
            } else {
                crate::IntrinsicOperation::AssertFailed
            },
            self.known_symbols().assert,
            &extra_data,
            Type::UNIT,
            span,
        )?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, Type::UNIT, span, temp_scope)?;
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
                    found: self.format_type_name(message_ty),
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
                    found: self.format_type_name(from_ty),
                })),
                span,
            ));
        }

        // Get the target type from HM inference
        let target_ty = match ctx.resolved_type_of(inst_ref) {
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
                        found: self.format_type_name(ty),
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

    /// Analyze the `@bitCast` intrinsic (RUE-952, spec 4.13:118-4.13:123).
    ///
    /// `@bitCast` is the same-width sibling of `@intCast`: it renames the
    /// operand's two's-complement bit pattern at the target type instead of
    /// preserving its numeric value, so it is total (it never traps) and is the
    /// only way to move a `u64` with its top bit set into an `i64`. The target
    /// type comes from context exactly as `@intCast`'s does, and the widths
    /// **must** agree — a reinterpretation neither invents nor discards bits, so
    /// a cross-width use is E0950 rather than a silent truncation.
    ///
    /// Because the operation only renames bits, it lowers to an ordinary
    /// intrinsic node (like `@ptr_to_int`) rather than to `IntCast`, which owns
    /// the range check.
    pub(super) fn analyze_bitcast_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = "bitCast";

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

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let from_ty = arg_result.ty;

        if !from_ty.is_integer() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: intrinsic_name.to_string(),
                    expected: "integer".to_string(),
                    found: self.format_type_name(from_ty),
                })),
                span,
            ));
        }

        // The target type comes from HM inference, exactly as `@intCast`'s does:
        // an annotation, a parameter, or a return position supplies it, and an
        // unconstrained result is the same "cannot infer the cast target"
        // diagnostic (RUE-588) rather than an unrelated fallback.
        let target_ty = match ctx.resolved_type_of(inst_ref) {
            Some(ty) if ty.is_integer() => ty,
            Some(Type::ERROR) | None => {
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
                        found: self.format_type_name(ty),
                    })),
                    span,
                ));
            }
        };

        // Same-width only (E0950). Both types are integers here, so both widths
        // are present.
        let from_bits = from_ty.int_bit_width().unwrap_or(0);
        let to_bits = target_ty.int_bit_width().unwrap_or(0);
        if from_bits != to_bits {
            return Err(CompileError::new(
                ErrorKind::BitCastWidthMismatch {
                    from: self.format_type_name(from_ty),
                    to: self.format_type_name(target_ty),
                    from_bits,
                    to_bits,
                },
                span,
            )
            .with_help(
                "'@bitCast' reinterprets a value's bits at the same width; use \
                 '@intCast' to convert between widths",
            ));
        }

        let air_ref = air.add_intrinsic(
            crate::IntrinsicOperation::BitCast,
            name,
            &[arg_result.air_ref],
            target_ty,
            span,
        )?;
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

    /// Resolve a fallible intrinsic's result to the trusted std `Option(payload)`
    /// and return that enum type.
    ///
    /// A fallible intrinsic (`@read_line` / `@parse_*`) OWNS the exact trusted
    /// std `Option(payload)` in every context (RUE-6, ADR-0038). The per-body
    /// well-known registry is the single canonical source for that identity: when
    /// it holds an `Option` for this payload, that instance is authoritative in
    /// bare, annotated, matched, and `?`-operand positions alike. Context (a `let`
    /// annotation, or the `match` arms the result feeds) never *selects* the
    /// nominal — it only *checks* it, and a same-shape user lookalike is a
    /// mismatch, not a parallel identity. A missing exact registry entry is
    /// typed internal incompleteness: production resolves the body's complete
    /// payload set before analysis, so neither context shape nor a lookalike may
    /// rescue it. The ordinary comptime-generic `Option` enum is used throughout;
    /// no "blessed builtin" tier is introduced.
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
        // Every fallible intrinsic OWNS the exact trusted std `Option(payload)`
        // in every context. The per-body well-known registry is the single
        // canonical source for that identity: when it holds an `Option` for this
        // payload, that instance is authoritative in bare, annotated, matched,
        // and `?`-operand positions alike. Context (a `let` annotation or the
        // `match` arms the result feeds) never selects the nominal — it only
        // *checks* the identity. A same-shape user lookalike is a mismatch
        // (E0702), never used to install a parallel identity.
        let canonical = self.well_known_option(expected_payload).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "fallible intrinsic @{intrinsic_display} reached body analysis without \
                         its exact trusted Option({payload_display}) registry entry"
                )),
                span,
            )
        })?;

        // Whatever context supplies, if anything. Under `?` no context can supply
        // the `Option` (the enclosing `Option(U)` may carry a different payload),
        // so a missing inferred type is not an error when the canonical registry
        // owns the identity.
        let context_ty = match result_expected {
            Some(ty) => Some(ty),
            None if ctx.try_operand => None,
            None => Some(Self::get_resolved_type(
                ctx,
                inst_ref,
                span,
                intrinsic_display,
            )?),
        };

        // The intrinsic result IS `canonical`. If context is present, it must
        // be that exact identity; a lookalike or a differently-payloaded trusted
        // `Option` is a mismatch. A context that already poisoned to `error`
        // produced its own diagnostic — don't cascade, but keep the canonical
        // identity so downstream composition still resolves.
        if let Some(ty) = context_ty
            && !ty.is_error()
            && ty != canonical
        {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: intrinsic_display.to_string(),
                    expected: format!("Option({payload_display})"),
                    found: self.format_type_name(ty),
                })),
                span,
            ));
        }
        Ok(canonical)
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
        let air_ref = air.add_intrinsic(
            crate::IntrinsicOperation::ReadLine,
            name,
            &[],
            option_ty,
            span,
        )?;
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
                    found: self.format_type_name(arg_type),
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
        let call_name = self.intern_body_symbol(runtime_helper.symbol)?;

        let air_ref = air.add_call(
            Some(if unsigned {
                crate::RuntimeCallKind::ToStringUnsigned
            } else {
                crate::RuntimeCallKind::ToString
            }),
            call_name,
            &[AirCallArg {
                value: arg_air_ref,
                mode: AirArgMode::Normal,
            }],
            string_type,
            span,
        )?;
        Ok(AnalysisResult::new(air_ref, string_type))
    }

    /// Analyze @parse_i32, @parse_i64, @parse_u32, @parse_u64 intrinsics.
    pub(super) fn analyze_parse_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        operation: crate::IntrinsicOperation,
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
                    found: self.format_type_name(arg_type),
                })),
                span,
            ));
        }

        // Determine the payload integer type from the semantic operation chosen
        // by symbol analysis. The spelling remains diagnostic-only.
        let (payload_type, payload_display) = match operation {
            crate::IntrinsicOperation::ParseI32 => (Type::I32, "i32"),
            crate::IntrinsicOperation::ParseI64 => (Type::I64, "i64"),
            crate::IntrinsicOperation::ParseU32 => (Type::U32, "u32"),
            crate::IntrinsicOperation::ParseU64 => (Type::U64, "u64"),
            _ => unreachable!("non-parse operation passed to parse analysis"),
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
            self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf)
        });
        let (arg_ref, temp_scope) = if source_strbuf {
            // RUE-1066: consume the StrBuf through its method-routed `{ptr, len}`
            // view so the parse helper receives the same 2-word `str` shape as a
            // native `str` argument and never reads the reordered 3-word header
            // by offset.
            self.strbuf_text_view(air, arg_result.air_ref, arg_type, span, ctx)?
        } else {
            (arg_result.air_ref, Vec::new())
        };
        let intrinsic_ref = air.add_intrinsic(operation, name, &[arg_ref], option_ty, span)?;
        let air_ref =
            self.wrap_value_with_temp_scope(air, intrinsic_ref, option_ty, span, temp_scope)?;
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
        let air_ref = air.add_intrinsic(
            crate::IntrinsicOperation::RandomU32,
            name,
            &[],
            Type::U32,
            span,
        )?;
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
        let air_ref = air.add_intrinsic(
            crate::IntrinsicOperation::RandomU64,
            name,
            &[],
            Type::U64,
            span,
        )?;
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze the nullary process-inventory intrinsics `@arg_count` /
    /// `@env_count` (RUE-935). Each takes no arguments and returns `u64`,
    /// mirroring `@random_u64`; the runtime read of the captured argc/env count
    /// imposes no caller obligation, so — like `@random_*` — these are not
    /// `checked`-gated.
    pub(super) fn analyze_process_count_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        display: &str,
        operation: crate::IntrinsicOperation,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: display.to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }
        let air_ref = air.add_intrinsic(operation, name, &[], Type::U64, span)?;
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze the indexed length intrinsics `@arg_len` / `@env_len`
    /// (RUE-935). Each takes a single `u64` index and returns `u64`; the
    /// runtime returns 0 for an out-of-range index, so no caller obligation is
    /// imposed and no `checked` block is required (a safe scalar read like
    /// `@str_byte_at`).
    pub(super) fn analyze_process_len_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        display: &str,
        operation: crate::IntrinsicOperation,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: display.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }
        let index = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_process_index_type(display, index.ty, span)?;
        let air_ref = air.add_intrinsic(operation, name, &[index.air_ref], Type::U64, span)?;
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze the indexed pointer intrinsics `@arg_ptr` / `@env_ptr`
    /// (RUE-935). Each takes a single `u64` index and returns `ptr mut u8`.
    /// The runtime returns a null pointer when the index is at or past the
    /// corresponding count; following `@alloc`'s conventions (ADR-0028) these
    /// pointer-producing intrinsics are only usable inside a `checked` block
    /// (gated together with the other raw-pointer intrinsics above).
    pub(super) fn analyze_process_ptr_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        display: &str,
        operation: crate::IntrinsicOperation,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: display.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }
        let index = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_process_index_type(display, index.ty, span)?;
        let result_ty = Type::new_ptr_mut(self.body_type_pool().intern_ptr_mut_from_type(Type::U8));
        if let Some(expected) = ctx.resolved_type_of(inst_ref)
            && !self.types_equivalent(expected, result_ty)
            && !expected.is_error()
            && !expected.is_never()
        {
            return Err(self.type_mismatch_error(expected, result_ty, span));
        }
        let air_ref = air.add_intrinsic(operation, name, &[index.air_ref], result_ty, span)?;
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// Require the index operand of a process argument/environment intrinsic to
    /// be `u64` (the runtime helpers take a single `u64` index).
    fn require_process_index_type(
        &self,
        display: &str,
        found: Type,
        span: Span,
    ) -> CompileResult<()> {
        if found == Type::U64 || found.is_error() || found.is_never() {
            return Ok(());
        }
        Err(CompileError::new(
            ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                name: format!("@{display}"),
                expected: "u64".to_string(),
                found: self.format_type_name(found),
            })),
            span,
        ))
    }

    /// Analyze `@wrapping_add` / `@wrapping_sub` / `@wrapping_mul` (RUE-647).
    ///
    /// Each takes exactly two operands of the same integer type and returns
    /// that type; the value is the true mathematical result reduced modulo
    /// 2^N (two's-complement), defined for every width (`i8`..`i64`,
    /// `u8`..`u64`) and both signednesses, and never traps. The
    /// operand/result equality-and-integer typing is exactly that of checked
    /// `+`/`-`/`*` (inference `generate_add`); this re-emits the resolved AIR
    /// node as a `WrappingAdd`/`WrappingSub`/`WrappingMul` so lowering drops
    /// the overflow-check step.
    pub(super) fn analyze_wrapping_arith_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let display = self.body_interner().resolve(&name).to_string();
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: display,
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let lhs = self.analyze_sequenced_intrinsic_operand(air, args[0].value, true, ctx)?;
        let rhs =
            self.analyze_sequenced_intrinsic_operand(air, args[1].value, lhs.continues, ctx)?;
        let lty = lhs.ty;
        let rty = rhs.ty;

        // Both operands must be integers. An `<error>`-typed operand already
        // produced its own diagnostic during inference, so let it pass through
        // silently (it never reaches codegen) — mirroring the other arithmetic
        // intrinsics.
        for (ty, arg) in [(lty, &args[0]), (rty, &args[1])] {
            if !ty.is_integer() && !ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: format!("@{display}"),
                        expected: "integer".to_string(),
                        found: self.format_type_name(ty),
                    })),
                    self.body_rir_ref().get(arg.value).span,
                ));
            }
        }

        // The operand and result types share one integer type through the
        // inference equality constraints; use the concrete operand type as the
        // result. (A mismatch between the two operands is reported by inference
        // as an ordinary type error before reaching here.)
        let result_ty = if lty.is_integer() {
            lty
        } else if rty.is_integer() {
            rty
        } else {
            Type::ERROR
        };

        let data = if name == self.known_symbols().wrapping_add {
            AirInstData::WrappingAdd(lhs.air_ref, rhs.air_ref)
        } else if name == self.known_symbols().wrapping_sub {
            AirInstData::WrappingSub(lhs.air_ref, rhs.air_ref)
        } else {
            AirInstData::WrappingMul(lhs.air_ref, rhs.air_ref)
        };

        let air_ref = air.add_inst(AirInst {
            data,
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
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
        let arg_inst = self.body_rir_ref().get(arg.value);
        let import_path = match &arg_inst.data {
            rue_rir::InstData::StringConst {
                content: path_spur, ..
            } => self.body_interner().resolve(path_spur).to_string(),
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
