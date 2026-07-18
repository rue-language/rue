//! Instruction category analysis methods.
//!
//! This module contains the per-category analysis methods extracted from `analyze_inst`.
//! Each category method handles a specific group of related RIR instructions:
//!
//! - [`analyze_literal`] - Integer, boolean, string, and unit constants
//! - [`analyze_unary_op`] - Negation, logical NOT, bitwise NOT
//!
//! Control-flow expressions are owned by the sibling `control_flow` module.
//! Place construction, variable access, assignment, field/index reads and
//! writes, and move/borrow checks are owned by `analysis::ownership`.
//!
//! - [`analyze_struct_ops`] - StructDecl, StructInit, FieldGet, FieldSet
//! - [`analyze_array_ops`] - ArrayInit, IndexGet, IndexSet
//! - [`analyze_enum_ops`] - EnumDecl, EnumVariant
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
use rue_error::{CompileError, CompileResult, ErrorKind, MissingFieldsError, OptionExt};
use rue_rir::{InstData, InstRef, RirParamMode};

use crate::sema::context::ConstValue;
use rue_span::Span;

use super::analysis::FirstClassStrSite;
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
    // Struct operations: StructDecl, StructInit, FieldGet, FieldSet
    // ========================================================================

    /// Analyze a struct operation instruction.
    ///
    /// Handles: StructDecl, StructInit, FieldGet, FieldSet
    pub(crate) fn analyze_struct_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::StructDecl { .. } => {
                // Struct declarations are handled at the top level
                Err(CompileError::new(
                    ErrorKind::InternalError(
                        "StructDecl should not appear in expression context".to_string(),
                    ),
                    inst.span,
                ))
            }

            InstData::StructInit {
                module,
                ctor_head,
                type_name,
                fields,
                shorthand_span,
            } => self.analyze_struct_init(
                air,
                *module,
                *ctor_head,
                *type_name,
                fields,
                *shorthand_span,
                inst.span,
                ctx,
            ),

            InstData::FieldGet { base, field } => {
                self.analyze_field_get(air, inst_ref, *base, *field, inst.span, ctx)
            }

            InstData::FieldSet { base, field, value } => {
                self.analyze_field_set(air, *base, *field, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_struct_ops called with non-struct instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a struct initialization.
    #[allow(clippy::too_many_arguments)]
    fn analyze_struct_init(
        &mut self,
        air: &mut Air,
        module: Option<InstRef>,
        ctor_head: Option<InstRef>,
        type_name: Spur,
        fields: &rue_rir::RirFieldInitsRange,
        _shorthand_span: Option<Span>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Field-init shorthand (`P { x }` desugaring to `P { x: x }`, RUE-613) is
        // stabilized (RUE-628): it needs no preview gate. AstGen already
        // desugared the shorthand to explicit `x: x` field inits before this
        // point, so `_shorthand_span` (the first shorthand field's span, retained
        // as diagnostic provenance) is no longer consumed here.
        let field_inits = self.rir.field_inits(fields);
        // Look up the struct type
        // First check if it's a comptime type variable (e.g., `let Point = make_point(); Point { ... }`)
        let type_name_str = self.interner.resolve(&type_name);
        let struct_id = if let Some(head_ref) = ctor_head {
            // Inline type-constructor struct-literal head `F(args) { ... }`
            // (RUE-596, spec 4.14:23): the head call reduces to a concrete type
            // at comptime; construct as if the type had been bound to a name
            // first (`let P = F(args); P { .. }`).
            let head_result = self.analyze_inst(air, head_ref, ctx)?;
            let AirInstData::TypeConst(reduced_ty) = air.get(head_result.air_ref).data else {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "a type".to_string(),
                        found: head_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            };
            match reduced_ty.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "struct type".to_string(),
                            found: reduced_ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            }
        } else if let Some(module_ref) = module {
            let module_result = self.analyze_inst(air, module_ref, ctx)?;
            let Some(module_id) = module_result.ty.as_module() else {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "module".to_string(),
                        found: module_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            };
            let module_def = self.module_registry.get_def(module_id);
            let module_file_id = Some(module_def.file_id);
            let struct_id = module_file_id
                .and_then(|file_id| {
                    self.structs_by_file_name
                        .get(&(file_id, type_name))
                        .copied()
                })
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.to_string()), span)?;
            // Module-qualified visibility is E0706 (RUE-525), uniform with
            // enum members and associated-function calls through a module;
            // E0460 is the diagnostic for unqualified naming forms.
            let def = self.type_pool.struct_def(struct_id);
            if !self.is_accessible(span.file_id, def.file_id, def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "struct".to_string(),
                        name: type_name_str.to_string(),
                    },
                    span,
                ));
            }
            struct_id
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
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
        } else if let Some(info) = self.constants_by_file_name.get(&(span.file_id, type_name))
            && let ConstValue::Type(ty) = info.value
        {
            // Module-level `const P = Point(i32); P { .. }` (RUE-595): the
            // specialization arrived through a `const` binding, mirroring the
            // comptime-type-variable branch above — privacy-exempt for the same
            // reason (the type value came from a binding, not by naming the
            // struct). Without this arm the literal head was `UnknownType`
            // (E0204) even though the annotation form (`fn f() -> P`) resolved.
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
            let struct_id = self
                .structs_by_file_name
                .get(&(span.file_id, type_name))
                .copied()
                .or_else(|| self.resolve_builtin_struct_name(type_name))
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.to_string()), span)?;
            // Privacy (E0460, RUE-183): a struct literal names the type
            // unqualified, so a private struct from another directory is not
            // constructible here — privacy is uniform across item kinds
            // (spec 10.3:1, 10.3:7). The comptime-type-variable branch above
            // is exempt: the type value arrived through a binding (e.g. a
            // `pub` comptime function's return), not by naming the struct.
            let def = self.type_pool.struct_def(struct_id);
            self.check_unqualified_visibility(
                "struct",
                type_name_str,
                def.file_id,
                def.is_pub,
                span,
            )?;
            struct_id
        };

        // Get struct def (returns owned copy from pool)
        self.record_resolved_declaration_type(Type::new_struct(struct_id));
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_type = Type::new_struct(struct_id);

        // Build a map from field name to struct field index
        let field_index_map: std::collections::HashMap<&str, usize> = struct_def
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();

        // Check for unknown or duplicate fields
        let mut seen_fields = std::collections::HashSet::new();
        for (init_field_name, _) in field_inits.values() {
            let init_name = self.interner.resolve(&init_field_name);

            if !field_index_map.contains_key(init_name) {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }

            if !seen_fields.insert(init_name) {
                return Err(CompileError::new(
                    ErrorKind::DuplicateField {
                        struct_name: struct_def.name.clone(),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }
        }

        // Check that all fields are provided
        if field_inits.len() != struct_def.fields.len() {
            let missing_fields: Vec<String> = struct_def
                .fields
                .iter()
                .filter(|f| !seen_fields.contains(f.name.as_str()))
                .map(|f| f.name.clone())
                .collect();
            return Err(CompileError::new(
                ErrorKind::MissingFields(Box::new(MissingFieldsError {
                    struct_name: struct_def.name.clone(),
                    missing_fields,
                })),
                span,
            ));
        }

        // Analyze field values in SOURCE ORDER (left-to-right as written)
        let mut analyzed_fields: Vec<Option<AirRef>> = vec![None; struct_def.fields.len()];
        let mut source_order: Vec<usize> = Vec::with_capacity(field_inits.len());

        for (init_field_name, field_value) in field_inits.values() {
            let init_name = self.interner.resolve(&init_field_name);
            let field_idx = field_index_map[init_name];
            let expected_field_type = struct_def.fields[field_idx].ty;

            // Check if this is an integer literal that needs type coercion
            // This handles the case where HM inference couldn't resolve the type
            // (e.g., when the struct comes from a comptime type variable)
            let field_inst = self.rir.get(field_value);
            let field_result = if let InstData::IntConst(value) = &field_inst.data {
                // Integer literal - use the expected field type directly, but
                // range-check it first because this shortcut bypasses
                // `analyze_literal`; `S { a: 300 }` with `a: u8` must produce
                // E0800 rather than truncate to 44. (RUE-72)
                if !expected_field_type.literal_fits(*value) {
                    return Err(CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: *value,
                            ty: expected_field_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        field_inst.span,
                    ));
                }
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty: expected_field_type,
                    span: field_inst.span,
                });
                AnalysisResult::new(air_ref, expected_field_type)
            } else if self.is_str_like(expected_field_type) {
                // A `str`-typed field (ADR-0043 Phase 3, RUE-324): supply the
                // field type as the expected type so a string-literal value
                // materializes as a static-backed 2-word `str` (first-class,
                // storable in a struct) rather than a 3-word `String`.
                let prev_expected = ctx.expected_type.replace(expected_field_type);
                let r = self.analyze_inst(air, field_value, ctx);
                ctx.expected_type = prev_expected;
                r?
            } else {
                // Not an integer literal - analyze normally
                self.analyze_inst(air, field_value, ctx)?
            };

            // Two-types model (ADR-0043, RUE-386): storing into a first-class
            // `str` field must not smuggle a borrowed `str` view (a
            // `borrow`/`inout str` parameter) into the aggregate — the view
            // would outlive its borrow and dangle. A buffer field value is
            // caught by the type-mismatch below (a `StrBuf`/`Str(N)` is not
            // `str`); only the same-typed view needs this dedicated check.
            if self.is_str_struct(expected_field_type) {
                self.reject_non_first_class_str(
                    field_value,
                    field_result.ty,
                    FirstClassStrSite::Field,
                    span,
                    ctx,
                )?;
            }

            // Type check the field value against the expected type
            if field_result.ty != expected_field_type {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_field_type.safe_name_with_pool(Some(&self.type_pool)),
                        found: field_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                )
                .with_label(
                    format!(
                        "field '{}' expects type {}",
                        init_name,
                        expected_field_type.safe_name_with_pool(Some(&self.type_pool))
                    ),
                    span,
                ));
            }

            analyzed_fields[field_idx] = Some(field_result.air_ref);
            source_order.push(field_idx);
        }

        // Collect field refs in DECLARATION ORDER
        let field_refs: Vec<AirRef> = analyzed_fields
            .into_iter()
            .map(|opt| opt.expect("all fields should be initialized"))
            .collect();

        let source_order_u32s: Vec<u32> = source_order.iter().map(|&i| i as u32).collect();
        let air_ref = air.add_struct_init(
            struct_id,
            &field_refs,
            &source_order_u32s,
            struct_type,
            span,
        )?;
        Ok(AnalysisResult::new(air_ref, struct_type))
    }

    /// Analyze module type member access: `module.StructName` or `module.EnumName`.
    ///
    /// When accessing a struct or enum through a module, we return a comptime type
    /// that can be used to construct values. For example:
    ///
    /// ```rue
    /// let utils = @import("utils");
    /// let Point = utils.Point;        // Returns Type::Struct as a comptime type
    /// let p = Point { x: 1, y: 2 };   // Uses the type to construct a value
    /// ```
    ///
    /// This enables the pattern of importing types through modules and using them
    /// for struct initialization or enum variant access.
    pub(crate) fn analyze_module_type_member_access(
        &mut self,
        air: &mut Air,
        module_id: crate::types::ModuleId,
        member_name: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let member_name_str = self.interner.resolve(&member_name).to_string();

        // Get the module definition and resolve its file to a canonical
        // FileId, so equivalent path spellings (`helper.rue` vs
        // `./helper.rue`) refer to the same module (spec 10.2:4, RUE-240).
        // `module_file_path` is then that file's stored path, used for the
        // directory-based visibility checks below.
        let module_def = self.module_registry.get_def(module_id);
        let module_file_id = Some(module_def.file_id);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or_else(|| module_def.file_path.clone());

        // Get the accessing file's directory for visibility check
        let accessing_file_path = self.get_source_path(span).map(|s| s.to_string());

        // First, try to find a struct with this name defined by the module's
        // file. Same-named structs in sibling modules are distinct nominal
        // types (RUE-454).
        if let Some(struct_id) = module_file_id.and_then(|file_id| {
            self.structs_by_file_name
                .get(&(file_id, member_name))
                .copied()
        }) {
            let struct_def = self.type_pool.struct_def(struct_id);

            // Check visibility: pub structs are visible to all, private only to same directory
            if !struct_def.is_pub {
                // Check if accessing from same directory
                let same_dir = match &accessing_file_path {
                    Some(accessing) => {
                        let accessing_dir = std::path::Path::new(accessing).parent();
                        let module_dir = std::path::Path::new(&module_file_path).parent();
                        accessing_dir == module_dir
                    }
                    None => true, // Be permissive if we can't determine the path
                };

                if !same_dir {
                    return Err(CompileError::new(
                        ErrorKind::PrivateMemberAccess {
                            item_kind: "struct".to_string(),
                            name: member_name_str,
                        },
                        span,
                    ));
                }
            }

            // Return a TypeConst instruction with the struct type
            let struct_type = Type::new_struct(struct_id);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(struct_type),
                ty: Type::COMPTIME_TYPE,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
        }

        // Next, try to find an enum with this name defined by the module's file.
        if let Some(enum_id) = module_file_id.and_then(|file_id| {
            self.enums_by_file_name
                .get(&(file_id, member_name))
                .copied()
        }) {
            let enum_def = self.type_pool.enum_def(enum_id);

            // Check visibility: pub enums are visible to all, private only to same directory
            if !enum_def.is_pub {
                // Check if accessing from same directory
                let same_dir = match &accessing_file_path {
                    Some(accessing) => {
                        let accessing_dir = std::path::Path::new(accessing).parent();
                        let module_dir = std::path::Path::new(&module_file_path).parent();
                        accessing_dir == module_dir
                    }
                    None => true, // Be permissive if we can't determine the path
                };

                if !same_dir {
                    return Err(CompileError::new(
                        ErrorKind::PrivateMemberAccess {
                            item_kind: "enum".to_string(),
                            name: member_name_str,
                        },
                        span,
                    ));
                }
            }

            // Return a TypeConst instruction with the enum type
            let enum_type = Type::new_enum(enum_id);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(enum_type),
                ty: Type::COMPTIME_TYPE,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
        }

        // Next, try a const defined in the module's file. The headline case is
        // ADR-0026's re-export idiom — `pub const math = @import("...")` in a
        // facade — where the const's type is itself a module: accessing it
        // yields that module, so chains like `std.math.abs(...)` resolve
        // member-by-member (RUE-136). Module bindings live in the per-file
        // `module_bindings` table keyed by the facade's FileId (RUE-113);
        // value consts are found by defining file and member name.
        let member_const = module_file_id
            .and_then(|file_id| self.module_bindings.get(&(file_id, member_name)))
            .or_else(|| {
                module_file_id
                    .and_then(|file_id| self.constants_by_file_name.get(&(file_id, member_name)))
            });
        if let Some(const_info) = member_const.cloned() {
            if !const_info.is_pub {
                let same_dir = match &accessing_file_path {
                    Some(accessing) => {
                        let accessing_dir = std::path::Path::new(accessing).parent();
                        let module_dir = std::path::Path::new(&module_file_path).parent();
                        accessing_dir == module_dir
                    }
                    None => true, // Be permissive if we can't determine the path
                };
                if !same_dir {
                    return Err(CompileError::new(
                        ErrorKind::PrivateMemberAccess {
                            item_kind: "const".to_string(),
                            name: member_name_str,
                        },
                        span,
                    ));
                }
            }

            self.record_body_named_dependency(if const_info.ty.is_module() {
                super::NamedConstDependencyTargetEvent::ModuleBinding {
                    file: const_info.span.file_id.index(),
                    name: member_name_str.clone(),
                }
            } else {
                super::NamedConstDependencyTargetEvent::ValueConst {
                    file: const_info.span.file_id.index(),
                    name: member_name_str.clone(),
                }
            });

            if const_info.ty.is_module() {
                // AIR doesn't have a ModuleConst instruction, so we use
                // UnitConst as a placeholder — the type is what matters
                // (mirrors how @import itself is lowered).
                let module_ty = const_info.ty;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: module_ty,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, module_ty));
            }
            if matches!(const_info.value, ConstValue::Function(_)) {
                return Err(CompileError::new(
                    ErrorKind::ConstExprNotSupported {
                        expr_kind: "a function reference".to_string(),
                    },
                    span,
                ));
            }
            // A value const (e.g. `pub const ANSWER = ...`) accessed as a
            // module member: materialize the value that was evaluated at
            // declaration time, typed as declared (RUE-160).
            let (data, ty) = self.materialize_const_value(ctx, const_info.value, const_info.ty);
            let air_ref = air.add_inst(AirInst { data, ty, span });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Member not found in the module
        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: module_def.import_path.clone(),
                member_name: member_name_str,
            },
            span,
        ))
    }

    // ========================================================================
    // Array operations: ArrayInit, IndexGet, IndexSet
    // ========================================================================

    /// Analyze an array operation instruction.
    ///
    /// Handles: ArrayInit, IndexGet, IndexSet
    pub(crate) fn analyze_array_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::ArrayInit { elements } => {
                self.analyze_array_init(air, inst_ref, elements, inst.span, ctx)
            }

            InstData::ArrayRepeat { value, .. } => {
                self.analyze_array_repeat(air, inst_ref, *value, inst.span, ctx)
            }

            InstData::IndexGet { base, index } => {
                self.analyze_index_get(air, inst_ref, *base, *index, inst.span, ctx)
            }

            InstData::IndexSet { base, index, value } => {
                self.analyze_index_set(air, *base, *index, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_array_ops called with non-array instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Reject an array element (of a literal or a repeat) whose type has no
    /// runtime representation, before it can reach the intern pool — which
    /// panics on both `type` values and modules (intern_pool.rs, RUE-253,
    /// RUE-265).
    ///
    /// - A `type` value is comptime-only (spec 4.14:6): E1200, matching the
    ///   diagnostic `let t = comptime { i32 };` gets.
    /// - A module is not a runtime value (spec 10.4:145): E0206, matching the
    ///   diagnostic a module passed as a function argument gets.
    fn reject_non_runtime_array_element(&self, elem_ty: Type, span: Span) -> CompileResult<()> {
        if elem_ty == Type::COMPTIME_TYPE {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "type values cannot exist at runtime".to_string(),
                },
                span,
            ));
        }
        if matches!(elem_ty.kind(), TypeKind::Module(_)) {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "a runtime value".to_string(),
                    found: elem_ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Return true when `name` is a runtime local or parameter in this function.
    ///
    /// Dotted type-member access (`Type.member`, RUE-196/RUE-488) must not steal
    /// ordinary value field/method access when a binding shadows a type name.
    /// Comptime type variables are intentionally not runtime bindings:
    /// `let O = Option(i32); O.Some(1)` names the type, not a runtime value.
    pub(crate) fn is_runtime_value_binding(&self, name: Spur, ctx: &AnalysisContext) -> bool {
        ctx.locals.contains_key(&name) || ctx.params.iter().any(|param| param.name == name)
    }

    /// Resolve the module a reference denotes, if any, without emitting AIR.
    ///
    /// Used to recognize module-qualified dotted member access
    /// (`module.Enum.Variant`, `module.Type.function()`; RUE-488). Handles a
    /// direct module binding — a `let`/`const`-bound `@import` (a local of module
    /// type or a per-file module binding) — and a nested submodule chain
    /// (`std.geo.Sign.Pos`), resolving `std.geo` by looking `geo` up as a module
    /// binding re-exported from `std`'s file.
    pub(crate) fn try_module_id_of(
        &self,
        inst_ref: rue_rir::InstRef,
        span: Span,
        ctx: &AnalysisContext,
    ) -> Option<crate::types::ModuleId> {
        match self.rir.get(inst_ref).data {
            InstData::VarRef { name } => {
                if let Some(local) = ctx.locals.get(&name) {
                    if let Some(module_id) = local.ty.as_module() {
                        return Some(module_id);
                    }
                }
                self.module_bindings
                    .get(&(span.file_id, name))
                    .and_then(|binding| binding.ty.as_module())
            }
            // Nested submodule: `parent.sub` where `parent` is a module and `sub`
            // is a module re-exported from `parent`'s file (`pub const sub =
            // @import(...)`).
            InstData::FieldGet { base, field } => {
                let parent_id = self.try_module_id_of(base, span, ctx)?;
                let parent_def = self.module_registry.get_def(parent_id);
                let parent_file = parent_def.file_id;
                self.module_bindings
                    .get(&(parent_file, field))
                    .and_then(|binding| binding.ty.as_module())
            }
            _ => None,
        }
    }

    /// Try to analyze a module-qualified type-member call:
    /// `module.Type.function(args)` (RUE-488) — an associated-function call on a
    /// struct, or a tuple-variant construction on an enum. `.` is the sole
    /// member-access spelling; this replaces the removed
    /// `module.Type::function(args)` form.
    ///
    /// Returns `Ok(None)` — falling through to ordinary value-method dispatch —
    /// unless `module_ref` names a module and `type_name` is a struct or enum
    /// defined by that module's file.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_analyze_module_qualified_type_call(
        &mut self,
        air: &mut Air,
        module_ref: rue_rir::InstRef,
        type_name: Spur,
        method: Spur,
        args: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let Some(module_id) = self.try_module_id_of(module_ref, span, ctx) else {
            return Ok(None);
        };
        let module_def = self.module_registry.get_def(module_id);
        let file_id = module_def.file_id;

        // Enum member: `module.Enum.Variant(payload)` is tuple-variant
        // construction. Resolve the enum in the receiver module's defining
        // file and apply module-qualified visibility (E0706).
        if let Some(enum_id) = self.enums_by_file_name.get(&(file_id, type_name)).copied() {
            let enum_def = self.type_pool.enum_def(enum_id);
            if !self.is_accessible(span.file_id, enum_def.file_id, enum_def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "enum".to_string(),
                        name: self.interner.resolve(&type_name).to_string(),
                    },
                    span,
                ));
            }
            let variant_name = self.interner.resolve(&method);
            let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                ErrorKind::UnknownVariant {
                    enum_name: enum_def.name.clone(),
                    variant_name: variant_name.to_string(),
                },
                span,
            )?;
            // Visibility was checked above (E0706), so skip the internal
            // unqualified (E0460) check.
            return self
                .analyze_enum_variant_construction(
                    air,
                    enum_id,
                    variant_index as u32,
                    type_name,
                    /* privacy_exempt = */ true,
                    args,
                    span,
                    ctx,
                )
                .map(Some);
        }

        // Struct member: `module.Struct.function(args)` is an associated-
        // function call. Resolve the struct in the RECEIVER MODULE's file and
        // pass it through (RUE-525): dispatching on the bare name would
        // re-resolve in the caller's file (module-local rules) and miss.
        // Module-qualified visibility is E0706, mirroring the enum branch.
        if let Some(struct_id) = self
            .structs_by_file_name
            .get(&(file_id, type_name))
            .copied()
        {
            let struct_def = self.type_pool.struct_def(struct_id);
            if !self.is_accessible(span.file_id, struct_def.file_id, struct_def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "struct".to_string(),
                        name: self.interner.resolve(&type_name).to_string(),
                    },
                    span,
                ));
            }
            return self
                .analyze_assoc_fn_call_impl(
                    air,
                    type_name,
                    method,
                    args,
                    span,
                    ctx,
                    Some(struct_id),
                )
                .map(Some);
        }

        Ok(None)
    }

    /// Try to resolve `module.Enum.Variant` as a module-qualified enum-variant
    /// path (RUE-488). `.` is the sole member-access spelling, and the path
    /// enforces E0706 module visibility.
    ///
    /// Returns `Ok(None)` — falling through to ordinary field access — unless
    /// `module_ref` names a module **and** `type_name` is an enum defined by that
    /// module's file. Once both hold the access is unambiguously a variant path
    /// (an enum type has no instance fields), so a bad `variant` is a real error.
    pub(crate) fn try_analyze_module_dotted_enum_variant(
        &mut self,
        air: &mut Air,
        module_ref: rue_rir::InstRef,
        type_name: Spur,
        variant: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let Some(module_id) = self.try_module_id_of(module_ref, span, ctx) else {
            return Ok(None);
        };
        let module_def = self.module_registry.get_def(module_id);
        let module_file_id = Some(module_def.file_id);
        let Some(enum_id) = module_file_id
            .and_then(|file_id| self.enums_by_file_name.get(&(file_id, type_name)).copied())
        else {
            // `type_name` is not an enum in this module: this is const/field
            // access through the module, not a variant path. Fall through.
            return Ok(None);
        };

        let enum_def = self.type_pool.enum_def(enum_id);
        let type_name_str = self.interner.resolve(&type_name).to_string();

        // Module-qualified visibility (E0706): a private enum is reachable only
        // from its defining directory.
        if !self.is_accessible(span.file_id, enum_def.file_id, enum_def.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "enum".to_string(),
                    name: type_name_str,
                },
                span,
            ));
        }

        let variant_name = self.interner.resolve(&variant);
        let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: enum_def.name.clone(),
                variant_name: variant_name.to_string(),
            },
            span,
        )?;

        // A tuple variant used as a bare path (no payload) is missing its data.
        let expected = enum_def.variant_payload(variant_index).len();
        if expected > 0 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount { expected, found: 0 },
                span,
            ));
        }

        let ty = Type::new_enum(enum_id);
        let air_ref = air.add_enum_variant(enum_id, variant_index as u32, &[], ty, span)?;
        Ok(Some(AnalysisResult::new(air_ref, ty)))
    }

    /// Try to resolve `Enum.Variant` as a dotted enum-variant path (RUE-196,
    /// RUE-488). `.` is the sole member-access spelling.
    pub(crate) fn try_analyze_dotted_enum_variant(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        variant: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let Some((enum_id, via_comptime)) = self.resolve_enum_type_name(type_name, ctx) else {
            return Ok(None);
        };

        // `type_name` names an enum type, so `type_name.variant` is unambiguously
        // a variant path (an enum type has no instance fields): a `variant` that
        // is not one of its variants is a real error, not a fall-through to
        // ordinary field access (which would report the opaque "field access on
        // non-struct type 'type'"). RUE-488.
        if !via_comptime {
            self.check_unqualified_visibility(
                "enum",
                self.interner.resolve(&type_name),
                self.type_pool.enum_def(enum_id).file_id,
                self.type_pool.enum_def(enum_id).is_pub,
                span,
            )?;
        }

        let enum_def = self.type_pool.enum_def(enum_id);
        let variant_name = self.interner.resolve(&variant);
        let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: enum_def.name.clone(),
                variant_name: variant_name.to_string(),
            },
            span,
        )?;

        let expected = enum_def.variant_payload(variant_index).len();
        if expected > 0 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount { expected, found: 0 },
                span,
            ));
        }

        let ty = Type::new_enum(enum_id);
        let air_ref = air.add_enum_variant(enum_id, variant_index as u32, &[], ty, span)?;
        Ok(Some(AnalysisResult::new(air_ref, ty)))
    }

    /// Analyze an array initialization.
    fn analyze_array_init(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        elements: &rue_rir::RirArrayElemsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let elem_refs = self.rir.array_elements(elements);

        // An array literal of `type` values (`[i32, i32]`) has no runtime
        // representation: type values only exist at compile time (spec
        // 4.14:6). Reject it with E1200 — the same diagnostic `let t =
        // comptime { i32 };` gets — before the array type, whose element is
        // the comptime-only `type`, would reach the intern pool and panic
        // (RUE-253).
        for elem_ref in &elem_refs {
            if let Some(elem_ty) = ctx.resolved_types.get(&elem_ref).copied() {
                self.reject_non_runtime_array_element(elem_ty, span)?;
            }
        }

        // Get the array type from HM inference
        let array_type = Self::get_resolved_type(ctx, inst_ref, span, "array literal")?;

        // If an element expression is itself ill-typed, HM inference collapses
        // the whole array to `<error>` rather than a real `[T; N]` (see
        // `infer_type_to_type`'s Array arm in typeck.rs). Analyzing the
        // elements here surfaces the element's *real* diagnostic (e.g. the
        // unknown-associated-function error on `[String::from(..)]`) instead of
        // masking it with an ICE about the array literal being a non-array
        // type (RUE-190).
        if array_type.is_error() {
            for elem_ref in &elem_refs {
                self.analyze_inst(air, *elem_ref, ctx)?;
            }
            // Analyzing the elements did not surface a diagnostic, yet the
            // array's type is still `<error>`. This is the empty-array (`[]`)
            // and unconstrained-element (`[[]]`) case: HM inference had no
            // constraint to fix the element type, so the element type variable
            // decayed to `<error>` with no diagnostic of its own (RUE-153).
            // The precise, actionable error is that the element type cannot be
            // inferred — emit "type annotation required for empty array"
            // (E0903) rather than returning a silent `<error>`-typed value that
            // would sail into codegen.
            return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
        }

        let (_array_type_id, _elem_type, expected_len) = match array_type.as_array() {
            Some(type_id) => {
                let (element_type, length) = self.type_pool.array_def(type_id);
                (type_id, element_type, length)
            }
            None => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "Array literal inferred as non-array type: {}",
                        array_type.safe_name_with_pool(Some(&self.type_pool))
                    )),
                    span,
                ));
            }
        };

        // Verify length matches
        if elem_refs.len() as u64 != expected_len {
            return Err(CompileError::new(
                ErrorKind::ArrayLengthMismatch {
                    expected: expected_len,
                    found: elem_refs.len() as u64,
                },
                span,
            ));
        }

        // Analyze elements
        let mut air_elems = Vec::with_capacity(elem_refs.len());
        for elem_ref in elem_refs {
            let elem_result = self.analyze_inst(air, elem_ref, ctx)?;
            air_elems.push(elem_result.air_ref);
        }

        let air_ref = air.add_array_init(&air_elems, array_type, span)?;
        Ok(AnalysisResult::new(air_ref, array_type))
    }

    /// Analyze an array-repeat literal `[value; count]` (RUE-235).
    ///
    /// The result type `[ElemType; count]` was inferred by HM (the count is a
    /// compile-time constant resolved during constraint generation via the
    /// array-length const-eval path). This analysis:
    /// 1. gates the form behind the `array_repeat` preview feature;
    /// 2. requires the element type to be `Copy` — a repeat materializes
    ///    `count` copies of one value, which is only sound for Copy elements
    ///    (matching Rust's `[v; N]: Copy`);
    /// 3. evaluates `value` exactly once and desugars to an `ArrayInit` whose
    ///    `count` elements all reference that single evaluated value, so the
    ///    existing per-element store lowering fills every slot on both
    ///    backends with no codegen changes.
    fn analyze_array_repeat(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        value_ref: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // A repeat literal of a non-runtime value — a `type` value (`[i32; 2]`,
        // spec 4.14:6) or a module (`[@import("m"); 2]`, spec 10.4:145) — has no
        // runtime representation. Reject it (E1200 / E0206) before the preview
        // gate below and before the comptime-only/module element type would
        // reach the intern pool and panic (RUE-253, RUE-265).
        if let Some(value_ty) = ctx.resolved_types.get(&value_ref).copied() {
            self.reject_non_runtime_array_element(value_ty, span)?;
        }

        let array_type = Self::get_resolved_type(ctx, inst_ref, span, "array-repeat literal")?;

        // If the value expression is ill-typed, HM collapses the array to
        // `<error>`; analyze the value to surface its real diagnostic rather
        // than masking it with an ICE about a non-array type (mirrors
        // `analyze_array_init`, RUE-190/RUE-153).
        if array_type.is_error() {
            self.analyze_inst(air, value_ref, ctx)?;
            return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
        }

        let (elem_type, length) = match array_type.as_array() {
            Some(type_id) => self.type_pool.array_def(type_id),
            None => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "Array-repeat literal inferred as non-array type: {}",
                        array_type.safe_name_with_pool(Some(&self.type_pool))
                    )),
                    span,
                ));
            }
        };

        // A repeat materializes the complete array value. Reject an oversized
        // layout before expanding its element-reference payload: for an input
        // such as `[0; 536870912]`, building that payload would otherwise do
        // work proportional to a value the compiler is required to reject.
        // This is the same checked materialization boundary used for locals,
        // temporaries, by-value parameters, and type-layout intrinsics.
        self.require_layout_slots(array_type, span)?;

        // Require the element type to be Copy (RUE-235).
        if !self.is_type_copy(elem_type) {
            return Err(CompileError::new(
                ErrorKind::ArrayRepeatNonCopy {
                    element_type: elem_type.safe_name_with_pool(Some(&self.type_pool)),
                },
                span,
            ));
        }

        // Evaluate the repeated value exactly once.
        let value_result = self.analyze_inst(air, value_ref, ctx)?;

        // Desugar to ArrayInit: `length` elements, each the single value.
        let elem_refs = vec![value_result.air_ref; length as usize];
        let air_ref = air.add_array_init(&elem_refs, array_type, span)?;

        // The value expression is evaluated exactly once even when the length
        // is 0 (spec 7.1:39). With no element referencing it the evaluation
        // would be orphaned — CFG lowering is demand-driven and only follows
        // the ArrayInit's element refs — so `[produce(); 0]` never called
        // produce and `[return 42; 0]` fell through (RUE-531). Anchor the
        // evaluation as a Block statement ahead of the empty array value;
        // Block lowering also handles a diverging value (`return`/`break`)
        // correctly, marking the array construction unreachable.
        if length == 0 {
            let block_ref = air.add_block(&[value_result.air_ref], air_ref, array_type, span)?;
            return Ok(AnalysisResult::new(block_ref, array_type));
        }
        Ok(AnalysisResult::new(air_ref, array_type))
    }

    // Enum operations: EnumDecl, EnumVariant
    // ========================================================================

    /// Analyze an enum operation instruction.
    ///
    /// Handles: EnumDecl, EnumVariant
    pub(crate) fn analyze_enum_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::EnumDecl { .. } => {
                // Enum declarations are processed during collection phase
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }

            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => {
                // Look up the enum type, potentially through a module
                let enum_id = if let Some(module_ref) = module {
                    // Qualified access: module.EnumName::Variant
                    self.resolve_enum_through_module(*module_ref, *type_name, inst.span, ctx)?
                } else {
                    // Unqualified access: EnumName::Variant, or the generic
                    // form `O::None` where `O` is a comptime type-variable
                    // bound to `Option(i32)` (RUE-6 phase 2).
                    let (enum_id, via_comptime) = self
                        .resolve_enum_type_name(*type_name, ctx)
                        .ok_or_compile_error(
                            ErrorKind::UnknownEnumType(
                                self.interner.resolve(&*type_name).to_string(),
                            ),
                            inst.span,
                        )?;
                    // Privacy (E0460, RUE-185): constructing a variant names
                    // the enum unqualified, so a private enum from another
                    // directory is not constructible here — privacy is
                    // uniform across item kinds (spec 10.3:1, 10.3:7). The
                    // module-qualified branch above does its own check
                    // (E0706, `resolve_enum_through_module`). A comptime-bound
                    // enum is exempt (the type arrived through a binding).
                    if !via_comptime {
                        let def = self.type_pool.enum_def(enum_id);
                        self.check_unqualified_visibility(
                            "enum",
                            self.interner.resolve(&*type_name),
                            def.file_id,
                            def.is_pub,
                            inst.span,
                        )?;
                    }
                    enum_id
                };
                let enum_def = self.type_pool.enum_def(enum_id);

                // Find the variant index
                let variant_name = self.interner.resolve(&*variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                    ErrorKind::UnknownVariant {
                        enum_name: enum_def.name.clone(),
                        variant_name: variant_name.to_string(),
                    },
                    inst.span,
                )?;

                // A tuple variant used as a bare path (no payload arguments)
                // is missing its data — reject it with an arity error (RUE-221).
                let expected = enum_def.variant_payload(variant_index).len();
                if expected > 0 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount { expected, found: 0 },
                        inst.span,
                    ));
                }

                let ty = Type::new_enum(enum_id);

                let air_ref =
                    air.add_enum_variant(enum_id, variant_index as u32, &[], ty, inst.span)?;
                Ok(AnalysisResult::new(air_ref, ty))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_enum_ops called with non-enum instruction: {:?}",
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
