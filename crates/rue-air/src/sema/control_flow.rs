//! Control-flow expression semantic analysis.
//!
//! This module owns branch, loop, match, try, return, and block analysis.
//! The methods operate directly on the shared body-analysis engine state so
//! control-flow lowering does not introduce a peer analysis context.

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use ahash::AHashMap;
use rue_builtins::IntrinsicName;
use std::sync::Arc;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, OptionExt, WarningKind};
use rue_rir::{InstData, InstRef, RirPattern, RirPatternView};
use rue_span::Span;

use super::analysis::FirstClassStrSite;
use super::anon_structs::TrustedTryProducer;
use super::context::{
    AnalysisContext, AnalysisResult, ConstValue, DivergenceKind, DivergenceKinds, LocalVar,
};
use super::ownership_state::{LoopEdgeStates, union_move_maps};
use crate::Node;
use crate::declaration_validation::{
    AccessorExitForm, AccessorMethodLink, AccessorYieldRootForm, accessor_method_link_error,
    accessor_yield_root_error,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPattern, AirPlaceBase, AirRef,
};
use crate::scope::ScopedContext;
use crate::types::{Type, TypeKind};

/// The failure kind a test body's `?` reports (ADR-0083 §1).
const TEST_FAILURE_KIND: &str = "unhandled_error";
/// The failure message a test body's `?` reports.
const TEST_FAILURE_MESSAGE: &str = "unhandled error";

/// Where one variant's arms decompose a nested payload pattern (RUE-2053).
///
/// The plan is derived from pattern *syntax* alone, before any arm is
/// analyzed, because a later arm can be the one that introduces the nesting:
/// `R.Err(e)` followed by `R.Err(E.A(b))` must place both arms in the same
/// inner match. A match's scrutinee type is fixed, so a variant name
/// identifies the variant at every level and can key the plan.
struct NestedPlanNode {
    /// The single payload position this variant's arms decompose.
    field: u32,
    /// Plans for the patterns nested at `field`, keyed by their variant name.
    children: AHashMap<Spur, NestedPlanNode>,
}

/// One node of a match's nested-pattern dispatch tree.
///
/// The root of that tree is the match's own arm list; every other node is an
/// ordinary match on a payload field extracted from the variant its parent
/// matched, so a nested pattern needs no new AIR or CFG construct.
struct NestedMatchNode {
    /// The extracted payload value this node dispatches on.
    scrutinee: AirRef,
    /// Its type — the payload field type of the parent's variant.
    scrutinee_type: Type,
    /// Enum whose variants this node's patterns name.
    enum_id: crate::types::EnumId,
    /// Source spelling of the path that reaches this node (`R.Err`), used to
    /// name missing patterns in the non-exhaustive diagnostic.
    path: String,
    /// Span of the arm that introduced this node.
    span: Span,
    /// Variants covered here, each with the span of its first arm.
    covered_variants: AHashMap<u32, Span>,
    /// Span of the first arm that matches every remaining value here — a
    /// binder or `_` at the parent's decomposed payload position.
    wildcard_span: Option<Span>,
    /// This node's arms, in source order.
    arms: Vec<NestedMatchArm>,
    /// Variant index to child node, for variants decomposed further still.
    children: AHashMap<u32, usize>,
}

/// One arm of a nested-dispatch node: either a body, or a child match that
/// discriminates the variant's payload further.
enum NestedMatchArm {
    Direct(AirPattern, AirRef),
    Child { pattern: AirPattern, node: usize },
}

/// The payload position of a variant pattern that a child match handles.
#[derive(Clone, Copy)]
enum DecomposedPosition {
    /// A nested pattern occupies the position; the child match consumes the
    /// field, so this arm binds nothing there.
    Consumed(u32),
    /// The arm has a binder (or `_`) at the position, so it is the child
    /// match's catch-all arm and binds the value the child dispatched on.
    Bound(u32, AirRef),
}

/// One level of a placed arm: a variant test and the payload it binds.
struct ArmLevel<'p> {
    pattern: &'p RirPattern,
    scrutinee: AirRef,
    enum_id: crate::types::EnumId,
    variant_index: u32,
    decomposed: Option<DecomposedPosition>,
}

/// Where one arm lands in the dispatch tree, and what it must bind to get
/// there.
struct ArmPlacement<'p> {
    /// Outermost variant first; the last level is the arm's own pattern.
    levels: Vec<ArmLevel<'p>>,
    /// Node owning the arm (`None` is the match's own arm list).
    node: Option<usize>,
    /// The value that node dispatches on.
    scrutinee: AirRef,
    /// Its type.
    scrutinee_type: Type,
    /// The pattern the arm contributes to that node.
    air_pattern: AirPattern,
    /// The variant it names, or `None` when the arm is a catch-all in a child
    /// match because it binds the decomposed position instead of nesting.
    variant_index: Option<u32>,
}

