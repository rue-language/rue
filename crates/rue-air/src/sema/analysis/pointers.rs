//! Pointer and low-level intrinsics: ptr read/write/offset, alloc/free/realloc, addr-of, syscall, target arch/os.
//!
//! This category owns pointer and platform-facing intrinsic analysis within
//! the canonical semantic-analysis implementation.

use super::*;

impl<'a> BodySema<'a> {
    /// Analyze @ptr_read intrinsic: reads value through pointer.
    /// Signature: @ptr_read(ptr: ptr const T) -> T
    pub(super) fn analyze_ptr_read_intrinsic(
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
                    name: "ptr_read".to_string(),
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
            TypeKind::PtrConst(ptr_id) => self.type_pool.ptr_const_def(ptr_id),
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_read".to_string(),
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
        if let Some(&expected) = ctx.resolved_types.get(&inst_ref)
            && expected != pointee_type
            && !expected.is_error()
            && !expected.is_never()
            && !pointee_type.is_error()
        {
            return Err(self.type_mismatch_error(expected, pointee_type, span));
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 1,
            },
            ty: pointee_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, pointee_type))
    }

    /// Analyze @ptr_write intrinsic: writes value through pointer.
    /// Signature: @ptr_write(ptr: ptr mut T, value: T) -> ()
    pub(super) fn analyze_ptr_write_intrinsic(
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
                    name: "ptr_write".to_string(),
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
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            TypeKind::PtrConst(_) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_write".to_string(),
                        expected: "ptr mut T (cannot write through ptr const)".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_write".to_string(),
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
        let value_inst = self.rir.get(args[1].value);
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
        let value_type = value_result.ty;

        // Check that value type matches pointee type
        if value_type != pointee_type && !value_type.is_error() && !value_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: self.format_type_name(pointee_type),
                    found: self.format_type_name(value_type),
                },
                span,
            ));
        }

        // Create the intrinsic call instruction
        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), value_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 2,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
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

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let offset_result = self.analyze_inst(air, args[1].value, ctx)?;
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
        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), offset_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 2,
            },
            ty: ptr_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ptr_type))
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
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 1,
            },
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
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
        let args_start = air.add_extra(&[addr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @alloc intrinsic: allocate an uninitialized heap block (RUE-1).
    /// Signature: @alloc(count: u64) -> ptr mut T
    /// The element type T (and thus the result pointer type `ptr mut T`) is
    /// inferred from context, exactly like @int_to_ptr. The allocation is
    /// `count * size_of(T)` bytes; the returned pointer is null on failure
    /// (the caller is expected to check), so this is an unchecked operation.
    pub(super) fn analyze_alloc_intrinsic(
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
                    name: "alloc".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let count_result = self.analyze_inst(air, args[0].value, ctx)?;
        let count_type = count_result.ty;
        if count_type != Type::U64 && !count_type.is_error() && !count_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "alloc".to_string(),
                    expected: "u64".to_string(),
                    found: self.format_type_name(count_type),
                })),
                span,
            ));
        }

        // The result type comes from HM inference (assignment/annotation
        // context) and must be a mutable pointer `ptr mut T`. In a discarded or
        // otherwise unconstrained position (`@alloc(4);`) the result variable
        // has no context to fix its pointee and decays to `<error>`; report
        // that cleanly rather than letting the `<error>`-typed value reach the
        // end of analysis as a graceful ICE (RUE-153 backstop, found by the
        // sema fuzzer). The `count` arg was analyzed above via `?`, so an
        // `<error>` result here is specifically the unresolved-pointee case.
        let result_type = Self::get_resolved_type(ctx, inst_ref, span, "@alloc intrinsic")?;
        if result_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::CannotInferPointeeType("alloc".to_string()),
                span,
            ));
        }
        if !result_type.is_ptr_mut() && !result_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "alloc".to_string(),
                    expected: "ptr mut T".to_string(),
                    found: self.format_type_name(result_type),
                })),
                span,
            ));
        }

        let args_start = air.add_extra(&[count_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::AllocTyped),
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @free intrinsic: free a block previously `@alloc`'d (RUE-1).
    /// Signature: @free(ptr: ptr mut T, count: u64) -> ()
    /// `count` must match the element count passed to `@alloc` so the runtime
    /// can compute the block size.
    pub(super) fn analyze_free_intrinsic(
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
                    name: "free".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;
        if !ptr_type.is_ptr_mut() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "free".to_string(),
                    expected: "ptr mut T".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        let count_result = self.analyze_inst(air, args[1].value, ctx)?;
        let count_type = count_result.ty;
        if count_type != Type::U64 && !count_type.is_error() && !count_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "free".to_string(),
                    expected: "u64".to_string(),
                    found: self.format_type_name(count_type),
                })),
                span,
            ));
        }

        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), count_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::FreeTyped),
                name,
                args_start,
                args_len: 2,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze @realloc intrinsic: grow/shrink an `@alloc`'d block (RUE-1).
    /// Signature: @realloc(ptr: ptr mut T, old_count: u64, new_count: u64) -> ptr mut T
    /// The result pointer has the same type as `ptr`; contents up to
    /// `min(old_count, new_count)` elements are preserved (runtime copies).
    pub(super) fn analyze_realloc_intrinsic(
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
                    name: "realloc".to_string(),
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;
        if !ptr_type.is_ptr_mut() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "realloc".to_string(),
                    expected: "ptr mut T".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        let old_result = self.analyze_inst(air, args[1].value, ctx)?;
        let new_result = self.analyze_inst(air, args[2].value, ctx)?;
        for count_result in [&old_result, &new_result] {
            let count_type = count_result.ty;
            if count_type != Type::U64 && !count_type.is_error() && !count_type.is_never() {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "realloc".to_string(),
                        expected: "u64".to_string(),
                        found: self.format_type_name(count_type),
                    })),
                    span,
                ));
            }
        }

        let args_start = air.add_extra(&[
            ptr_result.air_ref.as_u32(),
            old_result.air_ref.as_u32(),
            new_result.air_ref.as_u32(),
        ]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::ReallocTyped),
                name,
                args_start,
                args_len: 3,
            },
            ty: ptr_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ptr_type))
    }

    /// Analyze the preview raw-byte intrinsic family (RUE-879). Unlike typed
    /// pointer operations, byte counts and offsets here are physical bytes and
    /// access operations transfer exactly one byte.
    pub(super) fn analyze_alloc_bytes_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.require_preview(PreviewFeature::RawBytes, "@alloc_bytes intrinsic", span)?;
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "alloc_bytes".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }
        let size = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_intrinsic_type("alloc_bytes", size.ty, Type::U64, span)?;
        let result_ty = Type::new_ptr_mut(self.type_pool.intern_ptr_mut_from_type(Type::U8));
        if let Some(&expected) = ctx.resolved_types.get(&inst_ref)
            && expected != result_ty
            && !expected.is_error()
            && !expected.is_never()
        {
            return Err(self.type_mismatch_error(expected, result_ty, span));
        }
        let args_start = air.add_extra(&[size.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::AllocBytes),
                name,
                args_start,
                args_len: 1,
            },
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    pub(super) fn analyze_realloc_bytes_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.require_preview(PreviewFeature::RawBytes, "@realloc_bytes intrinsic", span)?;
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "realloc_bytes".to_string(),
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_mut_u8_pointer("realloc_bytes", ptr.ty, span)?;
        let old_size = self.analyze_inst(air, args[1].value, ctx)?;
        let new_size = self.analyze_inst(air, args[2].value, ctx)?;
        self.require_intrinsic_type("realloc_bytes", old_size.ty, Type::U64, span)?;
        self.require_intrinsic_type("realloc_bytes", new_size.ty, Type::U64, span)?;
        let args_start = air.add_extra(&[
            ptr.air_ref.as_u32(),
            old_size.air_ref.as_u32(),
            new_size.air_ref.as_u32(),
        ]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::ReallocBytes),
                name,
                args_start,
                args_len: 3,
            },
            ty: ptr.ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ptr.ty))
    }

    pub(super) fn analyze_free_bytes_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.require_preview(PreviewFeature::RawBytes, "@free_bytes intrinsic", span)?;
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "free_bytes".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_mut_u8_pointer("free_bytes", ptr.ty, span)?;
        let size = self.analyze_inst(air, args[1].value, ctx)?;
        self.require_intrinsic_type("free_bytes", size.ty, Type::U64, span)?;
        let args_start = air.add_extra(&[ptr.air_ref.as_u32(), size.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: Some(crate::RuntimeCallKind::FreeBytes),
                name,
                args_start,
                args_len: 2,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    pub(super) fn analyze_byte_read_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.require_preview(PreviewFeature::RawBytes, "@byte_read intrinsic", span)?;
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "byte_read".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_u8_pointer("byte_read", ptr.ty, span)?;
        let offset = self.analyze_inst(air, args[1].value, ctx)?;
        self.require_intrinsic_type("byte_read", offset.ty, Type::U64, span)?;
        let args_start = air.add_extra(&[ptr.air_ref.as_u32(), offset.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 2,
            },
            ty: Type::U8,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U8))
    }

    pub(super) fn analyze_byte_write_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        self.require_preview(PreviewFeature::RawBytes, "@byte_write intrinsic", span)?;
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "byte_write".to_string(),
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }
        let ptr = self.analyze_inst(air, args[0].value, ctx)?;
        self.require_mut_u8_pointer("byte_write", ptr.ty, span)?;
        let offset = self.analyze_inst(air, args[1].value, ctx)?;
        let value = self.analyze_inst(air, args[2].value, ctx)?;
        self.require_intrinsic_type("byte_write", offset.ty, Type::U64, span)?;
        self.require_intrinsic_type("byte_write", value.ty, Type::U8, span)?;
        let args_start = air.add_extra(&[
            ptr.air_ref.as_u32(),
            offset.air_ref.as_u32(),
            value.air_ref.as_u32(),
        ]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 3,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    fn require_intrinsic_type(
        &self,
        name: &str,
        found: Type,
        expected: Type,
        span: Span,
    ) -> CompileResult<()> {
        if found == expected || found.is_error() || found.is_never() {
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
            TypeKind::PtrConst(id) => self.type_pool.ptr_const_def(id) == Type::U8,
            TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id) == Type::U8,
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
            TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id) == Type::U8,
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
        let operand_move_state_before = operand_root.and_then(|v| ctx.moved_vars.get(&v).cloned());
        let arg_result = self.analyze_inst(air, operand, ctx)?;

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
        let mut place_ref = arg_result.air_ref;
        if let AirInstData::MarkMoved { value, .. } = air.get(place_ref).data {
            place_ref = value;
        }
        let operand_is_place = matches!(
            air.get(place_ref).data,
            AirInstData::Load { .. } | AirInstData::Param { .. } | AirInstData::PlaceRead { .. }
        );
        if !operand_is_place && !arg_result.ty.is_error() {
            return Err(CompileError::new(ErrorKind::RawRequiresPlace, span));
        }

        let pointee_type = arg_result.ty;
        if let Some(var) = operand_root {
            match operand_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(arg_result.air_ref);

        // Create the pointer type
        let result_type = if is_mut {
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_type);
            Type::new_ptr_mut(ptr_type_id)
        } else {
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_type);
            Type::new_ptr_const(ptr_type_id)
        };

        // Create the intrinsic call instruction. `result_name` distinguishes
        // @raw/@raw_mut/@field_ptr in the AIR; codegen lowers all three the
        // same way (address of the operand place).
        let name = result_name;
        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
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
        if !matches!(self.rir.get(args[0].value).data, InstData::FieldGet { .. }) {
            return Err(CompileError::new(ErrorKind::FieldPtrRequiresField, span));
        }

        // @field_ptr yields a mutable raw pointer (like `&raw mut`), so it
        // supports both @ptr_read and @ptr_write round-trips through the field.
        let field_ptr = self.known.field_ptr;
        self.analyze_addr_of_intrinsic(air, args, span, ctx, true, field_ptr, "field_ptr")
    }

    /// Analyze @syscall intrinsic: perform a raw OS syscall.
    /// Signature: @syscall(syscall_num: u64, arg0?: u64, ..., arg5?: u64) -> i64
    ///
    /// Takes a syscall number and up to 6 arguments, all of which must be u64.
    /// Returns i64 (the syscall return value, which may be negative for errors).
    /// Requires a checked block.
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
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
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

            arg_refs.push(arg_result.air_ref.as_u32());
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&arg_refs);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime: None,
                name,
                args_start,
                args_len: args.len() as u32,
            },
            ty: Type::I64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::I64))
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
            .builtin_arch_id
            .expect("Arch enum not injected - internal compiler error");

        // Determine variant index from the requested compilation target, not
        // the host running the compiler. Cross-target `--emit` must specialize
        // target intrinsics for the emitted target (RUE-417).
        let variant_index = match self.target.arch() {
            Arch::X86_64 => 0,
            Arch::Aarch64 => 1,
        };

        let result_type = Type::new_enum(arch_enum_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: arch_enum_id,
                variant_index,
                payload_start: 0,
                payload_len: 0,
            },
            ty: result_type,
            span,
        });
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
            .builtin_os_id
            .expect("Os enum not injected - internal compiler error");

        // Determine variant index from the requested compilation target, not
        // the host running the compiler. Cross-target `--emit` must specialize
        // target intrinsics for the emitted target (RUE-417).
        let variant_index = match self.target.os() {
            Os::Linux => 0,
            Os::Macos => 1,
        };

        let result_type = Type::new_enum(os_enum_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: os_enum_id,
                variant_index,
                payload_start: 0,
                payload_len: 0,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }
}
