//! Pointer and low-level intrinsics: ptr read/write/offset, alloc/free/realloc, addr-of, syscall, target arch/os.
//!
//! This category owns pointer and platform-facing intrinsic analysis within
//! the canonical semantic-analysis implementation.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    fn analyze_sequenced_pointer_operand(
        &mut self,
        air: &mut Air,
        operand: InstRef,
        reachable: bool,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let reachable_edges = ctx.loop_break_stack.clone();
        let result = self.analyze_inst(air, operand, ctx)?;
        if !reachable {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges);
        }
        Ok(result)
    }

    /// Analyze @ptr_read / @ptr_read_unaligned intrinsic: reads value through
    /// pointer. Signature: `@ptr_read(ptr: ptr const T) -> T`.
    ///
    /// `unaligned` selects `@ptr_read_unaligned` (ADR-0059 Phase 4, RUE-978):
    /// same pointee-typed shape, but the caller does not promise the address is
    /// aligned. On x86-64 and AArch64 the emitted access is
    /// identical to the aligned variant (both tolerate unaligned scalars); the
    /// distinction is the semantic contract of spec 9.2:14k.
    pub(super) fn analyze_ptr_read_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        unaligned: bool,
    ) -> CompileResult<AnalysisResult> {
        let diag = if unaligned {
            "ptr_read_unaligned"
        } else {
            "ptr_read"
        };
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: diag.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Get the pointee type from the pointer type
        let pointee_type = match ptr_type.kind() {
            TypeKind::PtrConst(ptr_id) => self.body_type_pool().ptr_const_def(ptr_id),
            TypeKind::PtrMut(ptr_id) => self.body_type_pool().ptr_mut_def(ptr_id),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: diag.to_string(),
                        expected: "ptr const T or ptr mut T".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
        };

        // The result type is the pointee type. Inference modeled @ptr_read's
        // result as a fresh type variable (the pointee is only known here), so
        // a binding/annotation constrained that variable to some concrete type
        // without ever comparing it to the pointee. Reconcile them now so a
        // mismatch (`let x: i32 = @ptr_read(p_to_i64)`) is E0206 like every
        // other path, instead of silently truncating (RUE-244). Skip when the
        // resolved type is unconstrained (`<error>` — e.g. no annotation) or
        // never; those carry no expectation to check against.
        if let Some(expected) = ctx.resolved_type_of(inst_ref)
            && !self.types_equivalent(expected, pointee_type)
            && !expected.is_error()
            && !expected.is_never()
            && !pointee_type.is_error()
        {
            return Err(self.type_mismatch_error(expected, pointee_type, span));
        }

        // Create the intrinsic call instruction
        let air_ref = air.add_intrinsic(None, name, &[ptr_result.air_ref], pointee_type, span)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            pointee_type,
            ptr_result.continues,
        ))
    }

    /// Analyze @ptr_write / @ptr_write_unaligned intrinsic: writes value through
    /// pointer. Signature: `@ptr_write(ptr: ptr mut T, value: T) -> ()`.
    ///
    /// `unaligned` selects `@ptr_write_unaligned` (ADR-0059 Phase 4, RUE-978);
    /// see [`Self::analyze_ptr_read_intrinsic`] for the aligned/unaligned split.
    pub(super) fn analyze_ptr_write_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        unaligned: bool,
    ) -> CompileResult<AnalysisResult> {
        let diag = if unaligned {
            "ptr_write_unaligned"
        } else {
            "ptr_write"
        };
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: diag.to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the pointer first so the pointee type is known before the
        // value argument is analyzed — a bare integer-literal value must infer
        // to the pointee type, not default to i32 (RUE-275).
        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Pointer must be ptr mut T
        let pointee_type = match ptr_type.kind() {
            TypeKind::PtrMut(ptr_id) => self.body_type_pool().ptr_mut_def(ptr_id),
            TypeKind::PtrConst(_) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: diag.to_string(),
                        expected: "ptr mut T (cannot write through ptr const)".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: diag.to_string(),
                        expected: "ptr mut T".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
        };

        // Analyze the value argument, propagating the expected pointee type into
        // a bare integer literal so `@ptr_write(p_i64, 99)` unifies the literal
        // to the pointee type (i64/u8/…) instead of defaulting to i32 and then
        // spuriously failing the equality check below (RUE-275). Mirrors the
        // struct-init field-literal coercion in `analyze_struct_init`; the
        // range check keeps `@ptr_write(p_u8, 300)` an honest E0800.
        let value_inst = self.body_rir_ref().get(args[1].value);
        let reachable_edges_before_value = ctx.loop_break_stack.clone();
        let value_result = match &value_inst.data {
            InstData::IntConst(value) if pointee_type.is_integer() => {
                if !pointee_type.literal_fits(*value) {
                    return Err(CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: *value,
                            ty: self.format_type_name(pointee_type),
                        },
                        value_inst.span,
                    ));
                }
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty: pointee_type,
                    span: value_inst.span,
                });
                AnalysisResult::new(air_ref, pointee_type)
            }
            _ => ctx.with_expected_type(Some(pointee_type), |ctx| {
                self.analyze_inst(air, args[1].value, ctx)
            })?,
        };
        if !ptr_result.continues {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_value);
        }
        let value_type = value_result.ty;

        // Check that value type matches pointee type
        if !self.types_compatible(value_type, pointee_type) {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: self.format_type_name(pointee_type),
                    found: self.format_type_name(value_type),
                },
                span,
            ));
        }

        // Create the intrinsic call instruction
        let air_ref = air.add_intrinsic(
            None,
            name,
            &[ptr_result.air_ref, value_result.air_ref],
            Type::UNIT,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            ptr_result.continues && value_result.continues,
        ))
    }

    /// Analyze @ptr_offset intrinsic: pointer arithmetic.
    /// Signature: @ptr_offset(ptr: ptr T, offset: i64) -> ptr T
    pub(super) fn analyze_ptr_offset_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_offset".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        let offset_result =
            self.analyze_sequenced_pointer_operand(air, args[1].value, ptr_result.continues, ctx)?;
        let ptr_type = ptr_result.ty;
        let offset_type = offset_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_offset".to_string(),
                    expected: "ptr const T or ptr mut T".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Validate offset type (must be integer)
        if !offset_type.is_integer() && !offset_type.is_error() && !offset_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_offset".to_string(),
                    expected: "integer offset".to_string(),
                    found: self.format_type_name(offset_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns same pointer type)
        let air_ref = air.add_intrinsic(
            None,
            name,
            &[ptr_result.air_ref, offset_result.air_ref],
            ptr_type,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            ptr_type,
            ptr_result.continues && offset_result.continues,
        ))
    }

    /// Analyze @ptr_to_int intrinsic: converts pointer to u64.
    /// Signature: @ptr_to_int(ptr: ptr T) -> u64
    pub(super) fn analyze_ptr_to_int_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_to_int".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_to_int".to_string(),
                    expected: "ptr const T or ptr mut T".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns u64)
        let air_ref = air.add_intrinsic(None, name, &[ptr_result.air_ref], Type::U64, span)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::U64,
            ptr_result.continues,
        ))
    }

    /// Analyze @int_to_ptr intrinsic: converts u64 to pointer.
    /// Signature: @int_to_ptr(addr: u64) -> ptr mut T
    /// The result type T is inferred from context (e.g., `let p: ptr mut i32 = @int_to_ptr(addr)`)
    pub(super) fn analyze_int_to_ptr_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "int_to_ptr".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let addr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let addr_type = addr_result.ty;

        // Validate address type (must be u64)
        if addr_type != Type::U64 && !addr_type.is_error() && !addr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "int_to_ptr".to_string(),
                    expected: "u64".to_string(),
                    found: self.format_type_name(addr_type),
                })),
                span,
            ));
        }

        // Get the result type from HM inference (must be a ptr mut T)
        let result_type = Self::get_resolved_type(ctx, inst_ref, span, "@int_to_ptr intrinsic")?;

        // The pointee type comes only from context. In a discarded/unconstrained
        // position (`@int_to_ptr(z);`) the result variable has nothing to fix it
        // and decays to `<error>`; report that cleanly instead of letting the
        // `<error>`-typed value reach the end of analysis as a graceful ICE
        // (RUE-153 backstop, found by the sema fuzzer). Args analyzed above via
        // `?`, so an `<error>` result here is specifically the unresolved-pointee
        // case, not a poisoned operand.
        if result_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::CannotInferPointeeType("int_to_ptr".to_string()),
                span,
            ));
        }
        // Validate that the inferred type is a mutable pointer
        if !result_type.is_ptr_mut() && !result_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "int_to_ptr".to_string(),
                    expected: "ptr mut T".to_string(),
                    found: self.format_type_name(result_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction
        let air_ref = air.add_intrinsic(None, name, &[addr_result.air_ref], result_type, span)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            result_type,
            addr_result.continues,
        ))
    }

    /// Analyze `@alloc(size, align)` and `@alloc_zeroed(size, align)`, the
    /// unified byte-and-alignment allocation entry points (ADR-0059 Phase 3,
    /// RUE-961 / RUE-968).
    ///
    /// `size` is a physical byte count and `align` a power-of-two byte count;
    /// the result is always `ptr mut u8`. Typed allocation is source-computed
    /// sugar — `@alloc(count * @size_of(T), @align_of(T))` — so nothing here
    /// consults a pointee type. `@alloc_zeroed` shares this shape and differs
    /// only in the dynamic guarantee that the storage reads as zero bytes.
    pub(super) fn analyze_alloc_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic = self.body_interner().resolve(&name).to_string();
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic,
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }
        let size = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        self.require_intrinsic_type(&intrinsic, size.ty, Type::U64, span)?;
        let align =
            self.analyze_sequenced_pointer_operand(air, args[1].value, size.continues, ctx)?;
        self.require_intrinsic_type(&intrinsic, align.ty, Type::U64, span)?;
        self.require_power_of_two_align(&intrinsic, args[1].value, span, ctx)?;
        let result_ty = Type::new_ptr_mut(self.body_type_pool().intern_ptr_mut_from_type(Type::U8));
        if let Some(expected) = ctx.resolved_type_of(inst_ref)
            && !self.types_equivalent(expected, result_ty)
            && !expected.is_error()
            && !expected.is_never()
        {
            return Err(self.type_mismatch_error(expected, result_ty, span));
        }
        let zeroed = name == self.known_symbols().alloc_zeroed;
        let air_ref = air.add_intrinsic(
            Some(if zeroed {
                crate::RuntimeCallKind::AllocZeroed
            } else {
                crate::RuntimeCallKind::Alloc
            }),
            name,
            &[size.air_ref, align.air_ref],
            result_ty,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            result_ty,
            size.continues && align.continues,
        ))
    }

    /// Analyze `@realloc(p, old_size, align, new_size) -> ptr mut u8`
    /// (ADR-0059 Phase 3, RUE-961). Every size is a physical byte count and
    /// `align` must equal the alignment the block was allocated with.
    pub(super) fn analyze_realloc_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 4 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "realloc".to_string(),
                    expected: 4,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        self.require_mut_u8_pointer("realloc", ptr.ty, span)?;
        let old_size =
            self.analyze_sequenced_pointer_operand(air, args[1].value, ptr.continues, ctx)?;
        let align = self.analyze_sequenced_pointer_operand(
            air,
            args[2].value,
            ptr.continues && old_size.continues,
            ctx,
        )?;
        let new_size = self.analyze_sequenced_pointer_operand(
            air,
            args[3].value,
            ptr.continues && old_size.continues && align.continues,
            ctx,
        )?;
        self.require_intrinsic_type("realloc", old_size.ty, Type::U64, span)?;
        self.require_intrinsic_type("realloc", align.ty, Type::U64, span)?;
        self.require_intrinsic_type("realloc", new_size.ty, Type::U64, span)?;
        self.require_power_of_two_align("realloc", args[2].value, span, ctx)?;
        let air_ref = air.add_intrinsic(
            Some(crate::RuntimeCallKind::Realloc),
            name,
            &[
                ptr.air_ref,
                old_size.air_ref,
                align.air_ref,
                new_size.air_ref,
            ],
            ptr.ty,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            ptr.ty,
            ptr.continues && old_size.continues && align.continues && new_size.continues,
        ))
    }

    /// Analyze `@resize(p, old_size, align, new_size) -> bool` (RUE-968), the
    /// in-place-only counterpart of `@realloc` modeled on Zig's
    /// `Allocator.resize`. The block never moves: the call either relabels the
    /// existing allocation as `new_size` bytes and evaluates to `true`, or
    /// changes nothing and evaluates to `false`. Its operand shape is exactly
    /// `@realloc`'s so a caller can fall back to `@realloc` without reordering
    /// arguments.
    pub(super) fn analyze_resize_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 4 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "resize".to_string(),
                    expected: 4,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        self.require_mut_u8_pointer("resize", ptr.ty, span)?;
        let old_size =
            self.analyze_sequenced_pointer_operand(air, args[1].value, ptr.continues, ctx)?;
        let align = self.analyze_sequenced_pointer_operand(
            air,
            args[2].value,
            ptr.continues && old_size.continues,
            ctx,
        )?;
        let new_size = self.analyze_sequenced_pointer_operand(
            air,
            args[3].value,
            ptr.continues && old_size.continues && align.continues,
            ctx,
        )?;
        self.require_intrinsic_type("resize", old_size.ty, Type::U64, span)?;
        self.require_intrinsic_type("resize", align.ty, Type::U64, span)?;
        self.require_intrinsic_type("resize", new_size.ty, Type::U64, span)?;
        self.require_power_of_two_align("resize", args[2].value, span, ctx)?;
        let air_ref = air.add_intrinsic(
            Some(crate::RuntimeCallKind::Resize),
            name,
            &[
                ptr.air_ref,
                old_size.air_ref,
                align.air_ref,
                new_size.air_ref,
            ],
            Type::BOOL,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::BOOL,
            ptr.continues && old_size.continues && align.continues && new_size.continues,
        ))
    }

    /// Analyze `@free(p, size, align)` (ADR-0059 Phase 3, RUE-961). The
    /// sizeless-allocator ABI: the caller returns the block's `(size, align)`
    /// so the runtime keeps no per-block header.
    pub(super) fn analyze_free_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "free".to_string(),
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        self.require_mut_u8_pointer("free", ptr.ty, span)?;
        let size =
            self.analyze_sequenced_pointer_operand(air, args[1].value, ptr.continues, ctx)?;
        let align = self.analyze_sequenced_pointer_operand(
            air,
            args[2].value,
            ptr.continues && size.continues,
            ctx,
        )?;
        self.require_intrinsic_type("free", size.ty, Type::U64, span)?;
        self.require_intrinsic_type("free", align.ty, Type::U64, span)?;
        self.require_power_of_two_align("free", args[2].value, span, ctx)?;
        let air_ref = air.add_intrinsic(
            Some(crate::RuntimeCallKind::Free),
            name,
            &[ptr.air_ref, size.air_ref, align.air_ref],
            Type::UNIT,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            ptr.continues && size.continues && align.continues,
        ))
    }

    /// Reject a comptime-constant allocator `align` argument that is zero or
    /// not a power of two (ADR-0059, RUE-960/RUE-961). A non-constant `align`
    /// evaluates to `None` here and is permitted: the power-of-two contract for
    /// runtime values is documented checked-gate territory (spec 9.2:13),
    /// enforced by the allocator rather than the compiler.
    fn require_power_of_two_align(
        &mut self,
        name: &str,
        align_ref: InstRef,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        if let Some(ConstValue::Integer(value)) = self.try_evaluate_const_in_fn(align_ref, ctx) {
            let bits = value as u64;
            if bits == 0 || !bits.is_power_of_two() {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicAlignNotPowerOfTwo {
                        name: name.to_string(),
                        value: bits,
                    },
                    span,
                ));
            }
        }
        Ok(())
    }

    /// Analyze the bulk byte-move pair `@byte_copy(dst, src, size)` and
    /// `@byte_move(dst, src, size)` (ADR-0059 Phase 1, RUE-937 / RUE-964).
    /// Both take `dst: ptr mut u8`, `src: ptr const u8 | ptr mut u8`, and a
    /// `size: u64` physical byte count, and both evaluate to `()` with
    /// `size == 0` a no-op. They differ only in the overlap contract:
    /// `@byte_copy` is memcpy-shaped and overlapping regions are undefined
    /// behavior, while `@byte_move` is memmove-shaped and copies as if through
    /// a temporary buffer. They lower to the `__rue_byte_copy` and
    /// `__rue_byte_move` runtime helpers respectively.
    pub(super) fn analyze_byte_copy_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic = self.body_interner().resolve(&name).to_string();
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic,
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }
        let dst = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        self.require_mut_u8_pointer(&intrinsic, dst.ty, span)?;
        let src = self.analyze_sequenced_pointer_operand(air, args[1].value, dst.continues, ctx)?;
        self.require_u8_pointer(&intrinsic, src.ty, span)?;
        let size = self.analyze_sequenced_pointer_operand(
            air,
            args[2].value,
            dst.continues && src.continues,
            ctx,
        )?;
        self.require_intrinsic_type(&intrinsic, size.ty, Type::U64, span)?;
        let overlapping = name == self.known_symbols().byte_move;
        let air_ref = air.add_intrinsic(
            Some(if overlapping {
                crate::RuntimeCallKind::ByteMove
            } else {
                crate::RuntimeCallKind::ByteCopy
            }),
            name,
            &[dst.air_ref, src.air_ref, size.air_ref],
            Type::UNIT,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            dst.continues && src.continues && size.continues,
        ))
    }

    /// Analyze `@byte_set(dst: ptr mut u8, byte: u8, size: u64) -> ()`
    /// (ADR-0058 Phase 1, RUE-937). A memset-shaped fill of `size` physical
    /// bytes with `byte`; `size == 0` is a no-op. Lowers to the shared
    /// `__rue_byte_set` runtime helper.
    pub(super) fn analyze_byte_set_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "byte_set".to_string(),
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }
        let dst = self.analyze_sequenced_pointer_operand(air, args[0].value, true, ctx)?;
        self.require_mut_u8_pointer("byte_set", dst.ty, span)?;
        let byte =
            self.analyze_sequenced_pointer_operand(air, args[1].value, dst.continues, ctx)?;
        self.require_intrinsic_type("byte_set", byte.ty, Type::U8, span)?;
        let size = self.analyze_sequenced_pointer_operand(
            air,
            args[2].value,
            dst.continues && byte.continues,
            ctx,
        )?;
        self.require_intrinsic_type("byte_set", size.ty, Type::U64, span)?;
        let air_ref = air.add_intrinsic(
            Some(crate::RuntimeCallKind::ByteSet),
            name,
            &[dst.air_ref, byte.air_ref, size.air_ref],
            Type::UNIT,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            dst.continues && byte.continues && size.continues,
        ))
    }

    fn require_intrinsic_type(
        &self,
        name: &str,
        found: Type,
        expected: Type,
        span: Span,
    ) -> CompileResult<()> {
        if self.types_compatible(found, expected) {
            return Ok(());
        }
        Err(CompileError::new(
            ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                name: name.to_string(),
                expected: self.format_type_name(expected),
                found: self.format_type_name(found),
            })),
            span,
        ))
    }

    fn require_u8_pointer(&self, name: &str, ty: Type, span: Span) -> CompileResult<()> {
        let is_u8 = match ty.kind() {
            TypeKind::PtrConst(id) => self.body_type_pool().ptr_const_def(id) == Type::U8,
            TypeKind::PtrMut(id) => self.body_type_pool().ptr_mut_def(id) == Type::U8,
            _ => false,
        };
        if is_u8 || ty.is_error() || ty.is_never() {
            return Ok(());
        }
        Err(CompileError::new(
            ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                name: name.to_string(),
                expected: "ptr const u8 or ptr mut u8".to_string(),
                found: self.format_type_name(ty),
            })),
            span,
        ))
    }

    fn require_mut_u8_pointer(&self, name: &str, ty: Type, span: Span) -> CompileResult<()> {
        let is_mut_u8 = match ty.kind() {
            TypeKind::PtrMut(id) => self.body_type_pool().ptr_mut_def(id) == Type::U8,
            _ => false,
        };
        if is_mut_u8 || ty.is_error() || ty.is_never() {
            return Ok(());
        }
        Err(CompileError::new(
            ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                name: name.to_string(),
                expected: "ptr mut u8".to_string(),
                found: self.format_type_name(ty),
            })),
            span,
        ))
    }

    /// Analyze @raw / @field_ptr address-of intrinsics: forms a raw pointer to
    /// an addressable place without taking a reference.
    ///
    /// `@raw(place) -> ptr const T` / `@raw_mut(place) -> ptr mut T` accept any
    /// place; `@field_ptr(s.field) -> ptr mut F` (RUE-301) is the same address
    /// computation restricted to a struct-field place (the field-place check is
    /// enforced by [`Self::analyze_field_ptr_intrinsic`] before it delegates
    /// here). `result_name` is the intrinsic name recorded in the AIR — codegen
    /// treats `raw`/`raw_mut`/`field_ptr` identically (all lower the place's
    /// address), so the three share this analysis and this lowering path.
    pub(super) fn analyze_addr_of_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        is_mut: bool,
        result_name: Spur,
        diag_name: &str,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = diag_name;

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

        // @raw / @raw_mut take the ADDRESS of a place; per spec 3.8:57 a pointer
        // does not own its pointee, so the operand is borrowed (address-of), not
        // consumed. Mirror the ByRef un-move (as `borrow` operands and String
        // index reads do): snapshot the root's move state, then cancel the move
        // the operand analysis records. This keeps the operand live so its
        // destructor still runs exactly once at scope exit and later uses remain
        // legal (RUE-222) — rather than silently leaking it or rejecting a valid
        // later use with E0205.
        let operand = args[0].value;
        let operand_root = self.extract_root_variable(operand);
        let operand_move_state_before = self.snapshot_move_state(operand_root, ctx);
        // Analyze the operand as a borrow (`byref_arg_root`), exactly as `@dbg`
        // and by-ref call arguments do. Without this, addressing a by-ref
        // parameter (`@raw_mut(a)` where `a: inout T` is non-Copy) reads the
        // param on the move path and is rejected outright (E0437 for `inout`,
        // E0429 for `borrow`) before the address-of semantics below cancel the
        // move.
        // Address-of is a borrow, so the read must not count as a move; this
        // makes `std.mem.swap` and other by-ref-param addressing work (RUE-943).
        let arg_result = self.analyze_with_borrow_root(air, operand, operand_root, ctx)?;

        // @raw/@raw_mut take the ADDRESS of an addressable PLACE (spec 9.1:12,
        // ADR-0028), so the operand MUST be a place. A non-place operand
        // (literal, arithmetic, call result, inlined global const, module
        // member) has no storage to address; codegen would reinterpret the
        // computed value's bits as a pointer and dereference garbage (RUE-274,
        // a soundness hole). A place read analyzes to exactly a variable load
        // (`Load`), a parameter read (`Param`), or a projected place-read
        // (field/element `PlaceRead`) — the same set `try_trace_place` accepts
        // for inout/borrow operands. A non-Copy place read is additionally
        // wrapped in a `MarkMoved` move marker (which the address-of below
        // cancels, since taking an address borrows rather than moves), so peel
        // that wrapper before inspecting the underlying read. Every non-place
        // lowers to some other AIR inst (`Const`, an arithmetic op, `Call`, a
        // non-place `FieldGet`/`IndexGet` off a temporary, ...) and is rejected.
        let operand_is_place = self.is_addressable_read(air, arg_result.air_ref);
        if !operand_is_place && !arg_result.ty.is_error() {
            return Err(CompileError::new(ErrorKind::RawRequiresPlace, span));
        }

        let pointee_type = arg_result.ty;
        self.restore_move_state_and_cancel(air, arg_result.air_ref, operand_move_state_before, ctx);

        // Create the pointer type
        let result_type = if is_mut {
            let ptr_type_id = self.body_type_pool().intern_ptr_mut_from_type(pointee_type);
            Type::new_ptr_mut(ptr_type_id)
        } else {
            let ptr_type_id = self
                .body_type_pool()
                .intern_ptr_const_from_type(pointee_type);
            Type::new_ptr_const(ptr_type_id)
        };

        // Create the intrinsic call instruction. `result_name` distinguishes
        // @raw/@raw_mut/@field_ptr in the AIR; codegen lowers all three the
        // same way (address of the operand place).
        let name = result_name;
        let air_ref = air.add_intrinsic(None, name, &[arg_result.air_ref], result_type, span)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            result_type,
            arg_result.continues,
        ))
    }

    /// Analyze `@field_ptr(s.field)` (RUE-301): a raw `ptr mut F` to a struct
    /// field place, the `&raw mut (*p).field` analog. Unlike `@raw`, the
    /// operand MUST be a field-access expression (`s.field`) — that is exactly
    /// what makes it "compiler-mediated field access": the pointer addresses
    /// the field the compiler placed, so unchecked code walks a struct without
    /// hardcoding slot offsets. The address computation, place liveness
    /// handling, and codegen are shared with `@raw_mut` via
    /// [`Self::analyze_addr_of_intrinsic`]; the only extra obligation here is
    /// requiring a field place.
    pub(super) fn analyze_field_ptr_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "field_ptr".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // The operand must be a field access `s.field`. Inspect the RIR operand
        // directly: a field access lowers to `InstData::FieldGet` before
        // analysis, so a non-field operand (a bare variable, an index, a call
        // result, a literal) is rejected up front with a targeted diagnostic
        // rather than @raw's generic "not a place" message.
        if !matches!(
            self.body_rir_ref().get(args[0].value).data,
            InstData::FieldGet { .. }
        ) {
            return Err(CompileError::new(ErrorKind::FieldPtrRequiresField, span));
        }

        // @field_ptr yields a mutable raw pointer (like `&raw mut`), so it
        // supports both @ptr_read and @ptr_write round-trips through the field.
        let field_ptr = self.known_symbols().field_ptr;
        self.analyze_addr_of_intrinsic(air, args, span, ctx, true, field_ptr, "field_ptr")
    }

    /// Analyze @syscall intrinsic: perform a raw OS syscall.
    /// Signature: @syscall(syscall_num: u64, arg0?: u64, ..., arg5?: u64) -> i64
    ///
    /// Takes a syscall number and up to 6 arguments, all of which must be u64.
    /// Returns i64 (the syscall return value, which may be negative for errors).
    ///
    /// `@syscall` is an unchecked operation (spec 9.2:3a): the shared
    /// checked-block gate in `analyze_intrinsic_impl` rejects it outside a
    /// `checked` block before this analysis runs.
    pub(super) fn analyze_syscall_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Syscall takes 1-7 arguments: syscall number + up to 6 arguments
        if args.is_empty() || args.len() > 7 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "syscall".to_string(),
                    expected: 7, // Show max expected for "at least 1, at most 7"
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze all arguments and verify they are u64
        let mut arg_refs = Vec::with_capacity(args.len());
        let mut continues = true;
        for (i, arg) in args.iter().enumerate() {
            let reachable_edges_before_arg = ctx.loop_break_stack.clone();
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
            if !continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_arg);
            }
            continues &= arg_result.continues;
            let arg_type = arg_result.ty;

            // All syscall arguments must be u64
            if arg_type != Type::U64 && !arg_type.is_error() && !arg_type.is_never() {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "syscall".to_string(),
                        expected: format!("u64 for argument {}", i),
                        found: self.format_type_name(arg_type),
                    })),
                    span,
                ));
            }

            arg_refs.push(arg_result.air_ref);
        }

        // Create the intrinsic call instruction
        let air_ref = air.add_intrinsic(None, name, &arg_refs, Type::I64, span)?;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::I64,
            continues,
        ))
    }

    /// Analyze @target_arch() intrinsic - returns target CPU architecture enum.
    ///
    /// This intrinsic takes no arguments and returns an Arch enum value
    /// representing the target CPU architecture (X86_64 or Aarch64).
    pub(super) fn analyze_target_arch_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_arch".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let arch_enum_id = self
            .builtin_arch_id()
            .expect("Arch enum not injected - internal compiler error");

        // Determine variant index from the requested compilation target, not
        // the host running the compiler. Cross-target `--emit` must specialize
        // target intrinsics for the emitted target (RUE-417).
        let variant_index = match self.target().arch() {
            Arch::X86_64 => 0,
            Arch::Aarch64 => 1,
        };

        let result_type = Type::new_enum(arch_enum_id);
        let air_ref = air.add_enum_variant(arch_enum_id, variant_index, &[], result_type, span)?;
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @target_os() intrinsic - returns target operating system enum.
    ///
    /// This intrinsic takes no arguments and returns an Os enum value
    /// representing the target operating system (Linux or Macos).
    pub(super) fn analyze_target_os_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_os".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let os_enum_id = self
            .builtin_os_id()
            .expect("Os enum not injected - internal compiler error");

        // Determine variant index from the requested compilation target, not
        // the host running the compiler. Cross-target `--emit` must specialize
        // target intrinsics for the emitted target (RUE-417).
        let variant_index = match self.target().os() {
            Os::Linux => 0,
            Os::Macos => 1,
        };

        let result_type = Type::new_enum(os_enum_id);
        let air_ref = air.add_enum_variant(os_enum_id, variant_index, &[], result_type, span)?;
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @target_data_model() intrinsic - returns the target C data model.
    ///
    /// This intrinsic takes no arguments and returns a DataModel enum value
    /// (Ilp32, Lp64, or Llp64) describing the widths the target's platform
    /// psABI assigns to C `int`, `long`, and pointers (ADR-0064 Amendment 1).
    /// Architecture alone does not fix the data model, so this is a distinct
    /// target fact.
    pub(super) fn analyze_target_data_model_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_data_model".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let data_model_enum_id = self
            .builtin_data_model_id()
            .expect("DataModel enum not injected - internal compiler error");

        // Determine variant index from the requested compilation target, not
        // the host running the compiler. Cross-target `--emit` must specialize
        // target intrinsics for the emitted target (RUE-417). Variant order
        // matches `rue_target::DataModel`.
        let variant_index = match self.target().data_model() {
            DataModel::Ilp32 => 0,
            DataModel::Lp64 => 1,
            DataModel::Llp64 => 2,
        };

        let result_type = Type::new_enum(data_model_enum_id);
        let air_ref =
            air.add_enum_variant(data_model_enum_id, variant_index, &[], result_type, span)?;
        Ok(AnalysisResult::new(air_ref, result_type))
    }
}
