//! Instruction category analysis methods.
//!
//! This module contains the per-category analysis methods extracted from `analyze_inst`.
//! Each category method handles a specific group of related RIR instructions:
//!
//! - [`analyze_literal`] - Integer, boolean, string, and unit constants
//! - [`analyze_unary_op`] - Negation, logical NOT, bitwise NOT
//! - [`analyze_control_flow`] - Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
//! - [`analyze_variable_ops`] - Alloc, VarRef, ParamRef, Assign
//! - [`analyze_struct_ops`] - StructDecl, StructInit, FieldGet, FieldSet
//! - [`analyze_array_ops`] - ArrayInit, IndexGet, IndexSet
//! - [`analyze_enum_ops`] - EnumDecl, EnumVariant
//! - [`analyze_call_ops`] - Call, MethodCall, AssocFnCall
//! - [`analyze_intrinsic_ops`] - Intrinsic, TypeIntrinsic
//! - [`analyze_decl_noop`] - ImplDecl, DropFnDecl (declarations that produce Unit)
//!
//! Binary operations (arithmetic, comparison, logical, bitwise) are handled
//! by existing helper methods in `analysis.rs`:
//! - `analyze_binary_arith` - Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
//! - `analyze_comparison` - Eq, Ne, Lt, Gt, Le, Ge
//! - Logical And/Or are simple enough to remain inline

use std::collections::{HashMap, HashSet};

use lasso::Spur;
use rue_error::{
    CompileError, CompileResult, CompileWarning, ErrorKind, MissingFieldsError, OptionExt,
    PreviewFeature, WarningKind,
};
use rue_rir::{InstData, InstRef, RirArgMode, RirParamMode, RirPattern};
use rue_span::Span;

use super::Sema;
use super::context::{AnalysisContext, AnalysisResult, LocalVar};
use crate::inst::{Air, AirInst, AirInstData, AirPattern, AirRef};
use crate::scope::ScopedContext;
use crate::types::Type;