/// Plan the nested payload dispatch for one level's patterns, keyed by variant
/// name. Returns an empty map when no pattern at this level nests.
fn plan_nested_dispatch(
    patterns: &[&RirPattern],
    interner: &lasso::ThreadedRodeo,
) -> CompileResult<AHashMap<Spur, NestedPlanNode>> {
    let mut groups: AHashMap<Spur, Vec<&RirPattern>> = AHashMap::new();
    for pattern in patterns.iter().copied() {
        if let RirPattern::Path { variant, .. } = pattern {
            groups.entry(*variant).or_default().push(pattern);
        }
    }
    let mut plan = AHashMap::new();
    for (variant, group) in groups {
        let mut field: Option<u32> = None;
        for pattern in &group {
            let RirPattern::Path { elements, span, .. } = pattern else {
                continue;
            };
            let mut nested = elements.iter().enumerate().filter_map(|(index, element)| {
                matches!(element, rue_rir::RirPatternElement::Nested(_)).then_some(index as u32)
            });
            let Some(position) = nested.next() else {
                continue;
            };
            // One payload position per pattern level, and the same position
            // across the arms that share a variant: the child match dispatches
            // on exactly one extracted field.
            let conflict =
                nested.next().is_some() || field.is_some_and(|existing| existing != position);
            if conflict {
                return Err(CompileError::new(
                    ErrorKind::NestedPatternPositionConflict {
                        variant: interner.resolve(&variant).to_string(),
                    },
                    *span,
                )
                .with_help(
                    "a variant pattern may nest another variant pattern in at most one payload \
                     position, and every arm matching the same variant must nest in that same \
                     position; bind the other positions and match them in a nested `match`",
                ));
            }
            field = Some(position);
        }
        let Some(field) = field else {
            continue;
        };
        let nested: Vec<&RirPattern> = group
            .iter()
            .filter_map(|pattern| match pattern {
                RirPattern::Path { elements, .. } => match elements.get(field as usize) {
                    Some(rue_rir::RirPatternElement::Nested(nested)) => Some(nested),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let children = plan_nested_dispatch(&nested, interner)?;
        plan.insert(variant, NestedPlanNode { field, children });
    }
    Ok(plan)
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Discard edge snapshots collected while analyzing code after a
    /// diverging child expression. Keep syntactic `break` classification for
    /// loop typing, but only retain moves from edges reachable at this
    /// sequencing boundary (spec 4.8:21; RUE-1615).
    pub(super) fn restore_reachable_loop_edges(
        ctx: &mut AnalysisContext,
        reachable_edges: &[LoopEdgeStates],
    ) {
        assert_eq!(reachable_edges.len(), ctx.ownership.loop_break_stack.len());
        let mut restored = reachable_edges.to_vec();
        for (reachable, analyzed) in restored.iter_mut().zip(&ctx.ownership.loop_break_stack) {
            reachable.broke |= analyzed.broke;
        }
        ctx.ownership.loop_break_stack = restored;
    }

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
            } => self.analyze_branch(
                air,
                inst_ref,
                *cond,
                *then_block,
                *else_block,
                inst.span,
                ctx,
            ),

            InstData::Loop { cond, body } => {
                self.analyze_while_loop(air, *cond, *body, inst.span, ctx)
            }

            InstData::InfiniteLoop { body, iter_borrow } => {
                self.analyze_infinite_loop(air, *body, *iter_borrow, inst.span, ctx)
            }

            InstData::Match { scrutinee, arms } => {
                self.analyze_match(air, inst_ref, *scrutinee, arms, inst.span, ctx)
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

                // The break unwinds the scopes between here and the loop:
                // those scopes end AT this edge and their live bindings are
                // dropped here (spec 4.8:21), so a linear value they hold
                // must already be consumed in the state in force at the edge
                // (RUE-1614). The enclosing joins exclude this diverging arm,
                // so no scope-exit check ever observes this state.
                if let Some(record) = ctx.ownership.loop_break_stack.last() {
                    self.check_linear_values_at_exit_edge(ctx, record.first_unwound_frame, false)?;
                }
                ctx.divergence_kinds.insert(DivergenceKind::Exit);

                // Record the break against the innermost enclosing loop: it
                // is now `()`-typed instead of `!` (spec 4.8:17), and the
                // move state in force HERE is one of the loop's exit states —
                // union-merged with the other breaks' states, it becomes the
                // ownership state after the loop (RUE-1293; formal core §5.7,
                // (Loop-Break): Σ_exit = join over the at-break states).
                let break_depth = ctx.ownership.moved_scope_stack.len();
                let break_moves = ctx.ownership.moved_vars.clone();
                if let Some(record) = ctx.ownership.loop_break_stack.last_mut() {
                    record.record_break(&break_moves, break_depth);
                }

                // Break has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Break,
                    ty: Type::NEVER,
                    span: inst.span,
                });
                Ok(AnalysisResult::diverged(air_ref, Type::NEVER))
            }

            InstData::Continue => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
                }

                // The continue unwinds the scopes between here and the loop
                // head: this iteration's bindings in those scopes end AT this
                // edge and are dropped here, so a linear value they hold must
                // already be consumed in the state in force at the edge
                // (RUE-1614), exactly as at a break.
                if let Some(record) = ctx.ownership.loop_break_stack.last() {
                    self.check_linear_values_at_exit_edge(ctx, record.first_unwound_frame, false)?;
                }
                ctx.divergence_kinds.insert(DivergenceKind::Exit);

                // The move state in force HERE rides the back edge into the
                // next iteration: record it so the loop's back-edge recheck
                // is seeded with it — the branch joins exclude this diverging
                // arm, so without the record a move on a continue path would
                // vanish and the next iteration could re-use the value
                // (RUE-1293, the continue-edge variant).
                let continue_depth = ctx.ownership.moved_scope_stack.len();
                let continue_moves = ctx.ownership.moved_vars.clone();
                if let Some(record) = ctx.ownership.loop_break_stack.last_mut() {
                    record.record_continue(&continue_moves, continue_depth);
                }

                // Continue has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Continue,
                    ty: Type::NEVER,
                    span: inst.span,
                });
                Ok(AnalysisResult::diverged(air_ref, Type::NEVER))
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
        branch_inst: InstRef,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Keep divergence classification local to this branch. A condition
        // or arm may contain an explicit panic, but an if with a continuing
        // alternative itself continues and must not leak that arm's
        // exemption into its enclosing expression.
        let prior_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        // Comptime-known branch selection (RUE-166, spec 4.14:17): inside a
        // body with comptime value parameters in scope (a value-specialized
        // function or an anonymous-struct method capturing comptime values),
        // an `if` whose condition is compile-time evaluable selects its
        // branch during analysis — only the taken branch is analyzed and
        // emitted. This is what lets comptime recursion terminate: in
        // `fact(comptime n: i32)`, the body specialized for n == 1 must not
        // analyze the `fact(n - 1)` call in the dead else-branch, or
        // specialization would recurse until the depth cap.
        if let Some(crate::sema::ComptimeSelection::Branch { taken }) =
            ctx.comptime_selections.get(&branch_inst)
        {
            let taken_block = if *taken { Some(then_block) } else { else_block };
            return match taken_block {
                Some(block) => {
                    ctx.push_scope();
                    let boundary = ctx.ownership.enter_full_expression();
                    let result = self.analyze_inst(air, block, ctx);
                    let loans = ctx.ownership.nested_expression_loans(&boundary);
                    ctx.ownership.exit_full_expression(boundary);
                    let result = result?;
                    let branch_divergence = ctx.divergence_kinds;
                    ctx.pop_scope();
                    ctx.divergence_kinds = prior_divergence.union(branch_divergence);
                    // With an `else`, the selected branch is this `if`'s
                    // value, so loans surviving its tail belong to the
                    // enclosing full expression (RUE-1678), exactly as on
                    // the ordinary two-armed path below.
                    self.readmit_arm_accessor_loans(
                        ctx,
                        loans,
                        else_block.is_some() && result.continues,
                    )?;
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
                                found: self.format_type_name(result.ty),
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

        // Condition must be bool
        let boundary = ctx.ownership.enter_full_expression();
        let cond_result = ctx.with_expected_type(None, |ctx| self.analyze_inst(air, cond, ctx));
        ctx.ownership.exit_full_expression(boundary);
        let cond_result = cond_result?;
        let cond_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        let reachable_edges_after_condition = ctx.ownership.loop_break_stack.clone();

        if let Some(else_b) = else_block {
            // Save move state before entering branches.
            let saved_moves = ctx.ownership.moved_vars.clone();

            // Analyze then branch with its own scope. Loans surviving the
            // arm's tail are harvested before its boundary closes and belong
            // to the enclosing full expression once both arms are analyzed
            // (ADR-0062 6.6:10, RUE-1678).
            ctx.push_scope();
            let boundary = ctx.ownership.enter_full_expression();
            let then_result = self.analyze_inst(air, then_block, ctx);
            let then_loans = ctx.ownership.nested_expression_loans(&boundary);
            ctx.ownership.exit_full_expression(boundary);
            let then_result = then_result?;
            let mut then_divergence = ctx.divergence_kinds;
            ctx.divergence_kinds = DivergenceKinds::NONE;
            let then_type = then_result.ty;
            let then_continues = then_result.continues;
            let then_span = self.body_rir_ref().get(then_block).span;
            if then_divergence.has_other()
                && (then_continues
                    || !matches!(
                        self.body_rir_ref().get(then_block).data,
                        InstData::Block { .. }
                    ))
            {
                self.check_linear_values_at_unchecked_divergence(ctx)?;
                then_divergence = then_divergence.without_other();
                ctx.divergence_kinds = ctx.divergence_kinds.without_other();
            }
            ctx.pop_scope();

            // Capture then-branch's move state
            let then_moves = ctx.ownership.moved_vars.clone();

            // Restore to saved state before analyzing else branch
            ctx.ownership.moved_vars = saved_moves;

            // Analyze else branch with its own scope
            ctx.push_scope();
            let boundary = ctx.ownership.enter_full_expression();
            let else_result = self.analyze_inst(air, else_b, ctx);
            let else_loans = ctx.ownership.nested_expression_loans(&boundary);
            ctx.ownership.exit_full_expression(boundary);
            let else_result = else_result?;
            let mut else_divergence = ctx.divergence_kinds;
            ctx.divergence_kinds = DivergenceKinds::NONE;
            let else_type = else_result.ty;
            let else_continues = else_result.continues;
            let else_span = self.body_rir_ref().get(else_b).span;
            if else_divergence.has_other()
                && (else_continues
                    || !matches!(self.body_rir_ref().get(else_b).data, InstData::Block { .. }))
            {
                self.check_linear_values_at_unchecked_divergence(ctx)?;
                else_divergence = else_divergence.without_other();
                ctx.divergence_kinds = ctx.divergence_kinds.without_other();
            }
            ctx.pop_scope();

            // Both arms are analyzed, so one arm's loan cannot be mistaken for
            // a conflicting sibling of the other arm's accessor call. Readmit
            // the loans that survived each arm's tail (RUE-1678).
            self.readmit_arm_accessor_loans(
                ctx,
                then_loans,
                cond_result.continues && then_continues,
            )?;
            self.readmit_arm_accessor_loans(
                ctx,
                else_loans,
                cond_result.continues && else_continues,
            )?;

            // Capture else-branch's move state
            let else_moves = ctx.ownership.moved_vars.clone();

            if !cond_result.continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_condition);
            }

            // Merge move states from both branches.
            ctx.ownership.merge_branch_moves(
                then_moves,
                else_moves,
                !then_continues,
                !else_continues,
                then_divergence,
                else_divergence,
            );

            // Compute the unified result type using never type coercion
            let result_type = match (then_continues, else_continues) {
                (false, false) => Type::NEVER,
                (false, true) => else_type,
                (true, false) => then_type,
                (true, true) => {
                    // Neither diverges - types must match exactly
                    if !self.types_equivalent(then_type, else_type)
                        && !then_type.is_error()
                        && !else_type.is_error()
                    {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: self.format_type_name(then_type),
                                found: self.format_type_name(else_type),
                            },
                            else_span,
                        )
                        .with_label(
                            format!("this is of type `{}`", self.format_type_name(then_type)),
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
            // Preserve every reachable terminating alternative. In
            // particular, a continuing arm can still have an internal panic
            // or return edge, and a later sibling must not erase it.
            let branch_divergence = if cond_result.continues {
                cond_divergence
                    .union(then_divergence)
                    .union(else_divergence)
            } else {
                cond_divergence
            };
            ctx.divergence_kinds = prior_divergence.union(branch_divergence);
            Ok(AnalysisResult::with_continues(
                air_ref,
                result_type,
                cond_result.continues && (then_result.continues || else_result.continues),
            ))
        } else {
            // No else branch - result is Unit
            // The then branch must have unit type (spec 4.6:5)

            // Save move state before entering then-branch.
            let saved_moves = ctx.ownership.moved_vars.clone();

            ctx.push_scope();
            let boundary = ctx.ownership.enter_full_expression();
            let then_result = self.analyze_inst(air, then_block, ctx);
            ctx.ownership.exit_full_expression(boundary);
            let then_result = then_result?;
            let then_divergence = ctx.divergence_kinds;
            ctx.divergence_kinds = DivergenceKinds::NONE;
            ctx.pop_scope();

            // Check that the then branch has unit type (or Never/Error)
            let then_type = then_result.ty;
            if then_type != Type::UNIT && then_result.continues && !then_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "()".to_string(),
                        found: self.format_type_name(then_type),
                    },
                    self.body_rir_ref().get(then_block).span,
                )
                .with_help(
                    "if expressions without else must have unit type; \
                     consider adding an else branch or making the body return ()",
                ));
            }

            // Capture then-branch's move state
            let then_moves = ctx.ownership.moved_vars.clone();

            if !cond_result.continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_condition);
            }

            // For if-without-else:
            if !then_result.continues {
                // Then-branch diverges - code after if only runs if cond was false
                ctx.ownership.moved_vars = saved_moves;
            } else {
                // Then-branch doesn't diverge - merge moves (union semantics).
                ctx.ownership.merge_branch_moves(
                    then_moves,
                    saved_moves,
                    false, // then doesn't diverge
                    false, // "else" (empty) doesn't diverge
                    then_divergence,
                    DivergenceKinds::NONE,
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
            let branch_divergence = if cond_result.continues {
                cond_divergence.union(then_divergence)
            } else {
                cond_divergence
            };
            ctx.divergence_kinds = prior_divergence.union(branch_divergence);
            Ok(AnalysisResult::with_continues(
                air_ref,
                Type::UNIT,
                cond_result.continues,
            ))
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
        let moves_before_loop = ctx.ownership.moved_vars.clone();

        // While loop: condition must be bool, result is Unit
        let boundary = ctx.ownership.enter_full_expression();
        let cond_result = self.analyze_inst(air, cond, ctx);
        ctx.ownership.exit_full_expression(boundary);
        let cond_result = cond_result?;
        let cond_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        // The condition-false path exits the while before its body runs.
        // Keep its ownership state separate from the body's fall-through:
        // a body that always diverges must not overwrite this zero-iteration
        // exit with an arbitrary state from one diverging arm.
        let moves_after_condition = ctx.ownership.moved_vars.clone();
        let reachable_edges_after_condition = ctx.ownership.loop_break_stack.clone();

        // Analyze body with its own scope. The loop_break_stack entry makes
        // breaks inside the body target this while loop, not an outer loop;
        // the flag itself is unused because a while loop is always `()`.
        ctx.push_scope();
        ctx.loop_depth += 1;
        ctx.ownership
            .loop_break_stack
            .push(LoopEdgeStates::entered_at(ctx.scope_stack.len() - 1));
        let boundary = ctx.ownership.enter_full_expression();
        let body_result = self.analyze_inst(air, body, ctx);
        ctx.ownership.exit_full_expression(boundary);
        let body_result = body_result?;
        let body_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        ctx.loop_depth -= 1;
        // pop_scope replays this scope's RUE-522 restoration onto the
        // break-site snapshots still on the stack, so the record popped
        // afterwards describes the post-loop scope view.
        ctx.pop_scope();
        let edge_record = ctx.ownership.loop_break_stack.pop().unwrap_or_default();
        if !cond_result.continues {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_condition);
        }
        let (break_moves, continue_moves) = edge_record.merged_moves();
        // The back edge carries the fall-through state joined with the
        // continue-site states — a continue re-enters the loop exactly like
        // falling off the body's end — while a break path never re-enters.
        // Keep the joined state for the recheck below (RUE-1293).
        let mut reachable_backedge_moves = body_result
            .continues
            .then(|| ctx.ownership.moved_vars.clone());
        if let Some(continue_moves) = &continue_moves {
            reachable_backedge_moves = Some(match reachable_backedge_moves {
                Some(fallthrough_moves) => union_move_maps(&fallthrough_moves, continue_moves),
                None => continue_moves.clone(),
            });
        }
        // Only a condition that can complete and a body path that reaches
        // either its fall-through or an explicit continue can return to the
        // loop head. Break-only paths end at the loop exit and must not make
        // the back-edge recheck reject a move that runs at most once
        // (RUE-1615).
        let backedge_reachable = cond_result.continues && reachable_backedge_moves.is_some();
        // A while loop's exits are the zero-iteration condition-false path,
        // later condition-false paths reached from the back edge, and its
        // breaks. Join only reachable contributors: the post-body state is
        // not an exit when every body path diverges (RUE-1615), while a move
        // on a break path remains visible after the loop (RUE-1293).
        let mut exit_moves = moves_after_condition;
        if cond_result.continues {
            if let Some(reachable_backedge_moves) = &reachable_backedge_moves {
                exit_moves = union_move_maps(&exit_moves, reachable_backedge_moves);
            }
            if let Some(break_moves) = &break_moves {
                exit_moves = union_move_maps(&exit_moves, break_moves);
            }
        }
        ctx.ownership.moved_vars = exit_moves;

        // A while loop discards its body's result value on every iteration;
        // discarding a value that carries a linear value would implicitly
        // drop it (RUE-176).
        self.reject_discarded_linear_value(body_result.ty, body)?;

        // Loop back-edge move check: if the loop changed any move state,
        // re-run the analysis once with the post-body state as the starting
        // state. Any use of a value moved by a previous iteration then errors.
        // The scratch Air and context are discarded - this pass exists only
        // for the checks.
        if backedge_reachable
            && !ctx.ownership.in_loop_move_recheck
            && reachable_backedge_moves.as_ref() != Some(&moves_before_loop)
        {
            let checkpoint = air.checkpoint();
            let mut scratch_ctx = ctx.fork_for_loop_recheck();
            // Seed the back-edge recheck with the back-edge state (the
            // fall-through joined with the continue states), not the exit
            // state: break-path moves never reach the back edge, and seeding
            // them would reject a value legitimately moved only on a path
            // that exits the loop (RUE-1293).
            scratch_ctx.ownership.moved_vars =
                reachable_backedge_moves.expect("reachable back edge must have a move state");
            let recovered_before = self.body_analysis_recovered_errors_mut().len();
            let result = (|| -> CompileResult<()> {
                let boundary = scratch_ctx.ownership.enter_full_expression();
                let result = self.analyze_inst(air, cond, &mut scratch_ctx);
                scratch_ctx.ownership.exit_full_expression(boundary);
                result?;
                scratch_ctx.push_scope();
                scratch_ctx.loop_depth += 1;
                scratch_ctx
                    .ownership
                    .loop_break_stack
                    .push(LoopEdgeStates::entered_at(
                        scratch_ctx.scope_stack.len() - 1,
                    ));
                let boundary = scratch_ctx.ownership.enter_full_expression();
                let result = self.analyze_inst(air, body, &mut scratch_ctx);
                scratch_ctx.ownership.exit_full_expression(boundary);
                result?;
                scratch_ctx.ownership.loop_break_stack.pop();
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
        let mut loop_divergence = cond_divergence;
        if cond_result.continues {
            loop_divergence = loop_divergence.union(body_divergence);
        }
        if !cond_result.continues && loop_divergence.is_empty() {
            loop_divergence.insert(DivergenceKind::Other);
        }
        ctx.divergence_kinds = loop_divergence;
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            cond_result.continues,
        ))
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
        let moves_before_loop = ctx.ownership.moved_vars.clone();

        ctx.push_scope();
        ctx.loop_depth += 1;
        ctx.ownership
            .loop_break_stack
            .push(LoopEdgeStates::entered_at(ctx.scope_stack.len() - 1));
        // A `for` over a named variable borrows it (shared) for the body's
        // duration (spec 4.8:26, RUE-233): record the borrow so a mutation of
        // the iterated collection inside the body is rejected (E0428).
        if let Some(var) = iter_borrow {
            ctx.ownership.iter_borrows.push(var);
        }
        let boundary = ctx.ownership.enter_full_expression();
        let body_result = self.analyze_inst(air, body, ctx);
        ctx.ownership.exit_full_expression(boundary);
        let body_result = body_result?;
        let body_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        if iter_borrow.is_some() {
            ctx.ownership.iter_borrows.pop();
        }
        ctx.loop_depth -= 1;
        // pop_scope replays this scope's RUE-522 restoration onto the
        // break-site snapshots still on the stack (see analyze_while_loop).
        ctx.pop_scope();
        let edge_record = ctx.ownership.loop_break_stack.pop().unwrap_or_default();
        // Loop classification is purely syntactic (spec 4.8:21): a loop
        // containing a targeting `break` is unit-typed even when that break
        // is unreachable.
        let has_break = edge_record.broke;
        let (break_moves, continue_moves) = edge_record.merged_moves();
        // `has_break` is syntactic and controls the loop's static type even
        // for an unreachable break (4.8:21). Only a reachable break snapshot
        // makes this loop continue to its enclosing context; an inner loop
        // whose reachable paths all return must not create an outer phantom
        // back edge (RUE-1615).
        let has_reachable_break = break_moves.is_some();
        // The back edge carries the fall-through state joined with the
        // continue-site states; keep it for the recheck below (RUE-1293).
        let mut reachable_backedge_moves = body_result
            .continues
            .then(|| ctx.ownership.moved_vars.clone());
        if let Some(continue_moves) = &continue_moves {
            reachable_backedge_moves = Some(match reachable_backedge_moves {
                Some(fallthrough_moves) => union_move_maps(&fallthrough_moves, continue_moves),
                None => continue_moves.clone(),
            });
        }
        // A break-only body has no path back to the loop head. Its move state
        // belongs exclusively to the exit join, not to a phantom iteration
        // (RUE-1615). Explicit continue edges remain real back edges even when
        // every other body path diverges.
        let backedge_reachable = reachable_backedge_moves.is_some();
        // An infinite loop's only exits are its breaks, so the union of the
        // at-break states IS the exit ownership state — the back-edge state
        // re-enters the loop and never reaches the code after it (RUE-1293;
        // formal core §5.7, (Loop-Break)). A breakless loop is `!`-typed and
        // the code after it unreachable; its state is left as the
        // fall-through, which nothing can observe.
        if let Some(break_moves) = break_moves {
            ctx.ownership.moved_vars = break_moves;
        }

        // The loop discards its body's result value on every iteration;
        // discarding a value that carries a linear value would implicitly
        // drop it (RUE-176).
        self.reject_discarded_linear_value(body_result.ty, body)?;

        // Loop back-edge move check (see analyze_while_loop for details).
        if backedge_reachable
            && !ctx.ownership.in_loop_move_recheck
            && reachable_backedge_moves.as_ref() != Some(&moves_before_loop)
        {
            let checkpoint = air.checkpoint();
            let mut scratch_ctx = ctx.fork_for_loop_recheck();
            // Seed with the back-edge state (fall-through joined with the
            // continue states), not the exit state: only the back edge's
            // moves reach the next iteration (RUE-1293).
            scratch_ctx.ownership.moved_vars =
                reachable_backedge_moves.expect("reachable back edge must have a move state");
            let recovered_before = self.body_analysis_recovered_errors_mut().len();
            scratch_ctx.push_scope();
            scratch_ctx.loop_depth += 1;
            scratch_ctx
                .ownership
                .loop_break_stack
                .push(LoopEdgeStates::entered_at(
                    scratch_ctx.scope_stack.len() - 1,
                ));
            if let Some(var) = iter_borrow {
                scratch_ctx.ownership.iter_borrows.push(var);
            }
            let boundary = scratch_ctx.ownership.enter_full_expression();
            let result = self.analyze_inst(air, body, &mut scratch_ctx);
            scratch_ctx.ownership.exit_full_expression(boundary);
            air.rollback(checkpoint);
            for error in &mut self.body_analysis_recovered_errors_mut()[recovered_before..] {
                *error = error
                    .clone()
                    .with_note("value was moved in a previous iteration of the loop");
            }
            result
                .map_err(|e| e.with_note("value was moved in a previous iteration of the loop"))?;
            if iter_borrow.is_some() {
                scratch_ctx.ownership.iter_borrows.pop();
            }
            scratch_ctx.ownership.loop_break_stack.pop();
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
        let mut loop_divergence = body_divergence;
        if !has_reachable_break && (body_result.continues || loop_divergence.is_empty()) {
            // A continuing body reaches the loop back-edge, which is an
            // ordinary non-panic divergence even when another body path
            // explicitly panics. A body with only exempt panic provenance
            // retains that provenance and does not acquire a generic edge.
            loop_divergence.insert(DivergenceKind::Other);
        }
        ctx.divergence_kinds = loop_divergence;
        Ok(AnalysisResult::with_continues(
            air_ref,
            loop_ty,
            has_reachable_break,
        ))
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
        let ty_name = self.format_type_name(scrutinee_type);
        if negative && scrutinee_type.is_unsigned() {
            return Err(
                CompileError::new(ErrorKind::CannotNegate(ty_name), span).with_note(
                    "unsigned values are never negative, so this pattern could never match",
                ),
            );
        }
        let denoted = pattern_int_denoted(value, negative);
        if !scrutinee_type
            .integer_semantics()
            .is_some_and(|integer| integer.fits_i128(denoted))
        {
            return Err(CompileError::new(
                ErrorKind::LiteralOutOfRange {
                    value: denoted,
                    ty: ty_name,
                },
                span,
            ));
        }
        Ok(denoted as i64)
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
    fn warn_unreachable_pruned_arms<'r>(
        &self,
        arms: impl Iterator<Item = (RirPatternView<'r>, InstRef)>,
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
                    RirPatternView::Wildcard(_) => "_".to_string(),
                    RirPatternView::Int {
                        value, negative, ..
                    } => {
                        if *negative {
                            format!("-{}", value)
                        } else {
                            value.to_string()
                        }
                    }
                    RirPatternView::Bool(b, _) => b.to_string(),
                    RirPatternView::Path {
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
                RirPatternView::Wildcard(_) => {
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
                RirPatternView::Int {
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
                RirPatternView::Bool(b, _) => {
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
                RirPatternView::Path { .. } => {}
            }
        }
    }

    /// Analyze a match expression.
    fn analyze_match(
        &mut self,
        air: &mut Air,
        match_inst: InstRef,
        scrutinee: InstRef,
        arms: &rue_rir::RirMatchArmsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let prior_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
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
        if let Some(crate::sema::ComptimeSelection::Match { arm: selected_arm }) =
            ctx.comptime_selections.get(&match_inst)
            && let Some(selected) =
                crate::sema::comptime::prunable_match_body(self.body_rir_ref(), arms, *selected_arm)
        {
            // These scans only read pattern shapes, so they iterate the
            // borrowed RIR view; nothing here needs the owned patterns the
            // old per-arm materialization allocated (RUE-1661).
            let arm_views = self.body_rir_ref().match_arms(arms);
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
            let scrutinee_type = Self::get_resolved_type(ctx, scrutinee, span, "match scrutinee")?;
            // Validate every scalar pattern before pruning. The selected
            // body is exempt from analysis, but a malformed later pattern
            // remains a source error (for example `0 => ..., true => ...`
            // on an integer scrutinee).
            for (pattern, _) in arm_views.iter() {
                match &pattern {
                    RirPatternView::Int {
                        value: magnitude,
                        negative,
                        ..
                    } if scrutinee_type.is_integer() => {
                        self.check_pattern_int(
                            *magnitude,
                            *negative,
                            scrutinee_type,
                            pattern.span(),
                        )?;
                    }
                    RirPatternView::Int { .. } => {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: self.format_type_name(scrutinee_type),
                                found: "integer".to_string(),
                            },
                            pattern.span(),
                        ));
                    }
                    RirPatternView::Bool(_, _) if scrutinee_type != Type::BOOL => {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: self.format_type_name(scrutinee_type),
                                found: "bool".to_string(),
                            },
                            pattern.span(),
                        ));
                    }
                    _ => {}
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
            self.warn_unreachable_pruned_arms(arm_views.iter(), scrutinee_type, ctx);
            ctx.push_scope();
            let boundary = ctx.ownership.enter_full_expression();
            let result = self.analyze_inst(air, selected, ctx);
            let loans = ctx.ownership.nested_expression_loans(&boundary);
            ctx.ownership.exit_full_expression(boundary);
            let result = result?;
            let selected_divergence = ctx.divergence_kinds;
            ctx.pop_scope();
            ctx.divergence_kinds = prior_divergence.union(selected_divergence);
            // The selected arm is this `match`'s value, so loans
            // surviving its tail belong to the enclosing full
            // expression (RUE-1678).
            self.readmit_arm_accessor_loans(ctx, loans, result.continues)?;
            return Ok(result);
        }

        // Derive the expected scrutinee type from the arm patterns, so a
        // fallible-intrinsic scrutinee can validate the pattern's `Option(T)`
        // against its exact registry-installed result (`match @read_line() {
        // Option::Some(l) => .., Option::None => .. }`, RUE-6). Context does not
        // select the intrinsic nominal. Resolution errors are ignored — pattern
        // legality is checked on the normal path below.
        // Only the path patterns' type names (Copy symbols) feed this probe,
        // so collect exactly those instead of cloning every arm's pattern
        // (RUE-1661). The resolver needs `&mut self`, so the RIR view cannot
        // stay borrowed across it.
        let path_pattern_type_names = self
            .body_rir_ref()
            .match_arms(arms)
            .iter()
            .filter_map(|(pattern, _)| match pattern {
                RirPatternView::Path { type_name, .. } => Some(type_name),
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected_scrutinee = path_pattern_type_names.into_iter().find_map(|type_name| {
            self.resolve_type_with_ctx(type_name, span, ctx)
                .ok()
                .filter(|ty| ty.is_enum())
        });
        // Analyze the scrutinee under only the pattern-derived contract. The
        // match expression's own result expectation belongs to its arms.
        let boundary = ctx.ownership.enter_full_expression();
        let scrutinee_result = ctx.with_expected_type(expected_scrutinee, |ctx| {
            self.analyze_inst(air, scrutinee, ctx)
        });
        ctx.ownership.exit_full_expression(boundary);
        let scrutinee_result = scrutinee_result?;
        let scrutinee_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        let scrutinee_type = scrutinee_result.ty;
        let reachable_edges_after_scrutinee = ctx.ownership.loop_break_stack.clone();

        // Validate that we can match on this type (integers, booleans, and enums)
        if !scrutinee_type.is_integer() && scrutinee_type != Type::BOOL && !scrutinee_type.is_enum()
        {
            return Err(CompileError::new(
                ErrorKind::InvalidMatchType(self.format_type_name(scrutinee_type)),
                span,
            ));
        }

        // The one owned materialization this match needs (RUE-1661): the
        // per-arm loop below alternates pattern reads with `analyze_inst`
        // calls that take `&mut self`, so the patterns must outlive the RIR
        // borrow, and AIR lowering consumes their owned binding lists.
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
            return Ok(AnalysisResult::diverged(air_ref, Type::NEVER));
        }

        // Nested payload patterns (RUE-2053) dispatch through child matches on
        // the payload field they decompose. The plan is derived from pattern
        // syntax before any arm is analyzed, because the arm that introduces
        // the nesting may come after an arm that merely binds that payload.
        let arm_patterns: Vec<&RirPattern> = arms.iter().map(|(pattern, _)| pattern).collect();
        let nested_plan = plan_nested_dispatch(&arm_patterns, self.body_interner())?;
        let mut nested_nodes: Vec<NestedMatchNode> = Vec::new();
        let mut nested_root_children: AHashMap<u32, usize> = AHashMap::new();

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
        let mut air_arms: Vec<NestedMatchArm> = Vec::new();
        let mut result_type: Option<Type> = None;
        let mut result_continues: Option<bool> = None;
        let mut arm_divergence_kinds: Vec<DivergenceKinds> = Vec::with_capacity(arms.len());

        // Move state before any arm runs (after the scrutinee, whose moves
        // happen on every path). Arms are alternatives, not a sequence:
        // each is analyzed from this state and the per-arm results are
        // merged after the loop (see merge_arm_moves).
        let moves_before_arms = ctx.ownership.moved_vars.clone();
        let mut arm_move_states = Vec::with_capacity(arms.len());
        // Accessor loans harvested from each arm before its nested
        // full-expression boundary closed. Arms are alternatives, so these are
        // readmitted only once every arm has been analyzed (RUE-1678).
        let mut arm_accessor_loans = Vec::with_capacity(arms.len());

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
                            let def = self.body_type_pool().enum_def(enum_id);
                            let external_non_exhaustive =
                                def.is_non_exhaustive && def.file_id != ctx.current_file_id;
                            !external_non_exhaustive
                                && covered_variants.len() == def.variant_count()
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
                                expected: self.format_type_name(scrutinee_type),
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
                                expected: self.format_type_name(scrutinee_type),
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
                // A variant pattern's legality is checked level by level as it
                // is placed, immediately below.
                RirPattern::Path { .. } => {}
            }

            // A variant pattern is placed in the match's dispatch tree: at the
            // match's own arm list, or — when this or another arm nests a
            // pattern in one of the variant's payload positions (RUE-2053) —
            // in the child match on that extracted payload. The walk validates
            // every level it passes through.
            let placement = match &pattern {
                RirPattern::Path { .. } => {
                    let placement = self.place_match_arm(
                        air,
                        pattern,
                        scrutinee_result.air_ref,
                        scrutinee_type,
                        &nested_plan,
                        &mut nested_nodes,
                        &mut nested_root_children,
                        &mut air_arms,
                        ctx,
                    )?;
                    pattern_enum_id = Some(placement.levels[0].enum_id);
                    // The outermost variant is covered by this arm's group,
                    // whether or not the arm discriminates its payload further.
                    // A second arm naming a variant that no arm discriminates
                    // further is unreachable, exactly as a repeated integer or
                    // boolean pattern is; when the variant *is* discriminated,
                    // reachability is decided at the child match instead.
                    let root_variant = placement.levels[0].variant_index;
                    if let Some(first_span) = covered_variants.get(&root_variant)
                        && placement.node.is_none()
                        && wildcard_span.is_none()
                    {
                        let pat_str = match &pattern {
                            RirPattern::Path {
                                type_name, variant, ..
                            } => format!(
                                "{}.{}",
                                self.body_interner().resolve(type_name),
                                self.body_interner().resolve(variant)
                            ),
                            _ => String::new(),
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
                    covered_variants.entry(root_variant).or_insert(pattern_span);
                    Self::record_placed_pattern(
                        &mut nested_nodes,
                        &placement,
                        pattern_span,
                        wildcard_span,
                        self.body_interner(),
                        &mut ctx.warnings,
                    );
                    Some(placement)
                }
                _ => None,
            };

            // Each arm gets its own scope and starts from the pre-match
            // move state (only one arm executes at runtime).
            ctx.ownership.moved_vars = moves_before_arms.clone();
            ctx.push_scope();

            // Materialize tuple-variant payload bindings (RUE-221) into fresh
            // locals before the body, so the body's references resolve to them.
            // The enclosing match dispatched on the discriminant, so in this
            // arm the payload is read (move mode) via `EnumPayloadGet`. A
            // nested arm binds every level it passed through, outermost first,
            // so scope-exit drops them innermost first.
            let mut binding_stmts = Vec::new();
            let mut innermost_bindings = 0usize;
            if let Some(placement) = &placement {
                for level in &placement.levels {
                    let level_stmts = self.materialize_match_bindings(air, level, ctx)?;
                    innermost_bindings = level_stmts.len();
                    binding_stmts.extend(level_stmts);
                }
            }
            // The value the arm's own pattern dispatched on: the match's
            // scrutinee, or the payload a child match discriminates.
            let (arm_scrutinee, arm_scrutinee_type) = match &placement {
                Some(placement) => (placement.scrutinee, placement.scrutinee_type),
                None => (scrutinee_result.air_ref, scrutinee_type),
            };

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
            // `innermost_bindings == 0` is precisely that guard: since
            // RUE-1592 `materialize_match_bindings` moves out EVERY payload
            // position of such a variant — named, `_`-discarded, or covered by
            // the bare-path form `E.A` — so those arms are never empty and
            // account for the whole payload themselves, each field dropped at
            // the arm's end in reverse declaration order. In a nested arm the
            // guard applies at the innermost level only: an outer level always
            // accounts for its own payload, whose decomposed position the
            // child match consumes (RUE-2053).
            if innermost_bindings == 0 && arm_scrutinee_type.is_enum() {
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop {
                        value: arm_scrutinee,
                    },
                    ty: Type::UNIT,
                    span: pattern_span,
                });
                binding_stmts.push(drop_ref.as_u32());
            }

            // Analyze arm body
            let boundary = ctx.ownership.enter_full_expression();
            let body_result = self.analyze_inst(air, *body, ctx);
            let body_loans = ctx.ownership.nested_expression_loans(&boundary);
            ctx.ownership.exit_full_expression(boundary);
            let body_result = body_result?;
            let mut body_divergence = ctx.divergence_kinds;
            ctx.divergence_kinds = DivergenceKinds::NONE;
            let body_type = body_result.ty;
            arm_accessor_loans.push((*body, body_loans, body_result.continues));

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
            if body_result.continues {
                self.check_unconsumed_linear_values(ctx)?;
            } else if body_divergence.has_other()
                && !matches!(self.body_rir_ref().get(*body).data, InstData::Block { .. })
            {
                // A value-position arm has no nested block whose edge check
                // can see the match's enclosing bindings. Validate an
                // unchecked generic divergence against every live scope at
                // this arm edge; block-shaped arms already perform that walk
                // at their own terminal edge.
                self.check_linear_values_at_unchecked_divergence(ctx)?;
                body_divergence = body_divergence.without_other();
                ctx.divergence_kinds = ctx.divergence_kinds.without_other();
            }
            arm_divergence_kinds.push(body_divergence);

            ctx.pop_scope();
            arm_move_states.push((
                std::mem::take(&mut ctx.ownership.moved_vars),
                !body_result.continues,
                body_divergence,
            ));

            // Update result type (handle Never type coercion)
            result_type = Some(match result_type {
                None => body_type,
                Some(prev) => {
                    if !result_continues.unwrap_or(true) {
                        body_type
                    } else if !body_result.continues {
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
            result_continues = Some(result_continues.unwrap_or(false) || body_result.continues);

            // Convert pattern to AIR pattern. A variant pattern already
            // resolved its enum and variant while it was placed, so nothing is
            // resolved a second time here.
            let air_pattern = match &pattern {
                RirPattern::Wildcard(_) => AirPattern::Wildcard,
                RirPattern::Int {
                    value, negative, ..
                } => {
                    // Already range-checked above.
                    AirPattern::Int(pattern_int_denoted(*value, *negative) as i64)
                }
                RirPattern::Bool(b, _) => AirPattern::Bool(*b),
                RirPattern::Path { .. } => placement
                    .as_ref()
                    .map(|placement| placement.air_pattern.clone())
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::InternalError("variant pattern was not placed".to_string()),
                            pattern_span,
                        )
                    })?,
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

            let arm = NestedMatchArm::Direct(air_pattern, arm_body_ref);
            match placement.as_ref().and_then(|placement| placement.node) {
                Some(node) => nested_nodes[node].arms.push(arm),
                None => air_arms.push(arm),
            }
        }

        // Every arm is analyzed, so one arm's loan cannot be mistaken for a
        // conflicting sibling of a later arm's accessor call. Readmit the
        // loans that survived each arm's tail (ADR-0062 6.6:10, RUE-1678).
        for (_, loans, body_continues) in arm_accessor_loans {
            self.readmit_arm_accessor_loans(
                ctx,
                loans,
                scrutinee_result.continues && body_continues,
            )?;
        }

        // Join the arms' move states (union of non-diverging arms;
        // `full_move_on_all_paths` intersects for the linear must-consume
        // check). Matches are exhaustive, so the arms cover every path.
        let continues = result_continues.unwrap_or(true);
        if !scrutinee_result.continues {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_scrutinee);
        }
        ctx.ownership.merge_arm_moves(arm_move_states);

        // Exhaustiveness checking
        let has_wildcard = wildcard_span.is_some();
        let bool_true_covered = bool_true_span.is_some();
        let bool_false_covered = bool_false_span.is_some();
        let is_exhaustive = if scrutinee_type == Type::BOOL {
            has_wildcard || (bool_true_covered && bool_false_covered)
        } else if let Some(enum_id) = pattern_enum_id {
            let enum_def = self.body_type_pool().enum_def(enum_id);
            let external_non_exhaustive =
                enum_def.is_non_exhaustive && enum_def.file_id != ctx.current_file_id;
            has_wildcard
                || (!external_non_exhaustive && covered_variants.len() == enum_def.variant_count())
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

        // A nested pattern's child match must be exhaustive over the payload
        // it discriminates, exactly as the match itself is over its scrutinee
        // (RUE-2053). Missing patterns are named with the path that reaches
        // them, so `R.Err(E.A(b))` alone reports `R.Err(E.B)`.
        for node in &nested_nodes {
            if node.wildcard_span.is_some() {
                continue;
            }
            let enum_def = self.body_type_pool().enum_def(node.enum_id);
            let external_non_exhaustive =
                enum_def.is_non_exhaustive && enum_def.file_id != ctx.current_file_id;
            if !external_non_exhaustive && node.covered_variants.len() == enum_def.variant_count() {
                continue;
            }
            let enum_name = self.format_type_name(Type::new_enum(node.enum_id));
            let missing: Vec<String> = enum_def
                .variants
                .iter()
                .enumerate()
                .filter(|(index, _)| !node.covered_variants.contains_key(&(*index as u32)))
                .map(|(_, variant)| format!("{}{enum_name}.{})", node.path, variant.as_ref()))
                .collect();
            let error = CompileError::new(ErrorKind::NonExhaustiveMatch, span)
                .with_label("this payload pattern is not exhaustive", node.span);
            return Err(if missing.is_empty() {
                error
            } else {
                error.with_help(format!("missing patterns: {}", missing.join(", ")))
            });
        }

        let final_type = result_type.unwrap_or(Type::UNIT);

        let air_arms = Self::build_nested_arms(air, &nested_nodes, &air_arms, final_type)?;
        let air_ref = air.add_match(scrutinee_result.air_ref, &air_arms, final_type, span)?;
        let match_divergence = if scrutinee_result.continues {
            arm_divergence_kinds
                .iter()
                .copied()
                .fold(scrutinee_divergence, DivergenceKinds::union)
        } else {
            scrutinee_divergence
        };
        ctx.divergence_kinds = prior_divergence.union(match_divergence);
        Ok(AnalysisResult::with_continues(
            air_ref,
            final_type,
            scrutinee_result.continues && continues,
        ))
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
                    found: sema.format_type_name(operand_ty),
                },
                span,
            )
        };
        let trusted_producer = self.trusted_try_producer(operand_ty);

        // The `?` failure arm is an early `return` (4.15): it ends every open
        // scope, dropping their live bindings and the by-value parameters at
        // this edge, so a linear value any of them holds must already be
        // consumed in the state in force HERE, after the operand's own
        // consumptions (RUE-1614) — exactly as at an explicit `return`. The
        // check runs only for a genuine try (a trusted Option/Result operand;
        // a lookalike reports E0504 instead) whose operand actually reaches
        // this edge.
        //
        // In a test body the failure arm is a trap, not a return (ADR-0083 §1,
        // spec 6.7:9): no scope ends, nothing is dropped, and the process is
        // gone before any obligation could be observed. That is the same edge
        // `@panic` produces, so it records the same divergence and is exempt
        // from the exit-edge check for the same reason.
        if trusted_producer.is_some() && operand_result.continues {
            if ctx.is_test_body {
                ctx.divergence_kinds.insert(DivergenceKind::Panic);
            } else {
                self.check_linear_values_at_exit_edge(ctx, 0, true)?;
                ctx.divergence_kinds.insert(DivergenceKind::Exit);
            }
        }

        match trusted_producer {
            Some(TrustedTryProducer::Option) => {
                let operand_enum_id = operand_ty
                    .as_enum()
                    .expect("a trusted Option producer is an enum type");
                let Some((some_idx, none_idx, payload_ty)) =
                    self.option_enum_shape(operand_enum_id)
                else {
                    return Err(non_option(self));
                };
                // A test body reports and traps instead of propagating
                // (ADR-0083 §1), so it never looks at the enclosing return
                // type: no `None` of an enclosing `Option` is constructed, and
                // 4.15:4 has nothing to constrain.
                if ctx.is_test_body {
                    return self.build_test_try_desugar(
                        air,
                        operand_result.air_ref,
                        operand_enum_id,
                        some_idx,
                        none_idx,
                        payload_ty,
                        None,
                        span,
                        ctx,
                    );
                }
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
                                return_type: self.format_type_name(return_type),
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
                // As for `Option`: no enclosing `Err` is constructed in a test
                // body, so the identical-error-type rule of 4.15:4 never
                // applies and each `?` site may carry its own error type.
                if ctx.is_test_body {
                    return self.build_test_try_desugar(
                        air,
                        operand_result.air_ref,
                        operand_enum_id,
                        ok_idx,
                        err_idx,
                        ok_ty,
                        Some(err_ty),
                        span,
                        ctx,
                    );
                }
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
                                return_type: self.format_type_name(return_type),
                            },
                            span,
                        ));
                    }
                };
                if !self.types_equivalent(err_ty, ret_err_ty) {
                    return Err(CompileError::new(
                        ErrorKind::QuestionErrTypeMismatch {
                            operand_err: self.format_type_name(err_ty),
                            fn_err: self.format_type_name(ret_err_ty),
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

    /// Build the test-body form of the `?` desugaring (ADR-0083 §1, spec 6.7).
    ///
    /// The success arm is the ordinary one. The failure arm reports and traps
    /// instead of returning: it renders the error, stages the `?` site, and
    /// calls the terminal failure helper, which writes one `unhandled_error`
    /// frame on the ADR-0083 §5.1 channel and aborts.
    ///
    /// The two channel calls are a pair by ABI — a failure record is ten
    /// arguments and every runtime helper is register-only — and the second
    /// adopts whatever site the first staged, so nothing may run between them.
    /// Everything the record carries is therefore materialized as a statement
    /// *before* the site call: the rendered payload, the kind, and the message.
    /// The payload is named twice, once as that statement and once as the
    /// terminal call's argument, which is what puts the rendering before the
    /// pair rather than inside it.
    #[allow(clippy::too_many_arguments)]
    fn build_test_try_desugar(
        &mut self,
        air: &mut Air,
        scrutinee: AirRef,
        operand_enum_id: crate::types::EnumId,
        success_idx: u32,
        fail_idx: u32,
        success_payload_ty: Type,
        fail_err_ty: Option<Type>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
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

        let str_ty = self.get_or_create_str_struct(span)?;
        let payload = match fail_err_ty {
            None => {
                // `Option`'s failure carries nothing, so its whole rendering is
                // the constant `None` and no printer is synthesized for it.
                self.synthesized_string(air, ctx, "None", str_ty, span)
            }
            Some(err_ty) => {
                let error = air.add_inst(AirInst {
                    data: AirInstData::EnumPayloadGet {
                        base: scrutinee,
                        enum_id: operand_enum_id,
                        variant_index: fail_idx,
                        field_index: 0,
                    },
                    ty: err_ty,
                    span,
                });
                let printer = self.structural_printer_symbol(err_ty, span, ctx)?;
                air.add_call(
                    None,
                    printer,
                    &[AirCallArg {
                        value: error,
                        mode: AirArgMode::Normal,
                    }],
                    str_ty,
                    span,
                )?
            }
        };

        let site = self.stage_failure_site(air, ctx, str_ty, span)?;

        let kind = self.synthesized_string(air, ctx, TEST_FAILURE_KIND, str_ty, span);
        let message = self.synthesized_string(air, ctx, TEST_FAILURE_MESSAGE, str_ty, span);
        let report = self.runtime_channel_call(
            air,
            crate::RuntimeCallKind::TestFail,
            &[kind, message, payload],
            Type::NEVER,
            span,
        )?;
        let fail_body =
            air.add_block(&[payload, kind, message, site], report, Type::NEVER, span)?;

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

    /// Emit one ADR-0083 §5.1 failure-channel call by its manifest symbol.
    ///
    /// Shared with the comparison intrinsics (`@assert_eq`/`@assert_ne`,
    /// ADR-0083 Phase 2.5), which report on the same channel.
    pub(in crate::sema) fn runtime_channel_call(
        &mut self,
        air: &mut Air,
        runtime: crate::RuntimeCallKind,
        args: &[AirRef],
        ty: Type,
        span: Span,
    ) -> CompileResult<AirRef> {
        let name = self.intern_body_symbol(runtime.helper().helper().symbol)?;
        let args = args
            .iter()
            .map(|value| AirCallArg {
                value: *value,
                mode: AirArgMode::Normal,
            })
            .collect::<Vec<_>>();
        Ok(air.add_call(Some(runtime), name, &args, ty, span)?)
    }

    /// Stage the source location the next failure record will carry.
    ///
    /// Every producer of an ADR-0083 §5.1 report that names a site reaches
    /// this: a test body's `?` failure arm, the assertion family, and — since
    /// RUE-2019 — `@panic`, whose record the runtime writes from inside the
    /// panic helper. The bounds checks are not among them. Each is a bare
    /// condition with no call beside it — the slice check is a `BoundsCheck`
    /// intrinsic, the fixed-array check is lowered below AIR, and `s[i]`'s is
    /// inside the runtime helper itself — and staging a site for any of them
    /// costs the passing path, so `__rue_bounds_check` states its class and
    /// leaves the location empty.
    ///
    /// The staged site is consumed by whatever aborts next, so this call must
    /// be the last thing before that terminal call: anything evaluated in
    /// between could abort on a path that stages nothing and would then adopt
    /// this site.
    ///
    /// A site the host cannot resolve is staged as the empty file at 0:0 rather
    /// than reported as a compile error: the ABI accepts an absent location,
    /// and a report that cannot name its line is still a better failure than no
    /// report. The runner answers an empty location from the test declaration's
    /// header.
    pub(in crate::sema) fn stage_failure_site(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        str_ty: Type,
        span: Span,
    ) -> CompileResult<AirRef> {
        let (path, line, column) = self
            .body_source_coordinate(span)
            .unwrap_or_else(|| (Arc::from(""), 0, 0));
        let file = self.synthesized_string(air, ctx, &path, str_ty, span);
        let line = air.add_inst(AirInst {
            data: AirInstData::Const(u64::from(line)),
            ty: Type::U32,
            span,
        });
        let column = air.add_inst(AirInst {
            data: AirInstData::Const(u64::from(column)),
            ty: Type::U32,
            span,
        });
        self.runtime_channel_call(
            air,
            crate::RuntimeCallKind::TestFailureSite,
            &[file, line, column],
            Type::UNIT,
            span,
        )
    }

    /// Materialize one compiler-authored `str` run in this body.
    pub(in crate::sema) fn synthesized_string(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        content: &str,
        str_ty: Type,
        span: Span,
    ) -> AirRef {
        let id = ctx.add_synthesized_string(content);
        air.add_inst(AirInst {
            data: AirInstData::StringConst(id),
            ty: str_ty,
            span,
        })
    }

    /// The body-local call symbol naming `value_ty`'s structural printer.
    ///
    /// The identity is the rendered type alone, so a second site on the same
    /// type reuses this symbol and the request synthesizes one printer for both
    /// (ADR-0083 §1). Sites are `?` failure arms and the operands of
    /// `@assert_eq`/`@assert_ne` (Phase 2.5) alike: both render a value the
    /// same way, so both name the same instance.
    pub(in crate::sema) fn structural_printer_symbol(
        &mut self,
        value_ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Spur> {
        if let Some(symbol) = ctx.error_printer_symbols.get(&value_ty) {
            return Ok(*symbol);
        }
        let owner = self.canonical_type_instance(value_ty).map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "a structural printer could not name its value type: {failure:?}"
                )),
                span,
            )
        })?;
        let ordinal = ctx.error_printer_symbols.len();
        let symbol = self.intern_body_symbol(format!("__rue_error_printer#{ordinal}"))?;
        self.register_synthesized_callable(
            symbol,
            crate::FunctionInstanceKey::ErrorPrinter(Node::new(owner)),
        );
        ctx.error_printer_symbols.insert(value_ty, symbol);
        Ok(symbol)
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

        // Failure arm: build the early `return`. Only a genuine side effect
        // belongs in the block's statement list: a block statement's own result
        // is a temporary the block drops at the end of the statement (RUE-65,
        // RUE-66), so naming a value there that the return expression still
        // consumes would drop it and then hand the dropped value to the
        // constructor (RUE-2051).
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
                (Some(drop_scrutinee), none_ctor)
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
                // `err_val` is the constructor's operand, not a statement: the
                // `Err` it builds owns the payload and carries it out of the
                // function.
                (None, err_ctor)
            }
        };
        let ret = air.add_inst(AirInst {
            data: AirInstData::Ret(Some(fail_ret_value)),
            ty: Type::NEVER,
            span,
        });
        let fail_body = air.add_block(fail_stmt.as_slice(), ret, Type::NEVER, span)?;

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
    /// enum), so a caller runs the E0706 visibility check only when it is
    /// false. `None` means the unqualified name is not an enum — the caller
    /// supplies the not-found diagnostic it wants (a user error at the legality
    /// check, an internal error during lowering).
    ///
    /// This is the single chokepoint every pattern-enum consumer funnels
    /// through. `resolve_variant_pattern` is its one caller inside match
    /// analysis, and every level of a pattern — the arm's own head and the
    /// nested payload patterns below it (RUE-2053) — resolves through that one
    /// call, so legality, AIR lowering and payload materialization all read
    /// the same answer. Folding the head forms here also made the inline
    /// type-constructor pattern head (`Result(i32, i32).Ok(v)`, RUE-596) a
    /// localized change: only this method comptime-evaluates a
    /// constructor-call head.
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

    /// Resolve one variant pattern against the value it is matched on: the
    /// enum it names, that enum's agreement with the scrutinee type, and the
    /// variant index. This is the per-level legality check shared by a match's
    /// own arms and by every nested payload pattern (RUE-2053).
    fn resolve_variant_pattern(
        &mut self,
        pattern: &RirPattern,
        scrutinee_type: Type,
        ctx: &AnalysisContext,
    ) -> CompileResult<(crate::types::EnumId, u32)> {
        let RirPattern::Path {
            module,
            ctor_head,
            type_name,
            variant,
            span,
            ..
        } = pattern
        else {
            return Err(CompileError::new(
                ErrorKind::InternalError("variant pattern expected".to_string()),
                pattern.span(),
            ));
        };
        let pattern_span = *span;
        // Look up the enum type — through a module, unqualified /
        // comptime-bound, or an inline type-constructor head — via the shared
        // pattern-enum chokepoint.
        let (enum_id, privacy_handled) = self
            .resolve_pattern_enum(*module, *ctor_head, *type_name, ctx, pattern_span)?
            .ok_or_compile_error(
                ErrorKind::UnknownEnumType(self.body_interner().resolve(&*type_name).to_string()),
                pattern_span,
            )?;
        // Privacy (E0706, RUE-185): a match pattern names the enum, so a
        // private enum from another directory cannot be matched on — privacy
        // is uniform across item kinds and positions (spec 10.3:1, 10.3:7).
        // Skipped when the name arrived through a module (already enforced on
        // the way in) or a comptime binding (exempt); both are reported as
        // `privacy_handled`.
        if !privacy_handled {
            let def = self.body_type_pool().enum_def(enum_id);
            self.check_item_visibility(
                crate::PrivateItemKind::Enum,
                self.body_interner().resolve(&*type_name),
                def.file_id,
                def.is_pub,
                pattern_span,
            )?;
        }
        // Check that the matched value's type is this pattern's enum type.
        if !self.types_equivalent(scrutinee_type, Type::new_enum(enum_id)) {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: self.format_type_name(scrutinee_type),
                    found: self.format_type_name(Type::new_enum(enum_id)),
                },
                pattern_span,
            ));
        }
        let enum_def = self.body_type_pool().enum_def(enum_id);
        let variant_name = self.body_interner().resolve(&*variant);
        let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: self.format_type_name(Type::new_enum(enum_id)),
                variant_name: variant_name.to_string(),
            },
            pattern_span,
        )?;
        Ok((enum_id, variant_index as u32))
    }

    /// Walk one arm's variant pattern down the match's nested-dispatch tree,
    /// creating the child matches its nested payload patterns need (RUE-2053).
    ///
    /// Every level is validated as it is reached, so an arm's placement and
    /// its legality come from a single pass. The returned levels are the
    /// variants the arm tests, outermost first; the arm binds each level's
    /// payload except the position a child match consumes.
    #[allow(clippy::too_many_arguments)]
    fn place_match_arm<'p>(
        &mut self,
        air: &mut Air,
        pattern: &'p RirPattern,
        root_scrutinee: AirRef,
        root_scrutinee_type: Type,
        plan: &AHashMap<Spur, NestedPlanNode>,
        nodes: &mut Vec<NestedMatchNode>,
        root_children: &mut AHashMap<u32, usize>,
        root_arms: &mut Vec<NestedMatchArm>,
        ctx: &AnalysisContext,
    ) -> CompileResult<ArmPlacement<'p>> {
        let mut levels = Vec::new();
        let mut node: Option<usize> = None;
        let mut scrutinee = root_scrutinee;
        let mut scrutinee_type = root_scrutinee_type;
        let mut plan = plan;
        let mut current = pattern;
        let mut path = String::new();
        loop {
            let (enum_id, variant_index) =
                self.resolve_variant_pattern(current, scrutinee_type, ctx)?;
            let RirPattern::Path {
                type_name,
                variant,
                elements,
                span,
                ..
            } = current
            else {
                unreachable!("resolve_variant_pattern accepts only path patterns")
            };
            let spelled = format!(
                "{}.{}",
                self.body_interner().resolve(type_name),
                self.body_interner().resolve(variant)
            );
            let Some(plan_node) = plan.get(variant) else {
                // No arm of this match decomposes this variant's payload, so
                // the arm lands here.
                levels.push(ArmLevel {
                    pattern: current,
                    scrutinee,
                    enum_id,
                    variant_index,
                    decomposed: None,
                });
                return Ok(ArmPlacement {
                    levels,
                    node,
                    scrutinee,
                    scrutinee_type,
                    air_pattern: AirPattern::EnumVariant {
                        enum_id,
                        variant_index,
                    },
                    variant_index: Some(variant_index),
                });
            };
            let field = plan_node.field;
            // Find or create the child match for this variant, extracting the
            // decomposed payload field once for every arm that reaches it.
            let children = match node {
                None => &mut *root_children,
                Some(index) => &mut nodes[index].children,
            };
            let child = match children.get(&variant_index).copied() {
                Some(child) => child,
                None => {
                    let payload_type =
                        self.variant_payload_field(enum_id, variant_index, field, *span, &spelled)?;
                    let payload = air.add_inst(AirInst {
                        data: AirInstData::EnumPayloadGet {
                            base: scrutinee,
                            enum_id,
                            variant_index,
                            field_index: field,
                        },
                        ty: payload_type,
                        span: *span,
                    });
                    let child_enum = payload_type.as_enum().ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::InvalidMatchType(self.format_type_name(payload_type)),
                            *span,
                        )
                    })?;
                    let index = nodes.len();
                    nodes.push(NestedMatchNode {
                        scrutinee: payload,
                        scrutinee_type: payload_type,
                        enum_id: child_enum,
                        path: format!("{path}{spelled}("),
                        span: *span,
                        covered_variants: AHashMap::new(),
                        wildcard_span: None,
                        arms: Vec::new(),
                        children: AHashMap::new(),
                    });
                    // The child match takes the arm slot of the variant it
                    // discriminates, where the first arm reaching it stood, so
                    // the dispatch keeps source order.
                    let arm = NestedMatchArm::Child {
                        pattern: AirPattern::EnumVariant {
                            enum_id,
                            variant_index,
                        },
                        node: index,
                    };
                    match node {
                        None => {
                            root_children.insert(variant_index, index);
                            root_arms.push(arm);
                        }
                        Some(parent) => {
                            nodes[parent].children.insert(variant_index, index);
                            nodes[parent].arms.push(arm);
                        }
                    }
                    index
                }
            };
            // Reaching the child covers this variant here, however the arm
            // discriminates the payload further; the match's own arm list keeps
            // that bookkeeping in `analyze_match`.
            if let Some(index) = node {
                nodes[index]
                    .covered_variants
                    .entry(variant_index)
                    .or_insert(*span);
            }
            let child_scrutinee = nodes[child].scrutinee;
            let child_type = nodes[child].scrutinee_type;
            match elements.get(field as usize) {
                Some(rue_rir::RirPatternElement::Nested(nested)) => {
                    levels.push(ArmLevel {
                        pattern: current,
                        scrutinee,
                        enum_id,
                        variant_index,
                        decomposed: Some(DecomposedPosition::Consumed(field)),
                    });
                    path = nodes[child].path.clone();
                    node = Some(child);
                    scrutinee = child_scrutinee;
                    scrutinee_type = child_type;
                    plan = &plan_node.children;
                    current = nested;
                }
                // A binder (or the bare all-wildcard form) at the decomposed
                // position: the arm binds the whole payload, so it is the
                // child match's catch-all arm.
                _ => {
                    levels.push(ArmLevel {
                        pattern: current,
                        scrutinee,
                        enum_id,
                        variant_index,
                        decomposed: Some(DecomposedPosition::Bound(field, child_scrutinee)),
                    });
                    return Ok(ArmPlacement {
                        levels,
                        node: Some(child),
                        scrutinee: child_scrutinee,
                        scrutinee_type: child_type,
                        air_pattern: AirPattern::Wildcard,
                        variant_index: None,
                    });
                }
            }
        }
    }

    /// The type of one payload field of a variant, rejecting a decomposed
    /// position the variant does not have.
    fn variant_payload_field(
        &mut self,
        enum_id: crate::types::EnumId,
        variant_index: u32,
        field: u32,
        span: Span,
        spelled: &str,
    ) -> CompileResult<Type> {
        let def = self.body_type_pool().enum_def(enum_id);
        let payload = def.variant_payload(variant_index as usize);
        payload
            .get(field as usize)
            .copied()
            .ok_or_compile_error(
                ErrorKind::WrongArgumentCount {
                    expected: payload.len(),
                    found: field as usize + 1,
                },
                span,
            )
            .map_err(|error| error.with_note(format!("in a payload pattern for `{spelled}`")))
    }

    /// Record a placed arm in its dispatch node's coverage bookkeeping, and
    /// warn when an earlier arm of that node already matches everything it
    /// could (spec 4.7:20). The match's own arm list keeps its bookkeeping in
    /// `analyze_match`; this covers the child matches nested patterns create.
    fn record_placed_pattern(
        nodes: &mut [NestedMatchNode],
        placement: &ArmPlacement<'_>,
        pattern_span: Span,
        outer_wildcard: Option<Span>,
        interner: &lasso::ThreadedRodeo,
        warnings: &mut Vec<CompileWarning>,
    ) {
        let Some(index) = placement.node else {
            return;
        };
        let path = nodes[index].path.clone();
        let spell = |node: &NestedMatchNode, text: &str| format!("{}{text})", node.path);
        let node = &mut nodes[index];
        // An arm reached through a child match is unreachable when an earlier
        // arm of that child already covered the same variant, or covered
        // everything with a binder at the decomposed position.
        let earlier = match placement.variant_index {
            Some(variant) => node
                .covered_variants
                .get(&variant)
                .copied()
                .or(node.wildcard_span),
            None => node.wildcard_span,
        };
        let unreachable_after = earlier.or(outer_wildcard);
        if let Some(first_span) = unreachable_after {
            let spelled = match placement.variant_index {
                Some(_) => match placement.levels.last().map(|level| level.pattern) {
                    Some(RirPattern::Path {
                        type_name, variant, ..
                    }) => spell(
                        node,
                        &format!(
                            "{}.{}",
                            interner.resolve(type_name),
                            interner.resolve(variant)
                        ),
                    ),
                    _ => format!("{path}_)"),
                },
                None => format!("{path}_)"),
            };
            warnings.push(
                CompileWarning::new(WarningKind::UnreachablePattern(spelled), pattern_span)
                    .with_label("first occurrence of this pattern", first_span)
                    .with_note(
                        "this pattern will never be matched because an earlier arm already \
                         matches the same value",
                    ),
            );
        }
        match placement.variant_index {
            Some(variant) => {
                node.covered_variants.entry(variant).or_insert(pattern_span);
            }
            None => {
                if node.wildcard_span.is_none() {
                    node.wildcard_span = Some(pattern_span);
                }
            }
        }
    }

    /// Assemble the AIR arms of one dispatch node, building each child match
    /// on the payload its parent extracted.
    fn build_nested_arms(
        air: &mut Air,
        nodes: &[NestedMatchNode],
        arms: &[NestedMatchArm],
        result_type: Type,
    ) -> CompileResult<Vec<(AirPattern, AirRef)>> {
        let mut out = Vec::with_capacity(arms.len());
        for arm in arms {
            match arm {
                NestedMatchArm::Direct(pattern, body) => out.push((pattern.clone(), *body)),
                NestedMatchArm::Child { pattern, node } => {
                    let child = &nodes[*node];
                    let inner = Self::build_nested_arms(air, nodes, &child.arms, result_type)?;
                    let body = air.add_match(child.scrutinee, &inner, result_type, child.span)?;
                    out.push((pattern.clone(), body));
                }
            }
        }
        Ok(out)
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
    ///
    /// The level carries the enum and variant `place_match_arm` already
    /// resolved for it, so nothing is resolved twice, along with the payload
    /// position a child match handles for a nested pattern (RUE-2053): a
    /// position the nested pattern consumes binds nothing here, and a position
    /// the arm binds takes the value that child match dispatched on rather
    /// than projecting the field again.
    fn materialize_match_bindings(
        &mut self,
        air: &mut Air,
        level: &ArmLevel<'_>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<u32>> {
        let RirPattern::Path {
            variant,
            elements,
            span,
            ..
        } = level.pattern
        else {
            return Ok(Vec::new());
        };
        let pattern_span = *span;
        let (enum_id, variant_index) = (level.enum_id, level.variant_index);
        let scrutinee_ref = level.scrutinee;
        let def = self.body_type_pool().enum_def(enum_id);
        let variant_name = self.body_interner().resolve(&*variant).to_string();
        let enum_name = self.format_type_name(Type::new_enum(enum_id));
        let payload = def.variant_payload(variant_index as usize).to_vec();

        // The bare-path carve-out (spec 4.7:30, RUE-1592): a payload-carrying
        // variant written with no binding list at all (`E.A`) IS the
        // all-wildcard form `E.A(_, …, _)`, so it is exempt from the arity
        // rule rather than being an arity error. Every other pattern must
        // supply exactly as many binding positions as the variant's arity —
        // `E.A(x, y)` on an arity-1 variant stays E0207.
        let bare_path = elements.is_empty();
        if !bare_path && elements.len() != payload.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: payload.len(),
                    found: elements.len(),
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
        let binders = |elements: &[rue_rir::RirPatternElement]| -> Vec<Option<Spur>> {
            elements
                .iter()
                .map(|element| match element {
                    rue_rir::RirPatternElement::Binding(name) => Some(*name),
                    rue_rir::RirPatternElement::Nested(_) => None,
                })
                .collect()
        };
        let binders = binders(elements);
        for (i, name) in binders.iter().enumerate() {
            let Some(name) = name else {
                continue;
            };
            if self.body_interner().resolve(name) == "_" {
                continue;
            }
            if binders
                .iter()
                .take(i)
                .any(|existing| existing.as_ref() == Some(name))
            {
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
            // A position a nested pattern occupies is consumed by the child
            // match this arm was dispatched through (RUE-2053): that match's
            // arms account for the field, so nothing is bound or dropped here.
            if matches!(
                level.decomposed,
                Some(DecomposedPosition::Consumed(field)) if field as usize == i
            ) {
                continue;
            }
            // The source name at this position, or `None` when the position
            // binds nothing: an explicit `_` discard (RUE-601), or any
            // position of the bare-path all-wildcard form `E.A` (RUE-1592).
            let binding_name = if bare_path {
                None
            } else {
                let name = binders[i];
                name.filter(|name| self.body_interner().resolve(name) != "_")
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
                        type_name: self.format_type_name(field_ty),
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

            // Read the payload field out of the scrutinee. A position a child
            // match already extracted takes that same value rather than
            // projecting it a second time (RUE-2053).
            let get_ref = match level.decomposed {
                Some(DecomposedPosition::Bound(field, extracted)) if field as usize == i => {
                    extracted
                }
                _ => air.add_inst(AirInst {
                    data: AirInstData::EnumPayloadGet {
                        base: scrutinee_ref,
                        enum_id,
                        variant_index,
                        field_index: i as u32,
                    },
                    ty: field_ty,
                    span: pattern_span,
                }),
            };

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
                InstData::Intrinsic { name, .. } if *name == self.known_symbols().intrinsic(IntrinsicName::Place)
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
                        expected: self.format_type_name(ctx.return_type),
                        found: self.format_type_name(pointee),
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
                    expected: self.format_type_name(ctx.return_type),
                    found: self.format_type_name(read.ty),
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
        let self_sym = self.intern_body_symbol("self")?;
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
        let (inner_air_ref, edge_reachable) = if let Some(inner) = inner {
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
                        expected: self.format_type_name(ctx.return_type),
                        found: self.format_type_name(inner_ty),
                    },
                    span,
                ));
            }
            (Some(inner_result.air_ref), inner_result.continues)
        } else {
            // `return;` without expression - only valid for unit-returning functions
            if ctx.return_type != Type::UNIT && !ctx.return_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: self.format_type_name(ctx.return_type),
                        found: "()".to_string(),
                    },
                    span,
                ));
            }
            (None, true)
        };

        // The return ends every open scope: their live bindings — and the
        // by-value parameters — are dropped at this edge (spec 4.9:7), so a
        // linear value any of them holds must already be consumed in the
        // state in force HERE, after the operand's own consumptions
        // (RUE-1614). The enclosing joins exclude this diverging arm, so no
        // scope-exit check ever observes this state; without the edge check a
        // conditional return leaked the value into the return path's unwind
        // drops. An operand that itself diverges never reaches this edge, so
        // its obligation sites report instead (no double-report).
        if edge_reachable {
            self.check_linear_values_at_exit_edge(ctx, 0, true)?;
            ctx.divergence_kinds.insert(DivergenceKind::Exit);
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Ret(inner_air_ref),
            ty: Type::NEVER, // Return expressions have Never type
            span,
        });
        Ok(AnalysisResult::diverged(air_ref, Type::NEVER))
    }

    /// Analyze a block expression.
    fn analyze_block(
        &mut self,
        air: &mut Air,
        instructions: &rue_rir::RirBlockInstsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let prior_divergence = ctx.divergence_kinds;
        ctx.divergence_kinds = DivergenceKinds::NONE;
        // Get the instruction refs from extra data
        let inst_refs = self.body_rir_ref().block_insts(instructions).to_vec();
        // Push a new scope for this block.
        ctx.push_scope();

        // Process all instructions in the block
        let mut statements = Vec::new();
        let mut last_result: Option<AnalysisResult> = None;
        let mut diverged = false;
        let mut reachable_divergence = DivergenceKinds::NONE;
        let mut diverged_context: Option<AnalysisContext> = None;
        let num_insts = inst_refs.len();
        for (i, inst_ref) in inst_refs.iter().copied().enumerate() {
            let is_last = i == num_insts - 1;
            // Statement recovery starts after body-wide inference has
            // succeeded. Inference failures use the exact selections observed
            // during constraint generation instead; substitution-dependent
            // selections make that transaction non-terminal.
            // Each statement is its own sequencing boundary. Clear the
            // transient edge classification so a dead suffix cannot change
            // the kind captured at the first reachable divergence.
            ctx.divergence_kinds = DivergenceKinds::NONE;
            let recovery_checkpoint = self
                .body_analysis_error_recovery()
                .then(|| (air.checkpoint(), ctx.clone()));
            // Each non-tail statement is a nested full expression. Enclosing
            // accessor loans remain active through it, but completed reads and
            // exclusive uses belong to the enclosing expression and must not
            // enter the child. The tail expression remains part of the
            // enclosing full expression and therefore needs no boundary.
            let boundary = (!is_last).then(|| ctx.ownership.enter_full_expression());
            let outcome = if is_last {
                self.analyze_inst(air, inst_ref, ctx)
            } else {
                ctx.with_expected_type(None, |ctx| self.analyze_inst(air, inst_ref, ctx))
            };
            if let Some(boundary) = boundary {
                ctx.ownership.exit_full_expression(boundary);
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

            let mut statement_divergence = ctx.divergence_kinds;
            if !diverged && !result.continues && statement_divergence.has_other() {
                // Validate the generic edge before any enclosing join can
                // replace its ownership state, then keep only provenance that
                // still needs downstream handling. A checked generic edge is
                // not allowed to contaminate a later explicit panic path.
                self.check_linear_values_at_unchecked_divergence(ctx)?;
                statement_divergence = statement_divergence.without_other();
                ctx.divergence_kinds = ctx.divergence_kinds.without_other();
            }
            if !diverged {
                reachable_divergence = reachable_divergence.union(statement_divergence);
            }

            if is_last {
                last_result = Some(result);
                if !result.continues && !diverged {
                    diverged = true;
                    diverged_context = Some(ctx.clone());
                }
            } else {
                // A non-final statement's value is discarded. Discarding a
                // value that carries a linear value would implicitly drop it
                // (`make_linear();` — RUE-176), which linearity forbids.
                self.reject_discarded_linear_value(result.ty, inst_ref)?;
                statements.push(result.air_ref);
                // The remaining instructions are still analyzed above so
                // unreachable-code diagnostics and ordinary semantic errors
                // remain intact. Once a statement diverges, however, no tail
                // reaches the block exit: its outgoing ownership state is
                // bottom and its synthesized block type is Never, even when
                // the parser supplied a synthetic unit tail.
                if !result.continues && !diverged {
                    diverged = true;
                    diverged_context = Some(ctx.clone());
                }
            }
        }

        // Instructions after a diverging statement are analyzed for
        // diagnostics, but their moves belong to no reachable path. Restore
        // the state at the divergence before the block's scope checks and
        // enclosing joins observe it.
        // Check live obligations against the snapshot, while retaining the
        // current context for dead-suffix diagnostics and append-only data.
        let reachable_moves = diverged_context
            .as_ref()
            .map(|reachable| reachable.ownership.moved_vars.clone());
        if diverged_context.is_some() {
            // Unchecked divergent edges were validated at the edge, while an
            // explicit panic edge has no scope-exit obligation. Keep only the
            // reachable ownership snapshot for the enclosing join.
        } else {
            self.check_unconsumed_linear_values(ctx)?;
        }

        // Check for unused variables before popping scope
        self.check_unused_locals_in_current_scope(ctx);

        if let Some(moves) = reachable_moves {
            ctx.ownership.moved_vars = moves;
        }
        if let Some(reachable_context) = diverged_context.as_ref() {
            // Dead suffixes are still analyzed for diagnostics, but their
            // break/continue snapshots cannot reach the enclosing join. Keep
            // the snapshots from the first reachable divergence while
            // preserving `broke` from the full analysis: loop typing is
            // syntactic even when a break occurs only in unreachable code
            // (4.8:21).
            assert_eq!(
                reachable_context.ownership.loop_break_stack.len(),
                ctx.ownership.loop_break_stack.len()
            );
            let mut reachable_edges = reachable_context.ownership.loop_break_stack.clone();
            for (reachable, analyzed) in reachable_edges
                .iter_mut()
                .zip(&ctx.ownership.loop_break_stack)
            {
                reachable.broke |= analyzed.broke;
            }
            ctx.ownership.loop_break_stack = reachable_edges;
        }
        // Pop scope to remove block-scoped variables. The reachable move
        // state is restored first so this frame removes dead locals and
        // restores any shadowed outer bindings normally.
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

        ctx.divergence_kinds = prior_divergence.union(reachable_divergence);

        // Only create a Block instruction if there are statements;
        // otherwise just return the value directly (optimization)
        if statements.is_empty() {
            Ok(if diverged && last.continues {
                AnalysisResult::diverged(last.air_ref, Type::NEVER)
            } else {
                last
            })
        } else {
            let ty = if diverged { Type::NEVER } else { last.ty };
            let air_ref = air.add_block(&statements, last.air_ref, ty, span)?;
            Ok(AnalysisResult::with_continues(air_ref, ty, !diverged))
        }
    }
}

