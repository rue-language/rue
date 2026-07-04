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
//! - [`analyze_decl_noop`] - DropFnDecl (declarations that produce Unit)
//!
//! Binary operations (arithmetic, comparison, logical, bitwise) are handled
//! by existing helper methods in `analysis.rs`:
//! - `analyze_binary_arith` - Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
//! - `analyze_comparison` - Eq, Ne, Lt, Gt, Le, Ge
//! - Logical And/Or are simple enough to remain inline

use std::collections::HashMap;

use lasso::Spur;
use rue_error::{
    CompileError, CompileResult, CompileWarning, ErrorKind, MissingFieldsError, OptionExt,
    WarningKind,
};
use rue_rir::{InstData, InstRef, RirArgMode, RirParamMode, RirPattern};

use crate::sema::context::ConstValue;
use rue_span::Span;

use super::Sema;
use super::analysis::move_out_of_inout_error;
use super::context::{AnalysisContext, AnalysisResult, LocalVar, VariableMoveState};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPattern, AirPlaceBase, AirPlaceRef,
    AirProjection, AirRef,
};
use crate::scope::ScopedContext;
use crate::types::{Type, TypeKind};

// ============================================================================
// Place Building (ADR-0030 Phase 8)
// ============================================================================

/// Projection info collected during place tracing.
///
/// This extends `AirProjection` with additional metadata needed for type checking
/// and move analysis.
#[derive(Debug)]
pub(crate) struct ProjectionInfo {
    /// The projection to emit
    pub proj: AirProjection,
    /// The type resulting from this projection
    pub result_type: Type,
    /// For field projections: the field name (for move checking)
    /// For index projections: None
    pub field_name: Option<Spur>,
    /// For index projections whose index is a compile-time constant: its
    /// value (for per-element move tracking, RUE-186). None for field
    /// projections and dynamic indices.
    pub const_index: Option<i64>,
    /// For index projections with a non-negative constant index: the interned
    /// decimal-string path segment for that element (`arr[0]` → `"0"`),
    /// matching the encoding [`super::analysis::index_path_segment`] uses for
    /// whole-element moves. Lets `field_path` nest a projection through a
    /// constant index under the correct element (`arr[0].s` → `["0", "s"]`)
    /// instead of stripping the index (RUE-279). None for field projections,
    /// dynamic indices, and negative constants.
    pub index_segment: Option<Spur>,
}

/// Result of tracing a place expression in RIR.
///
/// This contains all the information needed to build an `AirPlace` and emit
/// a `PlaceRead` or `PlaceWrite` instruction.
#[derive(Debug)]
pub(crate) struct PlaceTrace {
    /// The base of the place (local slot or param slot)
    pub base: AirPlaceBase,
    /// The type of the base (before projections)
    pub base_type: Type,
    /// Projections collected during tracing (in order from base to leaf)
    pub projections: Vec<ProjectionInfo>,
    /// The root variable name (for move checking)
    pub root_var: Spur,
    /// Whether the root is mutable (for write validation)
    pub is_root_mutable: bool,
    /// Whether this is a borrow parameter (for error messages)
    pub is_borrow_param: bool,
}

impl PlaceTrace {
    /// Get the final type of the place (after all projections).
    pub fn result_type(&self) -> Type {
        self.projections
            .last()
            .map(|p| p.result_type)
            .unwrap_or(self.base_type)
    }

    /// Build the field path for move checking (list of path segments).
    ///
    /// Fields contribute their name; an index through a **non-negative
    /// constant** contributes its decimal-string element segment, so a
    /// projection through a constant index nests under the right element
    /// (`arr[0].s` → `["0", "s"]`, matching whole-element moves — RUE-279).
    ///
    /// A **dynamic** (or negative) index cannot be named, so it resets the
    /// path: segments before it are dropped and collection continues after it.
    /// This keeps the old "you can't statically track a partial move through a
    /// runtime index" behavior for that case (the caller falls back to the
    /// conservative whole-array E0904 rejection), while constant indices are
    /// now tracked precisely instead of being conflated (every `arr[K].f`
    /// previously collapsed to `["f"]`, which both hid nested partial moves
    /// and false-rejected sibling-element moves).
    pub fn field_path(&self) -> Vec<Spur> {
        let mut path = Vec::new();
        for p in &self.projections {
            match p.proj {
                AirProjection::Index { .. } => match p.index_segment {
                    Some(seg) => path.push(seg),
                    // Dynamic/negative index: unnameable element — restart.
                    None => path.clear(),
                },
                AirProjection::Field { .. } => {
                    if let Some(name) = p.field_name {
                        path.push(name);
                    }
                }
            }
        }
        path
    }
}

impl<'a> Sema<'a> {
    // ========================================================================
    // Place Tracing (ADR-0030 Phase 8)
    // ========================================================================

    /// Try to trace an RIR expression to a place (lvalue).
    ///
    /// This walks the RIR instruction chain backward from a `FieldGet` or `IndexGet`
    /// to find the root `VarRef` or `ParamRef`, collecting projections along the way.
    ///
    /// Returns `None` if the expression is not a place (e.g., a function call result).
    ///
    /// # Arguments
    /// * `inst_ref` - The RIR instruction to trace
    /// * `air` - The AIR being built (needed to analyze index expressions)
    /// * `ctx` - Analysis context with local/param info
    ///
    /// # Returns
    /// * `Some(PlaceTrace)` if the expression is a place
    /// * `None` if it's not (e.g., `get_struct().field` where base is a call)
    pub(crate) fn try_trace_place(
        &mut self,
        inst_ref: InstRef,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        let trace = self.try_trace_place_inner(inst_ref, air, ctx)?;
        if let Some(trace) = &trace {
            // Any place access through a projection (field read `s.a`, array
            // index `a[i]`, method receiver `h.s.len()`, field/index write
            // `s.a = ...`) uses its root variable. Mark it here, at the single
            // shared tracing point, so the unused-variable lint doesn't fire
            // on variables that are only accessed through projections
            // (RUE-135). Direct assignment to the variable itself (`x = 5`)
            // does not go through place tracing and intentionally still
            // counts as unused.
            ctx.used_locals.insert(trace.root_var);
        }
        Ok(trace)
    }