impl<'a> Sema<'a> {
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
                            ty: ty.name().to_string(),
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
                let ty = Type::Bool;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::StringConst(symbol) => {
                // String literals use the builtin String struct type.
                let ty = self.builtin_string_type();
                // Add string to the local string table (per-function for parallel analysis)
                let string_content = self.interner.resolve(&*symbol).to_string();
                let local_string_id = ctx.add_local_string(string_content);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::StringConst(local_string_id),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::UnitConst => {
                let ty = Type::Unit;
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

                // Check if trying to negate an unsigned type.
                if ty.is_unsigned() {
                    return Err(CompileError::new(
                        ErrorKind::CannotNegateUnsigned(ty.name().to_string()),
                        inst.span,
                    )
                    .with_note("unsigned values cannot be negated"));
                }

                // Special case: negating a literal that equals |MIN| for signed types.
                let operand_inst = self.rir.get(*operand);
                if let InstData::IntConst(value) = &operand_inst.data {
                    // Check if this value, when negated, fits in the target signed type
                    if ty.negated_literal_fits(*value) && !ty.literal_fits(*value) {
                        // This is the MIN value case - store the MIN value directly.
                        let neg_value = match ty {
                            Type::I8 => (i8::MIN as i64) as u64,
                            Type::I16 => (i16::MIN as i64) as u64,
                            Type::I32 => (i32::MIN as i64) as u64,
                            Type::I64 => i64::MIN as u64,
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
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::BitNot { operand } => {
                // Get the resolved type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "bitwise NOT operator")?;

                // Bitwise NOT operates on integer types only
                if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "integer type".to_string(),
                            found: ty.name().to_string(),
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
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::Or { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
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
    // Control flow: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    // ========================================================================

    /// Analyze a control flow instruction.
    ///
    /// Handles: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    pub(crate) fn analyze_control_flow(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => self.analyze_branch(air, *cond, *then_block, *else_block, inst.span, ctx),

            InstData::Loop { cond, body } => {
                self.analyze_while_loop(air, *cond, *body, inst.span, ctx)
            }

            InstData::InfiniteLoop { body } => {
                self.analyze_infinite_loop(air, *body, inst.span, ctx)
            }

            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => self.analyze_match(air, *scrutinee, *arms_start, *arms_len, inst.span, ctx),

            InstData::Break => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::BreakOutsideLoop, inst.span));
                }

                // Break has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Break,
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Continue => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
                }

                // Continue has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Continue,
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Ret(inner) => {
                self.analyze_return(air, inner.as_ref().copied(), inst.span, ctx)
            }

            InstData::Block { extra_start, len } => {
                self.analyze_block(air, *extra_start, *len, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_control_flow called with non-control-flow instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a branch (if-else) expression.
    fn analyze_branch(
        &mut self,
        air: &mut Air,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Condition must be bool
        let cond_result = self.analyze_inst(air, cond, ctx)?;

        if let Some(else_b) = else_block {
            // Save move state before entering branches.
            let saved_moves = ctx.moved_vars.clone();

            // Analyze then branch with its own scope
            ctx.push_scope();
            let then_result = self.analyze_inst(air, then_block, ctx)?;
            let then_type = then_result.ty;
            let then_span = self.rir.get(then_block).span;
            ctx.pop_scope();

            // Capture then-branch's move state
            let then_moves = ctx.moved_vars.clone();

            // Restore to saved state before analyzing else branch
            ctx.moved_vars = saved_moves;

            // Analyze else branch with its own scope
            ctx.push_scope();
            let else_result = self.analyze_inst(air, else_b, ctx)?;
            let else_type = else_result.ty;
            let else_span = self.rir.get(else_b).span;
            ctx.pop_scope();

            // Capture else-branch's move state
            let else_moves = ctx.moved_vars.clone();

            // Merge move states from both branches.
            ctx.merge_branch_moves(
                then_moves,
                else_moves,
                then_type.is_never(),
                else_type.is_never(),
            );

            // Compute the unified result type using never type coercion
            let result_type = match (then_type.is_never(), else_type.is_never()) {
                (true, true) => Type::Never,
                (true, false) => else_type,
                (false, true) => then_type,
                (false, false) => {
                    // Neither diverges - types must match exactly
                    if then_type != else_type && !then_type.is_error() && !else_type.is_error() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: then_type.name().to_string(),
                                found: else_type.name().to_string(),
                            },
                            else_span,
                        )
                        .with_label(format!("this is of type `{}`", then_type.name()), then_span)
                        .with_note("if and else branches must have compatible types"));
                    }
                    then_type
                }
            };

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Branch {
                    cond: cond_result.air_ref,
                    then_value: then_result.air_ref,
                    else_value: Some(else_result.air_ref),
                },
                ty: result_type,
                span,
            });
            Ok(AnalysisResult::new(air_ref, result_type))
        } else {
            // No else branch - result is Unit
            // The then branch must have unit type (spec 4.6:5)

            // Save move state before entering then-branch.
            let saved_moves = ctx.moved_vars.clone();

            ctx.push_scope();
            let then_result = self.analyze_inst(air, then_block, ctx)?;
            ctx.pop_scope();

            // Check that the then branch has unit type (or Never/Error)
            let then_type = then_result.ty;
            if then_type != Type::Unit && !then_type.is_never() && !then_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "()".to_string(),
                        found: then_type.name().to_string(),
                    },
                    self.rir.get(then_block).span,
                )
                .with_help(
                    "if expressions without else must have unit type; \
                     consider adding an else branch or making the body return ()",
                ));
            }

            // Capture then-branch's move state
            let then_moves = ctx.moved_vars.clone();

            // For if-without-else:
            if then_type.is_never() {
                // Then-branch diverges - code after if only runs if cond was false
                ctx.moved_vars = saved_moves;
            } else {
                // Then-branch doesn't diverge - merge moves (union semantics).
                ctx.merge_branch_moves(
                    then_moves,
                    saved_moves,
                    false, // then doesn't diverge
                    false, // "else" (empty) doesn't diverge
                );
            }

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Branch {
                    cond: cond_result.air_ref,
                    then_value: then_result.air_ref,
                    else_value: None,
                },
                ty: Type::Unit,
                span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Unit))
        }
    }

    /// Analyze a while loop.
    fn analyze_while_loop(
        &mut self,
        air: &mut Air,
        cond: InstRef,
        body: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // While loop: condition must be bool, result is Unit
        let cond_result = self.analyze_inst(air, cond, ctx)?;

        // Analyze body with its own scope
        ctx.push_scope();
        ctx.loop_depth += 1;
        let body_result = self.analyze_inst(air, body, ctx)?;
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Loop {
                cond: cond_result.air_ref,
                body: body_result.air_ref,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Analyze an infinite loop.
    fn analyze_infinite_loop(
        &mut self,
        air: &mut Air,
        body: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Infinite loop: `loop { body }` - always produces Never type

        ctx.push_scope();
        ctx.loop_depth += 1;
        let body_result = self.analyze_inst(air, body, ctx)?;
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::InfiniteLoop {
                body: body_result.air_ref,
            },
            ty: Type::Never,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Never))
    }

    /// Analyze a match expression.
    fn analyze_match(
        &mut self,
        air: &mut Air,
        scrutinee: InstRef,
        arms_start: u32,
        arms_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Analyze the scrutinee to determine its type
        let scrutinee_result = self.analyze_inst(air, scrutinee, ctx)?;
        let scrutinee_type = scrutinee_result.ty;

        // Validate that we can match on this type (integers, booleans, and enums)
        if !scrutinee_type.is_integer() && scrutinee_type != Type::Bool && !scrutinee_type.is_enum()
        {
            return Err(CompileError::new(
                ErrorKind::InvalidMatchType(scrutinee_type.name().to_string()),
                span,
            ));
        }

        let arms = self.rir.get_match_arms(arms_start, arms_len);
        // Check for empty match
        if arms.is_empty() {
            return Err(CompileError::new(ErrorKind::EmptyMatch, span));
        }

        // Track patterns for exhaustiveness checking and duplicate detection
        let mut wildcard_span: Option<Span> = None;
        let mut bool_true_span: Option<Span> = None;
        let mut bool_false_span: Option<Span> = None;
        let mut seen_ints: HashMap<i64, Span> = HashMap::new();
        let mut covered_variants: HashSet<u32> = HashSet::new();
        let mut seen_variants: HashMap<u32, Span> = HashMap::new();
        let mut pattern_enum_id: Option<crate::types::EnumId> = None;

        // Analyze each arm (each arm gets its own scope)
        let mut air_arms = Vec::new();
        let mut result_type: Option<Type> = None;

        for (pattern, body) in arms.iter() {
            let pattern_span = pattern.span();

            // If we've seen a wildcard, everything after is unreachable
            if let Some(first_wildcard_span) = wildcard_span {
                let pat_str = match pattern {
                    RirPattern::Wildcard(_) => "_".to_string(),
                    RirPattern::Int(n, _) => n.to_string(),
                    RirPattern::Bool(b, _) => b.to_string(),
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        format!(
                            "{}::{}",
                            self.interner.resolve(&*type_name),
                            self.interner.resolve(&*variant)
                        )
                    }
                };
                ctx.warnings.push(
                    CompileWarning::new(
                        WarningKind::UnreachablePattern(pat_str),
                        pattern_span,
                    )
                    .with_label("previous wildcard pattern here", first_wildcard_span)
                    .with_note(
                        "this pattern will never be matched because the wildcard pattern above matches everything",
                    ),
                );
            }

            // Validate pattern against scrutinee type and check for duplicates
            match pattern {
                RirPattern::Wildcard(_) => {
                    if wildcard_span.is_none() {
                        wildcard_span = Some(pattern_span);
                    }
                }
                RirPattern::Int(n, _) => {
                    if !scrutinee_type.is_integer() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: "integer".to_string(),
                            },
                            pattern_span,
                        ));
                    }
                    // Check for duplicate integer pattern
                    if let Some(first_span) = seen_ints.get(n) {
                        if wildcard_span.is_none() {
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern(n.to_string()),
                                    pattern_span,
                                )
                                .with_label("first occurrence of this pattern", *first_span)
                                .with_note(
                                    "this pattern will never be matched because an earlier arm already matches the same value",
                                ),
                            );
                        }
                    } else {
                        seen_ints.insert(*n, pattern_span);
                    }
                }
                RirPattern::Bool(b, _) => {
                    if scrutinee_type != Type::Bool {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: "bool".to_string(),
                            },
                            pattern_span,
                        ));
                    }
                    // Check for duplicate boolean pattern
                    let (first_span_opt, is_true) = if *b {
                        (&mut bool_true_span, true)
                    } else {
                        (&mut bool_false_span, false)
                    };
                    if let Some(first_span) = *first_span_opt {
                        if wildcard_span.is_none() {
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern(is_true.to_string()),
                                    pattern_span,
                                )
                                .with_label("first occurrence of this pattern", first_span)
                                .with_note(
                                    "this pattern will never be matched because an earlier arm already matches the same value",
                                ),
                            );
                        }
                    } else {
                        *first_span_opt = Some(pattern_span);
                    }
                }
                RirPattern::Path {
                    type_name, variant, ..
                } => {
                    // Look up the enum type
                    let enum_id = self.enums.get(type_name).ok_or_compile_error(
                        ErrorKind::UnknownEnumType(self.interner.resolve(&*type_name).to_string()),
                        pattern_span,
                    )?;
                    let enum_def = &self.enum_defs[enum_id.0 as usize];

                    // Check that scrutinee type matches the pattern's enum type
                    if scrutinee_type != Type::Enum(*enum_id) {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: enum_def.name.clone(),
                            },
                            pattern_span,
                        ));
                    }

                    // Find the variant index
                    let variant_name = self.interner.resolve(&*variant);
                    let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                        ErrorKind::UnknownVariant {
                            enum_name: enum_def.name.clone(),
                            variant_name: variant_name.to_string(),
                        },
                        pattern_span,
                    )?;

                    covered_variants.insert(variant_index as u32);
                    pattern_enum_id = Some(*enum_id);
                }
            }

            // Each arm gets its own scope
            ctx.push_scope();

            // Analyze arm body
            let body_result = self.analyze_inst(air, *body, ctx)?;
            let body_type = body_result.ty;

            ctx.pop_scope();

            // Update result type (handle Never type coercion)
            result_type = Some(match result_type {
                None => body_type,
                Some(prev) => {
                    if prev.is_never() {
                        body_type
                    } else if body_type.is_never() {
                        prev
                    } else if prev != body_type && !prev.is_error() && !body_type.is_error() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: prev.name().to_string(),
                                found: body_type.name().to_string(),
                            },
                            self.rir.get(*body).span,
                        ));
                    } else {
                        prev
                    }
                }
            });

            // Convert pattern to AIR pattern
            let air_pattern = match pattern {
                RirPattern::Wildcard(_) => AirPattern::Wildcard,
                RirPattern::Int(n, _) => AirPattern::Int(*n),
                RirPattern::Bool(b, _) => AirPattern::Bool(*b),
                RirPattern::Path {
                    type_name, variant, ..
                } => {
                    let type_name_str = self.interner.resolve(&*type_name).to_string();
                    let enum_id = *self.enums.get(type_name).ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "enum type '{}' not found during pattern conversion",
                                type_name_str
                            )),
                            pattern_span,
                        )
                    })?;
                    let enum_def = &self.enum_defs[enum_id.0 as usize];
                    let variant_name = self.interner.resolve(&*variant);
                    let variant_index = enum_def.find_variant(variant_name).ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::InternalError(format!(
                                "enum variant '{}::{}' not found during pattern conversion",
                                type_name_str, variant_name
                            )),
                            pattern_span,
                        )
                    })?;
                    AirPattern::EnumVariant {
                        enum_id,
                        variant_index: variant_index as u32,
                    }
                }
            };

            air_arms.push((air_pattern, body_result.air_ref));
        }

        // Exhaustiveness checking
        let has_wildcard = wildcard_span.is_some();
        let bool_true_covered = bool_true_span.is_some();
        let bool_false_covered = bool_false_span.is_some();
        let is_exhaustive = if scrutinee_type == Type::Bool {
            has_wildcard || (bool_true_covered && bool_false_covered)
        } else if let Some(enum_id) = pattern_enum_id {
            let enum_def = &self.enum_defs[enum_id.0 as usize];
            has_wildcard || covered_variants.len() == enum_def.variant_count()
        } else {
            // For integers, must have wildcard
            has_wildcard
        };

        if !is_exhaustive {
            return Err(CompileError::new(ErrorKind::NonExhaustiveMatch, span));
        }

        let final_type = result_type.unwrap_or(Type::Unit);

        // Encode match arms into extra array
        let arms_len = air_arms.len() as u32;
        let mut extra_data = Vec::new();
        for (pattern, body) in &air_arms {
            pattern.encode(*body, &mut extra_data);
        }
        let arms_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Match {
                scrutinee: scrutinee_result.air_ref,
                arms_start,
                arms_len,
            },
            ty: final_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, final_type))
    }

    /// Analyze a return statement.
    fn analyze_return(
        &mut self,
        air: &mut Air,
        inner: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inner_air_ref = if let Some(inner) = inner {
            // Explicit return with value
            let inner_result = self.analyze_inst(air, inner, ctx)?;
            let inner_ty = inner_result.ty;

            // Type check: returned value must match function's return type.
            if !ctx.return_type.is_error()
                && !inner_ty.is_error()
                && !inner_ty.can_coerce_to(&ctx.return_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx.return_type.name().to_string(),
                        found: inner_ty.name().to_string(),
                    },
                    span,
                ));
            }
            Some(inner_result.air_ref)
        } else {
            // `return;` without expression - only valid for unit-returning functions
            if ctx.return_type != Type::Unit && !ctx.return_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx.return_type.name().to_string(),
                        found: "()".to_string(),
                    },
                    span,
                ));
            }
            None
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Ret(inner_air_ref),
            ty: Type::Never, // Return expressions have Never type
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Never))
    }

    /// Analyze a block expression.
    fn analyze_block(
        &mut self,
        air: &mut Air,
        extra_start: u32,
        len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Get the instruction refs from extra data
        let inst_refs = self.rir.get_extra(extra_start, len);

        // Push a new scope for this block.
        ctx.push_scope();

        // Process all instructions in the block
        let mut statements = Vec::new();
        let mut last_result: Option<AnalysisResult> = None;
        let num_insts = inst_refs.len();
        for (i, &raw_ref) in inst_refs.iter().enumerate() {
            let inst_ref = InstRef::from_raw(raw_ref);
            let is_last = i == num_insts - 1;
            let result = self.analyze_inst(air, inst_ref, ctx)?;

            if is_last {
                last_result = Some(result);
            } else {
                statements.push(result.air_ref);
            }
        }

        // Check for unconsumed linear values before popping scope
        self.check_unconsumed_linear_values(ctx)?;

        // Check for unused variables before popping scope
        self.check_unused_locals_in_current_scope(ctx);

        // Pop scope to remove block-scoped variables.
        ctx.pop_scope();

        // Handle empty blocks - they evaluate to Unit
        let last = match last_result {
            Some(result) => result,
            None => {
                // Empty block: create a UnitConst
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::Unit,
                    span,
                });
                AnalysisResult::new(air_ref, Type::Unit)
            }
        };

        // Only create a Block instruction if there are statements;
        // otherwise just return the value directly (optimization)
        if statements.is_empty() {
            Ok(last)
        } else {
            let ty = last.ty;
            let stmt_u32s: Vec<u32> = statements.iter().map(|r| r.as_u32()).collect();
            let stmts_start = air.add_extra(&stmt_u32s);
            let stmts_len = statements.len() as u32;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Block {
                    stmts_start,
                    stmts_len,
                    value: last.air_ref,
                },
                ty,
                span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }
    }

    // ========================================================================
    // Variable operations: Alloc, VarRef, ParamRef, Assign
    // ========================================================================

    /// Analyze a variable operation instruction.
    ///
    /// Handles: Alloc, VarRef, ParamRef, Assign
    pub(crate) fn analyze_variable_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Alloc {
                directives_start,
                directives_len,
                name,
                is_mut,
                ty: _,
                init,
            } => self.analyze_alloc(
                air,
                *directives_start,
                *directives_len,
                *name,
                *is_mut,
                *init,
                inst.span,
                ctx,
            ),

            InstData::VarRef { name } => self.analyze_var_ref(air, *name, inst.span, ctx),

            InstData::ParamRef { index: _, name } => {
                self.analyze_param_ref(air, *name, inst.span, ctx)
            }

            InstData::Assign { name, value } => {
                self.analyze_assign(air, *name, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_variable_ops called with non-variable instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a local variable allocation.
    fn analyze_alloc(
        &mut self,
        air: &mut Air,
        directives_start: u32,
        directives_len: u32,
        name: Option<Spur>,
        is_mut: bool,
        init: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Analyze the initializer
        let init_result = self.analyze_inst(air, init, ctx)?;
        let var_type = init_result.ty;

        // If name is None, this is a wildcard pattern `_` that discards the value
        let Some(name) = name else {
            return Ok(AnalysisResult::new(init_result.air_ref, Type::Unit));
        };

        // Check if @allow(unused_variable) directive is present
        let directives = self.rir.get_directives(directives_start, directives_len);
        let allow_unused = self.has_allow_directive(&directives, "unused_variable");

        // Allocate slots
        let slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(var_type);
        ctx.next_slot += num_slots;

        // Register the variable
        ctx.insert_local(
            name,
            LocalVar {
                slot,
                ty: var_type,
                is_mut,
                span,
                allow_unused,
            },
        );

        // Emit StorageLive to mark the slot as live
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty: var_type,
            span,
        });

        // Emit the alloc instruction
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot,
                init: init_result.air_ref,
            },
            ty: Type::Unit,
            span,
        });

        // Return a block containing both StorageLive and Alloc
        let stmts_start = air.add_extra(&[storage_live_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 1,
                value: alloc_ref,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(block_ref, Type::Unit))
    }

    /// Analyze a variable reference.
    fn analyze_var_ref(
        &mut self,
        air: &mut Air,
        name: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // First check if it's a parameter
        if let Some(param_info) = ctx.params.get(&name) {
            let ty = param_info.ty;
            let name_str = self.interner.resolve(&name);

            // Check if this parameter has been moved
            if let Some(move_state) = ctx.moved_vars.get(&name) {
                if let Some(moved_span) = move_state.is_any_part_moved() {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            // Handle move semantics based on parameter mode
            if !self.is_type_copy(ty) {
                match param_info.mode {
                    RirParamMode::Normal => {
                        ctx.moved_vars
                            .entry(name)
                            .or_default()
                            .mark_path_moved(&[], span);
                    }
                    RirParamMode::Inout => {
                        ctx.moved_vars
                            .entry(name)
                            .or_default()
                            .mark_path_moved(&[], span);
                    }
                    RirParamMode::Borrow => {
                        let name_str = self.interner.resolve(&name);
                        return Err(CompileError::new(
                            ErrorKind::MoveOutOfBorrow {
                                variable: name_str.to_string(),
                            },
                            span,
                        ));
                    }
                }
            }

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Param {
                    index: param_info.abi_slot,
                },
                ty,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Look up the variable in locals
        let name_str = self.interner.resolve(&name);
        let local = ctx
            .locals
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

        let ty = local.ty;
        let slot = local.slot;

        // Check if this variable has been moved
        if let Some(move_state) = ctx.moved_vars.get(&name) {
            if let Some(moved_span) = move_state.is_any_part_moved() {
                return Err(
                    CompileError::new(ErrorKind::UseAfterMove(name_str.to_string()), span)
                        .with_label("value moved here", moved_span),
                );
            }
        }

        // If type is not Copy, mark as moved
        if !self.is_type_copy(ty) {
            ctx.moved_vars
                .entry(name)
                .or_default()
                .mark_path_moved(&[], span);
        }

        // Mark variable as used
        ctx.used_locals.insert(name);

        // Load the variable
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Load { slot },
            ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ty))
    }

    /// Analyze a parameter reference.
    fn analyze_param_ref(
        &mut self,
        air: &mut Air,
        name: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let name_str = self.interner.resolve(&name);
        let param_info = ctx
            .params
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

        let ty = param_info.ty;

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Param {
                index: param_info.abi_slot,
            },
            ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ty))
    }

    /// Analyze an assignment.
    fn analyze_assign(
        &mut self,
        air: &mut Air,
        name: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let name_str = self.interner.resolve(&name);

        // First check if it's a parameter (for inout params)
        if let Some(param_info) = ctx.params.get(&name) {
            // Check parameter mode - only inout can be assigned to
            match param_info.mode {
                RirParamMode::Normal => {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        span,
                    )
                    .with_help(format!(
                        "consider making parameter `{}` inout: `inout {}: {}`",
                        name_str,
                        name_str,
                        param_info.ty.name()
                    )));
                }
                RirParamMode::Inout => {
                    // Inout parameters can be assigned to
                }
                RirParamMode::Borrow => {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: name_str.to_string(),
                        },
                        span,
                    ));
                }
            }

            let abi_slot = param_info.abi_slot;

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // Assignment to a parameter resets its move state
            ctx.moved_vars.remove(&name);

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Unit));
        }

        // Look up local variable
        let local = ctx
            .locals
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

        // Check mutability
        if !local.is_mut {
            return Err(CompileError::new(
                ErrorKind::AssignToImmutable(name_str.to_string()),
                span,
            )
            .with_label("variable declared as immutable here", local.span)
            .with_help(format!(
                "consider making `{}` mutable: `let mut {}`",
                name_str, name_str
            )));
        }

        let slot = local.slot;

        // Analyze the value
        let value_result = self.analyze_inst(air, value, ctx)?;

        // Assignment to a mutable variable resets its move state.
        ctx.moved_vars.remove(&name);

        // Emit store instruction
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot,
                value: value_result.air_ref,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
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
                type_name,
                fields_start,
                fields_len,
            } => self.analyze_struct_init(
                air,
                *type_name,
                *fields_start,
                *fields_len,
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
    fn analyze_struct_init(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        fields_start: u32,
        fields_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let field_inits = self.rir.get_field_inits(fields_start, fields_len);
        // Look up the struct type
        let type_name_str = self.interner.resolve(&type_name);
        let struct_id = *self
            .structs
            .get(&type_name)
            .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.to_string()), span)?;

        // Clone struct def data before mutable borrow
        let struct_def = self.struct_defs[struct_id.0 as usize].clone();
        let struct_type = Type::Struct(struct_id);

        // Build a map from field name to struct field index
        let field_index_map: std::collections::HashMap<&str, usize> = struct_def
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();

        // Check for unknown or duplicate fields
        let mut seen_fields = std::collections::HashSet::new();
        for (init_field_name, _) in field_inits.iter() {
            let init_name = self.interner.resolve(&*init_field_name);

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

        for (init_field_name, field_value) in field_inits.iter() {
            let init_name = self.interner.resolve(&*init_field_name);
            let field_idx = field_index_map[init_name];

            let field_result = self.analyze_inst(air, *field_value, ctx)?;
            analyzed_fields[field_idx] = Some(field_result.air_ref);
            source_order.push(field_idx);
        }

        // Collect field refs in DECLARATION ORDER
        let field_refs: Vec<AirRef> = analyzed_fields
            .into_iter()
            .map(|opt| opt.expect("all fields should be initialized"))
            .collect();

        // Encode into extra array
        let fields_len = field_refs.len() as u32;
        let field_u32s: Vec<u32> = field_refs.iter().map(|r| r.as_u32()).collect();
        let fields_start = air.add_extra(&field_u32s);
        let source_order_u32s: Vec<u32> = source_order.iter().map(|&i| i as u32).collect();
        let source_order_start = air.add_extra(&source_order_u32s);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id,
                fields_start,
                fields_len,
                source_order_start,
            },
            ty: struct_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, struct_type))
    }

    /// Analyze a field access.
    fn analyze_field_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        field: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Field access is a projection
        let base_result = self.analyze_inst_for_projection(air, base, ctx)?;
        let base_type = base_result.ty;

        let struct_id = match base_type {
            Type::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: base_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        let struct_def = &self.struct_defs[struct_id.0 as usize];
        let is_linear = struct_def.is_linear;
        let field_name_str = self.interner.resolve(&field).to_string();

        let (field_index, struct_field) =
            struct_def.find_field(&field_name_str).ok_or_compile_error(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.clone(),
                    field_name: field_name_str.clone(),
                },
                span,
            )?;

        let field_type = struct_field.ty;

        // For linear types, field access consumes the entire struct.
        if is_linear {
            if let Some(root_var) = self.extract_root_variable(inst_ref) {
                ctx.moved_vars
                    .entry(root_var)
                    .or_default()
                    .mark_path_moved(&[], span);
            }
        }
        // For non-linear types, check if accessing a non-Copy field
        else if !self.is_type_copy(field_type) {
            if let Some((root_var, field_path)) = self.extract_field_path(inst_ref) {
                // Check if this field path is already moved
                if let Some(state) = ctx.moved_vars.get(&root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        let root_name = self.interner.resolve(&root_var);
                        let path_str = if field_path.is_empty() {
                            root_name.to_string()
                        } else {
                            let field_names: Vec<_> = field_path
                                .iter()
                                .map(|s| self.interner.resolve(s).to_string())
                                .collect();
                            format!("{}.{}", root_name, field_names.join("."))
                        };
                        return Err(CompileError::new(ErrorKind::UseAfterMove(path_str), span)
                            .with_label("value moved here", moved_span));
                    }
                }

                // Mark this field path as moved
                ctx.moved_vars
                    .entry(root_var)
                    .or_default()
                    .mark_path_moved(&field_path, span);
            }
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::FieldGet {
                base: base_result.air_ref,
                struct_id,
                field_index: field_index as u32,
            },
            ty: field_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, field_type))
    }

    /// Analyze a field assignment.
    ///
    /// This is a complex operation that handles VarRef, ParamRef, and chained field access.
    /// The full implementation is in analysis.rs as it's quite large (~200 lines).
    fn analyze_field_set(
        &mut self,
        air: &mut Air,
        base: InstRef,
        field: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Delegate to the main implementation in analysis.rs
        // This is one of the larger handlers that we'll keep in the main file
        // for now and refactor in a future pass
        self.analyze_field_set_impl(air, base, field, value, span, ctx)
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
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => self.analyze_array_init(air, inst_ref, *elems_start, *elems_len, inst.span, ctx),

            InstData::IndexGet { base, index } => {
                self.analyze_index_get(air, *base, *index, inst.span, ctx)
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

    /// Analyze an array initialization.
    fn analyze_array_init(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        elems_start: u32,
        elems_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let elem_refs = self.rir.get_inst_refs(elems_start, elems_len);

        // Get the array type from HM inference
        let array_type = Self::get_resolved_type(ctx, inst_ref, span, "array literal")?;

        let (array_type_id, _elem_type, expected_len) = match array_type {
            Type::Array(type_id) => {
                let array_def = &self.array_type_defs[type_id.0 as usize];
                (type_id, array_def.element_type, array_def.length)
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "Array literal inferred as non-array type: {}",
                        array_type.name()
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

        // Encode into extra array
        let elems_len = air_elems.len() as u32;
        let elem_u32s: Vec<u32> = air_elems.iter().map(|r| r.as_u32()).collect();
        let elems_start = air.add_extra(&elem_u32s);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::ArrayInit {
                elems_start,
                elems_len,
            },
            ty: array_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, array_type))
    }

    /// Analyze an array index read.
    fn analyze_index_get(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Analyze base and index
        let base_result = self.analyze_inst_for_projection(air, base, ctx)?;
        let base_type = base_result.ty;

        let index_result = self.analyze_inst(air, index, ctx)?;

        // Verify base is an array
        let (array_type_id, elem_type, array_len) = match base_type {
            Type::Array(type_id) => {
                let array_def = &self.array_type_defs[type_id.0 as usize];
                (type_id, array_def.element_type, array_def.length)
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: base_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        // Check for constant out-of-bounds index
        if let Some(const_idx) = self.try_get_const_index(index) {
            if const_idx < 0 || const_idx as u64 >= array_len {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_idx,
                        length: array_len,
                    },
                    self.rir.get(index).span,
                ));
            }
        }

        // Prevent moving non-Copy elements out of arrays.
        // This check is only applied in consume context (analyze_inst), not in
        // projection context (analyze_inst_for_projection), which allows
        // patterns like `arr[i].field` where field is Copy.
        if !self.is_type_copy(elem_type) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: elem_type.name().to_string(),
                },
                span,
            )
            .with_help("use explicit methods like swap() or take() to remove elements"));
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::IndexGet {
                base: base_result.air_ref,
                array_type: base_type,
                index: index_result.air_ref,
            },
            ty: elem_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, elem_type))
    }

    /// Analyze an array index write.
    ///
    /// This is a complex operation that handles VarRef and ParamRef bases.
    /// The full implementation is in analysis.rs as it's quite large.
    fn analyze_index_set(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Delegate to the main implementation in analysis.rs
        self.analyze_index_set_impl(air, base, index, value, span, ctx)
    }

    // ========================================================================
    // Enum operations: EnumDecl, EnumVariant
    // ========================================================================

    /// Analyze an enum operation instruction.
    ///
    /// Handles: EnumDecl, EnumVariant
    pub(crate) fn analyze_enum_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        _ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::EnumDecl { .. } => {
                // Enum declarations are processed during collection phase
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::EnumVariant { type_name, variant } => {
                // Look up the enum type
                let enum_id = self.enums.get(type_name).ok_or_compile_error(
                    ErrorKind::UnknownEnumType(self.interner.resolve(&*type_name).to_string()),
                    inst.span,
                )?;
                let enum_def = &self.enum_defs[enum_id.0 as usize];

                // Find the variant index
                let variant_name = self.interner.resolve(&*variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                    ErrorKind::UnknownVariant {
                        enum_name: enum_def.name.clone(),
                        variant_name: variant_name.to_string(),
                    },
                    inst.span,
                )?;

                let ty = Type::Enum(*enum_id);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id: *enum_id,
                        variant_index: variant_index as u32,
                    },
                    ty,
                    span: inst.span,
                });
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
    // Call operations: Call, MethodCall, AssocFnCall
    // ========================================================================

    /// Analyze a call operation instruction.
    ///
    /// Handles: Call, MethodCall, AssocFnCall
    pub(crate) fn analyze_call_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Call {
                name,
                args_start,
                args_len,
            } => self.analyze_call(air, *name, *args_start, *args_len, inst.span, ctx),

            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => self.analyze_method_call(
                air,
                *receiver,
                *method,
                *args_start,
                *args_len,
                inst.span,
                ctx,
            ),

            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => self.analyze_assoc_fn_call(
                air,
                *type_name,
                *function,
                *args_start,
                *args_len,
                inst.span,
                ctx,
            ),

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_call_ops called with non-call instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a function call.
    fn analyze_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Look up the function
        let fn_name_str = self.interner.resolve(&name).to_string();
        let fn_info = self
            .functions
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;

        let args = self.rir.get_call_args(args_start, args_len);
        // Check argument count
        if args.len() != fn_info.param_types.len() {
            let expected = fn_info.param_types.len();
            let found = args.len();
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount { expected, found },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Check that call-site argument modes match function parameter modes
        // Do this before the mutable borrow in analyze_call_args, accessing fn_info directly
        for (i, (arg, expected_mode)) in args.iter().zip(fn_info.param_modes.iter()).enumerate() {
            match expected_mode {
                RirParamMode::Inout => {
                    if arg.mode != RirArgMode::Inout {
                        return Err(CompileError::new(
                            ErrorKind::InoutKeywordMissing,
                            self.rir.get(args[i].value).span,
                        ));
                    }
                }
                RirParamMode::Borrow => {
                    if arg.mode != RirArgMode::Borrow {
                        return Err(CompileError::new(
                            ErrorKind::BorrowKeywordMissing,
                            self.rir.get(args[i].value).span,
                        ));
                    }
                }
                RirParamMode::Normal => {
                    // Normal params accept any mode
                }
            }
        }

        // Check that comptime parameters receive compile-time constant values
        let has_comptime_params = fn_info.param_comptime.iter().any(|&c| c);
        if has_comptime_params {
            // Gate behind comptime preview feature
            self.require_preview(PreviewFeature::Comptime, "comptime parameters", span)?;

            // Validate each comptime parameter receives a compile-time constant
            for (i, (&is_comptime, arg)) in
                fn_info.param_comptime.iter().zip(args.iter()).enumerate()
            {
                if is_comptime {
                    // Try to evaluate the argument at compile time
                    if self.try_evaluate_const(arg.value).is_none() {
                        let param_name = self.interner.resolve(&fn_info.param_names[i]).to_string();
                        return Err(CompileError::new(
                            ErrorKind::ComptimeArgNotConst {
                                param_name: param_name.clone(),
                            },
                            self.rir.get(arg.value).span,
                        )
                        .with_help(format!(
                            "parameter '{}' is declared as 'comptime' and requires a compile-time known value",
                            param_name
                        )));
                    }
                }
            }
        }

        // Extract return_type before mutable borrow (Copy type, no allocation)
        let return_type = fn_info.return_type;

        // Analyze arguments
        let air_args = self.analyze_call_args(air, &args, ctx)?;

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
                name,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Analyze a method call.
    ///
    /// This is a complex operation that handles both user-defined methods and
    /// builtin methods. The full implementation is in analysis.rs.
    fn analyze_method_call(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        method: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Delegate to the main implementation in analysis.rs
        self.analyze_method_call_impl(air, receiver, method, args_start, args_len, span, ctx)
    }

    /// Analyze an associated function call.
    ///
    /// This is a complex operation. The full implementation is in analysis.rs.
    fn analyze_assoc_fn_call(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Delegate to the main implementation in analysis.rs
        self.analyze_assoc_fn_call_impl(air, type_name, function, args_start, args_len, span, ctx)
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

        match &inst.data {
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                self.analyze_intrinsic(air, inst_ref, *name, *args_start, *args_len, inst.span, ctx)
            }

            InstData::TypeIntrinsic { name, type_arg } => {
                self.analyze_type_intrinsic(air, *name, *type_arg, inst.span)
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

    /// Analyze a type intrinsic (@size_of, @align_of).
    fn analyze_type_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        type_arg: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = self.interner.resolve(&name);
        let ty = self.resolve_type(type_arg, span)?;

        // Calculate the value based on which intrinsic
        let value: u64 = match intrinsic_name {
            "size_of" => {
                // Calculate size in bytes (slot count * 8)
                let slot_count = self.abi_slot_count(ty);
                (slot_count * 8) as u64
            }
            "align_of" => {
                // Zero-sized types have 1-byte alignment, others have 8-byte
                let slot_count = self.abi_slot_count(ty);
                if slot_count == 0 { 1u64 } else { 8u64 }
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

    /// Analyze an intrinsic call.
    ///
    /// This is a complex operation that handles many different intrinsics.
    /// The full implementation is in analysis.rs.
    fn analyze_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Delegate to the main implementation in analysis.rs
        self.analyze_intrinsic_impl(air, inst_ref, name, args_start, args_len, span, ctx)
    }

    // ========================================================================
    // Declaration no-ops: ImplDecl, DropFnDecl, FnDecl
    // ========================================================================

    /// Analyze a declaration that produces Unit in expression context.
    ///
    /// Handles: ImplDecl, DropFnDecl
    pub(crate) fn analyze_decl_noop(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        _ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::ImplDecl { .. } | InstData::DropFnDecl { .. } => {
                // These are processed during collection phase, just return Unit
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
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