/// The value an integer pattern denotes: the magnitude its literal spells,
/// negated when the pattern is written with a leading `-`. The magnitude is
/// carried unsigned so `-9223372036854775808` names `i64::MIN` without the
/// operand itself having to be representable; the i128 arithmetic is what
/// makes that exact rather than a wrapping trick.
fn pattern_int_denoted(value: u64, negative: bool) -> i128 {
    let magnitude = i128::from(value);
    if negative { -magnitude } else { magnitude }
}

#[cfg(test)]
mod pattern_materialization_tests {
    /// RUE-1661: `analyze_match` materializes owned patterns exactly once —
    /// the per-arm loop whose AIR lowering consumes owned binding lists. The
    /// comptime-selection scans, the pruned-arm warning pass, and the
    /// expected-scrutinee probe all iterate the borrowed `RirPatternView`,
    /// so a reintroduced whole-arm clone fails here.
    #[test]
    fn analyze_match_materializes_patterns_once() {
        let production = include_str!("control_flow.rs")
            .split_once("mod pattern_materialization_tests")
            .expect("this test module marks the end of production code")
            .0;
        assert_eq!(
            production.matches("pattern.to_owned()").count(),
            1,
            "only the per-arm analysis loop may materialize owned patterns"
        );
        let probe = production
            .split_once("let path_pattern_type_names")
            .expect("expected-scrutinee probe exists")
            .1;
        assert!(
            probe
                .split_once("let expected_scrutinee")
                .expect("probe feeds the expected-scrutinee resolution")
                .0
                .contains("RirPatternView::Path { type_name, .. }"),
            "the expected-scrutinee probe reads type names through the view"
        );
    }
}