    /// Inner implementation that accumulates projections.
    fn try_trace_place_inner(
        &mut self,
        inst_ref: InstRef,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            // Base case: local variable reference
            InstData::VarRef { name } => {
                // Locals shadow parameters (spec 5.1:10): a `let` that rebinds a
                // parameter name makes every later reference resolve to the new
                // local, not the parameter (RUE-278). A local with a param's
                // name can only arise by shadowing, so locals always win here.
                if let Some(local) = ctx.locals.get(name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Local(local.slot),
                        base_type: local.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: local.is_mut,
                        is_borrow_param: false,
                    }));
                }

                // Otherwise it may be a parameter.
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type: param_info.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: matches!(param_info.mode, RirParamMode::Inout),
                        is_borrow_param: matches!(param_info.mode, RirParamMode::Borrow),
                    }));
                }

                // Not a variable - might be a constant or type name
                Ok(None)
            }

            // Base case: explicit parameter reference
            InstData::ParamRef { name, .. } => {
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type: param_info.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: matches!(param_info.mode, RirParamMode::Inout),
                        is_borrow_param: matches!(param_info.mode, RirParamMode::Borrow),
                    }));
                }
                Ok(None)
            }

            // Recursive case: field access
            InstData::FieldGet { base, field } => {
                // First, recursively trace the base
                let base_trace = self.try_trace_place_inner(*base, air, ctx)?;

                match base_trace {
                    Some(mut trace) => {
                        // Get the struct type from the base
                        let base_type = trace.result_type();
                        let struct_id = match base_type.as_struct() {
                            Some(id) => id,
                            None => {
                                // Module access or non-struct - not a place
                                return Ok(None);
                            }
                        };

                        // Look up field info
                        let struct_def = self.type_pool.struct_def(struct_id);
                        let field_name_str = self.interner.resolve(field);
                        let (field_index, struct_field) =
                            match struct_def.find_field(field_name_str) {
                                Some(info) => info,
                                None => return Ok(None), // Unknown field
                            };

                        let field_type = struct_field.ty;

                        // Add this projection with field name for move checking
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Field {
                                struct_id,
                                field_index: field_index as u32,
                            },
                            result_type: field_type,
                            field_name: Some(*field),
                            const_index: None,
                            index_segment: None,
                        });

                        Ok(Some(trace))
                    }
                    None => {
                        // Base is not a place (e.g., function call result)
                        Ok(None)
                    }
                }
            }

            // Recursive case: array index
            InstData::IndexGet { base, index } => {
                // First, recursively trace the base
                let base_trace = self.try_trace_place_inner(*base, air, ctx)?;

                match base_trace {
                    Some(mut trace) => {
                        // Get the array type from the base
                        let base_type = trace.result_type();
                        let (_array_type_id, elem_type) = match base_type.as_array() {
                            Some(id) => {
                                let (elem, _len) = self.type_pool.array_def(id);
                                (id, elem)
                            }
                            None => return Ok(None), // Not an array
                        };

                        // Analyze the index expression to get an AirRef. The
                        // index is an rvalue of its own, not part of the
                        // place: a surrounding by-ref argument's borrow
                        // (`byref_arg_root`, RUE-143) must not leak into it
                        // (in `f(inout a[take(x)])` the call `take(x)` moves
                        // x normally, even if x happens to be the root).
                        let saved_byref_root = ctx.byref_arg_root.take();
                        let index_result = self.analyze_inst(air, *index, ctx);
                        ctx.byref_arg_root = saved_byref_root;
                        let index_result = index_result?;

                        // Add this projection (no field name for indices).
                        // A non-negative constant index also gets its interned
                        // element path segment so field_path can nest through
                        // it (RUE-279); negative/dynamic indices leave it None.
                        let const_index = self.try_get_const_index(*index);
                        let index_segment = match const_index {
                            Some(k) if k >= 0 => {
                                Some(super::analysis::index_path_segment(self.interner, k as u64))
                            }
                            _ => None,
                        };
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Index {
                                array_type: base_type,
                                index: index_result.air_ref,
                            },
                            result_type: elem_type,
                            field_name: None,
                            const_index,
                            index_segment,
                        });

                        Ok(Some(trace))
                    }
                    None => {
                        // Base is not a place
                        Ok(None)
                    }
                }
            }

            // Not a place expression
            _ => Ok(None),
        }
    }

    /// Build an AirPlaceRef from a PlaceTrace, adding projections to the Air.
    pub(crate) fn build_place_ref(air: &mut Air, trace: &PlaceTrace) -> AirPlaceRef {
        let projs = trace.projections.iter().map(|p| p.proj);
        air.make_place(trace.base, projs)
    }

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
                let want_str = ctx.expected_type.is_some_and(|ty| self.is_str_struct(ty));
                let ty = if want_str {
                    ctx.expected_type.unwrap()
                } else {
                    self.builtin_string_type()
                };
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

            InstData::InfiniteLoop { body, iter_borrow } => {
                self.analyze_infinite_loop(air, *body, *iter_borrow, inst.span, ctx)
            }

            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => self.analyze_match(air, *scrutinee, *arms_start, *arms_len, inst.span, ctx),

            InstData::Try { operand } => self.analyze_try(air, *operand, inst.span, ctx),

            InstData::Break { value } => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::BreakOutsideLoop, inst.span));
                }

                // Break does not carry a value (spec 4.8:21)
                if value.is_some() {
                    return Err(CompileError::new(ErrorKind::BreakWithValue, inst.span));
                }

                // Record the break against the innermost enclosing loop, so
                // the loop can be typed `()` instead of `!` (spec 4.8:17).
                if let Some(broke) = ctx.loop_break_stack.last_mut() {
                    *broke = true;
                }

                // Break has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Break,
                    ty: Type::NEVER,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::NEVER))
            }

            InstData::Continue => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
                }

                // Continue has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Continue,
                    ty: Type::NEVER,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::NEVER))
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
        // Comptime-known branch selection (RUE-166, spec 4.14:17): inside a
        // body with comptime value parameters in scope (a value-specialized
        // function or an anonymous-struct method capturing comptime values),
        // an `if` whose condition is compile-time evaluable selects its
        // branch during analysis — only the taken branch is analyzed and
        // emitted. This is what lets comptime recursion terminate: in
        // `fact(comptime n: i32)`, the body specialized for n == 1 must not
        // analyze the `fact(n - 1)` call in the dead else-branch, or
        // specialization would recurse until the depth cap.
        if !ctx.comptime_value_vars.is_empty() {
            if let Some(ConstValue::Bool(taken)) = self.try_evaluate_const_in_fn(cond, ctx) {
                let taken_block = if taken { Some(then_block) } else { else_block };
                return match taken_block {
                    Some(block) => {
                        ctx.push_scope();
                        let result = self.analyze_inst(air, block, ctx)?;
                        ctx.pop_scope();
                        // An `if` without `else` is unit-typed, so its (taken)
                        // then-branch must still be unit (spec 4.6:5).
                        if else_block.is_none()
                            && result.ty != Type::UNIT
                            && !result.ty.is_never()
                            && !result.ty.is_error()
                        {
                            return Err(CompileError::new(
                                ErrorKind::TypeMismatch {
                                    expected: "()".to_string(),
                                    found: result.ty.safe_name_with_pool(Some(&self.type_pool)),
                                },
                                self.rir.get(block).span,
                            )
                            .with_help(
                                "if expressions without else must have unit type; \
                                 consider adding an else branch or making the body return ()",
                            ));
                        }
                        Ok(result)
                    }
                    // `if false { ... }` with no else: nothing runs; the
                    // expression is unit.
                    None => {
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
                            ty: Type::UNIT,
                            span,
                        });
                        Ok(AnalysisResult::new(air_ref, Type::UNIT))
                    }
                };
            }
        }

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
                (true, true) => Type::NEVER,
                (true, false) => else_type,
                (false, true) => then_type,
                (false, false) => {
                    // Neither diverges - types must match exactly
                    if then_type != else_type && !then_type.is_error() && !else_type.is_error() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: then_type.safe_name_with_pool(Some(&self.type_pool)),
                                found: else_type.safe_name_with_pool(Some(&self.type_pool)),
                            },
                            else_span,
                        )
                        .with_label(
                            format!(
                                "this is of type `{}`",
                                then_type.safe_name_with_pool(Some(&self.type_pool))
                            ),
                            then_span,
                        )
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
            if then_type != Type::UNIT && !then_type.is_never() && !then_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "()".to_string(),
                        found: then_type.safe_name_with_pool(Some(&self.type_pool)),
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
                ty: Type::UNIT,
                span,
            });
            Ok(AnalysisResult::new(air_ref, Type::UNIT))
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
        // Snapshot move state before the loop: the condition and body
        // re-execute on every iteration, so a value moved in either is already
        // moved when the back edge re-enters the loop (see the recheck below).
        let moves_before_loop = ctx.moved_vars.clone();

        // While loop: condition must be bool, result is Unit
        let cond_result = self.analyze_inst(air, cond, ctx)?;

        // Analyze body with its own scope. The loop_break_stack entry makes
        // breaks inside the body target this while loop, not an outer loop;
        // the flag itself is unused because a while loop is always `()`.
        ctx.push_scope();
        ctx.loop_depth += 1;
        ctx.loop_break_stack.push(false);
        let body_result = self.analyze_inst(air, body, ctx)?;
        ctx.loop_break_stack.pop();
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        // A while loop discards its body's result value on every iteration;
        // discarding a value that carries a linear value would implicitly
        // drop it (RUE-176).
        self.reject_discarded_linear_value(body_result.ty, body)?;

        // Loop back-edge move check: if the loop changed any move state,
        // re-run the analysis once with the post-body state as the starting
        // state. Any use of a value moved by a previous iteration then errors.
        // The scratch Air and context are discarded - this pass exists only
        // for the checks.
        if !ctx.in_loop_move_recheck && ctx.moved_vars != moves_before_loop {
            let mut scratch_air = air.clone();
            let mut scratch_ctx = ctx.fork_for_loop_recheck();
            (|| -> CompileResult<()> {
                self.analyze_inst(&mut scratch_air, cond, &mut scratch_ctx)?;
                scratch_ctx.push_scope();
                scratch_ctx.loop_depth += 1;
                scratch_ctx.loop_break_stack.push(false);
                self.analyze_inst(&mut scratch_air, body, &mut scratch_ctx)?;
                scratch_ctx.loop_break_stack.pop();
                scratch_ctx.loop_depth -= 1;
                scratch_ctx.pop_scope();
                Ok(())
            })()
            .map_err(|e| e.with_note("value was moved in a previous iteration of the loop"))?;
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Loop {
                cond: cond_result.air_ref,
                body: body_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze an infinite loop.
    fn analyze_infinite_loop(
        &mut self,
        air: &mut Air,
        body: InstRef,
        iter_borrow: Option<Spur>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Infinite loop: `loop { body }` - type `()` if the body contains a
        // break targeting this loop (the loop can exit), `!` otherwise
        // (spec 4.8:17 / 4.8:21).

        // Snapshot move state before the body for the back-edge recheck below.
        let moves_before_loop = ctx.moved_vars.clone();

        ctx.push_scope();
        ctx.loop_depth += 1;
        ctx.loop_break_stack.push(false);
        // A `for` over a named variable borrows it (shared) for the body's
        // duration (spec 4.8:26, RUE-233): record the borrow so a mutation of
        // the iterated collection inside the body is rejected (E0428).
        if let Some(var) = iter_borrow {
            ctx.iter_borrows.push(var);
        }
        let body_result = self.analyze_inst(air, body, ctx)?;
        if iter_borrow.is_some() {
            ctx.iter_borrows.pop();
        }
        let has_break = ctx.loop_break_stack.pop().unwrap_or(false);
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        // The loop discards its body's result value on every iteration;
        // discarding a value that carries a linear value would implicitly
        // drop it (RUE-176).
        self.reject_discarded_linear_value(body_result.ty, body)?;

        // Loop back-edge move check (see analyze_while_loop for details).
        // Note: like Rust, this is conservative - a body that unconditionally
        // breaks after the move still errors.
        if !ctx.in_loop_move_recheck && ctx.moved_vars != moves_before_loop {
            let mut scratch_air = air.clone();
            let mut scratch_ctx = ctx.fork_for_loop_recheck();
            scratch_ctx.push_scope();
            scratch_ctx.loop_depth += 1;
            scratch_ctx.loop_break_stack.push(false);
            if let Some(var) = iter_borrow {
                scratch_ctx.iter_borrows.push(var);
            }
            self.analyze_inst(&mut scratch_air, body, &mut scratch_ctx)
                .map_err(|e| e.with_note("value was moved in a previous iteration of the loop"))?;
            if iter_borrow.is_some() {
                scratch_ctx.iter_borrows.pop();
            }
            scratch_ctx.loop_break_stack.pop();
            scratch_ctx.loop_depth -= 1;
            scratch_ctx.pop_scope();
        }

        let loop_ty = if has_break { Type::UNIT } else { Type::NEVER };
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::InfiniteLoop {
                body: body_result.air_ref,
            },
            ty: loop_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, loop_ty))
    }

    /// Validate an integer pattern literal against the scrutinee type and
    /// return the value it compares as at runtime (the scrutinee-typed value,
    /// held as an i64 bit pattern).
    ///
    /// Mirrors the `let`-binding literal checks (RUE-74): out-of-range
    /// literals are E0800 (`LiteralOutOfRange`) and negative literals on
    /// unsigned scrutinees are E0801 (`CannotNegate`) instead of
    /// silently wrapping into a different (or unmatchable) value.
    fn check_pattern_int(
        &self,
        value: u64,
        negative: bool,
        scrutinee_type: Type,
        span: Span,
    ) -> CompileResult<i64> {
        let ty_name = scrutinee_type.safe_name_with_pool(Some(&self.type_pool));
        if negative {
            if scrutinee_type.is_unsigned() {
                return Err(
                    CompileError::new(ErrorKind::CannotNegate(ty_name), span).with_note(
                        "unsigned values are never negative, so this pattern could never match",
                    ),
                );
            }
            if !scrutinee_type.negated_literal_fits(value) {
                return Err(CompileError::new(
                    ErrorKind::LiteralOutOfRange { value, ty: ty_name },
                    span,
                )
                .with_note(format!("the pattern value is -{}", value)));
            }
            // wrapping_neg handles the i64::MIN magnitude (9223372036854775808).
            Ok((value as i64).wrapping_neg())
        } else {
            if !scrutinee_type.literal_fits(value) {
                return Err(CompileError::new(
                    ErrorKind::LiteralOutOfRange { value, ty: ty_name },
                    span,
                ));
            }
            Ok(value as i64)
        }
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
        // Comptime-known arm selection (RUE-191, spec 4.14:19): inside a body
        // with comptime value parameters in scope, a `match` whose scrutinee
        // is compile-time evaluable selects its arm during analysis — only
        // the matching arm's body is analyzed and emitted, exactly like
        // comptime-known `if` conditions (analyze_branch above). This is what
        // lets comptime recursion written with `match` terminate instead of
        // hitting the specialization depth cap, and keeps statically-dead
        // arms from being analyzed (they may only be legal for other
        // specializations). Rue match arms have no guards, so whether a
        // pattern matches is decidable from the pattern alone. Any
        // pattern/value shape this selection doesn't understand (enum
        // patterns, mismatched pattern types) falls back to analyzing all
        // arms, as does a comptime value no arm matches (the normal path
        // then reports non-exhaustiveness).
        if !ctx.comptime_value_vars.is_empty() {
            if let Some(value) = self.try_evaluate_const_in_fn(scrutinee, ctx) {
                let arms = self.rir.get_match_arms(arms_start, arms_len);
                let mut selected: Option<InstRef> = None;
                let mut prunable = !arms.is_empty();
                let mut has_wildcard = false;
                let mut bool_true_covered = false;
                let mut bool_false_covered = false;
                for (pattern, body) in &arms {
                    let matched = match (pattern, &value) {
                        (RirPattern::Wildcard(_), _) => {
                            has_wildcard = true;
                            true
                        }
                        (
                            RirPattern::Int {
                                value: magnitude,
                                negative,
                                ..
                            },
                            ConstValue::Integer(n),
                        ) => {
                            let pat = if *negative {
                                -(*magnitude as i128)
                            } else {
                                *magnitude as i128
                            };
                            pat == *n
                        }
                        (RirPattern::Bool(b, _), ConstValue::Bool(v)) => {
                            if *b {
                                bool_true_covered = true;
                            } else {
                                bool_false_covered = true;
                            }
                            b == v
                        }
                        _ => {
                            prunable = false;
                            break;
                        }
                    };
                    if matched && selected.is_none() {
                        selected = Some(*body);
                    }
                }
                // Exhaustiveness is a property of the pattern set, not of
                // the arm bodies, so it stays checked even when the match
                // value is comptime-known (spec 4.7:9): a wildcard, or
                // both bool values for a bool scrutinee. A non-exhaustive
                // match falls through to the normal path for the proper
                // diagnostic (which also covers "no arm matched").
                let exhaustive = has_wildcard
                    || (matches!(value, ConstValue::Bool(_))
                        && bool_true_covered
                        && bool_false_covered);
                if prunable && exhaustive {
                    // Pattern *legality* is independent of arm *selection*.
                    // Spec 4.14:19 exempts only the analysis of unselected arm
                    // *bodies* (and reaffirms exhaustiveness) — it does NOT
                    // exempt the per-pattern legality rules of 4.7. So before
                    // pruning we still range-check every integer pattern
                    // against the scrutinee's declared type, exactly as the
                    // normal path below does via check_pattern_int: E0800 for
                    // an out-of-range literal (4.7:23) and E0801 for a negative
                    // pattern on an unsigned scrutinee (4.7:24). The comptime
                    // value substituted for the scrutinee mistypes as i32 at
                    // AIR emission (a known limitation), so we take the
                    // scrutinee's true type from Hindley-Milner inference
                    // (RUE-215).
                    let scrutinee_type =
                        Self::get_resolved_type(ctx, scrutinee, span, "match scrutinee")?;
                    if scrutinee_type.is_integer() {
                        for (pattern, _) in &arms {
                            if let RirPattern::Int {
                                value: magnitude,
                                negative,
                                ..
                            } = pattern
                            {
                                self.check_pattern_int(
                                    *magnitude,
                                    *negative,
                                    scrutinee_type,
                                    pattern.span(),
                                )?;
                            }
                        }
                    }
                    if let Some(body) = selected {
                        ctx.push_scope();
                        let result = self.analyze_inst(air, body, ctx)?;
                        ctx.pop_scope();
                        return Ok(result);
                    }
                }
            }
        }

        // Derive the expected scrutinee type from the arm patterns, so a
        // fallible-intrinsic scrutinee learns which in-scope `Option(T)` to
        // return (`match @read_line() { Option::Some(l) => .., Option::None =>
        // .. }`, RUE-6). Only sema resolves the comptime-generic `Option`
        // alias the pattern names, so we resolve the first enum pattern's type
        // here and expose it while the scrutinee is analyzed. Resolution errors
        // are ignored — pattern legality is checked on the normal path below.
        let arms_for_expected = self.rir.get_match_arms(arms_start, arms_len);
        let expected_scrutinee = arms_for_expected.iter().find_map(|(pattern, _)| {
            if let RirPattern::Path { type_name, .. } = pattern {
                self.resolve_type_with_ctx(*type_name, span, ctx)
                    .ok()
                    .filter(|ty| ty.is_enum())
            } else {
                None
            }
        });
        let prev_expected = ctx.expected_type.take();
        ctx.expected_type = expected_scrutinee;

        // Analyze the scrutinee to determine its type
        let scrutinee_outcome = self.analyze_inst(air, scrutinee, ctx);
        ctx.expected_type = prev_expected;
        let scrutinee_result = scrutinee_outcome?;
        let scrutinee_type = scrutinee_result.ty;

        // Validate that we can match on this type (integers, booleans, and enums)
        if !scrutinee_type.is_integer() && scrutinee_type != Type::BOOL && !scrutinee_type.is_enum()
        {
            return Err(CompileError::new(
                ErrorKind::InvalidMatchType(
                    scrutinee_type.safe_name_with_pool(Some(&self.type_pool)),
                ),
                span,
            ));
        }

        let arms = self.rir.get_match_arms(arms_start, arms_len);
        // An empty match is only legal on a zero-variant (uninhabited) enum,
        // where zero arms vacuously satisfy exhaustiveness because the type
        // has no values (spec 4.7:26, RUE-169). The match can never be
        // reached with a value, so its type is `!` (spec 4.7:27).
        if arms.is_empty() {
            let is_uninhabited_enum = match scrutinee_type.try_kind() {
                Some(TypeKind::Enum(id)) => self.type_pool.enum_def(id).variant_count() == 0,
                _ => false,
            };
            if !is_uninhabited_enum {
                return Err(CompileError::new(ErrorKind::EmptyMatch, span));
            }
            let arms_start = air.add_extra(&[]);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Match {
                    scrutinee: scrutinee_result.air_ref,
                    arms_start,
                    arms_len: 0,
                },
                ty: Type::NEVER,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::NEVER));
        }

        // Track patterns for exhaustiveness checking and duplicate detection
        let mut wildcard_span: Option<Span> = None;
        let mut bool_true_span: Option<Span> = None;
        let mut bool_false_span: Option<Span> = None;
        let mut seen_ints: HashMap<i64, Span> = HashMap::new();
        // Maps each covered enum-variant index to the span of its first arm, so a
        // second arm matching the same variant can be reported as unreachable
        // (mirroring seen_ints / bool_*_span). The map's len() still drives the
        // exhaustiveness check below, identically to the former HashSet.
        let mut covered_variants: HashMap<u32, Span> = HashMap::new();
        let mut pattern_enum_id: Option<crate::types::EnumId> = None;

        // Analyze each arm (each arm gets its own scope)
        let mut air_arms = Vec::new();
        let mut result_type: Option<Type> = None;

        // Move state before any arm runs (after the scrutinee, whose moves
        // happen on every path). Arms are alternatives, not a sequence:
        // each is analyzed from this state and the per-arm results are
        // merged after the loop (see merge_arm_moves).
        let moves_before_arms = ctx.moved_vars.clone();
        let mut arm_move_states = Vec::with_capacity(arms.len());

        for (pattern, body) in arms.iter() {
            let pattern_span = pattern.span();

            // If we've seen a wildcard, everything after is unreachable
            if let Some(first_wildcard_span) = wildcard_span {
                let pat_str = match pattern {
                    RirPattern::Wildcard(_) => "_".to_string(),
                    RirPattern::Int {
                        value, negative, ..
                    } => {
                        if *negative {
                            format!("-{}", value)
                        } else {
                            value.to_string()
                        }
                    }
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
                    // A `_` arm after the preceding arms already cover every
                    // value (both bools, or every enum variant) is unreachable
                    // (spec 4.7:17 / 4.7:20, RUE-168).
                    if wildcard_span.is_none() {
                        let fully_covered = if scrutinee_type == Type::BOOL {
                            bool_true_span.is_some() && bool_false_span.is_some()
                        } else if let Some(enum_id) = pattern_enum_id {
                            covered_variants.len()
                                == self.type_pool.enum_def(enum_id).variant_count()
                        } else {
                            false
                        };
                        if fully_covered {
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern("_".to_string()),
                                    pattern_span,
                                )
                                .with_note(
                                    "this pattern will never be matched because the arms above already cover every possible value",
                                ),
                            );
                        }
                        wildcard_span = Some(pattern_span);
                    }
                }
                RirPattern::Int {
                    value, negative, ..
                } => {
                    if !scrutinee_type.is_integer() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.safe_name_with_pool(Some(&self.type_pool)),
                                found: "integer".to_string(),
                            },
                            pattern_span,
                        ));
                    }
                    // Range-check the literal against the scrutinee type
                    // (E0800/E0801, like `let` bindings) and get the value it
                    // compares as at runtime. Previously the literal wrapped to
                    // i64 untyped, so e.g. `4294967296` on a u32 scrutinee
                    // truncated and matched 0 (RUE-74).
                    let n =
                        self.check_pattern_int(*value, *negative, scrutinee_type, pattern_span)?;
                    // Check for duplicate integer pattern
                    if let Some(first_span) = seen_ints.get(&n) {
                        if wildcard_span.is_none() {
                            let pat_str = if *negative {
                                format!("-{}", value)
                            } else {
                                value.to_string()
                            };
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern(pat_str),
                                    pattern_span,
                                )
                                .with_label("first occurrence of this pattern", *first_span)
                                .with_note(
                                    "this pattern will never be matched because an earlier arm already matches the same value",
                                ),
                            );
                        }
                    } else {
                        seen_ints.insert(n, pattern_span);
                    }
                }
                RirPattern::Bool(b, _) => {
                    if scrutinee_type != Type::BOOL {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.safe_name_with_pool(Some(&self.type_pool)),
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
                    module,
                    type_name,
                    variant,
                    ..
                } => {
                    // Look up the enum type, potentially through a module
                    let enum_id = if let Some(module_ref) = module {
                        // Qualified access: module.EnumName::Variant
                        self.resolve_enum_through_module(*module_ref, *type_name, pattern_span)?
                    } else {
                        // Unqualified access: EnumName::Variant, or the generic
                        // form `O::Some(..)` where `O` is a comptime type
                        // variable bound to `Option(i32)` (RUE-6 phase 2).
                        let (enum_id, via_comptime) = self
                            .resolve_enum_type_name(*type_name, ctx)
                            .ok_or_compile_error(
                                ErrorKind::UnknownEnumType(
                                    self.interner.resolve(&*type_name).to_string(),
                                ),
                                pattern_span,
                            )?;
                        // Privacy (E0460, RUE-185): a match pattern names the
                        // enum unqualified, so a private enum from another
                        // directory cannot be matched on — privacy is uniform
                        // across item kinds (spec 10.3:1, 10.3:7). The
                        // module-qualified branch above does its own check
                        // (E0706). The pattern-to-AIR conversion later in
                        // this loop re-resolves the same name but runs only
                        // after this check has passed. A comptime-bound enum is
                        // exempt (the type arrived through a binding).
                        if !via_comptime {
                            let def = self.type_pool.enum_def(enum_id);
                            self.check_unqualified_visibility(
                                "enum",
                                self.interner.resolve(&*type_name),
                                def.file_id,
                                def.is_pub,
                                pattern_span,
                            )?;
                        }
                        enum_id
                    };
                    let enum_def = self.type_pool.enum_def(enum_id);

                    // Check that scrutinee type matches the pattern's enum type
                    if scrutinee_type != Type::new_enum(enum_id) {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.safe_name_with_pool(Some(&self.type_pool)),
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

                    pattern_enum_id = Some(enum_id);

                    // Check for duplicate enum-variant pattern (mirrors the integer
                    // and boolean duplicate checks above).
                    if let Some(first_span) = covered_variants.get(&(variant_index as u32)) {
                        if wildcard_span.is_none() {
                            let pat_str = format!(
                                "{}::{}",
                                self.interner.resolve(&*type_name),
                                self.interner.resolve(&*variant)
                            );
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern(pat_str),
                                    pattern_span,
                                )
                                .with_label("first occurrence of this pattern", *first_span)
                                .with_note(
                                    "this pattern will never be matched because an earlier arm already matches the same value",
                                ),
                            );
                        }
                    } else {
                        covered_variants.insert(variant_index as u32, pattern_span);
                    }
                }
            }

            // Each arm gets its own scope and starts from the pre-match
            // move state (only one arm executes at runtime).
            ctx.moved_vars = moves_before_arms.clone();
            ctx.push_scope();

            // Materialize tuple-variant payload bindings (RUE-221) into fresh
            // locals before the body, so the body's references resolve to them.
            // The enclosing match dispatched on the discriminant, so in this
            // arm the payload is read (move mode) via `EnumPayloadGet`.
            let mut binding_stmts =
                self.materialize_match_bindings(air, pattern, scrutinee_result.air_ref, ctx)?;

            // RUE-238: a non-binding arm (a wildcard `_`, or a variant matched
            // without binding its payload) still *consumes* the scrutinee — the
            // match marked it moved (see the `mark_moved` in the emitted AIR) —
            // but extracts nothing, so its active-variant payload would leak.
            // Emit a drop of the whole scrutinee value; for an enum this lowers
            // to the variant-dispatched drop glue (`__rue_drop_E`), which drops
            // exactly the active variant's payload (a no-op when that variant
            // carries nothing droppable). Arms that DO bind the payload move it
            // into their binding locals — dropped when those go out of scope —
            // so they must not also drop the scrutinee, or the payload would be
            // dropped twice; `binding_stmts.is_empty()` is precisely that guard
            // (Rue variant patterns bind either all payload fields or none).
            if binding_stmts.is_empty() && scrutinee_type.is_enum() {
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop {
                        value: scrutinee_result.air_ref,
                    },
                    ty: Type::UNIT,
                    span: pattern_span,
                });
                binding_stmts.push(drop_ref.as_u32());
            }

            // Analyze arm body
            let body_result = self.analyze_inst(air, *body, ctx)?;
            let body_type = body_result.ty;

            ctx.pop_scope();
            arm_move_states.push((std::mem::take(&mut ctx.moved_vars), body_type.is_never()));

            // Update result type (handle Never type coercion)
            result_type = Some(match result_type {
                None => body_type,
                Some(prev) => {
                    if prev.is_never() {
                        body_type
                    } else if body_type.is_never() {
                        prev
                    } else if prev != body_type && !prev.is_error() && !body_type.is_error() {
                        // Point at the offending arm's body, not the whole match.
                        return Err(self.type_mismatch_error(
                            prev,
                            body_type,
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
                RirPattern::Int {
                    value, negative, ..
                } => {
                    // Already range-checked above; wrapping_neg handles the
                    // i64::MIN magnitude (9223372036854775808).
                    let n = if *negative {
                        (*value as i64).wrapping_neg()
                    } else {
                        *value as i64
                    };
                    AirPattern::Int(n)
                }
                RirPattern::Bool(b, _) => AirPattern::Bool(*b),
                RirPattern::Path {
                    module,
                    type_name,
                    variant,
                    ..
                } => {
                    let type_name_str = self.interner.resolve(&*type_name).to_string();
                    let enum_id = if let Some(module_ref) = module {
                        self.resolve_enum_through_module(*module_ref, *type_name, pattern_span)?
                    } else {
                        self.resolve_enum_type_name(*type_name, ctx)
                            .map(|(id, _)| id)
                            .ok_or_else(|| {
                                CompileError::new(
                                    ErrorKind::InternalError(format!(
                                        "enum type '{}' not found during pattern conversion",
                                        type_name_str
                                    )),
                                    pattern_span,
                                )
                            })?
                    };
                    let enum_def = self.type_pool.enum_def(enum_id);
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

            // If the pattern bound payload data, wrap the body so the binding
            // Alloc statements run before it (RUE-221).
            let arm_body_ref = if binding_stmts.is_empty() {
                body_result.air_ref
            } else {
                let stmts_start = air.add_extra(&binding_stmts);
                air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start,
                        stmts_len: binding_stmts.len() as u32,
                        value: body_result.air_ref,
                    },
                    ty: body_type,
                    span: pattern_span,
                })
            };

            air_arms.push((air_pattern, arm_body_ref));
        }

        // Join the arms' move states (union of non-diverging arms;
        // `full_move_on_all_paths` intersects for the linear must-consume
        // check). Matches are exhaustive, so the arms cover every path.
        ctx.merge_arm_moves(arm_move_states);

        // Exhaustiveness checking
        let has_wildcard = wildcard_span.is_some();
        let bool_true_covered = bool_true_span.is_some();
        let bool_false_covered = bool_false_span.is_some();
        let is_exhaustive = if scrutinee_type == Type::BOOL {
            has_wildcard || (bool_true_covered && bool_false_covered)
        } else if let Some(enum_id) = pattern_enum_id {
            let enum_def = self.type_pool.enum_def(enum_id);
            has_wildcard || covered_variants.len() == enum_def.variant_count()
        } else {
            // For integers, must have wildcard
            has_wildcard
        };

        if !is_exhaustive {
            // Name what's missing: the enum definition is in scope here, so list
            // the uncovered variants instead of just "not exhaustive" (RUE-133).
            let enum_def = pattern_enum_id
                .or_else(|| match scrutinee_type.try_kind() {
                    Some(TypeKind::Enum(id)) => Some(id),
                    _ => None,
                })
                .map(|id| self.type_pool.enum_def(id));
            return Err(super::analysis::non_exhaustive_match_error(
                span,
                scrutinee_type,
                enum_def.as_ref(),
                |i| covered_variants.contains_key(&i),
                bool_true_covered,
                bool_false_covered,
            ));
        }

        let final_type = result_type.unwrap_or(Type::UNIT);

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

    /// If `enum_id` names an `Option`-shaped enum — exactly two variants, a
    /// single-payload `Some(T)` and an empty `None` — return
    /// `(some_index, none_index, payload_type)`. Used by the `?` operator to
    /// recognise the in-scope library `Option` by structure and name
    /// (RUE-6, ADR-0038), rather than as a privileged builtin.
    fn option_enum_shape(&self, enum_id: crate::types::EnumId) -> Option<(u32, u32, Type)> {
        let def = self.type_pool.enum_def(enum_id);
        if def.variant_count() != 2 {
            return None;
        }
        let some_idx = def.find_variant("Some")?;
        let none_idx = def.find_variant("None")?;
        let some_payload = def.variant_payload(some_idx);
        let none_payload = def.variant_payload(none_idx);
        if some_payload.len() == 1 && none_payload.is_empty() {
            Some((some_idx as u32, none_idx as u32, some_payload[0]))
        } else {
            None
        }
    }

    /// Analyze the `?` operator (RUE-6, ADR-0038).
    ///
    /// `operand?` requires `operand` to be an `Option(T)` and the enclosing
    /// function to return an `Option(U)`. It evaluates to `T` on `Some(v)` and
    /// early-returns the enclosing function's `None` on `None`. This is the
    /// desugaring `match operand { Some(v) => v, None => return None }`, built
    /// directly against the resolved enum types (so no source type name is
    /// needed): a two-arm discriminant `Match` whose `None` arm returns.
    fn analyze_try(
        &mut self,
        air: &mut Air,
        operand: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // The enclosing function must return an `Option`; `?` propagates its
        // `None`. Resolve that shape first so a clear error fires even when the
        // operand is fine (E0503).
        let return_type = ctx.return_type;
        let ret_shape = return_type.as_enum().and_then(|rid| {
            self.option_enum_shape(rid)
                .map(|(_, none_idx, _)| (rid, none_idx))
        });
        let (ret_enum_id, ret_none_idx) = match ret_shape {
            Some(s) => s,
            None => {
                // Still analyze the operand so unrelated errors inside it are
                // reported, but the `?` itself is the diagnostic.
                if return_type.is_error() {
                    let r = self.analyze_inst(air, operand, ctx)?;
                    return Ok(AnalysisResult::new(r.air_ref, Type::ERROR));
                }
                return Err(CompileError::new(
                    ErrorKind::QuestionOutsideOptionFn {
                        return_type: return_type.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }
        };

        // Analyze the operand.
        //
        // `?` works on any typed `Option` value (a function result, constructor,
        // variable, etc.), whose type is already known. A BARE fallible-intrinsic
        // operand — `@read_line()?` / `@parse_i64(s)?` — is special: the intrinsic
        // needs its exact `Option` return type (e.g. `Option(String)`), and the
        // `?` site cannot supply it as an `expected_type` — the enclosing fn's
        // `Option(U)` has the wrong payload (RUE-318). So we clear `expected_type`
        // and set `try_operand`, telling a fallible intrinsic to instantiate its
        // OWN fixed `Option(payload)` (it knows its payload). A non-intrinsic
        // operand ignores both flags — its type is resolved independently.
        let prev_expected = ctx.expected_type.take();
        let prev_try_operand = ctx.try_operand;
        ctx.try_operand = true;
        let operand_outcome = self.analyze_inst(air, operand, ctx);
        ctx.expected_type = prev_expected;
        ctx.try_operand = prev_try_operand;
        let operand_result = operand_outcome?;
        let operand_ty = operand_result.ty;

        if operand_ty.is_error() {
            return Ok(AnalysisResult::new(operand_result.air_ref, Type::ERROR));
        }

        // The operand must be an `Option`-shaped enum (E0504).
        let (some_idx, none_idx, payload_ty) = match operand_ty
            .as_enum()
            .and_then(|oid| self.option_enum_shape(oid).map(|s| (oid, s)))
        {
            Some((_oid, shape)) => shape,
            None => {
                return Err(CompileError::new(
                    ErrorKind::QuestionOnNonOption {
                        found: operand_ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }
        };
        let operand_enum_id = operand_ty
            .as_enum()
            .expect("operand is an Option-shaped enum");

        // Some(v) arm: read the payload out of the scrutinee; that value is the
        // arm's (and the whole `?`-expression's) result. Mirrors the payload
        // read that a `Some(v) => v` match arm performs (RUE-221).
        let some_body = air.add_inst(AirInst {
            data: AirInstData::EnumPayloadGet {
                base: operand_result.air_ref,
                enum_id: operand_enum_id,
                variant_index: some_idx,
                field_index: 0,
            },
            ty: payload_ty,
            span,
        });

        // None arm: drop the scrutinee (a non-binding arm consumes it; the
        // active `None` variant carries nothing, so the drop glue is a no-op),
        // then `return` the enclosing function's `None`.
        let drop_scrutinee = air.add_inst(AirInst {
            data: AirInstData::Drop {
                value: operand_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        let none_ctor = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: ret_enum_id,
                variant_index: ret_none_idx,
                payload_start: 0,
                payload_len: 0,
            },
            ty: return_type,
            span,
        });
        let ret = air.add_inst(AirInst {
            data: AirInstData::Ret(Some(none_ctor)),
            ty: Type::NEVER,
            span,
        });
        let none_stmts = air.add_extra(&[drop_scrutinee.as_u32()]);
        let none_body = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: none_stmts,
                stmts_len: 1,
                value: ret,
            },
            ty: Type::NEVER,
            span,
        });

        // Encode the two arms and emit the dispatching match. Its value type is
        // the `Some` payload (the `None` arm diverges).
        let air_arms = [
            (
                AirPattern::EnumVariant {
                    enum_id: operand_enum_id,
                    variant_index: some_idx,
                },
                some_body,
            ),
            (
                AirPattern::EnumVariant {
                    enum_id: operand_enum_id,
                    variant_index: none_idx,
                },
                none_body,
            ),
        ];
        let mut extra_data = Vec::new();
        for (pattern, body) in &air_arms {
            pattern.encode(*body, &mut extra_data);
        }
        let arms_start = air.add_extra(&extra_data);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Match {
                scrutinee: operand_result.air_ref,
                arms_start,
                arms_len: air_arms.len() as u32,
            },
            ty: payload_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, payload_ty))
    }

    /// Materialize the payload bindings of a tuple-variant match pattern
    /// (`Circle(r)`) into fresh locals in the current (arm) scope, returning
    /// the AIR statement refs (StorageLive + Alloc per binding) that must run
    /// before the arm body (RUE-221, ADR-0038).
    ///
    /// Returns an empty vector for patterns without payload bindings. Assumes
    /// the pattern has already been validated against the scrutinee type by
    /// the caller (`analyze_match`); re-resolves the enum for convenience.
    fn materialize_match_bindings(
        &mut self,
        air: &mut Air,
        pattern: &RirPattern,
        scrutinee_ref: AirRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<u32>> {
        let RirPattern::Path {
            module,
            type_name,
            variant,
            bindings,
            span,
        } = pattern
        else {
            return Ok(Vec::new());
        };
        if bindings.is_empty() {
            return Ok(Vec::new());
        }
        let pattern_span = *span;

        let enum_id = if let Some(module_ref) = module {
            self.resolve_enum_through_module(*module_ref, *type_name, pattern_span)?
        } else {
            self.resolve_enum_type_name(*type_name, ctx)
                .map(|(id, _)| id)
                .ok_or_compile_error(
                    ErrorKind::UnknownEnumType(self.interner.resolve(&*type_name).to_string()),
                    pattern_span,
                )?
        };
        let def = self.type_pool.enum_def(enum_id);
        let variant_name = self.interner.resolve(&*variant).to_string();
        let variant_index = def.find_variant(&variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: def.name.clone(),
                variant_name: variant_name.clone(),
            },
            pattern_span,
        )? as u32;
        let payload = def.variant_payload(variant_index as usize).to_vec();

        // The number of bindings must equal the variant's payload arity.
        if bindings.len() != payload.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: payload.len(),
                    found: bindings.len(),
                },
                pattern_span,
            ));
        }

        // Every payload binding must be a fresh name (spec 4.7:30). Reusing an
        // identifier — `Rect(w, w)` — silently shadows the earlier binding and
        // discards its value, so reject it (E0484, analogous to Rust E0416)
        // rather than losing a field (RUE-269). Wildcards never reach here:
        // `_` in payload position isn't a binding.
        for (i, name) in bindings.iter().enumerate() {
            if bindings[..i].contains(name) {
                return Err(CompileError::new(
                    ErrorKind::DuplicatePatternBinding {
                        name: self.interner.resolve(name).to_string(),
                    },
                    pattern_span,
                ));
            }
        }

        let mut stmts: Vec<u32> = Vec::with_capacity(bindings.len() * 2);
        for (i, binding_name) in bindings.iter().enumerate() {
            let field_ty = payload[i];

            // Read the payload field out of the scrutinee.
            let get_ref = air.add_inst(AirInst {
                data: AirInstData::EnumPayloadGet {
                    base: scrutinee_ref,
                    enum_id,
                    variant_index,
                    field_index: i as u32,
                },
                ty: field_ty,
                span: pattern_span,
            });

            // Allocate a local slot and register the binding.
            let slot = ctx.next_slot;
            let num_slots = self.abi_slot_count(field_ty);
            ctx.next_slot += num_slots;
            ctx.insert_local(
                *binding_name,
                LocalVar {
                    slot,
                    ty: field_ty,
                    is_mut: false,
                    span: pattern_span,
                    allow_unused: false,
                },
            );

            let storage_live = air.add_inst(AirInst {
                data: AirInstData::StorageLive { slot },
                ty: field_ty,
                span: pattern_span,
            });
            let alloc = air.add_inst(AirInst {
                data: AirInstData::Alloc {
                    slot,
                    init: get_ref,
                },
                ty: Type::UNIT,
                span: pattern_span,
            });
            stmts.push(storage_live.as_u32());
            stmts.push(alloc.as_u32());
        }

        Ok(stmts)
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
            // Explicit return with value. A `str`-returning function
            // (ADR-0043 Phase 3, RUE-324) supplies `str` as the expected type so
            // a string-literal `return "..."` materializes as a static-backed,
            // first-class `str` (it cannot dangle, so returning it is sound).
            let ret_ty = ctx.return_type;
            let inner_result = if self.is_str_struct(ret_ty) {
                let prev_expected = ctx.expected_type.replace(ret_ty);
                let r = self.analyze_inst(air, inner, ctx);
                ctx.expected_type = prev_expected;
                r?
            } else {
                self.analyze_inst(air, inner, ctx)?
            };
            let inner_ty = inner_result.ty;

            // Type check: returned value must match function's return type.
            if !ctx.return_type.is_error()
                && !inner_ty.is_error()
                && !inner_ty.can_coerce_to(&ctx.return_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx.return_type.safe_name_with_pool(Some(&self.type_pool)),
                        found: inner_ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }
            Some(inner_result.air_ref)
        } else {
            // `return;` without expression - only valid for unit-returning functions
            if ctx.return_type != Type::UNIT && !ctx.return_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx.return_type.safe_name_with_pool(Some(&self.type_pool)),
                        found: "()".to_string(),
                    },
                    span,
                ));
            }
            None
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Ret(inner_air_ref),
            ty: Type::NEVER, // Return expressions have Never type
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
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
                // A non-final statement's value is discarded. Discarding a
                // value that carries a linear value would implicitly drop it
                // (`make_linear();` — RUE-176), which linearity forbids.
                self.reject_discarded_linear_value(result.ty, inst_ref)?;
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
                    ty: Type::UNIT,
                    span,
                });
                AnalysisResult::new(air_ref, Type::UNIT)
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
                ty,
                init,
                iter_elem,
            } => self.analyze_alloc(
                air,
                *directives_start,
                *directives_len,
                *name,
                *is_mut,
                *ty,
                *init,
                *iter_elem,
                inst.span,
                ctx,
            ),

            InstData::VarRef { name } => {
                let resolved_ty = ctx.resolved_types.get(&inst_ref).copied();
                self.analyze_var_ref(air, *name, inst.span, resolved_ty, ctx)
            }

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
        ty: Option<Spur>,
        init: InstRef,
        iter_elem: bool,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Validate the type annotation, if any. Inference treats an
        // unresolvable annotation name as "no constraint" and silently falls
        // back to the initializer's type, so `let x: zzz_bogus = 5;` used to
        // compile (RUE-155). Resolve the name here so unknown annotations get
        // the same E0204 as signature positions. Comptime type variables
        // (e.g. `let P = Pair(i32); let p: P = ...`) and substituted type
        // parameters live in ctx, not in the type tables, so resolution goes
        // through `resolve_type_with_ctx`, which consults those local comptime
        // bindings at every level of a composite annotation — a scalar `P`, and
        // an inner element/pointee of `[P; 2]` / `ptr const P` alike (RUE-263).
        let annotation_type = if let Some(ty_sym) = ty {
            // A slice type `[T]` is second-class (ADR-0037, ADR-0043, RUE-322):
            // it may only name a function parameter, never a `let` local, so a
            // `let s: [i32] = ...` binding would let the view escape its
            // argument scope (E0489).
            self.reject_slice_escape(ty_sym, span, ErrorKind::SliceEscapesScope)?;
            Some(self.resolve_type_with_ctx(ty_sym, span, ctx)?)
        } else {
            None
        };

        // A resolved enum annotation is the expected type for the initializer.
        // This is how a fallible intrinsic learns its `Option(T)` return from a
        // `let x: Option(T) = @read_line()` annotation (RUE-6): only sema can
        // resolve the comptime-generic `Option` alias, so inference cannot
        // thread it. Narrow to enums so unrelated `let`s are unaffected.
        let prev_expected = ctx.expected_type.take();
        if let Some(annot) = annotation_type {
            // A `str` annotation is the expected type for the initializer so a
            // string literal `"..."` materializes as a 2-word static-backed
            // `str` rather than the 3-word heap `String` (ADR-0043 Phase 3,
            // RUE-324). Enums do the same for fallible-intrinsic `Option(T)`.
            if annot.is_enum() || self.is_str_struct(annot) {
                ctx.expected_type = Some(annot);
            }
        }

        // Analyze the initializer. A `for`-loop element binding is a shared
        // read of the collection (spec 4.8:26): analyze the element read under
        // `byref_arg_root` so it borrows the element in place rather than
        // moving it out — a non-Copy element is then read, not consumed
        // (RUE-259), exactly as a `borrow`-mode argument would be. The root is
        // the collection variable (`a` in `a[__p]`); a `.chars()` scalar read
        // has no place root, so this is a no-op there.
        let init_outcome = if iter_elem {
            let byref_root = super::analysis::root_variable_of(self.rir, init);
            let prev = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
            let r = self.analyze_inst(air, init, ctx);
            ctx.byref_arg_root = prev;
            r
        } else {
            self.analyze_inst(air, init, ctx)
        };
        // Restore the previous expected type before propagating any error so it
        // never leaks into a sibling statement.
        ctx.expected_type = prev_expected;
        let init_result = init_outcome?;
        let var_type = init_result.ty;

        // If name is None, this is a wildcard pattern `_` that discards the value.
        // `let _ = <expr>;` (and `let _: T = <expr>;`) is a discard site (spec
        // 3.9:18): discarding a value that carries a linear value would
        // implicitly drop it, which linearity forbids (spec 3.8:64). Reject it
        // with the same E0478 as a bare statement-expression discard (`make();`)
        // — without this, `let _` was a soundness hole that silently dropped
        // linear values (RUE-229). Once `@drop(x)` lands (RUE-187) that is the
        // sanctioned way to discard a linear value; `let _` stays an error.
        let Some(name) = name else {
            self.reject_discarded_linear_value(var_type, init)?;
            return Ok(AnalysisResult::new(init_result.air_ref, Type::UNIT));
        };

        // Special case: comptime type variables
        // When a variable is assigned a comptime type value (e.g., `let P = make_type()`),
        // we store the type in comptime_type_vars instead of creating a runtime variable.
        // This allows the variable to be used as a type annotation later (e.g., `let p: P = ...`).
        if var_type == Type::COMPTIME_TYPE {
            // Extract the type value from the TypeConst instruction
            let inst = air.get(init_result.air_ref);
            if let AirInstData::TypeConst(ty) = &inst.data {
                ctx.comptime_type_vars.insert(name, *ty);
                // Return Unit - no runtime code is generated for comptime type bindings
                let nop_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span,
                });
                return Ok(AnalysisResult::new(nop_ref, Type::UNIT));
            }
            // If it's not a TypeConst, fall through to error (can't store types at runtime)
            let name_str = self.interner.resolve(&name);
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "cannot store type value in variable '{}' at runtime; \
                         type values only exist at compile time",
                        name_str
                    ),
                },
                span,
            ));
        }

        // Check if @allow(unused_variable) directive is present
        let directives = self.rir.get_directives(directives_start, directives_len);
        let allow_unused = self.has_allow_directive(&directives, "unused_variable");

        // Allocate slots
        let slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(var_type);
        ctx.next_slot += num_slots;

        // A `for`-loop element binder over a NON-Copy collection aliases an
        // element the collection still owns and drops (spec 4.8:26): mark its
        // slot as a non-owning borrow so drop elaboration does not drop it too
        // (which would double-free the element, RUE-259). A Copy binder owns a
        // trivially-droppable copy, so it needs no marker.
        if iter_elem && !self.is_type_copy(var_type) {
            air.add_borrow_slot(slot);
        }

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
            ty: Type::UNIT,
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
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(block_ref, Type::UNIT))
    }

    /// Materialize a constant's evaluated value as an AIR instruction.
    ///
    /// Negative integers are sign-extended into the u64 payload (two's
    /// complement), matching how comptime-block results are emitted.
    fn materialize_const_value(value: ConstValue, ty: Type) -> (AirInstData, Type) {
        match value {
            ConstValue::Integer(v) => (AirInstData::Const(v as u64), ty),
            ConstValue::Bool(b) => (AirInstData::BoolConst(b), Type::BOOL),
            ConstValue::Unit => (AirInstData::UnitConst, Type::UNIT),
            // Value constants never hold type values (declaration collection
            // rejects them); this arm only fires defensively.
            ConstValue::Type(t) => (AirInstData::TypeConst(t), ty),
        }
    }

    /// Analyze a variable reference.
    pub(crate) fn analyze_var_ref(
        &mut self,
        air: &mut Air,
        name: Spur,
        span: Span,
        // The Hindley-Milner-resolved type of this reference, if known. Used
        // to recover the declared width of a captured comptime value parameter
        // (a `comptime n: u8` reference), whose `ConstValue` carries only the
        // integer magnitude, not its type (RUE-216).
        resolved_ty: Option<Type>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Check if it's a parameter — but a `let` that shadows the parameter
        // rebinds the name to a new local, and that local wins for all later
        // reads (spec 5.1:10, RUE-278). A same-named local can only exist by
        // shadowing, so its presence means "resolve as local" (handled below).
        if !ctx.locals.contains_key(&name) {
            if let Some(param_info) = ctx.params.iter().find(|p| p.name == name) {
                let ty = param_info.ty;
                let name_str = self.interner.resolve(&name);

                // Check if this parameter has been moved
                if let Some(move_state) = ctx.moved_vars.get(&name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span)
                        .with_help(super::analysis::borrow_instead_of_move_help(name_str)));
                    }
                }

                // Handle move semantics based on parameter mode.
                // A use as a by-ref call argument is a borrow, not a move
                // (`analyze_call_args` sets `byref_arg_root`), so it neither marks
                // the parameter moved nor counts as moving out of it. This is what
                // permits forwarding an inout parameter: `f(inout v)` inside
                // `fn g(inout v: T)`.
                let is_byref_arg_use = ctx.byref_arg_root == Some(name);
                let mut moves_out = false;
                if !self.is_type_copy(ty) {
                    match param_info.mode {
                        // Normal and comptime parameters behave similarly for moves
                        // (comptime params are substituted at compile time)
                        RirParamMode::Normal | RirParamMode::Comptime => {
                            if !is_byref_arg_use {
                                ctx.moved_vars
                                    .entry(name)
                                    .or_default()
                                    .mark_path_moved(&[], span);
                                // Only Normal params occupy a real ABI slot that
                                // drop elaboration would otherwise drop at exit.
                                moves_out = param_info.mode == RirParamMode::Normal;
                            }
                        }
                        RirParamMode::Inout => {
                            // Moving out of an inout parameter would leave the
                            // CALLER's variable moved-from after the call returns,
                            // so it is rejected outright (RUE-127).
                            if !is_byref_arg_use {
                                return Err(move_out_of_inout_error(name_str, span));
                            }
                        }
                        RirParamMode::Borrow => {
                            // A by-ref argument use re-borrows the parameter:
                            // `f(borrow v)` inside `fn g(borrow v: T)` is a sound
                            // read-only re-borrow and is allowed (RUE-143).
                            // `f(inout v)` — a mutable view of read-only memory —
                            // was already rejected in `analyze_call_args` (E0428),
                            // so a by-ref use reaching here is borrow-mode.
                            // Anything else moves out of the borrow: rejected.
                            if !is_byref_arg_use {
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
                }

                let mut air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span,
                });
                if moves_out {
                    // Export the move to drop elaboration: the callee-side drop
                    // of this parameter is suppressed on paths where its value
                    // moved out (RUE-61).
                    air_ref = air.add_inst(AirInst {
                        data: AirInstData::MarkMoved {
                            value: air_ref,
                            slot: param_info.abi_slot,
                            is_param: true,
                            place: None,
                        },
                        ty,
                        span,
                    });
                }
                return Ok(AnalysisResult::new(air_ref, ty));
            }
        }

        // Look up the variable in locals
        let name_str = self.interner.resolve(&name);

        // Check if this is a local variable first
        if let Some(local) = ctx.locals.get(&name) {
            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been moved
            if let Some(move_state) = ctx.moved_vars.get(&name) {
                if let Some(moved_span) = move_state.is_any_part_moved() {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::analysis::borrow_instead_of_move_help(name_str)));
                }
            }

            // If type is not Copy, mark as moved — unless this use is a by-ref
            // call argument, which borrows the variable rather than moving it.
            let moves_out = !self.is_type_copy(ty) && ctx.byref_arg_root != Some(name);
            // A `for`-loop element binder over a non-Copy collection is a
            // non-owning shared borrow of an element the collection still owns
            // (spec 4.8:26): reading it is fine, but moving it out would let
            // the new owner AND the collection both drop the element
            // (double-free), so a move is rejected like moving out of a
            // `borrow` parameter (RUE-259).
            if moves_out && air.is_borrow_slot(slot) {
                return Err(CompileError::new(
                    ErrorKind::MoveOutOfBorrow {
                        variable: name_str.to_string(),
                    },
                    span,
                ));
            }
            if moves_out {
                ctx.moved_vars
                    .entry(name)
                    .or_default()
                    .mark_path_moved(&[], span);
            }

            // Mark variable as used
            ctx.used_locals.insert(name);

            // Load the variable
            let mut air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span,
            });
            if moves_out {
                // Export the move to drop elaboration so the scope-exit drop
                // of this slot is suppressed on paths where its value moved
                // out (RUE-61).
                air_ref = air.add_inst(AirInst {
                    data: AirInstData::MarkMoved {
                        value: air_ref,
                        slot,
                        is_param: false,
                        place: None,
                    },
                    ty,
                    span,
                });
            }
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if it's a comptime type variable (e.g., `let P = Point();`)
        // These are stored in comptime_type_vars, not in locals
        if let Some(&ty) = ctx.comptime_type_vars.get(&name) {
            // Comptime type vars produce TypeConst instructions
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(ty),
                ty: Type::COMPTIME_TYPE,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
        }

        // Check if it's a comptime value variable (e.g., captured `comptime N: i32`)
        // When an anonymous struct method captures comptime parameters from its enclosing function,
        // references to those parameters are resolved here and emitted as const instructions.
        if let Some(const_value) = ctx.comptime_value_vars.get(&name) {
            match const_value {
                ConstValue::Integer(val) => {
                    // Emit the const with the parameter's declared width. The
                    // `ConstValue` carries only the integer magnitude, so the
                    // type comes from the HM-resolved type of this reference
                    // (a captured `comptime n: u8` reference resolves to u8).
                    // Falling back to i32 when no integer type was resolved
                    // preserves the historical behavior for the untyped case
                    // (RUE-216).
                    let ty = match resolved_ty {
                        Some(t) if t.is_integer() => t,
                        _ => Type::I32,
                    };
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
                ConstValue::Bool(val) => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty: Type::BOOL,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::BOOL));
                }
                ConstValue::Type(ty) => {
                    // If someone captured a type value, treat it like a type const
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(*ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                ConstValue::Unit => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(0),
                        ty: Type::UNIT,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::UNIT));
                }
            }
        }

        // Check if it's a module binding declared in this file (`const math =
        // @import("math")`). Module bindings are per-file scoped (RUE-113),
        // so the lookup is keyed by the reference's own file and takes
        // precedence over the global value-const table.
        if let Some(binding) = self.module_bindings.get(&(span.file_id, name)) {
            let ty = binding.ty;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(ty),
                ty,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if it's a value constant (e.g., `const VALUE: i32 = -42;`).
        // Module-typed constants never appear here: module bindings AND
        // aliases (`const m2 = std.math;`) live in `module_bindings`,
        // checked above. The value was evaluated once during declaration
        // gathering (RUE-171); materialize it directly — the initializer is
        // never re-analyzed at use sites.
        if let Some(const_info) = self.constants.get(&name) {
            // Privacy (E0460, RUE-183): the constants table is global, so an
            // unqualified reference can resolve to a private constant defined
            // in another directory — reject it, privacy is uniform across
            // item kinds (spec 10.3:1, 10.3:7). The declaration span's file
            // is the constant's defining file.
            self.check_unqualified_visibility(
                "constant",
                name_str,
                const_info.span.file_id,
                const_info.is_pub,
                span,
            )?;
            let (data, ty) = Self::materialize_const_value(const_info.value, const_info.ty);
            let air_ref = air.add_inst(AirInst { data, ty, span });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if this is a type name (for comptime type parameters)
        // Try to resolve it as a type - if successful, emit a TypeConst instruction
        match self.resolve_type(name, span) {
            Ok(resolved_type) => {
                // This is a type name being used as a value (e.g., `i32` passed to `comptime T: type`)
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(resolved_type),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
            // The name IS a known type, but a private one from another
            // directory (RUE-183): report the privacy error rather than
            // falling through to a misleading "undefined variable".
            Err(e) if matches!(e.kind, ErrorKind::PrivateUnqualifiedAccess(_)) => {
                return Err(e);
            }
            // Any other resolution failure: not a type name, keep falling
            // through to the undefined-variable error below.
            Err(_) => {}
        }

        // Not a parameter, local, type, or constant - undefined variable
        Err(CompileError::new(
            ErrorKind::UndefinedVariable(name_str.to_string()),
            span,
        ))
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
            .iter()
            .find(|p| p.name == name)
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

        // Reassigning a collection that an enclosing `for` loop is iterating
        // mutates a shared-borrowed value (spec 4.8:26, RUE-233) — E0428, just
        // like assigning through an explicit `borrow` parameter.
        if ctx.iter_borrows.contains(&name) {
            return Err(CompileError::new(
                ErrorKind::MutateBorrowedValue {
                    variable: name_str.to_string(),
                },
                span,
            ));
        }

        // Check if it's a parameter (for inout params) — unless a `let mut`
        // shadowed it with a local of the same name, in which case the
        // assignment targets that mutable local, not the parameter (RUE-278).
        // Without this guard, `let mut x = x; x = x + 1;` wrongly reports E0203
        // against the immutable parameter with a bogus "make it inout" hint.
        if !ctx.locals.contains_key(&name) {
            if let Some(param_info) = ctx.params.iter().find(|p| p.name == name) {
                // Check parameter mode - only inout can be assigned to
                match param_info.mode {
                    // Normal and comptime parameters are immutable
                    RirParamMode::Normal | RirParamMode::Comptime => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(name_str.to_string()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            name_str,
                            name_str,
                            param_info.ty.safe_name_with_pool(Some(&self.type_pool))
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
                    ty: Type::UNIT,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::UNIT));
            }
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
        let local_ty = local.ty;

        // Analyze the value. When the target is a `str` (ADR-0043 Phase 3,
        // RUE-324), supply it as the expected type so a string literal RHS
        // materializes as a 2-word `str` rather than a 3-word `String` (which
        // would corrupt the 2-slot local); this is what makes `let mut s: str;
        // s = "hi";` reassignment sound.
        let value_result = if self.is_str_struct(local_ty) {
            let prev_expected = ctx.expected_type.replace(local_ty);
            let r = self.analyze_inst(air, value, ctx);
            ctx.expected_type = prev_expected;
            r?
        } else {
            self.analyze_inst(air, value, ctx)?
        };

        // Assignment to a mutable variable resets its move state.
        ctx.moved_vars.remove(&name);

        // Emit store instruction
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot,
                value: value_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
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
                ..
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
        // First check if it's a comptime type variable (e.g., `let Point = make_point(); Point { ... }`)
        let type_name_str = self.interner.resolve(&type_name);
        let struct_id = if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
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
            let struct_id = *self
                .structs
                .get(&type_name)
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
            let expected_field_type = struct_def.fields[field_idx].ty;

            // Check if this is an integer literal that needs type coercion
            // This handles the case where HM inference couldn't resolve the type
            // (e.g., when the struct comes from a comptime type variable)
            let field_inst = self.rir.get(*field_value);
            let field_result = if let InstData::IntConst(value) = &field_inst.data {
                // Integer literal - use the expected field type directly, but
                // range-check it first: this shortcut bypasses analyze_literal,
                // and previously skipped the E0800 check entirely, so
                // `S { a: 300 }` with a: u8 silently truncated to 44. (RUE-72)
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
            } else if self.is_str_struct(expected_field_type) {
                // A `str`-typed field (ADR-0043 Phase 3, RUE-324): supply the
                // field type as the expected type so a string-literal value
                // materializes as a static-backed 2-word `str` (first-class,
                // storable in a struct) rather than a 3-word `String`.
                let prev_expected = ctx.expected_type.replace(expected_field_type);
                let r = self.analyze_inst(air, *field_value, ctx);
                ctx.expected_type = prev_expected;
                r?
            } else {
                // Not an integer literal - analyze normally
                self.analyze_inst(air, *field_value, ctx)?
            };

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

    /// Reject a move (full or partial) whose root is a by-ref parameter.
    ///
    /// Moving a non-Copy value out of an `inout` or `borrow` parameter would
    /// leave the CALLER's variable (partially) moved-from after the call
    /// returns, so both are rejected outright (RUE-127); reinitialization
    /// before exit is not tracked yet.
    pub(crate) fn reject_move_out_of_byref_param(
        &self,
        root_var: Spur,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<()> {
        // A `let` shadowing the by-ref parameter rebinds the name to an owned
        // local; moving out of that local is fine, so the parameter rule no
        // longer applies (RUE-278).
        if ctx.locals.contains_key(&root_var) {
            return Ok(());
        }
        if let Some(param_info) = ctx.params.iter().find(|p| p.name == root_var) {
            match param_info.mode {
                RirParamMode::Inout => {
                    return Err(move_out_of_inout_error(
                        self.interner.resolve(&root_var),
                        span,
                    ));
                }
                RirParamMode::Borrow => {
                    return Err(CompileError::new(
                        ErrorKind::MoveOutOfBorrow {
                            variable: self.interner.resolve(&root_var).to_string(),
                        },
                        span,
                    ));
                }
                RirParamMode::Normal | RirParamMode::Comptime => {}
            }
        }
        Ok(())
    }

    /// Reject a field access that consumes (destructures) a linear struct
    /// when the destructure would implicitly drop a *different* field that
    /// itself carries a linear value (E0474, spec 3.8:58, RUE-40).
    ///
    /// Destructuring consumption (spec 3.8:33) extracts the accessed leaf
    /// field and drops everything else in the value. That implicit drop must
    /// not lose a linear value. Every struct level along the projection
    /// chain is checked: at each level, all fields other than the projected
    /// one are dropped.
    fn reject_linear_destructure_dropping_linear_field(
        &self,
        trace: &PlaceTrace,
        span: Span,
    ) -> CompileResult<()> {
        let mut current_ty = trace.base_type;
        for proj in &trace.projections {
            if let AirProjection::Field {
                struct_id,
                field_index,
            } = proj.proj
            {
                let def = self.type_pool.struct_def(struct_id);
                for (i, field) in def.fields.iter().enumerate() {
                    if i as u32 != field_index && self.type_carries_linear(field.ty) {
                        let accessed = def.fields[field_index as usize].name.clone();
                        let err = CompileError::new(
                            ErrorKind::LinearFieldDroppedByDestructure(Box::new(
                                rue_error::LinearFieldDroppedByDestructureError {
                                    struct_name: def.name.clone(),
                                    accessed,
                                    dropped: field.name.clone(),
                                },
                            )),
                            span,
                        )
                        .with_help(
                            "destructuring consumption extracts one field and drops \
                             the rest; access the linear field instead and consume \
                             its value, or consume the whole value by passing it to \
                             a function",
                        );
                        return Err(self.attach_infectious_linear_note(err, current_ty));
                    }
                }
            }
            current_ty = proj.result_type;
        }
        Ok(())
    }

    /// Reject moving a field out of a value whose struct type has a
    /// destructor (RUE-158, the spirit of Rust's E0509).
    ///
    /// The destructor always runs on the *whole* value when it is dropped:
    /// it would observe the moved-out field (a use-after-free for heap
    /// fields), and the automatic field cleanup after the destructor would
    /// drop the field a second time. Inside `drop fn T(self)` this same rule
    /// rejects `self.field` moves — `self` has type `T`, which has the very
    /// destructor being defined (whole-`self` moves are E0442).
    ///
    /// Every container along the projection chain is checked (`o.a.b` moves
    /// out of both `o` and `o.a`), mirroring Rust: a deep move leaves every
    /// enclosing value partially moved. Whole-value moves and `borrow`/
    /// `inout` access of fields (RUE-143) stay legal and never reach here.
    fn reject_field_move_out_of_destructor_type(
        &self,
        trace: &PlaceTrace,
        span: Span,
    ) -> CompileResult<()> {
        for (i, proj_info) in trace.projections.iter().enumerate() {
            let container = if i == 0 {
                trace.base_type
            } else {
                trace.projections[i - 1].result_type
            };
            let Some(struct_id) = container.as_struct() else {
                continue;
            };
            let struct_def = self.type_pool.struct_def(struct_id);
            if struct_def.destructor.is_none() {
                continue;
            }
            let field_name = proj_info
                .field_name
                .map(|s| self.interner.resolve(&s).to_string())
                .unwrap_or_default();
            let mut err = CompileError::new(
                ErrorKind::MoveFieldOutOfDestructorType {
                    struct_name: struct_def.name.clone(),
                    field_name: field_name.clone(),
                },
                span,
            )
            .with_label(format!("field `{field_name}` is moved out here"), span)
            .with_note(format!(
                "the destructor for '{}' runs on the whole value when it is dropped: it \
                 would observe the moved-out field, and the automatic field cleanup after \
                 the destructor would drop `{field_name}` a second time",
                struct_def.name
            ))
            .with_help(format!(
                "borrow the field instead (`borrow value.{field_name}`), or move the whole value"
            ));
            if let Some(drop_span) = self.destructor_spans.get(&struct_id) {
                err = err.with_label(
                    format!("destructor for '{}' is defined here", struct_def.name),
                    *drop_span,
                );
            }
            return Err(err);
        }
        Ok(())
    }

    /// Analyze a field access.
    ///
    /// Uses place-based analysis (ADR-0030) when possible for efficient code generation.
    fn analyze_field_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        field: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // First, check if the base is a module access (special case, not a place)
        // We need to peek at the base type to detect module.Type access patterns.
        let base_inst = self.rir.get(base);
        if let InstData::VarRef { name } = &base_inst.data {
            // Check if this VarRef refers to a module
            if let Some(local) = ctx.locals.get(name) {
                if let Some(module_id) = local.ty.as_module() {
                    // This is module.Member access - handle specially.
                    // Member access is a use of the binding (`let m =
                    // @import(..); m.ANSWER` must not warn about `m`).
                    ctx.used_locals.insert(*name);
                    return self.analyze_module_type_member_access(air, module_id, field, span);
                }
            }
        }

        // Try to trace this expression to a place (lvalue)
        if let Some(trace) = self.try_trace_place(inst_ref, air, ctx)? {
            let field_type = trace.result_type();

            // Check if the root variable was fully moved (applies regardless of field type)
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.interner.resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::analysis::borrow_instead_of_move_help(root_name)));
                }
            }

            // A field read through an array element (`xs[0].f`) must not go
            // through a moved-out element (RUE-186).
            self.check_read_through_moved_element(&trace, ctx, span)?;

            // Get struct info for move checking
            // The trace's result type is the field type, but we need the parent struct type
            // to check if it's linear. The parent is the type *before* the last projection.
            let parent_type = if trace.projections.len() > 1 {
                trace.projections[trace.projections.len() - 2].result_type
            } else {
                trace.base_type
            };

            let is_linear = parent_type
                .as_struct()
                .map(|id| self.type_pool.struct_def(id).is_linear)
                .unwrap_or(false);

            // Move checking using the trace. `move_is_partial` selects the
            // MarkMoved marker's place component: absent for a whole-struct
            // (linear) move, the accessed place for a field-path move
            // (RUE-62, RUE-157).
            //
            // A use as a by-ref call argument (`f(borrow o.f)`, `f(inout
            // o.f)`) borrows the place rather than moving out of it
            // (RUE-143): no move is recorded and the by-ref-param move
            // rejections don't apply — but reading through an already-moved
            // path is still a use-after-move.
            let is_byref_arg_use = ctx.byref_arg_root == Some(trace.root_var);

            // Moving a (non-Copy) field or the whole value out of a `for`-loop
            // element binder over a non-Copy collection would let the new owner
            // AND the collection both drop it (double-free): iteration is a
            // shared read (spec 4.8:26), so the binder may be read but not
            // moved out, whole-value or field-granular (RUE-259).
            if !is_byref_arg_use
                && (is_linear || !self.is_type_copy(field_type))
                && matches!(trace.base, AirPlaceBase::Local(s) if air.is_borrow_slot(s))
            {
                return Err(CompileError::new(
                    ErrorKind::MoveOutOfBorrow {
                        variable: self.interner.resolve(&trace.root_var).to_string(),
                    },
                    span,
                ));
            }

            let mut emit_move_marker = false;
            let mut move_is_partial = false;
            if is_byref_arg_use {
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::analysis::use_after_move_path_error(
                            self.interner,
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
            } else if is_linear {
                // For linear types, field access consumes the entire struct
                self.reject_linear_destructure_dropping_linear_field(&trace, span)?;
                self.reject_move_out_of_byref_param(trace.root_var, ctx, span)?;
                ctx.moved_vars
                    .entry(trace.root_var)
                    .or_default()
                    .mark_path_moved(&[], span);
                emit_move_marker = true;
            } else if !self.is_type_copy(field_type) {
                // For non-linear types, check if accessing a non-Copy field
                self.reject_move_out_of_byref_param(trace.root_var, ctx, span)?;
                self.reject_field_move_out_of_destructor_type(&trace, span)?;
                let field_path = trace.field_path();

                // Check if this field path is already moved. Moving the field
                // out as a whole value is illegal if the path itself, an
                // ancestor, OR any descendant subfield was already moved
                // (`o.inner` cannot be passed by value once `o.inner.s` moved —
                // spec 3.8, RUE-279), so check both directions here, unlike a
                // Copy leaf read below which only cares about ancestors.
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_or_descendant_moved(&field_path) {
                        return Err(super::analysis::use_after_move_path_error(
                            self.interner,
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }

                // Mark this field path as moved
                ctx.moved_vars
                    .entry(trace.root_var)
                    .or_default()
                    .mark_path_moved(&field_path, span);

                // Export pure-field-path moves of any depth (`o.a`, `o.a.b`)
                // to drop elaboration so the moved path's drop inside the
                // struct's scope-exit drop is suppressed (RUE-62, RUE-157).
                // Paths THROUGH an array index (`arr[i].a`) get no marker:
                // drop elaboration keeps the whole-slot drop, which re-drops
                // the moved element (a known gap; root-level element moves
                // `arr[K]` ARE tracked — RUE-186 — but index-interior paths
                // are not). Only Normal params occupy a real ABI slot that
                // drop elaboration would drop.
                let pure_field_path = trace
                    .projections
                    .iter()
                    .all(|p| matches!(p.proj, AirProjection::Field { .. }));
                if pure_field_path {
                    let is_droppable_param_base = match trace.base {
                        AirPlaceBase::Local(_) => true,
                        AirPlaceBase::Param(_) => ctx
                            .params
                            .iter()
                            .any(|p| p.name == trace.root_var && p.mode == RirParamMode::Normal),
                    };
                    if is_droppable_param_base {
                        emit_move_marker = true;
                        move_is_partial = true;
                    }
                }
            } else {
                // Copy fields are read, not moved — but reading one THROUGH
                // a moved ancestor (`o.f.x` after `o.f` was moved out) reads
                // memory whose owner is gone: drops are move-aware, so the
                // moved part is logically dead. `is_path_moved` checks the
                // exact path and every ancestor prefix (the full-move case
                // was already rejected above).
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::analysis::use_after_move_path_error(
                            self.interner,
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
            }

            // Emit PlaceRead instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let mut air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: field_type,
                span,
            });
            if emit_move_marker {
                // Export the move (whole struct for linear types, the
                // accessed field path for partial moves) to drop elaboration.
                let (slot, is_param) = match trace.base {
                    AirPlaceBase::Local(slot) => (slot, false),
                    AirPlaceBase::Param(slot) => (slot, true),
                };
                air_ref = air.add_inst(AirInst {
                    data: AirInstData::MarkMoved {
                        value: air_ref,
                        slot,
                        is_param,
                        place: move_is_partial.then_some(place_ref),
                    },
                    ty: field_type,
                    span,
                });
            }
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // Fallback: base is not a place (e.g., function call result)
        // Spill the computed value to a temporary, then use PlaceRead.
        // This handles `get_struct().field` patterns.
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_type = base_result.ty;

        // Handle module member access that wasn't caught above
        if let Some(module_id) = base_type.as_module() {
            return self.analyze_module_type_member_access(air, module_id, field, span);
        }

        let struct_id = match base_type.as_struct() {
            Some(id) => id,
            None => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }
        };

        let struct_def = self.type_pool.struct_def(struct_id);
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

        // Allocate a temporary slot for the computed struct value
        let temp_slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(base_type);
        ctx.next_slot += num_slots;

        // Emit StorageLive for the temporary
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });

        // Emit Alloc to store the computed value
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Create PlaceRead with Field projection on the temp slot
        let place_ref = air.make_place(
            AirPlaceBase::Local(temp_slot),
            std::iter::once(AirProjection::Field {
                struct_id,
                field_index: field_index as u32,
            }),
        );
        let mut value_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: field_type,
            span,
        });

        // Projecting a non-Copy field out of this spilled temporary MOVES it
        // out of the temporary. The temporary is dropped whole at scope exit
        // (drop elaboration in rue-cfg), so without a move marker its per-field
        // drop glue would re-drop the extracted field that the new owner (e.g.
        // the `let` binding) also drops — a double free (RUE-258). Emit a
        // field-path MarkMoved on the temp slot, exactly as the named-place
        // path above does, so the temporary's scope-exit drop skips this field.
        if !self.is_type_copy(field_type) {
            value_ref = air.add_inst(AirInst {
                data: AirInstData::MarkMoved {
                    value: value_ref,
                    slot: temp_slot,
                    is_param: false,
                    place: Some(place_ref),
                },
                ty: field_type,
                span,
            });
        }

        // Note: We don't emit StorageDead here. The temporary will be cleaned up by
        // scope-based drop elaboration in the CFG builder. This is slightly conservative
        // (temp lives until scope exit rather than immediately after use) but correct.
        // A future optimization could add explicit StorageDead at the right point.
        let stmts_start = air.add_extra(&[storage_live_ref.as_u32(), alloc_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 2,
                value: value_ref,
            },
            ty: field_type,
            span,
        });
        Ok(AnalysisResult::new(block_ref, field_type))
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
    fn analyze_module_type_member_access(
        &mut self,
        air: &mut Air,
        module_id: crate::types::ModuleId,
        member_name: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let member_name_str = self.interner.resolve(&member_name).to_string();

        // Get the module definition and resolve its file to a canonical
        // FileId, so equivalent path spellings (`helper.rue` vs
        // `./helper.rue`) refer to the same module (spec 10.2:4, RUE-240).
        // `module_file_path` is then that file's stored path, used for the
        // directory-based visibility checks below.
        let module_def = self.module_registry.get_def(module_id);
        let module_file_id = self.canonical_file_id(&module_def.file_path);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or_else(|| module_def.file_path.clone());

        // Get the accessing file's directory for visibility check
        let accessing_file_path = self.get_source_path(span).map(|s| s.to_string());

        // First, try to find a struct with this name that belongs to the module's file
        if let Some(&struct_id) = self.structs.get(&member_name) {
            let struct_def = self.type_pool.struct_def(struct_id);

            // Check if this struct was defined in the module's file
            {
                if module_file_id == Some(struct_def.file_id) {
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
            }
        }

        // Next, try to find an enum with this name that belongs to the module's file
        if let Some(&enum_id) = self.enums.get(&member_name) {
            let enum_def = self.type_pool.enum_def(enum_id);

            // Check if this enum was defined in the module's file
            {
                if module_file_id == Some(enum_def.file_id) {
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
            }
        }

        // Next, try a const defined in the module's file. The headline case is
        // ADR-0026's re-export idiom — `pub const math = @import("...")` in a
        // facade — where the const's type is itself a module: accessing it
        // yields that module, so chains like `std.math.abs(...)` resolve
        // member-by-member (RUE-136). Module bindings live in the per-file
        // `module_bindings` table keyed by the facade's FileId (RUE-113);
        // value consts are found by name in the flat constants table,
        // filtered to the module's file via the declaration span.
        let member_const = module_file_id
            .and_then(|file_id| self.module_bindings.get(&(file_id, member_name)))
            .or_else(|| {
                self.constants
                    .get(&member_name)
                    .filter(|const_info| module_file_id == Some(const_info.span.file_id))
            });
        if let Some(const_info) = member_const {
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
            // A value const (e.g. `pub const ANSWER = ...`) accessed as a
            // module member: materialize the value that was evaluated at
            // declaration time, typed as declared (RUE-160).
            let (data, ty) = Self::materialize_const_value(const_info.value, const_info.ty);
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
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => self.analyze_array_init(air, inst_ref, *elems_start, *elems_len, inst.span, ctx),

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

        // An array literal of `type` values (`[i32, i32]`) has no runtime
        // representation: type values only exist at compile time (spec
        // 4.14:6). Reject it with E1200 — the same diagnostic `let t =
        // comptime { i32 };` gets — before the array type, whose element is
        // the comptime-only `type`, would reach the intern pool and panic
        // (RUE-253).
        for elem_ref in &elem_refs {
            if let Some(elem_ty) = ctx.resolved_types.get(elem_ref).copied() {
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
        let elem_u32s: Vec<u32> = vec![value_result.air_ref.as_u32(); length as usize];
        let elems_len = elem_u32s.len() as u32;
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
    ///
    /// Uses place-based analysis (ADR-0030) when possible for efficient code generation.
    fn analyze_index_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Check for constant out-of-bounds index early (before tracing)
        // We need the array type for bounds checking, so peek at the base first
        let _base_inst = self.rir.get(base);

        // Try to trace this expression to a place (lvalue)
        if let Some(trace) = self.try_trace_place(inst_ref, air, ctx)? {
            let elem_type = trace.result_type();

            // Reading through an index expression whose base place has been
            // moved is a use-after-move (RUE-232), exactly like a field read
            // of a moved place. A plain (Copy-element, non-byref) index read
            // reached neither move check below — the byref branch checks its
            // own path and the non-Copy branch records an element move — so
            // `moved.field[i]` after `let b = moved` slipped through. Check the
            // base place's move state here for every index read. `field_path()`
            // names this element (constant index) or the base up to a dynamic
            // index, so `is_path_moved` catches a full move of the root, a
            // moved field/element prefix, or this exact constant element,
            // without flagging a *sibling* element (`arr[1]` after `arr[0]`
            // moved has a disjoint path).
            {
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::analysis::use_after_move_path_error(
                            self.interner,
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
            }

            // Get array info from the parent type (before the last projection)
            let parent_type = if trace.projections.len() > 1 {
                trace.projections[trace.projections.len() - 2].result_type
            } else {
                trace.base_type
            };

            let array_len = match parent_type.as_array() {
                Some(type_id) => {
                    let (_elem, len) = self.type_pool.array_def(type_id);
                    len
                }
                None => {
                    // This shouldn't happen if try_trace_place worked correctly
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: parent_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            };

            // Check for constant out-of-bounds index. Evaluate at the index's
            // resolved operand types so an expression that overflows its own
            // integer type (`arr[X + 1]`, X: i8 = 127) is a compile-time error
            // rather than a folded-then-bounds-checked value (RUE-234).
            if let Some(const_idx) = self.try_get_const_index_checked(index, ctx)? {
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

            // A use as a by-ref call argument (`f(borrow a[i])`, `f(inout
            // a[i])`) borrows the element in place rather than moving it out
            // of the array (RUE-143), so the non-Copy rejection below does
            // not apply — but indexing an already-moved array is still a
            // use-after-move (`field_path()` names this element for a constant
            // index, so this catches the root's full move or this exact
            // element; the per-element check below also covers a moved-out
            // element, RUE-186).
            let is_byref_arg_use = ctx.byref_arg_root == Some(trace.root_var);
            let mut element_move: Option<i64> = None;
            if is_byref_arg_use {
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::analysis::use_after_move_path_error(
                            self.interner,
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
                self.check_read_through_moved_element(&trace, ctx, span)?;
            } else if !self.is_type_copy(elem_type) {
                // A CONSTANT index directly into an array variable moves
                // just that element out (per-element tracking, RUE-186,
                // spec 3.8:68). Everything else — dynamic index, or an
                // array that is not the trace root — keeps the rejection:
                // with a runtime index sema cannot know which element
                // moved, so neither use-after-move checking nor drop
                // suppression could stay sound (spec 7.1:28).
                element_move = self.record_element_move_out(&trace, ctx, span)?;
                if element_move.is_none() {
                    return Err(CompileError::new(
                        ErrorKind::MoveOutOfIndex {
                            element_type: elem_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    )
                    .with_help(
                        "moving an element out requires a compile-time \
                         constant index into an array variable",
                    ));
                }
            }

            // Emit PlaceRead instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let mut air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: elem_type,
                span,
            });
            if let Some(k) = element_move {
                // Export the element move to drop elaboration (RUE-186).
                air_ref = self.emit_element_move_marker(
                    air,
                    &trace,
                    ctx,
                    air_ref,
                    k,
                    elem_type,
                    parent_type,
                    span,
                );
            }
            return Ok(AnalysisResult::new(air_ref, elem_type));
        }

        // Fallback: base is not an array place (e.g. function-call result, or
        // a String — which is a builtin struct, so `try_trace_place` bails at
        // its non-array projection). Snapshot the base root's move state
        // *before* analyzing it, in case this turns out to be a borrowing
        // String index (see below).
        let base_root = self.extract_root_variable(base);
        let base_move_state_before = base_root.and_then(|v| ctx.moved_vars.get(&v).cloned());
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_type = base_result.ty;

        // String byte indexing: `s[i]` reads the i-th BYTE of a String as `u8`
        // (RUE-17 Phase 2, ADR-0035). O(1), bounds-checked at runtime: an
        // `index >= s.len()` traps (exit 101), exactly like array indexing.
        if self.is_builtin_string(base_type) {
            return self.analyze_string_index_get(
                air,
                base_result,
                base_root,
                base_move_state_before,
                index,
                span,
                ctx,
            );
        }

        // `str` byte indexing: `s[i]` reads the i-th BYTE of a `str` as `u8`
        // (ADR-0043 Phase 3, RUE-324). A `str` is `[u8]` + UTF-8, but its bytes
        // are PACKED (1 byte each in `.rodata`), unlike an array slice whose
        // elements are 8-byte-slotted. So it cannot reuse the slice
        // `@ptr_offset`/`@ptr_read` path (which strides by `slot_count * 8`);
        // it lowers to the same packed-byte runtime read as `String`, but with
        // the 2-word `{ptr, len}` receiver: `__rue_str_byte_at(ptr, len, index)`.
        if self.is_str_struct(base_type) {
            return self.analyze_str_index_get(
                air,
                base_result,
                base_root,
                base_move_state_before,
                index,
                span,
                ctx,
            );
        }

        // Slice read-indexing `s[i]` (ADR-0043, RUE-322): the base is the
        // synthetic 2-word fat-pointer struct `{ptr, len}`. Lower to a
        // runtime-bounds-checked pointer load through the fat pointer's `ptr`
        // word — no array place is involved.
        if let Some(elem_ty) = self.slice_element_type(base_type) {
            return self.analyze_slice_index_get(
                air,
                base_result,
                base_root,
                base_move_state_before,
                index,
                elem_ty,
                span,
                ctx,
            );
        }

        let index_result = self.analyze_inst(air, index, ctx)?;

        // Verify base is an array
        let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
            Some(type_id) => {
                let (element_type, length) = self.type_pool.array_def(type_id);
                (type_id, element_type, length)
            }
            None => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }
        };

        // Check for constant out-of-bounds index (at the index's resolved
        // operand types, so an overflowing index expression is a compile-time
        // error rather than a folded runtime panic — RUE-234).
        if let Some(const_idx) = self.try_get_const_index_checked(index, ctx)? {
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
        if !self.is_type_copy(elem_type) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: elem_type.safe_name_with_pool(Some(&self.type_pool)),
                },
                span,
            )
            .with_help("use explicit methods like swap() or take() to remove elements"));
        }

        // Allocate a temporary slot for the computed array value
        let temp_slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(base_type);
        ctx.next_slot += num_slots;

        // Emit StorageLive for the temporary
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });

        // Emit Alloc to store the computed array
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Create PlaceRead with Index projection on the temp slot
        let place_ref = air.make_place(
            AirPlaceBase::Local(temp_slot),
            std::iter::once(AirProjection::Index {
                array_type: base_type,
                index: index_result.air_ref,
            }),
        );
        let read_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: elem_type,
            span,
        });

        // Note: We don't emit StorageDead here. The temporary will be cleaned up by
        // scope-based drop elaboration in the CFG builder.
        let stmts_start = air.add_extra(&[storage_live_ref.as_u32(), alloc_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 2,
                value: read_ref,
            },
            ty: elem_type,
            span,
        });
        Ok(AnalysisResult::new(block_ref, elem_type))
    }

    /// Analyze a String byte index read: `s[i] -> u8` (RUE-17 Phase 2,
    /// ADR-0035).
    ///
    /// Indexing a String yields the i-th BYTE (not a char) as `u8`, lowering to
    /// a checked runtime call `__rue_String_byte_at(ptr, len, cap, index)`. The
    /// bounds check lives in the runtime: an `index >= len` traps (exit 101),
    /// mirroring array indexing rather than producing UB.
    ///
    /// The index only *borrows* the String (it is neither consumed nor
    /// mutated), so — like a `ByRef` builtin method — we undo the move the base
    /// analysis recorded (`base_result` already analyzed) by restoring the
    /// pre-analysis move state and cancelling the emitted move marker. That
    /// keeps later uses of `s` valid and ensures the String is dropped exactly
    /// once.
    #[allow(clippy::too_many_arguments)]
    fn analyze_string_index_get(
        &mut self,
        air: &mut Air,
        base_result: AnalysisResult,
        base_root: Option<Spur>,
        base_move_state_before: Option<VariableMoveState>,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Un-move the borrowed receiver (mirrors ByRef builtin methods).
        if let Some(var) = base_root {
            match base_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(base_result.air_ref);

        // The index is an ordinary rvalue. Analyze it and require an integer
        // type (signed or unsigned), matching array indexing (spec 7.1:7).
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "an integer".to_string(),
                    found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                self.rir.get(index).span,
            ));
        }

        // Lower to `__rue_String_byte_at(self, index) -> u8`. The String is
        // passed by value in the AIR (codegen decomposes it into ptr/len/cap
        // argument registers, as for other builtin String methods); the move
        // was already cancelled above so this is a non-consuming read.
        let call_name = self.interner.get_or_intern("__rue_String_byte_at");
        let extra = [
            base_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
            index_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
        ];
        let args_start = air.add_extra(&extra);
        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: 2,
            },
            ty: Type::U8,
            span,
        });
        Ok(AnalysisResult::new(call_ref, Type::U8))
    }

    /// Analyze a `str` byte index read: `s[i] -> u8` (ADR-0043 Phase 3,
    /// RUE-324).
    ///
    /// A `str` is `[u8]` + UTF-8, but its bytes are PACKED (1 byte each, in
    /// `.rodata` for a literal), so indexing yields the i-th BYTE via a checked
    /// runtime call `__rue_str_byte_at(ptr, len, index)` — the same packed-byte
    /// read as `String`, minus the (nonexistent) `cap` word. The bounds check
    /// lives in the runtime: an `index >= len` traps (exit 101), mirroring array
    /// and `String` indexing rather than producing UB.
    ///
    /// Like `String` indexing, the read only *borrows* the receiver (a `str` is
    /// `Copy` anyway), so the pre-analysis move state is restored and the move
    /// marker cancelled.
    fn analyze_str_index_get(
        &mut self,
        air: &mut Air,
        base_result: AnalysisResult,
        base_root: Option<Spur>,
        base_move_state_before: Option<VariableMoveState>,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Un-move the borrowed receiver (mirrors the String index path).
        if let Some(var) = base_root {
            match base_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(base_result.air_ref);

        // The index is an ordinary rvalue; require an integer (spec 7.1:7).
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "an integer".to_string(),
                    found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                self.rir.get(index).span,
            ));
        }

        // Lower to `__rue_str_byte_at(self, index) -> u8`. The 2-word `str`
        // value is passed by value; codegen decomposes it into ptr/len argument
        // registers, exactly as it decomposes the 3-word String for byte_at.
        let call_name = self.interner.get_or_intern("__rue_str_byte_at");
        let extra = [
            base_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
            index_result.air_ref.as_u32(),
            AirArgMode::Normal.as_u32(),
        ];
        let args_start = air.add_extra(&extra);
        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: 2,
            },
            ty: Type::U8,
            span,
        });
        Ok(AnalysisResult::new(call_ref, Type::U8))
    }

    /// Analyze a slice read-index `s[i]` (ADR-0043, RUE-322).
    ///
    /// The slice `base_result` is the synthetic 2-word fat-pointer struct
    /// `{ptr: ptr const T, len: u64}`. The read desugars to, in order:
    ///
    /// 1. a runtime bounds check `@assert(i < len)` — traps with exit 101 (the
    ///    same discipline as array indexing) when the index is out of range;
    /// 2. `@ptr_read(@ptr_offset(ptr, i))` — `@ptr_offset` scales by
    ///    `size_of(T)`, so this reads the i-th element.
    ///
    /// Everything is built from existing intrinsics, so no new codegen (or
    /// backend-specific work) is required; the fat pointer flows through the
    /// same struct/field/pointer paths the manual `{ptr,len}` form already uses.
    #[allow(clippy::too_many_arguments)]
    fn analyze_slice_index_get(
        &mut self,
        air: &mut Air,
        base_result: AnalysisResult,
        base_root: Option<Spur>,
        base_move_state_before: Option<VariableMoveState>,
        index: InstRef,
        elem_ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Indexing reads (never consumes) the slice receiver; undo any move the
        // receiver analysis recorded (mirrors the String index / ByRef paths).
        if let Some(var) = base_root {
            match base_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(base_result.air_ref);

        let slice_struct_id = base_result
            .ty
            .as_struct()
            .expect("slice receiver is a synthetic struct");
        let ptr_ty = self.type_pool.struct_def(slice_struct_id).fields[0].ty;

        // The index is an ordinary rvalue; require an integer (spec 7.1:7).
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "an integer".to_string(),
                    found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                self.rir.get(index).span,
            ));
        }

        // Read the fat pointer's two words from the slice value.
        let ptr_ref = air.add_inst(AirInst {
            data: AirInstData::FieldGet {
                base: base_result.air_ref,
                struct_id: slice_struct_id,
                field_index: 0,
            },
            ty: ptr_ty,
            span,
        });
        let len_ref = air.add_inst(AirInst {
            data: AirInstData::FieldGet {
                base: base_result.air_ref,
                struct_id: slice_struct_id,
                field_index: 1,
            },
            ty: Type::U64,
            span,
        });

        // Runtime bounds check: `@assert(index < len)` traps (exit 101) when the
        // index is out of range.
        let cond_ref = air.add_inst(AirInst {
            data: AirInstData::Lt(index_result.air_ref, len_ref),
            ty: Type::BOOL,
            span,
        });
        let assert_args = air.add_extra(&[cond_ref.as_u32()]);
        let assert_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.assert,
                args_start: assert_args,
                args_len: 1,
            },
            ty: Type::UNIT,
            span,
        });

        // element = @ptr_read(@ptr_offset(ptr, index)).
        let off_args = air.add_extra(&[ptr_ref.as_u32(), index_result.air_ref.as_u32()]);
        let off_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.ptr_offset,
                args_start: off_args,
                args_len: 2,
            },
            ty: ptr_ty,
            span,
        });
        let read_args = air.add_extra(&[off_ref.as_u32()]);
        let elem_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.ptr_read,
                args_start: read_args,
                args_len: 1,
            },
            ty: elem_ty,
            span,
        });

        // Demand-driven lowering only pulls the returned value's dependencies,
        // so the bounds-check assertion (a pure side effect) must be an explicit
        // statement of the block that yields the element.
        let stmts = air.add_extra(&[assert_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: stmts,
                stmts_len: 1,
                value: elem_ref,
            },
            ty: elem_ty,
            span,
        });
        Ok(AnalysisResult::new(block_ref, elem_ty))
    }

    /// Analyze a slice method call (ADR-0043, RUE-322). Only `.len()` is
    /// supported today: it reads the fat pointer's `len` word (runtime length).
    pub(crate) fn analyze_slice_method(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        receiver_var: Option<Spur>,
        method_name: &str,
        arg_count: usize,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // The receiver is read, not consumed: analyze it as a borrow and undo
        // the move the analysis records.
        let move_state_before = receiver_var.and_then(|v| ctx.moved_vars.get(&v).cloned());
        let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, receiver_var);
        let recv_result = self.analyze_inst(air, receiver, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let recv_result = recv_result?;
        if let Some(var) = receiver_var {
            match move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(recv_result.air_ref);

        let slice_struct_id = recv_result
            .ty
            .as_struct()
            .expect("slice receiver is a synthetic struct");

        if method_name == "len" && arg_count == 0 {
            // Read the `len` word (field 1) from the fat pointer.
            let len_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: recv_result.air_ref,
                    struct_id: slice_struct_id,
                    field_index: 1,
                },
                ty: Type::U64,
                span,
            });
            return Ok(AnalysisResult::new(len_ref, Type::U64));
        }

        Err(CompileError::new(
            ErrorKind::UndefinedMethod {
                method_name: method_name.to_string(),
                type_name: self.type_pool.struct_def(slice_struct_id).name.clone(),
            },
            span,
        ))
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
                    self.resolve_enum_through_module(*module_ref, *type_name, inst.span)?
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

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id,
                        variant_index: variant_index as u32,
                        payload_start: 0,
                        payload_len: 0,
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
    ///
    /// Also used by the module-member-call path for callees with comptime
    /// parameters, which must go through generic specialization (RUE-166).
    pub(crate) fn analyze_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // `print(s)` / `println(s)` are builtin free functions (RUE-1), not
        // user-defined ones: intercept them here before the function lookup,
        // but only when the program hasn't shadowed the name with its own
        // `fn print`/`fn println` (a user definition wins, keeping these names
        // unreserved).
        if (name == self.known.print || name == self.known.println)
            && !self.functions.contains_key(&name)
        {
            return self.analyze_print_builtin(air, name, args_start, args_len, span, ctx);
        }

        // Look up the function
        let fn_name_str = self.interner.resolve(&name).to_string();
        let fn_info = self
            .functions
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;

        // Visibility (E0460, RUE-37/RUE-180): an unqualified call must not
        // reach a private function defined in another directory — privacy is
        // uniform in every multi-file compilation, imports or not (spec
        // 10.3:7), so the flat namespace resolves the name but the callee
        // must be `pub` (or in the caller's directory).
        self.check_unqualified_visibility(
            "function",
            &fn_name_str,
            fn_info.file_id,
            fn_info.is_pub,
            span,
        )?;

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

        let args = self.rir.get_call_args(args_start, args_len);
        // Check argument count
        if args.len() != param_types.len() {
            let expected = param_types.len();
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
        for (i, (arg, expected_mode)) in args.iter().zip(param_modes.iter()).enumerate() {
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
                // Normal and comptime params accept any mode
                // (comptime params are substituted at compile time, not passed at runtime)
                RirParamMode::Normal | RirParamMode::Comptime => {
                    // Normal params accept any mode
                }
            }
        }

        // Extract info before any mutable borrow
        let is_generic = fn_info.is_generic;
        let param_types = param_types.to_vec();
        let param_comptime = param_comptime.to_vec();
        let param_names = param_names.to_vec();
        let return_type_sym = fn_info.return_type_sym;
        let base_return_type = fn_info.return_type;
        let fn_body = fn_info.body;
        let rir_params_start = fn_info.rir_params_start;
        let rir_params_len = fn_info.rir_params_len;

        // Special case: functions that return `type` with no parameters or only comptime parameters
        // are implicitly comptime and should be evaluated at compile time.
        // This handles both:
        //   - `fn SimpleType() -> type { struct { x: i32 } }`  (no params)
        //   - `fn FixedBuffer(comptime N: i32) -> type { struct { fn capacity(self) -> i32 { N } } }`
        let all_params_comptime = param_comptime.iter().all(|&c| c);
        if base_return_type == Type::COMPTIME_TYPE && (args.is_empty() || all_params_comptime) {
            // Build the substitutions for the callee body from its comptime
            // arguments: TYPE parameters (`comptime T: type`) into `type_subst`
            // and VALUE parameters (`comptime N: i32`) into `value_subst`. Both
            // are needed so a type constructor whose body mentions a type
            // parameter (`struct { value: T }`) reduces here — including when
            // this call is itself a nested argument (`WrapA(WrapA(i32))`), so
            // the inner call analyzes to a `TypeConst` (RUE-251).
            let mut type_subst: std::collections::HashMap<Spur, Type> =
                std::collections::HashMap::new();
            let mut value_subst: std::collections::HashMap<Spur, ConstValue> =
                std::collections::HashMap::new();
            for (i, is_comptime) in param_comptime.iter().enumerate() {
                if !*is_comptime {
                    continue;
                }
                // Evaluated in the calling function's context so comptime
                // parameters in scope and resolved types are visible.
                match self.try_evaluate_const_in_fn(args[i].value, ctx) {
                    Some(ConstValue::Type(t)) if param_types[i] == Type::COMPTIME_TYPE => {
                        type_subst.insert(param_names[i], t);
                    }
                    Some(const_val) if param_types[i] != Type::COMPTIME_TYPE => {
                        value_subst.insert(param_names[i], const_val);
                    }
                    _ => {}
                }
            }
            // Try to evaluate the function body at compile time. A hard error
            // raised while reducing the constructor (e.g. an unbounded
            // self-recursive `-> type` function exceeding the comptime depth
            // limit, RUE-261) must surface as its real diagnostic (E1200)
            // rather than being swallowed into a downstream link error, so use
            // the propagating reduction entry point.
            if let Some(ConstValue::Type(ty)) =
                self.eval_type_constructor_body(fn_body, &type_subst, &value_subst)?
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
                    let is_comptime_known = self.try_evaluate_const_in_fn(arg.value, ctx).is_some()
                        || self.is_comptime_type_var(arg.value, ctx)
                        || self.is_comptime_param_forward(arg.value, ctx);
                    if !is_comptime_known {
                        let param_name = self.interner.resolve(&param_names[i]).to_string();
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

        // Analyze all arguments. Slice parameters (ADR-0043, RUE-322) coerce a
        // `borrow arr` argument into a by-value fat pointer here.
        let air_args = self.analyze_call_args_coerced(air, &args, &param_types, ctx)?;

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
                    // Check if this is a type parameter (param type is ComptimeType)
                    // vs a value parameter (param type is i32, bool, etc.)
                    if param_types[i] == Type::COMPTIME_TYPE {
                        // This is a TYPE parameter - expect a TypeConst instruction
                        let inst = air.get(air_arg.value);
                        if let AirInstData::TypeConst(ty) = &inst.data {
                            type_args.push(*ty);
                            // Record the substitution: param_name -> concrete_type
                            type_subst.insert(param_names[i], *ty);
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
                        match self.try_evaluate_const_in_fn(args[i].value, ctx) {
                            Some(const_val) => {
                                value_args.push(const_val);
                                value_subst.insert(param_names[i], const_val);
                            }
                            None => {
                                let param_name = self.interner.resolve(&param_names[i]).to_string();
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeArgNotConst {
                                        param_name: param_name.clone(),
                                    },
                                    self.rir.get(args[i].value).span,
                                )
                                .with_help(format!(
                                    "parameter '{}' is declared as 'comptime' and requires \
                                     a compile-time known value",
                                    param_name
                                )));
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
            let rir_param_type_syms: Vec<Spur> = self
                .rir
                .get_params(rir_params_start, rir_params_len)
                .iter()
                .map(|p| p.ty)
                .collect();
            for (i, (air_arg, &is_comptime)) in
                air_args.iter().zip(param_comptime.iter()).enumerate()
            {
                let declared = param_types[i];
                if is_comptime && declared == Type::COMPTIME_TYPE {
                    // The comptime type argument itself - already validated above.
                    continue;
                }
                let expected = if declared == Type::COMPTIME_TYPE {
                    // Generic parameter like `x: T` or a composite mentioning a
                    // type parameter like `a: [T; 3]` (RUE-172) - substitute T.
                    // If the type parameter wasn't resolved (e.g. it's a local
                    // bound to a type value), the check happens after
                    // specialization instead.
                    let sym = rir_param_type_syms.get(i).copied();
                    match sym.and_then(|sym| {
                        self.resolve_type_for_comptime_with_subst_and_values(
                            sym,
                            &type_subst,
                            &value_subst,
                        )
                    }) {
                        Some(ty) => ty,
                        None => continue,
                    }
                } else {
                    declared
                };
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
                        self.rir.get(args[i].value).span,
                    ));
                }
            }

            // Determine the actual return type by substituting type parameters.
            // Handles bare type parameters (`-> T`), composites mentioning one
            // (`-> [T; 3]`, RUE-172), and the literal `type` return (which
            // resolves back to COMPTIME_TYPE and is comptime-evaluated below).
            let return_type = if base_return_type == Type::COMPTIME_TYPE {
                self.resolve_type_for_comptime_with_subst_and_values(
                    return_type_sym,
                    &type_subst,
                    &value_subst,
                )
                .unwrap_or(base_return_type)
            } else {
                base_return_type
            };

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
                    if *is_comptime && param_types[i] != Type::COMPTIME_TYPE {
                        // This is a comptime VALUE parameter - extract its const value
                        // (evaluated in the calling function's context)
                        if let Some(const_val) = self.try_evaluate_const_in_fn(args[i].value, ctx) {
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

            // Encode type arguments into extra array (as raw Type discriminants)
            let mut type_extra = Vec::with_capacity(type_args.len());
            for ty in &type_args {
                type_extra.push(ty.as_u32());
            }
            let type_args_start = air.add_extra(&type_extra);
            let type_args_len = type_args.len() as u32;

            // Encode comptime value arguments into extra array (as a tagged
            // word stream; the length is in words, not values)
            let value_words = crate::specialize::encode_const_values(&value_args);
            let value_args_start = air.add_extra(&value_words);
            let value_args_len = value_words.len() as u32;

            // Encode runtime args into extra array
            let mut args_extra = Vec::with_capacity(runtime_args.len() * 2);
            for arg in &runtime_args {
                args_extra.push(arg.value.as_u32());
                args_extra.push(arg.mode.as_u32());
            }
            let runtime_args_start = air.add_extra(&args_extra);
            let runtime_args_len = runtime_args.len() as u32;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::CallGeneric {
                    name,
                    type_args_start,
                    type_args_len,
                    value_args_start,
                    value_args_len,
                    args_start: runtime_args_start,
                    args_len: runtime_args_len,
                },
                ty: return_type,
                span,
            });
            Ok(AnalysisResult::new(air_ref, return_type))
        } else {
            // Regular non-generic call
            let return_type = base_return_type;

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
        self.enums.get(&type_name).map(|&id| (id, false))
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
                    args_start,
                    args_len,
                    span,
                    ctx,
                );
            }
        }

        // Delegate to the main implementation in analysis.rs
        self.analyze_assoc_fn_call_impl(air, type_name, function, args_start, args_len, span, ctx)
    }

    /// Analyze construction of an enum tuple variant with a payload
    /// (`Shape::Circle(5)`), producing an `EnumVariant` AIR value (RUE-221).
    #[allow(clippy::too_many_arguments)]
    fn analyze_enum_variant_construction(
        &mut self,
        air: &mut Air,
        enum_id: crate::types::EnumId,
        variant_index: u32,
        type_name: Spur,
        privacy_exempt: bool,
        args_start: u32,
        args_len: u32,
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

        let args = self.rir.get_call_args(args_start, args_len);

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

        // Analyze each payload argument and type-check against the declared
        // payload type (inference already constrained them; this is the final
        // legality check).
        let mut payload_refs: Vec<u32> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
            let expected = payload_types[i];
            let actual = arg_result.ty;
            if actual != expected && !actual.can_coerce_to(&expected) && actual != Type::ERROR {
                return Err(self.type_mismatch_error(
                    expected,
                    actual,
                    self.rir.get(arg.value).span,
                ));
            }
            payload_refs.push(arg_result.air_ref.as_u32());
        }

        let payload_start = air.add_extra(&payload_refs);
        let payload_len = payload_refs.len() as u32;
        let ty = Type::new_enum(enum_id);

        // Suppress unused-variable warnings for names only used in messages.
        let _ = (&variant_name, &enum_name);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id,
                variant_index,
                payload_start,
                payload_len,
            },
            ty,
            span,
        });
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

    /// Analyze `@offset_of(T, field)` (RUE-301): the compile-time byte offset of
    /// `field` within struct type `T`.
    ///
    /// The offset is computed from the layout the compiler assigns — the sum of
    /// the ABI slot counts of all preceding fields, times the 8-byte slot size
    /// (spec 3.6). This MUST match `struct_field_slot_offset` in
    /// `rue-codegen::types` (which multiplies the same preceding-field slot sum
    /// by 8 when addressing a field), so that `@offset_of(T, f)` and
    /// `@field_ptr(s.f)` agree with direct `s.f` access under any layout. The
    /// result is a comptime-known `u64`, mirroring Rust's
    /// `core::mem::offset_of!` (return type) and `@size_of`/`@align_of` (which
    /// likewise fold to a `Const` at analysis time).
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

        // Sum the slot counts of every field preceding `field`, then scale by
        // the 8-byte slot size. Cloning the field types first keeps the
        // immutable borrow of `struct_def` from colliding with `abi_slot_count`
        // (which borrows `self`).
        let preceding_field_types: Vec<Type> = struct_def
            .fields
            .iter()
            .take(field_index)
            .map(|f| f.ty)
            .collect();
        let slot_offset: u32 = preceding_field_types
            .iter()
            .map(|&fty| self.abi_slot_count(fty))
            .sum();
        let byte_offset = (slot_offset as u64) * 8;

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Const(byte_offset),
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
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
