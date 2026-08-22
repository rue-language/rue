//! Control-flow expression semantic analysis.
//!
//! This module owns branch, loop, match, try, return, and block analysis.
//! The methods operate directly on the shared body-analysis engine state so
//! control-flow lowering does not introduce a peer analysis context.

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use ahash::AHashMap;
use std::sync::Arc;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, OptionExt, WarningKind};
use rue_rir::{InstData, InstRef, RirPattern};
use rue_span::Span;

use super::analysis::FirstClassStrSite;
use super::anon_structs::TrustedTryProducer;
use super::context::{
    AnalysisContext, AnalysisResult, ConstValue, LocalVar, LoopEdgeStates, union_move_maps,
};
use crate::declaration_validation::{
    AccessorExitForm, AccessorMethodLink, AccessorYieldRootForm, accessor_method_link_error,
    accessor_yield_root_error,
};
use crate::inst::{Air, AirInst, AirInstData, AirPattern, AirPlaceBase, AirRef};
use crate::scope::ScopedContext;
use crate::types::{Type, TypeKind};

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Analyze a control flow instruction.
    ///
    /// Handles: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    pub(crate) fn analyze_control_flow(
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

            InstData::Match { scrutinee, arms } => {
                self.analyze_match(air, *scrutinee, arms, inst.span, ctx)
            }

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

                // Record the break against the innermost enclosing loop: it
                // is now `()`-typed instead of `!` (spec 4.8:17), and the
                // move state in force HERE is one of the loop's exit states —
                // union-merged with the other breaks' states, it becomes the
                // ownership state after the loop (RUE-1293; formal core §5.7,
                // (Loop-Break): Σ_exit = join over the at-break states).
                let break_depth = ctx.moved_scope_stack.len();
                let break_moves = ctx.moved_vars.clone();
                if let Some(record) = ctx.loop_break_stack.last_mut() {
                    record.record_break(&break_moves, break_depth);
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

                // The move state in force HERE rides the back edge into the
                // next iteration: record it so the loop's back-edge recheck
                // is seeded with it — the branch joins exclude this diverging
                // arm, so without the record a move on a continue path would
                // vanish and the next iteration could re-use the value
                // (RUE-1293, the continue-edge variant).
                let continue_depth = ctx.moved_scope_stack.len();
                let continue_moves = ctx.moved_vars.clone();
                if let Some(record) = ctx.loop_break_stack.last_mut() {
                    record.record_continue(&continue_moves, continue_depth);
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

            InstData::Yield(operand) => self.analyze_yield(air, *operand, inst_ref, inst.span, ctx),

            InstData::Block { instructions } => {
                self.analyze_block(air, instructions, inst.span, ctx)
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
                                    found: result
                                        .ty
                                        .safe_name_with_pool(Some(self.body_type_pool())),
                                },
                                self.body_rir_ref().get(block).span,
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
        let cond_result = ctx.with_expected_type(None, |ctx| self.analyze_inst(air, cond, ctx))?;

        if let Some(else_b) = else_block {
            // Save move state before entering branches.
            let saved_moves = ctx.moved_vars.clone();

            // Analyze then branch with its own scope
            ctx.push_scope();
            let then_result = self.analyze_inst(air, then_block, ctx)?;
            let then_type = then_result.ty;
            let then_span = self.body_rir_ref().get(then_block).span;
            ctx.pop_scope();

            // Capture then-branch's move state
            let then_moves = ctx.moved_vars.clone();

            // Restore to saved state before analyzing else branch
            ctx.moved_vars = saved_moves;

            // Analyze else branch with its own scope
            ctx.push_scope();
            let else_result = self.analyze_inst(air, else_b, ctx)?;
            let else_type = else_result.ty;
            let else_span = self.body_rir_ref().get(else_b).span;
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
                    if !self.types_equivalent(then_type, else_type)
                        && !then_type.is_error()
                        && !else_type.is_error()
                    {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: then_type
                                    .safe_name_with_pool(Some(self.body_type_pool())),
                                found: else_type.safe_name_with_pool(Some(self.body_type_pool())),
                            },
                            else_span,
                        )
                        .with_label(
                            format!(
                                "this is of type `{}`",
                                then_type.safe_name_with_pool(Some(self.body_type_pool()))
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
                        found: then_type.safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    self.body_rir_ref().get(then_block).span,
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
        ctx.loop_break_stack.push(LoopEdgeStates::default());
        let body_result = self.analyze_inst(air, body, ctx)?;
        ctx.loop_depth -= 1;
        // pop_scope replays this scope's RUE-522 restoration onto the
        // break-site snapshots still on the stack, so the record popped
        // afterwards describes the post-loop scope view.
        ctx.pop_scope();
        let (break_moves, continue_moves) = ctx
            .loop_break_stack
            .pop()
            .unwrap_or_default()
            .merged_moves();
        // The back edge carries the fall-through state joined with the
        // continue-site states — a continue re-enters the loop exactly like
        // falling off the body's end — while a break path never re-enters.
        // Keep the joined state for the recheck below (RUE-1293).
        let mut backedge_moves = ctx.moved_vars.clone();
        if let Some(continue_moves) = &continue_moves {
            backedge_moves = union_move_maps(&backedge_moves, continue_moves);
        }
        // A while loop's exits are the condition-false fall-through — whose
        // state on iterations past the first is the back-edge state — AND its
        // breaks; union in the at-break states so a move on a break path is
        // marked after the loop too (RUE-1293).
        ctx.moved_vars = match &break_moves {
            Some(break_moves) => union_move_maps(&backedge_moves, break_moves),
            None => backedge_moves.clone(),
        };

        // A while loop discards its body's result value on every iteration;
        // discarding a value that carries a linear value would implicitly
        // drop it (RUE-176).
        self.reject_discarded_linear_value(body_result.ty, body)?;

        // Loop back-edge move check: if the loop changed any move state,
        // re-run the analysis once with the post-body state as the starting
        // state. Any use of a value moved by a previous iteration then errors.
        // The scratch Air and context are discarded - this pass exists only
        // for the checks.
        if !ctx.in_loop_move_recheck && backedge_moves != moves_before_loop {
            let checkpoint = air.checkpoint();
            let mut scratch_ctx = ctx.fork_for_loop_recheck();
            // Seed the back-edge recheck with the back-edge state (the
            // fall-through joined with the continue states), not the exit
            // state: break-path moves never reach the back edge, and seeding
            // them would reject a value legitimately moved only on a path
            // that exits the loop (RUE-1293).
            scratch_ctx.moved_vars = backedge_moves;
            let recovered_before = self.body_analysis_recovered_errors_mut().len();
            let result = (|| -> CompileResult<()> {
                self.analyze_inst(air, cond, &mut scratch_ctx)?;
                scratch_ctx.push_scope();
                scratch_ctx.loop_depth += 1;
                scratch_ctx.loop_break_stack.push(LoopEdgeStates::default());
                self.analyze_inst(air, body, &mut scratch_ctx)?;
                scratch_ctx.loop_break_stack.pop();
                scratch_ctx.loop_depth -= 1;
                scratch_ctx.pop_scope();
                Ok(())
            })();
            air.rollback(checkpoint);
            for error in &mut self.body_analysis_recovered_errors_mut()[recovered_before..] {
                *error = error
                    .clone()
                    .with_note("value was moved in a previous iteration of the loop");
            }
            result
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
        ctx.loop_break_stack.push(LoopEdgeStates::default());
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
        ctx.loop_depth -= 1;
        // pop_scope replays this scope's RUE-522 restoration onto the
        // break-site snapshots still on the stack (see analyze_while_loop).
        ctx.pop_scope();
        let edge_record = ctx.loop_break_stack.pop().unwrap_or_default();
        let has_break = edge_record.broke;
        let (break_moves, continue_moves) = edge_record.merged_moves();
        // The back edge carries the fall-through state joined with the
        // continue-site states; keep it for the recheck below (RUE-1293).
        let mut backedge_moves = ctx.moved_vars.clone();
        if let Some(continue_moves) = &continue_moves {
            backedge_moves = union_move_maps(&backedge_moves, continue_moves);
        }
        // An infinite loop's only exits are its breaks, so the union of the
        // at-break states IS the exit ownership state — the back-edge state
        // re-enters the loop and never reaches the code after it (RUE-1293;
        // formal core §5.7, (Loop-Break)). A breakless loop is `!`-typed and
        // the code after it unreachable; its state is left as the
        // fall-through, which nothing can observe.
        if let Some(break_moves) = break_moves {
            ctx.moved_vars = break_moves;
        }

        // The loop discards its body's result value on every iteration;
        // discarding a value that carries a linear value would implicitly
        // drop it (RUE-176).
        self.reject_discarded_linear_value(body_result.ty, body)?;

        // Loop back-edge move check (see analyze_while_loop for details).
        // Note: like Rust, this is conservative - a body that unconditionally
        // breaks after the move still errors.
        if !ctx.in_loop_move_recheck && backedge_moves != moves_before_loop {
            let checkpoint = air.checkpoint();
            let mut scratch_ctx = ctx.fork_for_loop_recheck();
            // Seed with the back-edge state (fall-through joined with the
            // continue states), not the exit state: only the back edge's
            // moves reach the next iteration (RUE-1293).
            scratch_ctx.moved_vars = backedge_moves;
            let recovered_before = self.body_analysis_recovered_errors_mut().len();
            scratch_ctx.push_scope();
            scratch_ctx.loop_depth += 1;
            scratch_ctx.loop_break_stack.push(LoopEdgeStates::default());
            if let Some(var) = iter_borrow {
                scratch_ctx.iter_borrows.push(var);
            }
            let result = self.analyze_inst(air, body, &mut scratch_ctx);
            air.rollback(checkpoint);
            for error in &mut self.body_analysis_recovered_errors_mut()[recovered_before..] {
                *error = error
                    .clone()
                    .with_note("value was moved in a previous iteration of the loop");
            }
            result
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
        let ty_name = scrutinee_type.safe_name_with_pool(Some(self.body_type_pool()));
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

    /// Emit unreachable-pattern warnings (spec 4.7:20) for a `match` that the
    /// comptime-selection path is about to prune to its single selected arm.
    /// Pruning returns that arm's body without running the normal per-arm loop
    /// in [`Self::analyze_match`], which is where these warnings are otherwise
    /// produced; without this a comptime-specialized match silently accepts
    /// unreachable arms that a structurally identical ordinary match rejects
    /// (RUE-555). Only the wildcard / integer / boolean pattern shapes a
    /// prunable match can contain are inspected — an enum pattern makes the
    /// match non-prunable, so it takes the normal path — and no arm *body* is
    /// analyzed, honoring 4.14:19. `scrutinee_type` is the HM-resolved type,
    /// used only to canonicalize integer patterns for duplicate detection;
    /// every pattern was already range-checked by the caller, so the
    /// `check_pattern_int` below never errors here.
    ///
    /// The warning shapes and messages mirror the normal per-arm loop exactly,
    /// so a pruned match and an ordinary match report identical diagnostics.
    fn warn_unreachable_pruned_arms(
        &self,
        arms: impl Iterator<Item = (RirPattern, InstRef)>,
        scrutinee_type: Type,
        ctx: &mut AnalysisContext,
    ) {
        let mut wildcard_span: Option<Span> = None;
        let mut bool_true_span: Option<Span> = None;
        let mut bool_false_span: Option<Span> = None;
        let mut seen_ints: AHashMap<i64, Span> = AHashMap::new();
        for (pattern, _) in arms {
            let pattern_span = pattern.span();

            // Any arm after a wildcard is unreachable.
            if let Some(first_wildcard_span) = wildcard_span {
                let pat_str = match &pattern {
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
                    } => format!(
                        "{}.{}",
                        self.body_interner().resolve(&*type_name),
                        self.body_interner().resolve(&*variant)
                    ),
                };
                ctx.warnings.push(
                    CompileWarning::new(WarningKind::UnreachablePattern(pat_str), pattern_span)
                        .with_label("previous wildcard pattern here", first_wildcard_span)
                        .with_note(
                            "this pattern will never be matched because the wildcard pattern above matches everything",
                        ),
                );
                continue;
            }

            match &pattern {
                RirPattern::Wildcard(_) => {
                    // A `_` after both booleans are already covered is
                    // unreachable. An integer scrutinee is never fully covered
                    // by literals, and enum patterns can't reach this path.
                    if scrutinee_type == Type::BOOL
                        && bool_true_span.is_some()
                        && bool_false_span.is_some()
                    {
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
                RirPattern::Int {
                    value, negative, ..
                } => {
                    // Every pattern already passed the caller's range check, so
                    // this only recomputes the canonical value for dedup.
                    let Ok(n) =
                        self.check_pattern_int(*value, *negative, scrutinee_type, pattern_span)
                    else {
                        continue;
                    };
                    if let Some(first_span) = seen_ints.get(&n) {
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
                    } else {
                        seen_ints.insert(n, pattern_span);
                    }
                }
                RirPattern::Bool(b, _) => {
                    let first_span_opt = if *b {
                        &mut bool_true_span
                    } else {
                        &mut bool_false_span
                    };
                    if let Some(first_span) = *first_span_opt {
                        ctx.warnings.push(
                            CompileWarning::new(
                                WarningKind::UnreachablePattern(b.to_string()),
                                pattern_span,
                            )
                            .with_label("first occurrence of this pattern", first_span)
                            .with_note(
                                "this pattern will never be matched because an earlier arm already matches the same value",
                            ),
                        );
                    } else {
                        *first_span_opt = Some(pattern_span);
                    }
                }
                // Enum patterns can't appear in a prunable match (they set
                // `prunable = false` in the caller), so there's nothing to do.
                RirPattern::Path { .. } => {}
            }
        }
    }

    /// Analyze a match expression.
    fn analyze_match(
        &mut self,
        air: &mut Air,
        scrutinee: InstRef,
        arms: &rue_rir::RirMatchArmsRange,
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
                let arms = self
                    .body_rir_ref()
                    .match_arms(arms)
                    .iter()
                    .map(|(pattern, body)| (pattern.to_owned(), body))
                    .collect::<Vec<_>>();
                let mut selected: Option<InstRef> = None;
                let mut prunable = !arms.is_empty();
                let mut has_wildcard = false;
                let mut bool_true_covered = false;
                let mut bool_false_covered = false;
                for (pattern, body) in arms.iter() {
                    let matched = match (&pattern, &value) {
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
                        for (pattern, _) in arms.iter() {
                            if let RirPattern::Int {
                                value: magnitude,
                                negative,
                                ..
                            } = &pattern
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
                    // Unreachable-pattern diagnostics (spec 4.7:20) are a
                    // property of the pattern *set*, not of which arm the
                    // comptime value selects, so they must still fire even
                    // though we prune to a single body below (RUE-555). The
                    // normal per-arm loop that would otherwise emit them is
                    // skipped by the early return, so run them here. Only
                    // pattern shapes are inspected — no arm body is analyzed,
                    // honoring 4.14:19.
                    self.warn_unreachable_pruned_arms(arms.iter().cloned(), scrutinee_type, ctx);
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
        // fallible-intrinsic scrutinee can validate the pattern's `Option(T)`
        // against its exact registry-installed result (`match @read_line() {
        // Option::Some(l) => .., Option::None => .. }`, RUE-6). Context does not
        // select the intrinsic nominal. Resolution errors are ignored — pattern
        // legality is checked on the normal path below.
        let arms_for_expected = self
            .body_rir_ref()
            .match_arms(arms)
            .iter()
            .map(|(pattern, body)| (pattern.to_owned(), body))
            .collect::<Vec<_>>();
        let expected_scrutinee = arms_for_expected.iter().find_map(|(pattern, _)| {
            if let RirPattern::Path { type_name, .. } = &pattern {
                self.resolve_type_with_ctx(*type_name, span, ctx)
                    .ok()
                    .filter(|ty| ty.is_enum())
            } else {
                None
            }
        });
        // Analyze the scrutinee under only the pattern-derived contract. The
        // match expression's own result expectation belongs to its arms.
        let scrutinee_result = ctx.with_expected_type(expected_scrutinee, |ctx| {
            self.analyze_inst(air, scrutinee, ctx)
        })?;
        let scrutinee_type = scrutinee_result.ty;

        // Validate that we can match on this type (integers, booleans, and enums)
        if !scrutinee_type.is_integer() && scrutinee_type != Type::BOOL && !scrutinee_type.is_enum()
        {
            return Err(CompileError::new(
                ErrorKind::InvalidMatchType(
                    scrutinee_type.safe_name_with_pool(Some(self.body_type_pool())),
                ),
                span,
            ));
        }

        let arms = self
            .body_rir_ref()
            .match_arms(arms)
            .iter()
            .map(|(pattern, body)| (pattern.to_owned(), body))
            .collect::<Vec<_>>();
        // An empty match is only legal on a zero-variant (uninhabited) enum,
        // where zero arms vacuously satisfy exhaustiveness because the type
        // has no values (spec 4.7:26, RUE-169). The match can never be
        // reached with a value, so its type is `!` (spec 4.7:27).
        if arms.is_empty() {
            let is_uninhabited_enum = match scrutinee_type.try_kind() {
                Some(TypeKind::Enum(id)) => self.body_type_pool().enum_def(id).variant_count() == 0,
                _ => false,
            };
            if !is_uninhabited_enum {
                return Err(CompileError::new(ErrorKind::EmptyMatch, span));
            }
            let air_ref = air.add_match(scrutinee_result.air_ref, &[], Type::NEVER, span)?;
            return Ok(AnalysisResult::new(air_ref, Type::NEVER));
        }

        // Track patterns for exhaustiveness checking and duplicate detection
        let mut wildcard_span: Option<Span> = None;
        let mut bool_true_span: Option<Span> = None;
        let mut bool_false_span: Option<Span> = None;
        let mut seen_ints: AHashMap<i64, Span> = AHashMap::new();
        // Maps each covered enum-variant index to the span of its first arm, so a
        // second arm matching the same variant can be reported as unreachable
        // (mirroring seen_ints / bool_*_span). The map's len() still drives the
        // exhaustiveness check below, identically to the former HashSet.
        let mut covered_variants: AHashMap<u32, Span> = AHashMap::new();
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
                let pat_str = match &pattern {
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
                            "{}.{}",
                            self.body_interner().resolve(&*type_name),
                            self.body_interner().resolve(&*variant)
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
            match &pattern {
                RirPattern::Wildcard(_) => {
                    // A `_` arm after the preceding arms already cover every
                    // value (both bools, or every enum variant) is unreachable
                    // (spec 4.7:17 / 4.7:20, RUE-168).
                    if wildcard_span.is_none() {
                        let fully_covered = if scrutinee_type == Type::BOOL {
                            bool_true_span.is_some() && bool_false_span.is_some()
                        } else if let Some(enum_id) = pattern_enum_id {
                            covered_variants.len()
                                == self.body_type_pool().enum_def(enum_id).variant_count()
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
                                expected: scrutinee_type
                                    .safe_name_with_pool(Some(self.body_type_pool())),
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
                                expected: scrutinee_type
                                    .safe_name_with_pool(Some(self.body_type_pool())),
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
                    ctor_head,
                    type_name,
                    variant,
                    ..
                } => {
                    // Look up the enum type — through a module, unqualified /
                    // comptime-bound, or an inline type-constructor head — via
                    // the shared pattern-enum chokepoint.
                    let (enum_id, privacy_handled) = self
                        .resolve_pattern_enum(*module, *ctor_head, *type_name, ctx, pattern_span)?
                        .ok_or_compile_error(
                            ErrorKind::UnknownEnumType(
                                self.body_interner().resolve(&*type_name).to_string(),
                            ),
                            pattern_span,
                        )?;
                    // Privacy (E0460, RUE-185): a match pattern names the enum
                    // unqualified, so a private enum from another directory
                    // cannot be matched on — privacy is uniform across item
                    // kinds (spec 10.3:1, 10.3:7). Skipped when the name arrived
                    // through a module (E0706 already enforced) or a comptime
                    // binding (exempt); both are reported as `privacy_handled`.
                    if !privacy_handled {
                        let def = self.body_type_pool().enum_def(enum_id);
                        self.check_unqualified_visibility(
                            "enum",
                            self.body_interner().resolve(&*type_name),
                            def.file_id,
                            def.is_pub,
                            pattern_span,
                        )?;
                    }
                    let enum_def = self.body_type_pool().enum_def(enum_id);

                    // Check that scrutinee type matches the pattern's enum type
                    if !self.types_equivalent(scrutinee_type, Type::new_enum(enum_id)) {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type
                                    .safe_name_with_pool(Some(self.body_type_pool())),
                                found: enum_def.name.to_string(),
                            },
                            pattern_span,
                        ));
                    }

                    // Find the variant index
                    let variant_name = self.body_interner().resolve(&*variant);
                    let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                        ErrorKind::UnknownVariant {
                            enum_name: enum_def.name.to_string(),
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
                                "{}.{}",
                                self.body_interner().resolve(&*type_name),
                                self.body_interner().resolve(&*variant)
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

            // RUE-238: an arm that extracts NO payload — a wildcard `_` arm,
            // or an arm matching a discriminant-only variant — still
            // *consumes* the scrutinee (the match marked it moved; see the
            // `mark_moved` in the emitted AIR), so its active-variant payload
            // would leak. Emit a drop of the whole scrutinee value; for an
            // enum this lowers to the variant-dispatched drop glue
            // (`__rue_drop_E`), which drops exactly the active variant's
            // payload (a no-op when that variant carries nothing droppable).
            //
            // An arm on a payload-carrying variant must NOT also drop the
            // scrutinee, or its moved-out fields would be dropped twice.
            // `binding_stmts.is_empty()` is precisely that guard: since
            // RUE-1592 `materialize_match_bindings` moves out EVERY payload
            // position of such a variant — named, `_`-discarded, or covered by
            // the bare-path form `E.A` — so those arms are never empty and
            // account for the whole payload themselves, each field dropped at
            // the arm's end in reverse declaration order.
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

            // Payload bindings are ordinary locals living in the ARM scope,
            // not in the body's own block scope, so the block-exit
            // must-consume check never sees them (RUE-1603). Enforce the
            // linear obligation here, exactly as `analyze_block` does before
            // its pop: unconditionally (a diverging body discharges the
            // obligation only by actually consuming the value on that path),
            // per arm (this arm's own move state, before it is harvested
            // below). Unnameable positions (RUE-1592) are registered in no
            // scope and were already rejected up front when linear (E0486),
            // so this visits exactly the named bindings.
            self.check_unconsumed_linear_values(ctx)?;

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
                    } else if !self.types_equivalent(prev, body_type)
                        && !prev.is_error()
                        && !body_type.is_error()
                    {
                        // Point at the offending arm's body, not the whole match.
                        return Err(self.type_mismatch_error(
                            prev,
                            body_type,
                            self.body_rir_ref().get(*body).span,
                        ));
                    } else {
                        prev
                    }
                }
            });

            // Convert pattern to AIR pattern
            let air_pattern = match &pattern {
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
                    ctor_head,
                    type_name,
                    variant,
                    ..
                } => {
                    let type_name_str = self.body_interner().resolve(&*type_name).to_string();
                    let enum_id = self
                        .resolve_pattern_enum(*module, *ctor_head, *type_name, ctx, pattern_span)?
                        .map(|(id, _)| id)
                        .ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::InternalError(format!(
                                    "enum type '{}' not found during pattern conversion",
                                    type_name_str
                                )),
                                pattern_span,
                            )
                        })?;
                    let enum_def = self.body_type_pool().enum_def(enum_id);
                    let variant_name = self.body_interner().resolve(&*variant);
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
                let statements: Vec<_> = binding_stmts
                    .iter()
                    .copied()
                    .map(AirRef::from_raw)
                    .collect();
                air.add_block(&statements, body_result.air_ref, body_type, pattern_span)?
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
            let enum_def = self.body_type_pool().enum_def(enum_id);
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
                .map(|id| self.body_type_pool().enum_def(id));
            return Err(super::analysis::non_exhaustive_match_error(
                span,
                scrutinee_type,
                enum_def.as_deref().map(|entry| &**entry),
                |i| covered_variants.contains_key(&i),
                bool_true_covered,
                bool_false_covered,
            ));
        }

        let final_type = result_type.unwrap_or(Type::UNIT);

        let air_ref = air.add_match(scrutinee_result.air_ref, &air_arms, final_type, span)?;
        Ok(AnalysisResult::new(air_ref, final_type))
    }

    /// If `enum_id` names an `Option`-shaped enum — exactly two variants, a
    /// single-payload `Some(T)` and an empty `None` — return
    /// `(some_index, none_index, payload_type)`. Used by the `?` operator to
    /// recognise the in-scope library `Option` by structure and name
    /// (RUE-6, ADR-0038), rather than as a privileged builtin.
    fn option_enum_shape(&self, enum_id: crate::types::EnumId) -> Option<(u32, u32, Type)> {
        let def = self.body_type_pool().enum_def(enum_id);
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

    /// Recognize a `Result`-shaped enum (`Ok(T)` / `Err(E)`) by variant name and
    /// payload arity, mirroring [`Self::option_enum_shape`] (ADR-0038). Returns
    /// `(ok_idx, err_idx, ok_payload_ty, err_payload_ty)`.
    fn result_enum_shape(&self, enum_id: crate::types::EnumId) -> Option<(u32, u32, Type, Type)> {
        let def = self.body_type_pool().enum_def(enum_id);
        if def.variant_count() != 2 {
            return None;
        }
        let ok_idx = def.find_variant("Ok")?;
        let err_idx = def.find_variant("Err")?;
        let ok_payload = def.variant_payload(ok_idx);
        let err_payload = def.variant_payload(err_idx);
        if ok_payload.len() == 1 && err_payload.len() == 1 {
            Some((ok_idx as u32, err_idx as u32, ok_payload[0], err_payload[0]))
        } else {
            None
        }
    }

    /// Analyze the `?` operator (RUE-6, ADR-0038, RUE-1112).
    ///
    /// `operand?` requires `operand` to be an exact specialization of a trusted
    /// std producer (`std/option.rue::Option` or `std/result.rue::Result`) and
    /// the enclosing function to return an exact specialization of the *same*
    /// trusted producer (an `Option` success payload may differ; a `Result`
    /// keeps the exact error type). Legality is producer identity, not shape: a
    /// same-shape user lookalike gets no `?` behavior. For the Option form it
    /// evaluates to `T` on `Some(v)` and early-returns the enclosing function's
    /// `None`; this is the desugaring `match operand { Some(v) => v, None =>
    /// return None }`, built directly against the resolved enum types (so no
    /// source type name is needed): a two-arm discriminant `Match` whose failure
    /// arm returns.
    fn analyze_try(
        &mut self,
        air: &mut Air,
        operand: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // A `?` early return is a non-diverging exit that bypasses an
        // accessor body's single trailing `yield` (ADR-0062 phase 1).
        if ctx.accessor_trailing_yield.is_some() {
            return Err(CompileError::new(
                crate::declaration_validation::accessor_exit_error(AccessorExitForm::Try),
                span,
            ));
        }
        let return_type = ctx.return_type;

        // Analyze the operand first, then dispatch on ITS shape (Option vs
        // Result); the enclosing return type must match (ADR-0038).
        //
        // A BARE fallible-intrinsic operand — `@read_line()?` / `@parse_i64(s)?`
        // — is special: the intrinsic needs its exact `Option` return type and
        // the `?` site cannot supply it as an `expected_type` (RUE-318), so we
        // clear `expected_type` and set `try_operand` so the intrinsic uses its
        // exact registry-installed fixed-payload result. A non-intrinsic operand
        // ignores both flags — its type is resolved independently.
        let prev_expected = ctx.expected_type.take();
        let prev_try_operand = ctx.try_operand;
        ctx.try_operand = true;
        let operand_outcome = self.analyze_inst(air, operand, ctx);
        ctx.expected_type = prev_expected;
        ctx.try_operand = prev_try_operand;
        let operand_result = operand_outcome?;
        let operand_ty = operand_result.ty;

        if operand_ty.is_error() || return_type.is_error() {
            return Ok(AnalysisResult::new(operand_result.air_ref, Type::ERROR));
        }

        // RUE-1112: `?` legality is exact trusted-producer identity, not shape.
        // The operand must be an exact specialization of the trusted
        // std `Option` or `Result`; a same-shape user lookalike is an ordinary
        // enum with no `?` behavior. `trusted_try_producer` compares the
        // operand's producer key against the well-known trusted identity and
        // never materializes std to reject a lookalike. The
        // `option_enum_shape`/`result_enum_shape` helpers survive only to read
        // the confirmed-trusted producer's own `Some(T)`/`None` or
        // `Ok(T)`/`Err(E)` layout.
        let non_option = |sema: &Self| {
            CompileError::new(
                ErrorKind::QuestionOnNonOption {
                    found: operand_ty.safe_name_with_pool(Some(sema.body_type_pool())),
                },
                span,
            )
        };
        match self.trusted_try_producer(operand_ty) {
            Some(TrustedTryProducer::Option) => {
                let operand_enum_id = operand_ty
                    .as_enum()
                    .expect("a trusted Option producer is an enum type");
                let Some((some_idx, none_idx, payload_ty)) =
                    self.option_enum_shape(operand_enum_id)
                else {
                    return Err(non_option(self));
                };
                // The enclosing function must return an exact std `Option`
                // specialization too; the success payload may differ (4.15:4).
                let ret_shape = return_type.as_enum().and_then(|rid| {
                    (self.trusted_try_producer(return_type) == Some(TrustedTryProducer::Option))
                        .then(|| self.option_enum_shape(rid).map(|(_, n, _)| (rid, n)))
                        .flatten()
                });
                let (ret_enum_id, ret_none_idx) = match ret_shape {
                    Some(s) => s,
                    None => {
                        return Err(CompileError::new(
                            ErrorKind::QuestionOutsideOptionFn {
                                return_type: return_type
                                    .safe_name_with_pool(Some(self.body_type_pool())),
                            },
                            span,
                        ));
                    }
                };
                self.build_try_desugar(
                    air,
                    operand_result.air_ref,
                    operand_enum_id,
                    some_idx,
                    none_idx,
                    payload_ty,
                    return_type,
                    ret_enum_id,
                    ret_none_idx,
                    None,
                    span,
                )
            }
            Some(TrustedTryProducer::Result) => {
                let operand_enum_id = operand_ty
                    .as_enum()
                    .expect("a trusted Result producer is an enum type");
                let Some((ok_idx, err_idx, ok_ty, err_ty)) =
                    self.result_enum_shape(operand_enum_id)
                else {
                    return Err(non_option(self));
                };
                // The enclosing function must return an exact std `Result`
                // specialization; the error type must match exactly (ADR-0038,
                // no conversion).
                let ret_shape = return_type.as_enum().and_then(|rid| {
                    (self.trusted_try_producer(return_type) == Some(TrustedTryProducer::Result))
                        .then(|| {
                            self.result_enum_shape(rid)
                                .map(|(_, e, _, et)| (rid, e, et))
                        })
                        .flatten()
                });
                let (ret_enum_id, ret_err_idx, ret_err_ty) = match ret_shape {
                    Some(s) => s,
                    None => {
                        return Err(CompileError::new(
                            ErrorKind::QuestionOutsideResultFn {
                                return_type: return_type
                                    .safe_name_with_pool(Some(self.body_type_pool())),
                            },
                            span,
                        ));
                    }
                };
                if !self.types_equivalent(err_ty, ret_err_ty) {
                    return Err(CompileError::new(
                        ErrorKind::QuestionErrTypeMismatch {
                            operand_err: err_ty.safe_name_with_pool(Some(self.body_type_pool())),
                            fn_err: ret_err_ty.safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        span,
                    ));
                }
                self.build_try_desugar(
                    air,
                    operand_result.air_ref,
                    operand_enum_id,
                    ok_idx,
                    err_idx,
                    ok_ty,
                    return_type,
                    ret_enum_id,
                    ret_err_idx,
                    Some(err_ty),
                    span,
                )
            }
            // A non-enum operand, or a same-shape user lookalike: no `?`
            // behavior (4.15:3, E0504).
            None => Err(non_option(self)),
        }
    }

    /// Build the `match`-desugaring shared by the `?` operator's Option and
    /// Result forms: a two-arm discriminant match whose success arm reads the
    /// payload out and whose failure arm early-`return`s.
    ///
    /// `fail_err_ty` distinguishes the two: `None` is the Option case (the
    /// failure arm drops the scrutinee and returns the nullary `None`);
    /// `Some(err_ty)` is the Result case (the failure arm moves the error
    /// payload out and returns `Err(e)`).
    #[allow(clippy::too_many_arguments)]
    fn build_try_desugar(
        &mut self,
        air: &mut Air,
        scrutinee: AirRef,
        operand_enum_id: crate::types::EnumId,
        success_idx: u32,
        fail_idx: u32,
        success_payload_ty: Type,
        return_type: Type,
        ret_enum_id: crate::types::EnumId,
        ret_fail_idx: u32,
        fail_err_ty: Option<Type>,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Success arm (`Some(v)` / `Ok(v)`): read the payload out; it is the
        // whole `?`-expression's value (RUE-221).
        let success_body = air.add_inst(AirInst {
            data: AirInstData::EnumPayloadGet {
                base: scrutinee,
                enum_id: operand_enum_id,
                variant_index: success_idx,
                field_index: 0,
            },
            ty: success_payload_ty,
            span,
        });

        // Failure arm: build the early `return`.
        let (fail_stmt, fail_ret_value) = match fail_err_ty {
            None => {
                // Option `None`: drop the scrutinee (nullary variant, no-op glue)
                // and return the enclosing `None`.
                let drop_scrutinee = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: scrutinee },
                    ty: Type::UNIT,
                    span,
                });
                let none_ctor =
                    air.add_enum_variant(ret_enum_id, ret_fail_idx, &[], return_type, span)?;
                (drop_scrutinee, none_ctor)
            }
            Some(err_ty) => {
                // Result `Err(e)`: move the error payload out of the scrutinee
                // and return `Err(e)` of the enclosing function's Result type.
                let err_val = air.add_inst(AirInst {
                    data: AirInstData::EnumPayloadGet {
                        base: scrutinee,
                        enum_id: operand_enum_id,
                        variant_index: fail_idx,
                        field_index: 0,
                    },
                    ty: err_ty,
                    span,
                });
                let err_ctor =
                    air.add_enum_variant(ret_enum_id, ret_fail_idx, &[err_val], return_type, span)?;
                (err_val, err_ctor)
            }
        };
        let ret = air.add_inst(AirInst {
            data: AirInstData::Ret(Some(fail_ret_value)),
            ty: Type::NEVER,
            span,
        });
        let fail_body = air.add_block(&[fail_stmt], ret, Type::NEVER, span)?;

        // Encode the two arms and emit the dispatching match. Its value type is
        // the success payload (the failure arm diverges).
        let air_arms = [
            (
                AirPattern::EnumVariant {
                    enum_id: operand_enum_id,
                    variant_index: success_idx,
                },
                success_body,
            ),
            (
                AirPattern::EnumVariant {
                    enum_id: operand_enum_id,
                    variant_index: fail_idx,
                },
                fail_body,
            ),
        ];
        let air_ref = air.add_match(scrutinee, &air_arms, success_payload_ty, span)?;
        Ok(AnalysisResult::new(air_ref, success_payload_ty))
    }

    /// Resolve the enum a `RirPattern::Path` names — through a module
    /// (`m.Enum.Variant`, whose visibility is checked as E0706 by
    /// `resolve_enum_through_module`) or unqualified / comptime-bound
    /// (`Enum.Variant`, or `O.Variant` where `O` is a bound comptime type,
    /// RUE-6). Returns `(enum_id, privacy_handled)`: `privacy_handled` is true
    /// when visibility has already been enforced (module path) or does not apply
    /// (a comptime-bound type arrived through a binding, not by naming the
    /// enum), so a caller runs the unqualified E0460 check only when it is
    /// false. `None` means the unqualified name is not an enum — the caller
    /// supplies the not-found diagnostic it wants (a user error at the legality
    /// check, an internal error during lowering).
    ///
    /// This is the single chokepoint the pattern-enum consumers funnel through
    /// (legality check, pattern→AIR lowering, payload-binding materialization),
    /// replacing three copy-pasted `if module { … } else { … }` blocks. Folding
    /// them here also makes the inline type-constructor pattern head
    /// (`Result(i32, i32).Ok(v)`, RUE-596) a localized follow-up: it need only
    /// teach this one method to comptime-evaluate a constructor-call head.
    fn resolve_pattern_enum(
        &mut self,
        module: Option<InstRef>,
        ctor_head: Option<InstRef>,
        type_name: Spur,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<Option<(crate::types::EnumId, bool)>> {
        // Inline type-constructor pattern head `F(args).Variant(..)` (RUE-596,
        // spec 4.14:23): comptime-evaluate the constructor call to a concrete
        // type (no AIR emitted) and take its enum. The head arrived through a
        // call, not by naming the enum, so it is privacy-exempt — reported as
        // `privacy_handled = true`, exactly like a comptime binding.
        if let Some(head_ref) = ctor_head {
            // Unlike the inference prepass, this is the authoritative pattern
            // resolver. Preserve hard source diagnostics from comptime call
            // reduction (including exact argument-mode errors) instead of
            // degrading them into an unrelated unknown-enum failure.
            let mut env = super::comptime_eval::ComptimeEnv::for_analysis(ctx);
            return Ok(match self.eval_const_expr(head_ref, &mut env)? {
                Some(ConstValue::Type(ty)) => ty.as_enum().map(|id| (id, true)),
                _ => None,
            });
        }
        if let Some(module_ref) = module {
            let enum_id = self.resolve_enum_through_module(module_ref, type_name, span, ctx)?;
            Ok(Some((enum_id, true)))
        } else {
            Ok(self.resolve_enum_type_name(type_name, ctx))
        }
    }

    /// Materialize the payload bindings of a tuple-variant match pattern
    /// (`Circle(r)`) into fresh locals in the current (arm) scope, returning
    /// the AIR statement refs (StorageLive + Alloc per binding) that must run
    /// before the arm body (RUE-221, ADR-0038).
    ///
    /// EVERY payload position of a payload-carrying variant is materialized,
    /// including the ones that bind no source name (RUE-1592, ratified
    /// 2026-08-22). A `_` position — and every position of the all-wildcard
    /// *bare* variant pattern `E.A` (spec 4.7:30's bare-path carve-out) —
    /// becomes a fresh **unnameable** binding, exactly as the formal core's §2
    /// elaboration note states: it is registered in no scope, so nothing can
    /// name or consume it, but it owns a real frame slot and is therefore
    /// dropped by the ordinary §5.6 scope-exit machinery at the ARM'S END,
    /// interleaved with its named siblings in reverse declaration order.
    /// A linear such field can never discharge its must-consume obligation,
    /// so it is rejected here (E0486).
    ///
    /// Returns an empty vector only for a pattern that is not a path pattern,
    /// or for a discriminant-only variant (no payload to materialize) — the
    /// two cases where the caller's whole-scrutinee drop still applies.
    /// Assumes the pattern has already been validated against the scrutinee
    /// type by the caller (`analyze_match`); re-resolves the enum for
    /// convenience.
    fn materialize_match_bindings(
        &mut self,
        air: &mut Air,
        pattern: &RirPattern,
        scrutinee_ref: AirRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<u32>> {
        let RirPattern::Path {
            module,
            ctor_head,
            type_name,
            variant,
            bindings,
            span,
        } = pattern
        else {
            return Ok(Vec::new());
        };
        let pattern_span = *span;

        let enum_id = self
            .resolve_pattern_enum(*module, *ctor_head, *type_name, ctx, pattern_span)?
            .map(|(id, _)| id)
            .ok_or_compile_error(
                ErrorKind::UnknownEnumType(self.body_interner().resolve(&*type_name).to_string()),
                pattern_span,
            )?;
        let def = self.body_type_pool().enum_def(enum_id);
        let variant_name = self.body_interner().resolve(&*variant).to_string();
        let variant_index = def.find_variant(&variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: def.name.to_string(),
                variant_name: variant_name.clone(),
            },
            pattern_span,
        )? as u32;
        let enum_name = def.name.to_string();
        let payload = def.variant_payload(variant_index as usize).to_vec();

        // The bare-path carve-out (spec 4.7:30, RUE-1592): a payload-carrying
        // variant written with no binding list at all (`E.A`) IS the
        // all-wildcard form `E.A(_, …, _)`, so it is exempt from the arity
        // rule rather than being an arity error. Every other pattern must
        // supply exactly as many binding positions as the variant's arity —
        // `E.A(x, y)` on an arity-1 variant stays E0207.
        let bare_path = bindings.is_empty();
        if !bare_path && bindings.len() != payload.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: payload.len(),
                    found: bindings.len(),
                },
                pattern_span,
            ));
        }

        // A discriminant-only variant has nothing to materialize. The caller's
        // whole-scrutinee drop (the RUE-238 guard) covers such an arm, exactly
        // as it covers a bare `_` arm.
        if payload.is_empty() {
            return Ok(Vec::new());
        }

        // Every payload binding must be a fresh name (spec 4.7:30). Reusing an
        // identifier — `Rect(w, w)` — silently shadows the earlier binding and
        // discards its value, so reject it (E0484, analogous to Rust E0416)
        // rather than losing a field (RUE-269). The `_` discard (RUE-601) is
        // exempt: it binds nothing, so any number may repeat (`Rect(_, _)`).
        for (i, name) in bindings.iter().enumerate() {
            if self.body_interner().resolve(name) == "_" {
                continue;
            }
            if bindings.iter().take(i).any(|existing| existing == name) {
                return Err(CompileError::new(
                    ErrorKind::DuplicatePatternBinding {
                        name: self.body_interner().resolve(name).to_string(),
                    },
                    pattern_span,
                ));
            }
        }

        // Every payload position — named, `_`-discarded, or covered by the
        // bare-path form — is moved out of the scrutinee into its own frame
        // slot, so the arm's binding statements always suppress the
        // whole-scrutinee drop (the RUE-238 guard at the call site) and the
        // payload is accounted for field by field. An enum's drop glue is
        // exactly "drop the active variant's payload" (6.3:20), so the two
        // accountings coincide; what differs is *when* and in what order,
        // which is the RUE-1592 ruling below.
        let mut stmts: Vec<u32> = Vec::with_capacity(payload.len() * 2);
        for (i, field_ty) in payload.iter().copied().enumerate() {
            // The source name at this position, or `None` when the position
            // binds nothing: an explicit `_` discard (RUE-601), or any
            // position of the bare-path all-wildcard form `E.A` (RUE-1592).
            let binding_name = if bare_path {
                None
            } else {
                let name = bindings[i];
                (self.body_interner().resolve(&name) != "_").then_some(name)
            };

            // A non-binding position is a fresh UNNAMEABLE binding (formal
            // core §2; RUE-1592, ratified 2026-08-22) — not an eager
            // extract-and-drop (the pre-RUE-1592 behavior), and not a leak
            // into the scrutinee's own drop. It owns a slot like any other
            // binding, so §5.6 drops it at the ARM'S END in reverse
            // declaration order, interleaved with its named siblings.
            //
            // Nothing can name it, so a LINEAR field here could never
            // discharge its must-consume obligation (3.8:52). Rather than let
            // the ordinary machinery report an unnameable binding, reject the
            // position up front with a diagnostic that names the variant and
            // the position (E0486) and points at the escape hatches.
            if binding_name.is_none() && self.type_requires_consumption(field_ty) {
                let position = format!("field {i} of `{enum_name}.{variant_name}`");
                let err = CompileError::new(
                    ErrorKind::LinearPayloadDiscarded {
                        position,
                        type_name: field_ty.safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    pattern_span,
                )
                .with_help(if bare_path {
                    "bind the payload — `Enum.Variant(x, ...)` — and consume each linear field, \
                     or `@drop` it"
                } else {
                    "replace `_` with a name and consume the value, or `@drop` it"
                });
                return Err(self.attach_infectious_linear_note(err, field_ty));
            }

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

            // Allocate the binding through the canonical local-storage owner,
            // then register the match arm's source-level name in this scope.
            // An unnameable binding skips only that registration: it is in no
            // scope, so no expression can refer to it, while its slot still
            // participates in scope-exit drop elaboration.
            let (slot, storage_live, alloc) =
                self.allocate_local_storage(air, get_ref, field_ty, pattern_span, ctx)?;
            if let Some(binding_name) = binding_name {
                ctx.insert_local(
                    binding_name,
                    LocalVar {
                        slot,
                        ty: field_ty,
                        is_mut: false,
                        span: pattern_span,
                        allow_unused: false,
                    },
                );
            }

            stmts.push(storage_live.as_u32());
            stmts.push(alloc.as_u32());
        }

        Ok(stmts)
    }

    /// Analyze a return statement.
    /// Analyze a `yield` — the exit form of a place-returning accessor body
    /// (ADR-0062). Outside an accessor body it is E0256; inside one, only the
    /// body's single trailing `yield` is legal (E0254 otherwise). The
    /// trailing `yield` of an *inlined* accessor body never reaches this
    /// dispatch — call-site expansion consumes it directly as a place — so
    /// this arm covers the standalone compilation of the accessor itself,
    /// where the yielded place is validated and the exit lowers to an
    /// unreachable trap (every real call site inlines the body; no
    /// out-of-line accessor is ever invoked, which is what keeps this
    /// construct free of any new call shape or ABI).
    fn analyze_yield(
        &mut self,
        air: &mut Air,
        operand: InstRef,
        inst_ref: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let Some(trailing) = ctx.accessor_trailing_yield else {
            return Err(CompileError::new(ErrorKind::YieldOutsideAccessor, span));
        };
        if trailing != inst_ref {
            return Err(CompileError::new(
                crate::declaration_validation::accessor_exit_error(AccessorExitForm::SecondYield),
                span,
            ));
        }

        // Trusted std accessors may yield the checked pointer-to-place bridge.
        // Analyze the checked expression normally (so checked-depth and all
        // pointer rules remain authoritative), then retain its pointer value
        // as an indirect AIR place base. Mandatory accessor splicing embeds
        // that ordinary place in the caller CFG; no accessor ABI is involved.
        let bridge = match &self.body_rir_ref().get(operand).data {
            InstData::Checked { expr } => matches!(
                &self.body_rir_ref().get(*expr).data,
                InstData::Intrinsic { name, .. } if *name == self.known_symbols().place
            ),
            _ => false,
        };
        if bridge {
            let pointer = self.analyze_inst(air, operand, ctx)?;
            let pointee = if let Some(id) = pointer.ty.as_ptr_const() {
                self.body_type_pool().ptr_const_def(id)
            } else if let Some(id) = pointer.ty.as_ptr_mut() {
                self.body_type_pool().ptr_mut_def(id)
            } else {
                return Err(CompileError::new(
                    accessor_yield_root_error(&AccessorYieldRootForm::Value),
                    span,
                ));
            };
            let place = air.make_place(AirPlaceBase::Indirect(pointer.air_ref), pointee, [])?;
            let read = air.add_inst(crate::AirInst {
                data: crate::AirInstData::PlaceRead { place },
                ty: pointee,
                span,
            });
            if !ctx.return_type.is_error() && !self.types_compatible(pointee, ctx.return_type) {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx
                            .return_type
                            .safe_name_with_pool(Some(self.body_type_pool())),
                        found: pointee.safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    span,
                ));
            }
            return Ok(AnalysisResult::new(read, pointee));
        }

        // The yielded place must be a projection chain rooted at the receiver
        // parameter (E0255). Checked syntactically before the operand is read
        // so a local or temporary is named as such rather than surfacing as a
        // downstream ownership error.
        self.check_yield_rooted_at_receiver(operand, ctx)?;

        // Preserve the yielded receiver projection as the accessor CFG's
        // distinguished return operand. The mandatory CFG splice consumes
        // this `PlaceRead` as a place descriptor before codegen; no accessor
        // return ABI exists (RUE-1208).
        let trace = self.try_trace_place(operand, air, ctx)?.ok_or_else(|| {
            CompileError::new(
                accessor_yield_root_error(&AccessorYieldRootForm::Value),
                span,
            )
        })?;
        let ty = trace.result_type();
        let place = Self::build_place_ref(air, &trace)?;
        let read = AnalysisResult::new(
            air.add_inst(crate::AirInst {
                data: crate::AirInstData::PlaceRead { place },
                ty,
                span,
            }),
            ty,
        );
        if !ctx.return_type.is_error()
            && !read.ty.is_error()
            && !self.types_compatible(read.ty, ctx.return_type)
        {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: ctx
                        .return_type
                        .safe_name_with_pool(Some(self.body_type_pool())),
                    found: read.ty.safe_name_with_pool(Some(self.body_type_pool())),
                },
                span,
            ));
        }

        Ok(read)
    }

    /// Walk a yield operand's projection chain to its root and require that
    /// root to be the receiver parameter `self` (ADR-0062, E0255). A nested
    /// method-call link is legal only when it is itself an accessor.
    fn check_yield_rooted_at_receiver(
        &mut self,
        operand: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<()> {
        let self_sym = self.body_interner().get_or_intern("self");
        let mut current = operand;
        loop {
            let inst = self.body_rir_ref().get(current);
            let span = inst.span;
            match &inst.data {
                InstData::VarRef { name, .. } => {
                    if *name == self_sym {
                        return Ok(());
                    }
                    let root =
                        AccessorYieldRootForm::Named(Arc::from(self.body_interner().resolve(name)));
                    return Err(CompileError::new(accessor_yield_root_error(&root), span));
                }
                InstData::FieldGet { base, .. } => current = *base,
                InstData::IndexGet { base, .. } => current = *base,
                InstData::MethodCall {
                    receiver, method, ..
                } => {
                    // The demanded path holds the receiver's resolved type, so
                    // it decides every link: an unresolvable callee here is a
                    // plain method as far as 6.6:7 is concerned, since a
                    // legal link has to name an accessor.
                    let resolved_method = ctx
                        .resolved_type_of(*receiver)
                        .and_then(|ty| ty.as_struct())
                        .and_then(|struct_id| {
                            self.call_facts().call_method_info(struct_id, *method)
                        });
                    let is_accessor = resolved_method
                        .is_some_and(|info| info.returns_borrow || info.returns_inout);
                    let link = if is_accessor {
                        AccessorMethodLink::Accessor
                    } else {
                        AccessorMethodLink::PlainMethod
                    };
                    if let Some(kind) = accessor_method_link_error(link) {
                        return Err(CompileError::new(kind, span));
                    }
                    current = *receiver;
                }
                _ => {
                    return Err(CompileError::new(
                        accessor_yield_root_error(&AccessorYieldRootForm::Value),
                        span,
                    ));
                }
            }
        }
    }

    fn analyze_return(
        &mut self,
        air: &mut Air,
        inner: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // An accessor body has exactly one exit form — its trailing `yield`
        // (ADR-0062 phase 1); an early `return` would be a non-diverging exit
        // that bypasses it.
        if ctx.accessor_trailing_yield.is_some() {
            return Err(CompileError::new(
                crate::declaration_validation::accessor_exit_error(AccessorExitForm::Return),
                span,
            ));
        }
        let inner_air_ref = if let Some(inner) = inner {
            // Explicit return with value. A `str`-returning function
            // (ADR-0043 Phase 3, RUE-324) supplies `str` as the expected type so
            // a string-literal `return "..."` materializes as a static-backed,
            // first-class `str` (it cannot dangle, so returning it is sound).
            let ret_ty = ctx.return_type;
            let inner_result = if self.is_str_like(ret_ty) {
                let prev_expected = ctx.expected_type.replace(ret_ty);
                let r = self.analyze_inst(air, inner, ctx);
                ctx.expected_type = prev_expected;
                r?
            } else {
                self.analyze_inst(air, inner, ctx)?
            };
            let inner_ty = inner_result.ty;

            // Two-types model (ADR-0043, RUE-386): an explicit `return <expr>;`
            // in a `str`-returning function must return a first-class `str`,
            // not a buffer or a borrowed view (which would dangle after its
            // storage is dropped). Checked before the generic type-mismatch so
            // the targeted E0495/E0497 wins over E0206.
            if self.is_str_struct(ctx.return_type) {
                self.reject_non_first_class_str(
                    inner,
                    inner_ty,
                    FirstClassStrSite::Return,
                    span,
                    ctx,
                )?;
            }

            // An accessor result is a second-class borrowed place scoped to
            // its full expression (ADR-0062); returning it would let the
            // loan escape the receiver access that justifies it.
            self.reject_accessor_result_escape(
                inner,
                super::analysis::AccessorEscapeSite::Return,
                span,
                ctx,
            )?;

            // Type check: returned value must match function's return type.
            if !ctx.return_type.is_error()
                && !inner_ty.is_error()
                && !self.types_compatible(inner_ty, ctx.return_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx
                            .return_type
                            .safe_name_with_pool(Some(self.body_type_pool())),
                        found: inner_ty.safe_name_with_pool(Some(self.body_type_pool())),
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
                        expected: ctx
                            .return_type
                            .safe_name_with_pool(Some(self.body_type_pool())),
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
        instructions: &rue_rir::RirBlockInstsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Get the instruction refs from extra data
        let inst_refs = self.body_rir_ref().block_insts(instructions).to_vec();

        // Push a new scope for this block.
        ctx.push_scope();

        // Process all instructions in the block
        let mut statements = Vec::new();
        let mut last_result: Option<AnalysisResult> = None;
        let num_insts = inst_refs.len();
        for (i, inst_ref) in inst_refs.iter().copied().enumerate() {
            let is_last = i == num_insts - 1;
            // Statement recovery starts after body-wide inference has
            // succeeded. Inference failures use the exact selections observed
            // during constraint generation instead; substitution-dependent
            // selections make that transaction non-terminal.
            let recovery_checkpoint = self
                .body_analysis_error_recovery()
                .then(|| (air.checkpoint(), ctx.clone()));
            // Each non-tail statement is a full expression: accessor-result
            // loans taken inside it (ADR-0062) end when it does. Loans taken
            // by an *enclosing* statement (this block nested inside an
            // expression) stay live across it, so this truncates rather than
            // clears. The tail expression's loans are part of the enclosing
            // full expression and survive the block.
            let expression_loans_before = ctx.expression_loans.len();
            let expression_shared_reads_before = ctx.expression_shared_reads.len();
            let outcome = if is_last {
                self.analyze_inst(air, inst_ref, ctx)
            } else {
                ctx.with_expected_type(None, |ctx| self.analyze_inst(air, inst_ref, ctx))
            };
            if !is_last {
                ctx.expression_loans.truncate(expression_loans_before);
                ctx.expression_shared_reads
                    .truncate(expression_shared_reads_before);
            }
            let result = match outcome {
                Ok(result) => result,
                Err(error) if self.body_analysis_error_recovery() => {
                    let (air_checkpoint, ctx_checkpoint) = recovery_checkpoint
                        .expect("body-analysis recovery checkpoint must accompany recovery mode");
                    air.rollback(air_checkpoint);
                    *ctx = ctx_checkpoint;
                    self.body_analysis_recovered_errors_mut().push(error);
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::UnitConst,
                        ty: Type::ERROR,
                        span: self.body_rir_ref().get(inst_ref).span,
                    });
                    AnalysisResult::new(air_ref, Type::ERROR)
                }
                Err(error) => return Err(error),
            };

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
            let air_ref = air.add_block(&statements, last.air_ref, ty, span)?;
            Ok(AnalysisResult::new(air_ref, ty))
        }
    }
}
