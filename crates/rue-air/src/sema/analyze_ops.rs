//! Instruction category analysis methods.
//!
//! This module contains the per-category analysis methods extracted from `analyze_inst`.
//! Each category method handles a specific group of related RIR instructions:
//!
//! - [`analyze_literal`] - Integer, boolean, string, and unit constants
//! - [`analyze_unary_op`] - Negation, logical NOT, bitwise NOT
//!
//! It also owns constant materialization for the whole crate: every position
//! that turns an integer or floating-point constant into an AIR `Const` goes
//! through [`OrdinaryBodyEngine::materialize_int_const`] or
//! [`OrdinaryBodyEngine::materialize_float_const`], so one range rule, one
//! "does not fit" diagnostic, and one payload encoding stand behind literals,
//! struct-initializer fields, pointer writes, named constant uses, and
//! comptime-block results alike.
//!
//! Control-flow expressions are owned by the sibling `control_flow` module.
//! Aggregate construction and member operations are owned by the sibling
//! `aggregates` module. Place and ownership behavior remains canonical in
//! `analysis::ownership`.
//!
//! - [`analyze_decl_noop`] - DropFnDecl (declarations that produce Unit)
//!
//! Binary operations (arithmetic, comparison, logical, bitwise) are handled
//! by helpers in `sema::analysis::builtin_ops`:
//! - `analyze_binary_arith` - Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
//! - `analyze_comparison` - Eq, Ne, Lt, Gt, Le, Ge
//! - Logical And/Or are simple enough to remain inline

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::{InstData, InstRef, RirStructuralAnchor};

use super::context::{AnalysisContext, AnalysisResult};
use crate::inst::{Air, AirInst, AirInstData};
use crate::types::Type;

// ============================================================================

