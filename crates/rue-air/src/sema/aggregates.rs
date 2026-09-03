//! Aggregate construction, member access, indexing, and enum operations.
//!
//! This module owns aggregate operation dispatch and aggregate-specific type and
//! layout validation. Field and index reads/writes delegate place construction,
//! bounds, move, borrow, and reinitialization semantics to the canonical
//! `analysis::ownership` API.

use ahash::{AHashMap, AHashSet};

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind, MissingFieldsError, OptionExt};
use rue_rir::{InstData, InstRef, RirParamMode};
use rue_span::Span;

use super::aggregate_resolution::{
    ModuleTypeMember, QualifiedType, StructLiteralHead, resolve_aggregate_module_ref,
    resolve_enum_type_name, resolve_struct_type_name, select_module_type_member,
    select_qualified_enum, select_qualified_type, select_struct_literal_head,
};
use super::analysis::FirstClassStrSite;
use super::context::{AnalysisContext, AnalysisResult, ConstValue};
use crate::inst::{Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirRef};
use crate::types::{Type, TypeKind};

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
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
        let facts = self.aggregate_facts();
        resolve_enum_type_name(
            facts,
            ctx.comptime_type_vars.get(&type_name).copied(),
            ctx.current_file_id,
            type_name,
        )
    }

    /// Resolve a `Type.assoc()` / `Type { .. }` struct type name that may be a
    /// comptime type-variable binding (`let P = Point(i32)`) or a module-level
    /// `const` binding (`const P = Point(i32)`), falling back to the named-struct
    /// table and builtins. Returns `(struct_id, via_binding)`, or `None` if the
    /// name is not a struct. `via_binding` is true when the struct arrived
    /// through a `let`/`const` binding (an anonymous struct from a comptime type
    /// function), so privacy does not apply — the exact mirror of
    /// `resolve_enum_type_name` for the struct side (RUE-595). Without the
    /// tagged value-const arm a module-`const`-bound struct type resolved
    /// as a type namespace nowhere, so `const C = Counter(i32); C.zero()` failed
    /// (E0413) and `const P = Point(i32); P { .. }` failed (E0204) while the
    /// enum-bound and local-`let`-bound forms worked.
    pub(crate) fn resolve_struct_type_name(
        &self,
        type_name: Spur,
        ctx: &AnalysisContext,
    ) -> Option<(crate::types::StructId, bool)> {
        let facts = self.aggregate_facts();
        resolve_struct_type_name(
            facts,
            ctx.comptime_type_vars.get(&type_name).copied(),
            ctx.current_file_id,
            type_name,
        )
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
        let def = self.body_type_pool().enum_def(enum_id);
        let payload_types = def.variant_payload(variant_index as usize).to_vec();

        // Visibility check, mirroring the bare-path `EnumVariant` handler
        // (E0460, privacy is uniform across item kinds). A comptime-bound enum
        // (`let O = Option(i32); O::Some(..)`) is exempt: the type value
        // arrived through a binding, not by naming the enum (privacy_exempt).
        if !privacy_exempt {
            self.check_unqualified_visibility(
                "enum",
                self.body_interner().resolve(&type_name),
                def.file_id,
                def.is_pub,
                span,
            )?;
        }

        let args = self.body_rir_ref().call_args(args).to_vec();

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
        let mut continues = true;
        for (i, arg) in args.iter().enumerate() {
            let reachable_edges_before_arg = ctx.ownership.loop_break_stack.clone();
            let divergence_before_arg = ctx.divergence_kinds;
            let expected = payload_types[i];
            let arg_result = ctx
                .with_expected_type(Some(expected), |ctx| self.analyze_inst(air, arg.value, ctx))?;
            let actual = arg_result.ty;
            if !continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_arg);
                ctx.divergence_kinds = divergence_before_arg;
            }
            continues &= arg_result.continues;
            if !self.types_compatible(actual, expected) && actual != Type::ERROR {
                return Err(self.type_mismatch_error(
                    expected,
                    actual,
                    self.body_rir_ref().get(arg.value).span,
                ));
            }
            payload_refs.push(arg_result.air_ref);
        }

        let ty = Type::new_enum(enum_id);

        let air_ref = air.add_enum_variant(enum_id, variant_index, &payload_refs, ty, span)?;
        Ok(AnalysisResult::with_continues(air_ref, ty, continues))
    }

    /// Validate the source operand type accepted by structural equality.
    ///
    /// Aggregate equality bottoms out in the scalar cases listed here; keeping
    /// this check beside aggregate construction prevents comparison dispatch
    /// from becoming a second aggregate type authority.
    pub(super) fn validate_equality_operand_type(&self, ty: Type, span: Span) -> CompileResult<()> {
        if ty.is_integer()
            || ty.is_float()
            || ty == Type::BOOL
            || ty == Type::UNIT
            || ty.is_struct()
            || ty.is_array()
            || ty.is_enum()
            || self.is_strbuf(ty)
        {
            return Ok(());
        }
        Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: "integer, float, bool, string, unit, struct, array, or enum".to_string(),
                found: self.format_type_name(ty),
            },
            span,
        ))
    }

    /// Lower one comparison over two already-analyzed operands.
    ///
    /// This is the single comparison lowering. `==`/`!=`/`<`/… reach it from
    /// `analyze_comparison` once their operands are analyzed, and
    /// `@assert_eq`/`@assert_ne` reach it with the operands they evaluated once
    /// for both the comparison and the rendering (ADR-0083 Phase 2.5). Sharing
    /// it is what keeps `@assert_eq(a, b)` and `a == b` from being two answers
    /// to the same question: the aggregate route (a `StrBuf`'s borrowed
    /// `equals_borrowed`, 4.3:3) is reachable only here, so an intrinsic that
    /// built its own `Eq` node would silently compare two string headers.
    pub(super) fn build_comparison(
        &mut self,
        air: &mut Air,
        comparison: AirInstData,
        lhs: AnalysisResult,
        rhs: AnalysisResult,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if matches!(comparison, AirInstData::Eq(..) | AirInstData::Ne(..))
            && let Some(result) =
                self.try_prepare_aggregate_equality(air, &comparison, lhs, rhs, span, ctx)?
        {
            return Ok(result);
        }
        let air_ref = air.add_inst(AirInst {
            data: comparison,
            ty: Type::BOOL,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    /// Prepare source-defined aggregate equality when the aggregate provides
    /// the canonical borrowed equality method.
    pub(super) fn try_prepare_aggregate_equality(
        &mut self,
        air: &mut Air,
        comparison: &AirInstData,
        lhs: AnalysisResult,
        rhs: AnalysisResult,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let Some(struct_id) = lhs.ty.as_struct().filter(|struct_id| {
            self.body_type_pool().struct_lang_item(*struct_id) == Some(crate::LangItem::StrBuf)
        }) else {
            return Ok(None);
        };
        let method = self.intern_body_symbol("equals_borrowed")?;
        if self.method_info((struct_id, method)).is_none() {
            return Ok(None);
        }
        ctx.referenced_methods.insert((struct_id, method));
        let (lhs_arg, mut temp_scope) =
            self.materialize_borrow_argument(air, lhs.air_ref, lhs.ty, span, ctx)?;
        let (rhs_arg, rhs_scope) =
            self.materialize_borrow_argument(air, rhs.air_ref, rhs.ty, span, ctx)?;
        temp_scope.extend(rhs_scope);
        let call_name =
            self.intern_body_symbol(&self.method_symbol(struct_id, "equals_borrowed", false))?;
        let equal = air.add_call(
            None,
            call_name,
            &[
                AirCallArg {
                    value: lhs_arg,
                    mode: AirArgMode::Borrow,
                },
                AirCallArg {
                    value: rhs_arg,
                    mode: AirArgMode::Borrow,
                },
            ],
            Type::BOOL,
            span,
        )?;
        let equal = self.wrap_value_with_temp_scope(air, equal, Type::BOOL, span, temp_scope)?;
        let value = if matches!(comparison, AirInstData::Ne(..)) {
            air.add_inst(AirInst {
                data: AirInstData::Not(equal),
                ty: Type::BOOL,
                span,
            })
        } else {
            equal
        };
        Ok(Some(AnalysisResult::new(value, Type::BOOL)))
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

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
        let field_inits = self.body_rir_ref().field_inits(fields).to_vec();
        // Look up the struct type
        // First check if it's a comptime type variable (e.g., `let Point = make_point(); Point { ... }`)
        let type_name_str = self.body_interner().resolve(&type_name).to_owned();
        let mut continues = true;
        let struct_id = if let Some(head_ref) = ctor_head {
            // Inline type-constructor struct-literal head `F(args) { ... }`
            // (RUE-596, spec 4.14:23): the head call reduces to a concrete type
            // at comptime; construct as if the type had been bound to a name
            // first (`let P = F(args); P { .. }`).
            let recover_missing_arguments = ctx.resolved_type_of(head_ref).is_none();
            let previous_recovery_scope = std::mem::replace(
                &mut ctx.recover_missing_ctor_head_arguments,
                recover_missing_arguments,
            );
            let head_result = self.analyze_inst(air, head_ref, ctx);
            ctx.recover_missing_ctor_head_arguments = previous_recovery_scope;
            let head_result = head_result?;
            continues &= head_result.continues;
            let AirInstData::TypeConst(reduced_ty) = air.get(head_result.air_ref).data else {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "a type".to_string(),
                        found: self.format_type_name(head_result.ty),
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
                            found: self.format_type_name(reduced_ty),
                        },
                        span,
                    ));
                }
            }
        } else if let Some(module_ref) = module {
            let module_result = self.analyze_inst(air, module_ref, ctx)?;
            continues &= module_result.continues;
            let Some(module_id) = module_result.ty.as_module() else {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "module".to_string(),
                        found: self.format_type_name(module_result.ty),
                    },
                    span,
                ));
            };
            let struct_id = {
                let facts = self.aggregate_facts();
                facts.aggregate_struct_in_file(facts.aggregate_module(module_id).file, type_name)
            }
            .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.to_string()), span)?;
            // Module-qualified visibility is E0706 (RUE-525), uniform with
            // enum members and associated-function calls through a module;
            // E0460 is the diagnostic for unqualified naming forms.
            let def = self.body_type_pool().struct_def(struct_id);
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
        } else {
            let head = {
                let facts = self.aggregate_facts();
                select_struct_literal_head(
                    facts,
                    ctx.comptime_type_vars.get(&type_name).copied(),
                    span.file_id,
                    type_name,
                )
            };
            match head {
                StructLiteralHead::Bound(ty) => match ty.kind() {
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
                },
                StructLiteralHead::Named(struct_id) => {
                    let def = self.body_type_pool().struct_def(struct_id);
                    self.check_unqualified_visibility(
                        "struct",
                        &type_name_str,
                        def.file_id,
                        def.is_pub,
                        span,
                    )?;
                    struct_id
                }
                StructLiteralHead::Absent => {
                    return Err(CompileError::new(
                        ErrorKind::UnknownType(type_name_str.to_string()),
                        span,
                    ));
                }
            }
        };

        // Get struct def (returns owned copy from pool)
        self.record_resolved_declaration_type(Type::new_struct(struct_id));
        let struct_def = self.body_type_pool().struct_def(struct_id);
        let struct_type = Type::new_struct(struct_id);

        // Build a map from field name to struct field index
        let field_index_map: AHashMap<&str, usize> = struct_def
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();

        // Check for unknown or duplicate fields
        let mut seen_fields = AHashSet::new();
        for &(init_field_name, _) in &field_inits {
            let init_name = self.body_interner().resolve(&init_field_name).to_owned();

            if !field_index_map.contains_key(init_name.as_str()) {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: self.format_type_name(struct_type),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }

            if !seen_fields.insert(init_name.clone()) {
                return Err(CompileError::new(
                    ErrorKind::DuplicateField {
                        struct_name: self.format_type_name(struct_type),
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
                    struct_name: self.format_type_name(struct_type),
                    missing_fields,
                })),
                span,
            ));
        }

        // Analyze field values in SOURCE ORDER (left-to-right as written)
        let mut analyzed_fields: Vec<Option<AirRef>> = vec![None; struct_def.fields.len()];
        let mut source_order: Vec<usize> = Vec::with_capacity(field_inits.len());

        for &(init_field_name, field_value) in &field_inits {
            let reachable_edges_before_field = ctx.ownership.loop_break_stack.clone();
            let divergence_before_field = ctx.divergence_kinds;
            let init_name = self.body_interner().resolve(&init_field_name).to_owned();
            let field_idx = field_index_map[init_name.as_str()];
            let expected_field_type = struct_def.fields[field_idx].ty;

            // Check if this is an integer literal that needs type coercion
            // This handles the case where HM inference couldn't resolve the type
            // (e.g., when the struct comes from a comptime type variable)
            let field_span = self.body_rir_ref().get(field_value).span;
            let field_inst = self.body_rir_ref().get(field_value);
            let field_result = if let InstData::IntConst(value) = &field_inst.data {
                // Integer literal - use the expected field type directly, but
                // range-check it first because this shortcut bypasses
                // `analyze_literal`; `S { a: 300 }` with `a: u8` must produce
                // E0800 rather than truncate to 44. (RUE-72)
                let encoded = if expected_field_type == Type::F32 {
                    u64::from((*value as f32).to_bits())
                } else if expected_field_type == Type::F64 {
                    (*value as f64).to_bits()
                } else if expected_field_type.literal_fits(*value) {
                    *value
                } else {
                    return Err(CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: *value,
                            ty: self.format_type_name(expected_field_type),
                        },
                        field_inst.span,
                    ));
                };
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(encoded),
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
            if !continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_field);
                ctx.divergence_kinds = divergence_before_field;
            }
            continues &= field_result.continues;

            // An accessor result is a second-class borrowed place (ADR-0062):
            // capturing it as an aggregate member would store the borrow.
            self.reject_accessor_result_escape(
                field_value,
                super::analysis::AccessorEscapeSite::Capture,
                span,
                ctx,
            )?;

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
            if !self.types_compatible(field_result.ty, expected_field_type) {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: self.format_type_name(expected_field_type),
                        found: self.format_type_name(field_result.ty),
                    },
                    field_span,
                )
                .with_label(
                    format!(
                        "field '{}' expects type {}",
                        init_name,
                        self.format_type_name(expected_field_type)
                    ),
                    field_span,
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
        Ok(AnalysisResult::with_continues(
            air_ref,
            struct_type,
            continues,
        ))
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
        atom_anchor: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let member_name_str = self.body_interner().resolve(&member_name).to_string();

        // Resolve the module to its canonical request-local file identity, so
        // equivalent path spellings select the same visibility domain.
        let module_fact = self.aggregate_facts().aggregate_module(module_id);
        let member = {
            let facts = self.aggregate_facts();
            select_module_type_member(facts, module_fact.file, member_name)
        };

        // First, try to find a struct with this name defined by the module's
        // file. Same-named structs in sibling modules are distinct nominal
        // types (RUE-454).
        if let ModuleTypeMember::Struct(struct_id) = &member {
            let struct_id = *struct_id;
            let struct_def = self.body_type_pool().struct_def(struct_id);

            // Check visibility: pub structs are visible to all, private only to same directory
            if !self.is_accessible(span.file_id, struct_def.file_id, struct_def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "struct".to_string(),
                        name: member_name_str,
                    },
                    span,
                ));
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
        if let ModuleTypeMember::Enum(enum_id) = &member {
            let enum_id = *enum_id;
            let enum_def = self.body_type_pool().enum_def(enum_id);

            // Check visibility: pub enums are visible to all, private only to same directory
            if !self.is_accessible(span.file_id, enum_def.file_id, enum_def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "enum".to_string(),
                        name: member_name_str,
                    },
                    span,
                ));
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
        // tagged module-binding variant keyed by the facade's FileId (RUE-113);
        // value consts are found by defining file and member name.
        if let ModuleTypeMember::Const(const_info) = member {
            if !self.is_accessible(span.file_id, module_fact.file, const_info.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "const".to_string(),
                        name: member_name_str,
                    },
                    span,
                ));
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
            let atom_anchor = match const_info.value {
                ConstValue::String(_) => atom_anchor.and_then(|instruction| {
                    self.body_rir_ref()
                        .materialize_const_use_anchor(instruction)
                }),
                _ => None,
            };
            let (data, ty) = self.materialize_const_value(
                ctx,
                const_info.value,
                const_info.ty,
                atom_anchor,
                span,
            )?;
            let air_ref = air.add_inst(AirInst { data, ty, span });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Member not found in the module
        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: std::path::Path::new(module_fact.import_path())
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or_else(|| module_fact.import_path())
                    .to_string(),
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

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
                    found: self.format_type_name(elem_ty),
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
        ctx.locals.contains_key(&name) || ctx.has_param(name)
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
        let facts = self.aggregate_facts();
        resolve_aggregate_module_ref(
            facts,
            self.body_rir_ref(),
            inst_ref,
            span.file_id,
            &ctx.locals,
        )
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
        let selected = {
            let facts = self.aggregate_facts();
            let file = facts.aggregate_module(module_id).file;
            select_qualified_type(facts, file, type_name)
        };

        // Enum member: `module.Enum.Variant(payload)` is tuple-variant
        // construction. Resolve the enum in the receiver module's defining
        // file and apply module-qualified visibility (E0706).
        if let QualifiedType::Enum(enum_id) = selected {
            let enum_def = self.body_type_pool().enum_def(enum_id);
            if !self.is_accessible(span.file_id, enum_def.file_id, enum_def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "enum".to_string(),
                        name: self.body_interner().resolve(&type_name).to_string(),
                    },
                    span,
                ));
            }
            let variant_name = self.body_interner().resolve(&method);
            let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                ErrorKind::UnknownVariant {
                    enum_name: self.format_type_name(Type::new_enum(enum_id)),
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
        if let QualifiedType::Struct(struct_id) = selected {
            let struct_def = self.body_type_pool().struct_def(struct_id);
            if !self.is_accessible(span.file_id, struct_def.file_id, struct_def.is_pub) {
                return Err(CompileError::new(
                    ErrorKind::PrivateMemberAccess {
                        item_kind: "struct".to_string(),
                        name: self.body_interner().resolve(&type_name).to_string(),
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
        let Some(enum_id) = ({
            let facts = self.aggregate_facts();
            let file = facts.aggregate_module(module_id).file;
            select_qualified_enum(facts, file, type_name)
        }) else {
            // `type_name` is not an enum in this module: this is const/field
            // access through the module, not a variant path. Fall through.
            return Ok(None);
        };

        let enum_def = self.body_type_pool().enum_def(enum_id);
        let type_name_str = self.body_interner().resolve(&type_name).to_string();

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

        let variant_name = self.body_interner().resolve(&variant);
        let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: self.format_type_name(Type::new_enum(enum_id)),
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
                self.body_interner().resolve(&type_name),
                self.body_type_pool().enum_def(enum_id).file_id,
                self.body_type_pool().enum_def(enum_id).is_pub,
                span,
            )?;
        }

        let enum_def = self.body_type_pool().enum_def(enum_id);
        let variant_name = self.body_interner().resolve(&variant);
        let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: self.format_type_name(Type::new_enum(enum_id)),
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
        let elem_refs = self.body_rir_ref().array_elements(elements).to_vec();

        // An array literal of `type` values (`[i32, i32]`) has no runtime
        // representation: type values only exist at compile time (spec
        // 4.14:6). Reject it with E1200 — the same diagnostic `let t =
        // comptime { i32 };` gets — before the array type, whose element is
        // the comptime-only `type`, would reach the intern pool and panic
        // (RUE-253).
        for elem_ref in &elem_refs {
            if let Some(elem_ty) = ctx.resolved_type_of(*elem_ref) {
                self.reject_non_runtime_array_element(elem_ty, span)?;
            }
        }

        // Get the array type from HM inference. An unresolved element
        // variable can survive recovery around a malformed or partially
        // specialized comptime construction; report the same actionable
        // annotation diagnostic used for an unconstrained empty array rather
        // than turning the missing map entry into an internal compiler error.
        let Some(array_type) = ctx.resolved_type_of(inst_ref) else {
            return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
        };

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
                let (element_type, length) = self.body_type_pool().array_def(type_id);
                (type_id, element_type, length)
            }
            None => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "Array literal inferred as non-array type: {}",
                        self.format_internal_type_name(array_type)
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
        let mut continues = true;
        for elem_ref in elem_refs {
            let reachable_edges_before_elem = ctx.ownership.loop_break_stack.clone();
            let divergence_before_elem = ctx.divergence_kinds;
            let elem_result = self.analyze_inst(air, elem_ref, ctx)?;
            if !continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_elem);
                ctx.divergence_kinds = divergence_before_elem;
            }
            continues &= elem_result.continues;
            // An accessor result cannot be captured as an array element
            // (ADR-0062): the member would store a second-class borrow.
            self.reject_accessor_result_escape(
                elem_ref,
                super::analysis::AccessorEscapeSite::Capture,
                span,
                ctx,
            )?;
            air_elems.push(elem_result.air_ref);
        }

        let air_ref = air.add_array_init(&air_elems, array_type, span)?;
        Ok(AnalysisResult::with_continues(
            air_ref, array_type, continues,
        ))
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
        if let Some(value_ty) = ctx.resolved_type_of(value_ref) {
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
            Some(type_id) => self.body_type_pool().array_def(type_id),
            None => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "Array-repeat literal inferred as non-array type: {}",
                        self.format_internal_type_name(array_type)
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
                    element_type: self.format_type_name(elem_type),
                },
                span,
            ));
        }

        // Evaluate the repeated value exactly once.
        let value_result = self.analyze_inst(air, value_ref, ctx)?;
        // An accessor result cannot seed an array-repeat literal (ADR-0062).
        self.reject_accessor_result_escape(
            value_ref,
            super::analysis::AccessorEscapeSite::Capture,
            span,
            ctx,
        )?;

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
            return Ok(AnalysisResult::with_continues(
                block_ref,
                array_type,
                value_result.continues,
            ));
        }
        Ok(AnalysisResult::with_continues(
            air_ref,
            array_type,
            value_result.continues,
        ))
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

        match &inst.data {
            InstData::EnumDecl {
                is_pub,
                is_non_exhaustive,
                ..
            } => {
                if *is_non_exhaustive {
                    self.require_preview(
                        rue_error::PreviewFeature::NonExhaustiveEnums,
                        "@non_exhaustive enums",
                        inst.span,
                    )?;
                    if !*is_pub {
                        return Err(CompileError::new(
                            ErrorKind::ParseError(
                                "@non_exhaustive can only be applied to public enums".to_string(),
                            ),
                            inst.span,
                        ));
                    }
                }
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
                                self.body_interner().resolve(&*type_name).to_string(),
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
                        let def = self.body_type_pool().enum_def(enum_id);
                        self.check_unqualified_visibility(
                            "enum",
                            self.body_interner().resolve(&*type_name),
                            def.file_id,
                            def.is_pub,
                            inst.span,
                        )?;
                    }
                    enum_id
                };
                let enum_def = self.body_type_pool().enum_def(enum_id);

                // Find the variant index
                let variant_name = self.body_interner().resolve(&*variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                    ErrorKind::UnknownVariant {
                        enum_name: self.format_type_name(Type::new_enum(enum_id)),
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
}
