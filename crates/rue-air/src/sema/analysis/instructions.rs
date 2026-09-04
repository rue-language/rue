//! Core per-instruction analysis dispatch and field/index assignment analysis.
//!
//! This category is the canonical instruction dispatcher and owns assignment
//! analysis for projected places.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;
use crate::sema::comptime::{ComptimeEngine, ComptimeMethodType, ComptimeOutcome};
use crate::sema::context::{DivergenceKind, DivergenceKinds};

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Analyze an RIR instruction, producing AIR instructions.
    ///
    /// Types are determined by Hindley-Milner inference (stored in `resolved_types`).
    /// Returns both the AIR reference and the synthesized type.
    /// Analyze a single RIR instruction and produce the corresponding AIR instruction.
    ///
    /// This method dispatches to category-specific semantic modules. Each category
    /// handles related instruction types together.
    ///
    /// # Categories
    ///
    /// - **Literals**: IntConst, BoolConst, StringConst, UnitConst
    /// - **Binary arithmetic**: Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
    /// - **Comparison**: Eq, Ne, Lt, Gt, Le, Ge
    /// - **Logical**: And, Or
    /// - **Unary**: Neg, Not, BitNot
    /// - **Control flow**: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    /// - **Variables**: Alloc, VarRef, Assign
    /// - **Structs**: StructDecl, StructInit, FieldGet, FieldSet
    /// - **Arrays**: ArrayInit, IndexGet, IndexSet
    /// - **Enums**: EnumDecl, EnumVariant
    /// - **Calls**: Call, MethodCall
    /// - **Intrinsics**: Intrinsic, TypeIntrinsic, OffsetOf
    /// - **Declarations**: DropFnDecl, FnDecl
    pub(crate) fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // These expressions synthesize their result from their own operands or
        // declaration. A surrounding result expectation belongs to the whole
        // expression and must not contextually type those operands. Keep this
        // boundary at dispatch so every implementation path (including
        // String `+` and place/projection fast paths) gets identical cleanup.
        let clears_result_expectation = matches!(
            &self.body_rir_ref().get(inst_ref).data,
            InstData::Add { .. }
                | InstData::Sub { .. }
                | InstData::Mul { .. }
                | InstData::Div { .. }
                | InstData::Mod { .. }
                | InstData::BitAnd { .. }
                | InstData::BitOr { .. }
                | InstData::BitXor { .. }
                | InstData::Shl { .. }
                | InstData::Shr { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
                | InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::And { .. }
                | InstData::Or { .. }
                | InstData::Neg { .. }
                | InstData::Not { .. }
                | InstData::BitNot { .. }
                | InstData::Loop { .. }
                | InstData::InfiniteLoop { .. }
                | InstData::FieldGet { .. }
                | InstData::FieldSet { .. }
                | InstData::IndexGet { .. }
                | InstData::IndexSet { .. }
        );
        let mut result = if clears_result_expectation {
            ctx.with_expected_type(None, |ctx| self.analyze_inst_dispatch(air, inst_ref, ctx))?
        } else {
            self.analyze_inst_dispatch(air, inst_ref, ctx)?
        };
        if let Some(continues) = ctx.resolved_continues_of(inst_ref) {
            result.continues = continues;
        }
        // Most expression adapters rebuild their result after analyzing one
        // or more operands, so the explicit panic classification is carried
        // by the context while the normal-continuation bit is propagated by
        // the adapter. Preserve an already-observed child edge; otherwise a
        // newly non-continuing result is an ordinary unwinding divergence.
        if !result.continues && ctx.divergence_kinds.is_empty() {
            ctx.divergence_kinds = DivergenceKinds::from_kind(DivergenceKind::Other);
        }
        Ok(result)
    }

    fn analyze_inst_dispatch(
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
            // Literals
            InstData::IntConst(_)
            | InstData::FloatConst { .. }
            | InstData::BoolConst(_)
            | InstData::StringConst { .. }
            | InstData::UnitConst => self.analyze_literal(air, inst_ref, ctx),

            // Binary arithmetic operations (Add also covers String + String
            // concatenation — see analyze_add).
            InstData::Add { lhs, rhs } => {
                self.analyze_add(air, inst_ref, *lhs, *rhs, inst.span, ctx)
            }
            InstData::Sub { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Sub, inst.span, ctx)
            }
            InstData::Mul { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mul, inst.span, ctx)
            }
            InstData::Div { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Div, inst.span, ctx)
            }
            InstData::Mod { lhs, rhs } => {
                if ctx.resolved_type_of(*lhs).is_some_and(|ty| ty.is_float()) {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected:
                                "integer operand (`%` is not defined on floats; use std.math.rem)"
                                    .to_string(),
                            found: "floating-point operand".to_string(),
                        },
                        inst.span,
                    ));
                }
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mod, inst.span, ctx)
            }

            // Bitwise binary operations
            InstData::BitAnd { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitAnd, inst.span, ctx)
            }
            InstData::BitOr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitOr, inst.span, ctx)
            }
            InstData::BitXor { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitXor, inst.span, ctx)
            }
            InstData::Shl { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shl, inst.span, ctx)
            }
            InstData::Shr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shr, inst.span, ctx)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Eq, inst.span, ctx)
            }
            InstData::Ne { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Ne, inst.span, ctx)
            }
            InstData::Lt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Lt, inst.span, ctx)
            }
            InstData::Gt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Gt, inst.span, ctx)
            }
            InstData::Le { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Le, inst.span, ctx)
            }
            InstData::Ge { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Ge, inst.span, ctx)
            }

            // Logical operations
            InstData::And { .. } | InstData::Or { .. } => {
                self.analyze_logical_op(air, inst_ref, ctx)
            }

            // Unary operations
            InstData::Neg { .. } | InstData::Not { .. } | InstData::BitNot { .. } => {
                self.analyze_unary_op(air, inst_ref, ctx)
            }

            // Control flow
            InstData::Branch { .. }
            | InstData::Loop { .. }
            | InstData::InfiniteLoop { .. }
            | InstData::Match { .. }
            | InstData::Try { .. }
            | InstData::Break { .. }
            | InstData::Continue
            | InstData::Ret(_)
            | InstData::Yield(_)
            | InstData::Block { .. } => self.analyze_control_flow(air, inst_ref, ctx),

            // Variable operations
            InstData::Alloc { .. } | InstData::VarRef { .. } | InstData::Assign { .. } => {
                self.analyze_variable_ops(air, inst_ref, ctx)
            }
            InstData::PlaceSet { place, value } => {
                self.analyze_place_set(air, *place, *value, inst.span, ctx)
            }

            // Struct operations
            InstData::StructDecl { .. }
            | InstData::StructInit { .. }
            | InstData::FieldGet { .. }
            | InstData::FieldSet { .. } => self.analyze_struct_ops(air, inst_ref, ctx),

            // Array operations
            InstData::ArrayInit { .. }
            | InstData::ArrayRepeat { .. }
            | InstData::IndexGet { .. }
            | InstData::IndexSet { .. } => self.analyze_array_ops(air, inst_ref, ctx),

            // Enum operations
            InstData::EnumDecl { .. } | InstData::EnumVariant { .. } => {
                self.analyze_enum_ops(air, inst_ref, ctx)
            }

            // Call operations
            InstData::Call { .. } | InstData::MethodCall { .. } => {
                self.analyze_call_ops(air, inst_ref, ctx)
            }

            // Intrinsic operations
            InstData::Intrinsic { .. }
            | InstData::InternalIntrinsic { .. }
            | InstData::TypeIntrinsic { .. }
            | InstData::OffsetOf { .. } => self.analyze_intrinsic_ops(air, inst_ref, ctx),

            // Declaration no-ops (produce Unit in expression context)
            InstData::DropFnDecl { .. } | InstData::FnDecl { .. } | InstData::ConstDecl { .. } => {
                self.analyze_decl_noop(air, inst_ref, ctx)
            }

            // Comptime block expression
            InstData::Comptime { expr } => {
                // Evaluate the inner expression at compile time. The
                // environment carries the comptime parameters in scope and
                // the HM-resolved types, so arithmetic is checked at the
                // operand type (spec 8.1 / 4.14:4) and comptime parameters
                // are usable as constants (spec 4.14:5). A would-panic
                // operation (overflow, division by zero) propagates as a
                // compile error here.
                let result = {
                    let mut env = super::super::comptime_eval::ComptimeEnv::for_analysis(ctx);
                    self.eval_const_expr(*expr, &mut env)?
                };
                match result {
                    Some(ConstValue::Integer(value)) => {
                        // Get the expected type from resolved types
                        let ty =
                            Self::get_resolved_type(ctx, inst_ref, inst.span, "comptime block")?;

                        // Backstop range check: negative results are legal
                        // for signed targets (RUE-71); the value just has to
                        // be representable in the target type.
                        if !super::super::comptime_eval::const_int_fits(value, ty) {
                            return if value >= 0 {
                                Err(CompileError::new(
                                    ErrorKind::LiteralOutOfRange {
                                        value: value as u64,
                                        ty: self.format_type_name(ty),
                                    },
                                    inst.span,
                                ))
                            } else {
                                Err(CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "value {} is out of range for type {}",
                                            value,
                                            self.format_type_name(ty)
                                        ),
                                    },
                                    inst.span,
                                ))
                            };
                        }

                        // Two's-complement encoding: negative values are
                        // sign-extended into the u64 payload, matching how
                        // negative literals are emitted elsewhere.
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(value as u64),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Bool(value)) => {
                        let ty = Type::BOOL;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::BoolConst(value),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Type(_type_val)) => {
                        // Type values can only exist at comptime - they cannot be returned
                        // from a comptime block since they can't exist at runtime.
                        Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "type values cannot exist at runtime".to_string(),
                            },
                            inst.span,
                        ))
                    }
                    Some(ConstValue::Function(_)) => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "function references cannot exist at runtime".to_string(),
                        },
                        inst.span,
                    )),
                    // The engine never produces string values (string consts
                    // are non-evaluable in comptime position, RUE-957); keep
                    // a clean diagnostic should that ever change.
                    Some(ConstValue::String(_)) => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "string values are not supported in comptime blocks"
                                .to_string(),
                        },
                        inst.span,
                    )),
                    Some(ConstValue::Float(content)) => {
                        let ty = Self::get_resolved_type(
                            ctx,
                            inst_ref,
                            inst.span,
                            "comptime floating-point value",
                        )?;
                        let spelling = self.body_interner().resolve(&content.spur());
                        if !ty.is_float() {
                            return Err(CompileError::new(
                                ErrorKind::TypeMismatch {
                                    expected: "f32 or f64".to_owned(),
                                    found: self.format_type_name(ty),
                                },
                                inst.span,
                            ));
                        }
                        // A computed comptime value may be `inf` or `NaN`; only a
                        // source literal is held to the finite-range rule.
                        let bits = crate::float_value_bits(spelling, ty).ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::TypeMismatch {
                                    expected: format!("{} value", self.format_type_name(ty)),
                                    found: spelling.to_owned(),
                                },
                                inst.span,
                            )
                        })?;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(bits),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Unit) => {
                        let ty = Type::UNIT;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    None => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason:
                                "expression contains values that cannot be known at compile time"
                                    .to_string(),
                        },
                        inst.span,
                    )),
                }
            }

            // Type constant: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                // Resolve the type name to a concrete type
                let ty = self.resolve_rir_type(*type_name, inst.span)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous struct type: a struct type constructed at comptime
            // (e.g., `struct { first: T, second: T, fn get(self) -> T { ... } }` in a comptime function)
            InstData::AnonStructType {
                fields,
                methods,
                anchor,
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.body_rir_ref().anon_struct_fields(fields).to_vec();

                // Empty structs are not allowed (unless they have methods)
                if field_decls.is_empty()
                    && self.body_rir_ref().anon_struct_methods(methods).is_empty()
                {
                    return Err(CompileError::new(ErrorKind::EmptyStruct, inst.span));
                }

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.body_interner().resolve(&name_sym).to_string();
                    let field_ty = self
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            type_sym,
                            &ctx.comptime_type_vars,
                            &ctx.comptime_value_vars,
                            inst.span,
                        )
                        .ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::UnknownType(self.render_rir_type(type_sym)),
                                inst.span,
                            )
                        })?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Signature decoding is owned by the canonical comptime
                // engine. Ordinary analysis consumes its resolved descriptors
                // and only adapts them to the legacy registration record.
                let descriptors = match ComptimeEngine::new(self).decode_anon_method_descriptors(
                    &(),
                    methods,
                    &ctx.comptime_type_vars,
                    &ctx.comptime_value_vars,
                ) {
                    ComptimeOutcome::Known(value) => value,
                    ComptimeOutcome::RuntimeDependent => Vec::new(),
                    ComptimeOutcome::UnsupportedContext => Vec::new(),
                    ComptimeOutcome::NotReady => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "anonymous method signature was not ready".to_owned(),
                            },
                            inst.span,
                        ));
                    }
                    ComptimeOutcome::Trap(trap) => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: trap.operation.to_owned(),
                            },
                            trap.span,
                        ));
                    }
                    ComptimeOutcome::HostFailure(error) | ComptimeOutcome::Abort(error) => {
                        return Err(error);
                    }
                };
                let method_sigs = descriptors
                    .iter()
                    .map(|descriptor| super::super::info::AnonMethodSig {
                        name: descriptor.name,
                        has_self: descriptor.has_self,
                        self_mode: descriptor.self_mode,
                        returns_borrow: descriptor.returns_borrow,
                        returns_inout: descriptor.returns_inout,
                        param_types: descriptor
                            .parameters
                            .clone()
                            .into_iter()
                            .map(|parameter| match parameter.ty {
                                ComptimeMethodType::SelfType => {
                                    super::super::info::AnonMethodType::SelfType
                                }
                                ComptimeMethodType::Concrete(ty) => {
                                    super::super::info::AnonMethodType::Concrete(ty)
                                }
                                ComptimeMethodType::Unsupported(syntax) => {
                                    super::super::info::AnonMethodType::Syntax(syntax.into())
                                }
                            })
                            .collect(),
                        param_modes: descriptor
                            .parameters
                            .iter()
                            .map(|parameter| parameter.mode)
                            .collect(),
                        param_comptime: descriptor
                            .parameters
                            .iter()
                            .map(|parameter| parameter.is_comptime)
                            .collect(),
                        return_type: match &descriptor.result {
                            ComptimeMethodType::SelfType => {
                                super::super::info::AnonMethodType::SelfType
                            }
                            ComptimeMethodType::Concrete(ty) => {
                                super::super::info::AnonMethodType::Concrete(*ty)
                            }
                            ComptimeMethodType::Unsupported(syntax) => {
                                super::super::info::AnonMethodType::Syntax(syntax.clone().into())
                            }
                        },
                    })
                    .collect::<Vec<_>>();

                // Check if an equivalent anonymous struct already exists (structural equality)
                // This now compares fields, method signatures, AND captured comptime values
                let (struct_ty, is_new) = self.find_or_create_anon_struct(
                    crate::AnonymousNominalKey {
                        kind: crate::AnonymousNominalKind::Struct,
                        producer: ctx.canonical_producer.clone(),
                        anchor: anchor.clone(),
                    },
                    &struct_fields,
                    &method_sigs,
                    &ctx.comptime_value_vars,
                )?;
                if is_new && !descriptors.is_empty() {
                    let struct_id = struct_ty
                        .as_struct()
                        .expect("anonymous struct must have a StructId");
                    self.register_anon_struct_method_bodies(
                        struct_id,
                        struct_ty,
                        methods,
                        &descriptors,
                    )
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "anonymous method registration failed".to_owned(),
                            },
                            inst.span,
                        )
                    })?;
                    if !ctx.comptime_type_vars.is_empty() {
                        self.set_anon_struct_type_subst(
                            struct_id,
                            ctx.comptime_type_vars.snapshot(),
                        );
                    }
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(struct_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous enum type: an enum (sum) type constructed at comptime
            // (e.g., `enum { Some(T), None }` in a comptime type function). The
            // enum analog of the AnonStructType arm above. Generic anon enums
            // (payloads mentioning a `comptime T`) are comptime-evaluated, not
            // analyzed here — this path resolves a concrete anon enum, exactly
            // as the struct arm does (ADR-0038, RUE-6 phase 2).
            InstData::AnonEnumType {
                variants,
                payloads,
                anchor,
            } => {
                let variant_syms = self.body_rir_ref().anon_enum_variants(variants).to_vec();
                let payload_symbols: Vec<Vec<rue_rir::RirTypeSyntaxRef>> = self
                    .body_rir_ref()
                    .anon_enum_payloads(payloads, variants)
                    .map(|payload| payload.to_vec())
                    .collect();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<Type>> = Vec::with_capacity(variant_syms.len());
                for (vsym, symbols) in variant_syms.iter().zip(payload_symbols) {
                    variant_names.push(self.body_interner().resolve(vsym).to_string());
                    let mut tys: Vec<Type> = Vec::with_capacity(symbols.len());
                    for ty_sym in symbols {
                        let field_ty = self
                            .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                                ty_sym,
                                &ctx.comptime_type_vars,
                                &ctx.comptime_value_vars,
                                inst.span,
                            )
                            .ok_or_else(|| {
                                CompileError::new(
                                    ErrorKind::UnknownType(self.render_rir_type(ty_sym)),
                                    inst.span,
                                )
                            })?;
                        // A payload of type `type` cannot exist at runtime
                        // (spec 4.14:6); reject it like struct fields / enum
                        // declarations do.
                        if field_ty.is_comptime_type() {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "type values cannot exist at runtime".to_string(),
                                },
                                inst.span,
                            ));
                        }
                        tys.push(field_ty);
                    }
                    variant_payloads.push(tys);
                }

                let enum_ty = self.find_or_create_anon_enum(
                    crate::AnonymousNominalKey {
                        kind: crate::AnonymousNominalKind::Enum,
                        producer: ctx.canonical_producer.clone(),
                        anchor: anchor.clone(),
                    },
                    &variant_names,
                    &variant_payloads,
                )?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(enum_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Checked block: evaluate the inner expression within an unchecked
            // context. Raw-pointer intrinsics and calls to `unchecked fn`s are
            // only legal while `checked_depth > 0` (spec 9.1:1, chapter 9).
            InstData::Checked { expr } => {
                ctx.checked_depth += 1;
                let result = self.analyze_inst(air, *expr, ctx);
                ctx.checked_depth -= 1;
                result
            }
        }
    }
}