/// How a floating-point constant reached materialization.
///
/// A literal is held to the source-literal rule — its spelling must name a
/// finite value at the target width — while a value computed at compile time
/// may legitimately be `inf` or `NaN`, so the two read the same spelling with
/// different parsers.
pub(crate) enum FloatConstSource<'a> {
    Literal { spelling: &'a str, negated: bool },
    ComputedValue { spelling: &'a str },
}

/// Where a string value came from when it is materialized into AIR.
pub(crate) enum StringConstSource {
    Literal(RirStructuralAnchor),
    Synthesized,
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Resolve an integer expression through canonical inference, admitting
    /// the narrowly scoped constructor-head recovery context when that walk
    /// was deliberately skipped.
    fn resolved_integer_type(
        ctx: &AnalysisContext,
        inst_ref: InstRef,
        span: rue_span::Span,
        context: &str,
    ) -> CompileResult<Type> {
        if let Some(ty) = ctx
            .resolved_type_of(inst_ref)
            .or(ctx.missing_inference_integer_type)
        {
            Ok(ty)
        } else {
            Self::get_resolved_type(ctx, inst_ref, span, context)
        }
    }

    // ========================================================================
    // Constant materialization: the one owner
    // ========================================================================

    /// Materialize a string through the same local string table and capacity
    /// rule used by source literals. Comptime results have no source anchor,
    /// so they use the synthesized atom path while retaining the contextual
    /// type's fixed-capacity check.
    pub(crate) fn materialize_string_const(
        &self,
        ctx: &mut AnalysisContext,
        content: String,
        ty: Type,
        span: rue_span::Span,
        source: StringConstSource,
    ) -> CompileResult<AirInstData> {
        if let Some(capacity) = self.str_fixed_capacity(ty) {
            let byte_len = content.len() as u64;
            if byte_len > capacity {
                return Err(CompileError::new(
                    ErrorKind::StrFixedCapacityExceeded { capacity, byte_len },
                    span,
                ));
            }
        }
        let local_id = match source {
            StringConstSource::Literal(anchor) => ctx.add_local_string(content, anchor),
            StringConstSource::Synthesized => ctx.add_synthesized_string(&content),
        };
        Ok(AirInstData::StringConst(local_id))
    }

    /// Materialize an integer constant as the AIR `Const` payload for `ty`.
    ///
    /// Every position that turns an integer written in source — or computed
    /// at compile time from integers written in source — into an AIR constant
    /// comes through here: ordinary literals, negated literals, the
    /// struct-initializer field shortcut, `@ptr_write` value coercion, named
    /// constant uses, and comptime-block results. One range rule and one
    /// "does not fit" diagnostic (E0800, spec 6.5:5) therefore stand behind
    /// all of them, and the payload has one encoding.
    ///
    /// `value` is the mathematical value, so a negated literal passes the
    /// value it denotes (`-129`), not the magnitude its operand spells.
    pub(crate) fn materialize_int_const(
        &self,
        value: i128,
        ty: Type,
        span: rue_span::Span,
    ) -> CompileResult<AirInstData> {
        // An integer is admitted directly where a float is expected
        // (ADR-0065 §3): it becomes the float nearest to it. This is literal
        // contextualization, not a runtime integer/float conversion.
        if ty.is_float() {
            let bits = if ty == Type::F32 {
                u64::from((value as f32).to_bits())
            } else {
                (value as f64).to_bits()
            };
            return Ok(AirInstData::Const(bits));
        }

        // A target with no integer range at all — an array, a struct — is one
        // the value cannot be represented in either, so it takes the same
        // report rather than materializing a payload the backend would then
        // have to make sense of.
        if !ty
            .integer_semantics()
            .is_some_and(|integer| integer.fits_i128(value))
        {
            return Err(CompileError::new(
                ErrorKind::LiteralOutOfRange {
                    value,
                    ty: self.format_type_name(ty),
                },
                span,
            ));
        }

        // Two's-complement encoding: a negative value is sign-extended into
        // the u64 payload.
        Ok(AirInstData::Const(value as u64))
    }

    /// Materialize a floating-point constant as the AIR `Const` payload for
    /// `ty`, with the spelling read by the parser its source calls for.
    pub(crate) fn materialize_float_const(
        &self,
        source: FloatConstSource<'_>,
        ty: Type,
        span: rue_span::Span,
    ) -> CompileResult<AirInstData> {
        // `comptime_float` has no runtime width to round to; its uses are
        // resolved before lowering, so the placeholder payload is zero.
        if ty == Type::COMPTIME_FLOAT {
            return Ok(AirInstData::Const(0));
        }
        if !ty.is_float() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "f32 or f64".to_owned(),
                    found: self.format_type_name(ty),
                },
                span,
            ));
        }
        let type_name = self.format_type_name(ty);
        let (bits, expected, found) = match source {
            FloatConstSource::Literal { spelling, negated } => (
                crate::finite_float_literal_bits_with_sign(spelling, ty, negated),
                format!("finite {type_name} literal"),
                if negated {
                    format!("-{spelling}")
                } else {
                    spelling.to_owned()
                },
            ),
            FloatConstSource::ComputedValue { spelling } => (
                crate::float_value_bits(spelling, ty),
                format!("{type_name} value"),
                spelling.to_owned(),
            ),
        };
        let bits = bits
            .ok_or_else(|| CompileError::new(ErrorKind::TypeMismatch { expected, found }, span))?;
        Ok(AirInstData::Const(bits))
    }

    // ========================================================================
    // Literals: IntConst, BoolConst, StringConst, UnitConst
    // ========================================================================

    /// Analyze a literal constant instruction.
    ///
    /// Handles primitive literals, including context-rounded float literals.
    pub(crate) fn analyze_literal(
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
            InstData::FloatConst { text } => {
                let ty =
                    Self::get_resolved_type(ctx, inst_ref, inst.span, "floating-point literal")?;
                let spelling = self.body_interner().resolve(text);
                let data = self.materialize_float_const(
                    FloatConstSource::Literal {
                        spelling,
                        negated: false,
                    },
                    ty,
                    inst.span,
                )?;
                let air_ref = air.add_inst(AirInst {
                    data,
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }
            InstData::IntConst(value) => {
                // Constructor-head recovery can make a call operand reachable
                // to sema even though the unsuccessful head reduction kept it
                // outside the ordinary inference walk. The call boundary
                // supplies its declared parameter type as recovery context;
                // use that integer type rather than turning the source-level
                // "head is not a type" error into an unresolved-type ICE.
                // Normal inferred literals still take the resolved-map path.
                let ty = Self::resolved_integer_type(ctx, inst_ref, inst.span, "integer literal")?;

                let data = self.materialize_int_const(i128::from(*value), ty, inst.span)?;
                let air_ref = air.add_inst(AirInst {
                    data,
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

            InstData::StringConst {
                content: symbol,
                anchor,
            } => {
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
                let string_content = self.body_interner().resolve(&*symbol).to_string();

                let data = self.materialize_string_const(
                    ctx,
                    string_content,
                    ty,
                    inst.span,
                    StringConstSource::Literal(anchor.clone()),
                )?;

                let air_ref = air.add_inst(AirInst {
                    data,
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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

        match &inst.data {
            InstData::Neg { operand } => {
                // Get the resolved type from HM inference
                let ty =
                    Self::resolved_integer_type(ctx, inst_ref, inst.span, "negation operator")?;

                // Unary `-` requires a signed integer operand (i8/i16/i32/i64/
                // isize). Reject unsigned integers (no negative range), bool, and
                // every other non-signed type. `<error>`/`never` pass through so a
                // prior error isn't masked by a spurious second diagnostic.
                if !ty.is_signed() && !ty.is_float() && !ty.is_error() && !ty.is_never() {
                    let note = if ty.is_unsigned() {
                        "unsigned values cannot be negated"
                    } else {
                        "unary `-` requires a signed integer or floating-point operand"
                    };
                    return Err(CompileError::new(
                        ErrorKind::CannotNegate(self.format_type_name(ty)),
                        inst.span,
                    )
                    .with_note(note));
                }

                // Special case: negating a literal that equals |MIN| for signed types.
                let operand_inst = self.body_rir_ref().get(*operand);
                if let InstData::FloatConst { text } = &operand_inst.data
                    && ty.is_float()
                {
                    let spelling = self.body_interner().resolve(text);
                    let data = self.materialize_float_const(
                        FloatConstSource::Literal {
                            spelling,
                            negated: true,
                        },
                        ty,
                        inst.span,
                    )?;
                    let air_ref = air.add_inst(AirInst {
                        data,
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
                // A negated integer literal whose magnitude alone is out of
                // range is one literal, not an operation on one: `-128` at
                // `i8` denotes the type minimum even though `128` does not
                // fit, so the operand cannot be analyzed on its own. Handing
                // the owner the value the literal denotes admits that case
                // and, when the denoted value does not fit either, reports
                // `-129` rather than the magnitude the operand spells.
                if let InstData::IntConst(magnitude) = &operand_inst.data
                    && let Some(integer) = ty.integer_semantics()
                    && !integer.fits_i128(i128::from(*magnitude))
                {
                    let data =
                        self.materialize_int_const(-i128::from(*magnitude), ty, inst.span)?;
                    let air_ref = air.add_inst(AirInst {
                        data,
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                if !operand_result.continues {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Neg(operand_result.air_ref),
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::with_continues(air_ref, ty, false));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Neg(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Not { operand } => {
                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                if !operand_result.continues {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Not(operand_result.air_ref),
                        ty: Type::BOOL,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::with_continues(air_ref, Type::BOOL, false));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Not(operand_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            InstData::BitNot { operand } => {
                // Get the resolved type from HM inference
                let ty =
                    Self::resolved_integer_type(ctx, inst_ref, inst.span, "bitwise NOT operator")?;

                // Bitwise NOT operates on integer types only
                if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "integer type".to_string(),
                            found: self.format_type_name(ty),
                        },
                        inst.span,
                    ));
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                if !operand_result.continues {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::BitNot(operand_result.air_ref),
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::with_continues(air_ref, ty, false));
                }

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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

        match &inst.data {
            InstData::And { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let reachable_edges_after_lhs = ctx.ownership.loop_break_stack.clone();
                let divergence_before_rhs = ctx.divergence_kinds;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;
                if !lhs_result.continues {
                    Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_lhs);
                    ctx.divergence_kinds = divergence_before_rhs;
                } else if ctx.divergence_kinds.has_other() {
                    // The RHS is reachable on the lhs-true path. Validate an
                    // unchecked generic divergence against the ownership
                    // state at that short-circuit edge, then keep it from
                    // contaminating the later normal-path join or panic.
                    self.check_linear_values_at_unchecked_divergence(ctx)?;
                    ctx.divergence_kinds = ctx.divergence_kinds.without_other();
                }

                if !lhs_result.continues || !rhs_result.continues {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                        ty: Type::BOOL,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::with_continues(air_ref, Type::BOOL, false));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            InstData::Or { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let reachable_edges_after_lhs = ctx.ownership.loop_break_stack.clone();
                let divergence_before_rhs = ctx.divergence_kinds;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;
                if !lhs_result.continues {
                    Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_lhs);
                    ctx.divergence_kinds = divergence_before_rhs;
                } else if ctx.divergence_kinds.has_other() {
                    // The RHS is reachable on the lhs-false path. Validate
                    // its generic divergence before short-circuit joining.
                    self.check_linear_values_at_unchecked_divergence(ctx)?;
                    ctx.divergence_kinds = ctx.divergence_kinds.without_other();
                }

                if !lhs_result.continues || !rhs_result.continues {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                        ty: Type::BOOL,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::with_continues(air_ref, Type::BOOL, false));
                }

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
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

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
