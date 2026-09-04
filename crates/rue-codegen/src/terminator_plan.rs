//! Target-independent CFG block and terminator lowering policy.
//!
//! This module is the single owner of CFG control-flow topology.  It copies
//! only normalized values and logical slots into [`TerminatorPlan`]; concrete
//! backends decide how those facts become branch, move, return, and trap MIR.

use std::ops::Range;

use ahash::AHashSet;
use rue_cfg::{BasicBlock, BlockId, CfgValue, Terminator, Type};

use crate::call_plan::{self, ReturnPlan};
use crate::cfg_lower::CfgLowerContext;
use crate::value_plan::{self, MaterializedValue, ValueLowerAdapter, ValuePlan, ValueShape};
use crate::vreg::VReg;

/// One logical slot move performed on a control-flow edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotMove {
    /// Position of the CFG edge argument in the target block's parameter
    /// list. This distinguishes two aggregate arguments carried by one edge.
    pub argument_index: u32,
    /// Logical slot within that edge argument, in ascending layout order.
    pub slot_index: u32,
    pub source: VReg,
    pub destination: VReg,
    pub float_width: Option<crate::value_plan::FloatWidth>,
}

/// Normalized work for one CFG successor edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgePlan {
    pub target: BlockId,
    pub moves: Vec<SlotMove>,
    /// Whether the target is the next logical block in deterministic CFG
    /// order.  The adapter may omit an unconditional jump only in this case.
    pub fallthrough: bool,
}

/// Ordered switch case information.  The source CFG order is preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchCasePlan {
    pub value: i64,
    pub target: BlockId,
}

/// Target-neutral return value shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnValuePlan {
    ZeroSized,
    Scalar {
        value: VReg,
        float_width: Option<crate::value_plan::FloatWidth>,
    },
    Aggregate {
        slots: Vec<VReg>,
        return_plan: ReturnPlan,
    },
}

/// Target-neutral return behavior. `Exit` is the language-level `main`
/// termination mode; its manifest-derived call plan is shared, while the
/// adapter assigns the platform argument registers and call instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnMode {
    Exit {
        call: crate::runtime_call_plan::RuntimeCallPlan,
    },
    Function {
        value: ReturnValuePlan,
    },
}

/// Exhaustive target-neutral terminator plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminatorPlan {
    Goto {
        edge: EdgePlan,
    },
    Branch {
        condition: VReg,
        then_edge: EdgePlan,
        else_edge: EdgePlan,
    },
    Switch {
        scrutinee: VReg,
        /// The comparison width selected by the language policy.
        width: value_plan::IntegerWidth,
        cases: Vec<SwitchCasePlan>,
        default: BlockId,
    },
    Return {
        mode: ReturnMode,
    },
    Unreachable,
}

/// Stable cross-backend evidence for the policy decisions in a terminator
/// plan.  It contains no vregs or target labels, so x86-64 and AArch64 traces
/// can be compared before instruction encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminatorTrace {
    pub kind: TerminatorTraceKind,
    pub successors: Vec<BlockId>,
    pub fallthrough: Vec<bool>,
    pub edge_move_counts: Vec<usize>,
    /// Ordered logical source/destination slot positions for every edge.
    pub edge_work: Vec<Vec<(u32, u32)>>,
    pub switch_width: Option<value_plan::IntegerWidth>,
    pub switch_cases: Vec<(i64, BlockId)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminatorTraceKind {
    Goto,
    Branch,
    Switch,
    ReturnExit,
    ReturnUnit,
    ReturnScalar,
    ReturnAggregateRegisters,
    ReturnAggregateSret,
    ReturnAggregateZeroSized,
    Unreachable,
}

impl TerminatorPlan {
    /// Extract only policy facts for cross-backend trace comparisons.
    pub fn trace(&self) -> TerminatorTrace {
        match self {
            Self::Goto { edge } => TerminatorTrace {
                kind: TerminatorTraceKind::Goto,
                successors: vec![edge.target],
                fallthrough: vec![edge.fallthrough],
                edge_move_counts: vec![edge.moves.len()],
                edge_work: vec![
                    edge.moves
                        .iter()
                        .map(|movement| (movement.argument_index, movement.slot_index))
                        .collect(),
                ],
                switch_width: None,
                switch_cases: Vec::new(),
            },
            Self::Branch {
                then_edge,
                else_edge,
                ..
            } => TerminatorTrace {
                kind: TerminatorTraceKind::Branch,
                successors: vec![then_edge.target, else_edge.target],
                fallthrough: vec![then_edge.fallthrough, else_edge.fallthrough],
                edge_move_counts: vec![then_edge.moves.len(), else_edge.moves.len()],
                edge_work: vec![
                    then_edge
                        .moves
                        .iter()
                        .map(|movement| (movement.argument_index, movement.slot_index))
                        .collect(),
                    else_edge
                        .moves
                        .iter()
                        .map(|movement| (movement.argument_index, movement.slot_index))
                        .collect(),
                ],
                switch_width: None,
                switch_cases: Vec::new(),
            },
            Self::Switch {
                width,
                cases,
                default,
                ..
            } => TerminatorTrace {
                kind: TerminatorTraceKind::Switch,
                successors: cases
                    .iter()
                    .map(|case| case.target)
                    .chain(std::iter::once(*default))
                    .collect(),
                fallthrough: Vec::new(),
                edge_move_counts: Vec::new(),
                edge_work: Vec::new(),
                switch_width: Some(*width),
                switch_cases: cases.iter().map(|case| (case.value, case.target)).collect(),
            },
            Self::Return { mode } => {
                let kind = match mode {
                    ReturnMode::Exit { .. } => TerminatorTraceKind::ReturnExit,
                    ReturnMode::Function { value } => match value {
                        ReturnValuePlan::ZeroSized => TerminatorTraceKind::ReturnUnit,
                        ReturnValuePlan::Scalar { .. } => TerminatorTraceKind::ReturnScalar,
                        ReturnValuePlan::Aggregate { return_plan, .. } => match return_plan {
                            ReturnPlan::ZeroSized => TerminatorTraceKind::ReturnAggregateZeroSized,
                            ReturnPlan::Registers { .. } => {
                                TerminatorTraceKind::ReturnAggregateRegisters
                            }
                            ReturnPlan::Sret { .. } => TerminatorTraceKind::ReturnAggregateSret,
                            ReturnPlan::Scalar => unreachable!("aggregate return cannot be scalar"),
                        },
                    },
                };
                TerminatorTrace {
                    kind,
                    successors: Vec::new(),
                    fallthrough: Vec::new(),
                    edge_move_counts: Vec::new(),
                    edge_work: Vec::new(),
                    switch_width: None,
                    switch_cases: Vec::new(),
                }
            }
            Self::Unreachable => TerminatorTrace {
                kind: TerminatorTraceKind::Unreachable,
                successors: Vec::new(),
                fallthrough: Vec::new(),
                edge_move_counts: Vec::new(),
                edge_work: Vec::new(),
                switch_width: None,
                switch_cases: Vec::new(),
            },
        }
    }
}

/// The operations needed by the shared policy to obtain vregs and emit one
/// normalized plan.  No operation accepts a target instruction, register, or
/// target label.  CFG values are lookup keys used while building the plan and
/// never appear in the plan itself.
pub trait TerminatorAdapter {
    fn materialize_value(&mut self, value: CfgValue, plan: ValuePlan) -> MaterializedValue;
    fn materialize_block_param(
        &mut self,
        target: BlockId,
        param_index: u32,
        target_value: CfgValue,
        plan: ValuePlan,
    ) -> MaterializedValue;
    fn emit_block_label(&mut self, block: BlockId);
    fn emit_terminator(&mut self, plan: TerminatorPlan);
}

/// Debug and presentation hooks layered on the shared CFG/value traversal.
pub trait CfgLowerAdapter: TerminatorAdapter + ValueLowerAdapter {
    fn preload_by_ref_params(&mut self);
    fn prepare_block_param(&mut self, block: BlockId, index: u32, value: CfgValue, ty: Type);
    fn value_description(&self, value: CfgValue) -> String;
    fn instruction_count(&self) -> usize;
    fn instruction_strings(&self, range: Range<usize>) -> Vec<String>;
    fn value_rationale(&self, kind: value_plan::ValueKind, ty: Type) -> Option<String>;
    fn terminator_rationale(&self, plan: &TerminatorPlan) -> Option<String>;
}

/// The block visiting order lowering uses, plus the inverse lookup the
/// fallthrough policy needs.
///
/// Lowering materializes a CFG value into whichever block is being lowered when
/// the value is first demanded, so the visiting order has to place every
/// definition's block before every block that uses it (RUE-1758). The order
/// comes from [`rue_cfg::Cfg::block_lowering_order`], which derives it from the
/// dominator tree the verifier already holds every operand to; it equals arena
/// order whenever arena order is itself such an order.
pub(crate) struct BlockOrder {
    blocks: Vec<BlockId>,
    /// Position in `blocks` by raw block id, which is also the block's arena
    /// index.
    position: Vec<u32>,
}

impl BlockOrder {
    fn of(ctx: &CfgLowerContext<'_>) -> Self {
        let blocks = ctx.cfg.block_lowering_order();
        let mut position = vec![0; ctx.cfg.block_count()];
        for (index, block) in blocks.iter().enumerate() {
            position[block.as_u32() as usize] = index as u32;
        }
        Self { blocks, position }
    }

    fn blocks(&self) -> &[BlockId] {
        &self.blocks
    }

    /// The block emitted immediately after `block`, or `None` at the end of the
    /// function.
    fn next(&self, block: BlockId) -> Option<BlockId> {
        let index = *self.position.get(block.as_u32() as usize)? as usize;
        self.blocks.get(index + 1).copied()
    }
}

fn moves_for_edge<A: TerminatorAdapter>(
    ctx: &CfgLowerContext<'_>,
    order: &BlockOrder,
    adapter: &mut A,
    source_values: &[CfgValue],
    target: BlockId,
    source_block: BlockId,
) -> EdgePlan {
    let target_block = ctx.cfg.get_block(target);
    assert_eq!(
        source_values.len(),
        target_block.params.len(),
        "edge argument count must match target block parameter count"
    );

    let mut moves = Vec::new();
    for (index, &value) in source_values.iter().enumerate() {
        let plan = ValuePlan::for_value(ctx, value);
        let source = adapter.materialize_value(value, plan);
        let target_value = target_block.params[index].0;
        let destination = adapter.materialize_block_param(target, index as u32, target_value, plan);

        match plan.shape {
            ValueShape::ZeroSized => {
                assert!(
                    source.slots.is_empty() && destination.slots.is_empty(),
                    "zero-sized edge values must not fall back to a scalar slot"
                );
            }
            ValueShape::Scalar => {
                assert_eq!(
                    source.slots.len(),
                    0,
                    "scalar values have no aggregate slots"
                );
                assert_eq!(destination.slots.len(), 0);
                moves.push(SlotMove {
                    argument_index: index as u32,
                    slot_index: 0,
                    source: source.primary,
                    destination: destination.primary,
                    float_width: crate::value_plan::float_width(ctx.cfg.get_inst(value).ty),
                });
            }
            ValueShape::CompleteAggregate { slot_count } => {
                assert_eq!(source.slots.len(), slot_count as usize);
                assert_eq!(destination.slots.len(), slot_count as usize);
                let leaf_types =
                    crate::types::aggregate_leaf_types(ctx.type_pool, ctx.cfg.get_inst(value).ty);
                moves.extend(
                    source
                        .slots
                        .iter()
                        .copied()
                        .zip(destination.slots.iter().copied())
                        .enumerate()
                        .map(|(slot_index, (source, destination))| SlotMove {
                            argument_index: index as u32,
                            slot_index: slot_index as u32,
                            source,
                            destination,
                            float_width: crate::value_plan::float_width(leaf_types[slot_index]),
                        }),
                );
            }
        }
    }

    // Edge moves are emitted sequentially, so consuming an earlier
    // destination would silently change the generated program.
    assert_sequential_move_safety(&moves);

    EdgePlan {
        target,
        moves,
        fallthrough: order.next(source_block) == Some(target),
    }
}

fn assert_sequential_move_safety(moves: &[SlotMove]) {
    let mut earlier_destinations = AHashSet::with_capacity(moves.len());
    for movement in moves {
        assert!(
            !earlier_destinations.contains(&movement.source),
            "edge move destination must not be used as a later move source"
        );
        earlier_destinations.insert(movement.destination);
    }
}

fn return_value<A: TerminatorAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    value: CfgValue,
    ret_reg_budget: u32,
) -> ReturnValuePlan {
    let value_plan = ValuePlan::for_value(ctx, value);
    let materialized = adapter.materialize_value(value, value_plan);
    match value_plan.shape {
        ValueShape::ZeroSized => ReturnValuePlan::ZeroSized,
        ValueShape::Scalar => ReturnValuePlan::Scalar {
            value: materialized.primary,
            float_width: crate::value_plan::float_width(ctx.cfg.get_inst(value).ty),
        },
        ValueShape::CompleteAggregate { slot_count } => {
            assert_eq!(materialized.slots.len(), slot_count as usize);
            let return_plan =
                call_plan::return_plan(ctx.type_pool, ctx.cfg.return_type(), ret_reg_budget);
            assert_eq!(return_plan.slot_count(), slot_count);
            ReturnValuePlan::Aggregate {
                slots: materialized.slots,
                return_plan,
            }
        }
    }
}

/// Build the one canonical plan for a CFG terminator.
pub(crate) fn plan_terminator<A: TerminatorAdapter>(
    ctx: &CfgLowerContext<'_>,
    order: &BlockOrder,
    adapter: &mut A,
    block: &BasicBlock,
    fn_name: &str,
    ret_reg_budget: u32,
) -> TerminatorPlan {
    match &block.terminator {
        Terminator::Goto { target, .. } => TerminatorPlan::Goto {
            edge: moves_for_edge(
                ctx,
                order,
                adapter,
                ctx.cfg.get_goto_args(&block.terminator),
                *target,
                block.id,
            ),
        },
        Terminator::Branch {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let condition_plan = ValuePlan::for_value(ctx, *cond);
            let condition = adapter.materialize_value(*cond, condition_plan).primary;
            let then_edge = moves_for_edge(
                ctx,
                order,
                adapter,
                ctx.cfg.get_branch_then_args(&block.terminator),
                *then_block,
                block.id,
            );
            let mut else_edge = moves_for_edge(
                ctx,
                order,
                adapter,
                ctx.cfg.get_branch_else_args(&block.terminator),
                *else_block,
                block.id,
            );
            // A branch whose two arms name the next physical block still has
            // only one fallthrough layout. Keep the then arm as the emitted
            // fallthrough and make the else arm's explicit jump visible in
            // the normalized policy consumed by both adapters.
            if then_edge.fallthrough && else_edge.fallthrough {
                else_edge.fallthrough = false;
            }
            TerminatorPlan::Branch {
                condition,
                then_edge,
                else_edge,
            }
        }
        Terminator::Switch {
            scrutinee, default, ..
        } => {
            let value_plan = ValuePlan::for_value(ctx, *scrutinee);
            let scrutinee_vreg = adapter.materialize_value(*scrutinee, value_plan).primary;
            let ty = ctx.cfg.get_inst(*scrutinee).ty;
            let width = value_plan::comparison_integer_width(ty);
            let cases = ctx
                .cfg
                .get_switch_cases(&block.terminator)
                .iter()
                .copied()
                .map(|(value, target)| SwitchCasePlan { value, target })
                .collect();
            TerminatorPlan::Switch {
                scrutinee: scrutinee_vreg,
                width,
                cases,
                default: *default,
            }
        }
        Terminator::Return { value } => {
            let mode = if fn_name == "main" {
                let value = (*value).map(|value| {
                    let plan = ValuePlan::for_value(ctx, value);
                    adapter.materialize_value(value, plan).primary
                });
                ReturnMode::Exit {
                    call: crate::runtime_call_plan::RuntimeCallPlan::expect_manifest(
                        rue_runtime_abi::RuntimeHelperId::Exit,
                        [value.map_or_else(
                            || {
                                crate::runtime_call_plan::RuntimeCallArg::immediate(
                                    0,
                                    rue_runtime_abi::AbiType::I32,
                                )
                            },
                            |value| {
                                crate::runtime_call_plan::RuntimeCallArg::value(
                                    value,
                                    rue_runtime_abi::AbiType::I32,
                                )
                            },
                        )],
                    ),
                }
            } else {
                ReturnMode::Function {
                    value: (*value).map_or(ReturnValuePlan::ZeroSized, |value| {
                        return_value(ctx, adapter, value, ret_reg_budget)
                    }),
                }
            };
            TerminatorPlan::Return { mode }
        }
        Terminator::Unreachable => TerminatorPlan::Unreachable,
        Terminator::None => panic!("block has no terminator"),
    }
}

/// Lower every CFG block through the same block order, label policy, and
/// terminator planner.  The optional debug sink observes these exact plans;
/// it does not invoke a presentation-specific lowerer.
///
/// Blocks are visited in [`BlockOrder`], not arena order: a value is emitted
/// into whichever block first demands it, so a use must never be lowered
/// before its definition's block (RUE-1758).
pub(crate) fn lower_cfg<A: CfgLowerAdapter>(
    ctx: &CfgLowerContext<'_>,
    adapter: &mut A,
    mut debug_info: Option<&mut crate::LoweringDebugInfo>,
    ret_reg_budget: u32,
    cancellation: crate::GenerationCancellation<'_>,
) -> rue_error::CompileResult<()> {
    adapter.preload_by_ref_params();
    let order = BlockOrder::of(ctx);

    for block in ctx.cfg.blocks() {
        for (index, &(value, ty)) in block.params.iter().enumerate() {
            adapter.prepare_block_param(block.id, index as u32, value, ty);
        }
    }

    for &block_id in order.blocks() {
        // The per-block check bounds a stale attempt's residual work to one
        // block of lowering (RUE-1827).
        cancellation.check()?;
        let block = ctx.cfg.get_block(block_id);
        let mut block_info = debug_info.as_deref_mut().map(|_| crate::BlockLoweringInfo {
            block_id: block.id,
            instructions: Vec::new(),
            terminator: None,
        });

        if let Some(info) = block_info.as_mut() {
            for &(value, _) in &block.params {
                let inst = ctx.cfg.get_inst(value);
                info.instructions.push(crate::LoweringDecision {
                    cfg_value: value,
                    cfg_inst_desc: adapter.value_description(value),
                    cfg_type: inst.ty.name().to_string(),
                    mir_insts: Vec::new(),
                    rationale: Some("Block parameter preallocated at the join".to_string()),
                });
            }
        }

        if block.id != ctx.cfg.entry {
            adapter.emit_block_label(block.id);
        }

        for &value in &block.insts {
            if let Some(info) = block_info.as_mut() {
                if adapter.value_is_lowered(value) {
                    continue;
                }
                let inst = ctx.cfg.get_inst(value);
                let start = adapter.instruction_count();
                let kind = value_plan::lower_value(ctx, adapter, value)
                    .expect("debug lowering should visit each value exactly once");
                let end = adapter.instruction_count();
                info.instructions.push(crate::LoweringDecision {
                    cfg_value: value,
                    cfg_inst_desc: adapter.value_description(value),
                    cfg_type: inst.ty.name().to_string(),
                    mir_insts: adapter.instruction_strings(start..end),
                    rationale: adapter.value_rationale(kind, inst.ty),
                });
            } else {
                value_plan::lower_value(ctx, adapter, value);
            }
        }

        let plan = plan_terminator(
            ctx,
            &order,
            adapter,
            block,
            ctx.cfg.fn_name(),
            ret_reg_budget,
        );
        let term_start = adapter.instruction_count();
        adapter.emit_terminator(plan.clone());
        let term_end = adapter.instruction_count();

        if let Some(info) = block_info.as_mut() {
            info.terminator = Some(crate::TerminatorLoweringDecision {
                terminator_desc: format_terminator_plan(&plan),
                mir_insts: adapter.instruction_strings(term_start..term_end),
                rationale: adapter.terminator_rationale(&plan),
                policy_trace: plan.trace(),
            });
        }

        if let (Some(debug), Some(info)) = (debug_info.as_deref_mut(), block_info) {
            debug.blocks.push(info);
        }
    }
    Ok(())
}

/// Stable, target-independent debug rendering of a normalized terminator.
pub fn format_terminator_plan(plan: &TerminatorPlan) -> String {
    match plan {
        TerminatorPlan::Goto { edge } => format!("goto {}", edge.target),
        TerminatorPlan::Branch {
            then_edge,
            else_edge,
            ..
        } => format!("branch {} else {}", then_edge.target, else_edge.target),
        TerminatorPlan::Switch { cases, default, .. } => {
            format!("switch {} cases default {}", cases.len(), default)
        }
        TerminatorPlan::Return { mode } => match mode {
            ReturnMode::Exit { .. } => "return exit".to_string(),
            ReturnMode::Function { .. } => "return function".to_string(),
        },
        TerminatorPlan::Unreachable => "unreachable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use rue_air::{FrozenTypeInternPool, StructDef, StructField, TypeInternPool};
    use rue_cfg::{Cfg, CfgInst, CfgInstData, CfgValue};
    use rue_span::{FileId, Span};
    use rue_target::Target;

    use crate::aarch64::{Aarch64Inst, CfgLower as Aarch64CfgLower};
    use crate::x86_64::{CfgLower as X86CfgLower, X86Inst};

    fn inst(data: rue_cfg::CfgInstData, ty: Type) -> CfgInst {
        CfgInst {
            data,
            ty,
            span: Span::new(0, 0),
        }
    }

    fn lower_both(
        cfg: &Cfg,
        pool: &FrozenTypeInternPool,
        interner: &ThreadedRodeo,
    ) -> (
        crate::x86_64::X86Mir,
        crate::LoweringDebugInfo,
        crate::aarch64::Aarch64Mir,
        crate::LoweringDebugInfo,
    ) {
        let (x86, x86_debug) = X86CfgLower::new_unchecked(cfg, pool, interner)
            .lower_with_debug()
            .expect("x86 fixture should lower");
        let (arm, arm_debug) =
            Aarch64CfgLower::new_unchecked(cfg, pool, interner, Target::Aarch64Linux)
                .lower_with_debug()
                .expect("AArch64 fixture should lower");
        (x86, x86_debug, arm, arm_debug)
    }

    fn policy_traces(debug: &crate::LoweringDebugInfo) -> Vec<TerminatorTrace> {
        debug
            .blocks
            .iter()
            .map(|block| {
                block
                    .terminator
                    .as_ref()
                    .expect("every fixture block has a terminator")
                    .policy_trace
                    .clone()
            })
            .collect()
    }

    fn assert_same_policy_trace(
        cfg: &Cfg,
        x86_debug: &crate::LoweringDebugInfo,
        arm_debug: &crate::LoweringDebugInfo,
    ) -> Vec<TerminatorTrace> {
        let x86_trace = policy_traces(x86_debug);
        let arm_trace = policy_traces(arm_debug);
        assert_eq!(
            x86_trace,
            arm_trace,
            "policy trace diverged for {}",
            cfg.fn_name()
        );
        x86_trace
    }

    struct CollisionAdapter;

    impl TerminatorAdapter for CollisionAdapter {
        fn materialize_value(&mut self, value: CfgValue, plan: ValuePlan) -> MaterializedValue {
            match plan.shape {
                ValueShape::ZeroSized => MaterializedValue {
                    primary: VReg::new(0),
                    slots: Vec::new(),
                },
                ValueShape::Scalar => MaterializedValue {
                    primary: VReg::new(value.as_u32()),
                    slots: Vec::new(),
                },
                ValueShape::CompleteAggregate { slot_count } => MaterializedValue {
                    primary: VReg::new(0),
                    slots: (0..slot_count).map(VReg::new).collect(),
                },
            }
        }

        fn materialize_block_param(
            &mut self,
            _target: BlockId,
            param_index: u32,
            _target_value: CfgValue,
            plan: ValuePlan,
        ) -> MaterializedValue {
            match plan.shape {
                ValueShape::ZeroSized => MaterializedValue {
                    primary: VReg::new(0),
                    slots: Vec::new(),
                },
                ValueShape::Scalar => MaterializedValue {
                    primary: VReg::new(param_index + 1),
                    slots: Vec::new(),
                },
                ValueShape::CompleteAggregate { slot_count } => MaterializedValue {
                    primary: VReg::new(1),
                    slots: (1..=slot_count).map(VReg::new).collect(),
                },
            }
        }

        fn emit_block_label(&mut self, _block: BlockId) {}

        fn emit_terminator(&mut self, _plan: TerminatorPlan) {}
    }

    #[test]
    #[should_panic(expected = "edge move destination must not be used as a later move source")]
    fn moves_for_edge_rejects_aggregate_slot_collision() {
        let (pool, aggregate_ty, _interner) =
            struct_pool("CollisionPair", vec![Type::I32, Type::I32]);
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "aggregate_collision".to_string(), vec![]);
        let source_block = cfg.new_block();
        let target = cfg.new_block();
        cfg.entry = source_block;
        let left = cfg.append_inst(source_block, inst(CfgInstData::Const(1), Type::I32));
        let right = cfg.append_inst(source_block, inst(CfgInstData::Const(2), Type::I32));
        let struct_id = match aggregate_ty.kind() {
            rue_air::TypeKind::Struct(id) => id,
            _ => panic!("fixture aggregate must be a struct"),
        };
        let aggregate = cfg
            .append_struct_init(
                source_block,
                struct_id,
                [left, right],
                aggregate_ty,
                Span::new(0, 0),
            )
            .unwrap();
        cfg.add_block_param(target, aggregate_ty);
        cfg.set_goto(source_block, target, [aggregate]);
        cfg.set_return(target, None);

        let ctx = CfgLowerContext::new(&cfg, &pool);
        let order = BlockOrder::of(&ctx);
        moves_for_edge(
            &ctx,
            &order,
            &mut CollisionAdapter,
            &[aggregate],
            target,
            source_block,
        );
    }

    #[test]
    #[should_panic(expected = "edge move destination must not be used as a later move source")]
    fn moves_for_edge_rejects_cross_argument_collision() {
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "argument_collision".to_string(), vec![]);
        let source_block = cfg.new_block();
        let target = cfg.new_block();
        cfg.entry = source_block;
        let first = cfg.append_inst(source_block, inst(CfgInstData::Const(1), Type::I32));
        let second = cfg.append_inst(source_block, inst(CfgInstData::Const(2), Type::I32));
        cfg.add_block_param(target, Type::I32);
        cfg.add_block_param(target, Type::I32);
        cfg.set_goto(source_block, target, [first, second]);
        cfg.set_return(target, None);

        let ctx = CfgLowerContext::new(&cfg, &pool);
        let order = BlockOrder::of(&ctx);
        moves_for_edge(
            &ctx,
            &order,
            &mut CollisionAdapter,
            &[first, second],
            target,
            source_block,
        );
    }

    fn struct_pool(name: &str, fields: Vec<Type>) -> (FrozenTypeInternPool, Type, ThreadedRodeo) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let struct_fields = fields
            .into_iter()
            .enumerate()
            .map(|(index, ty)| StructField {
                name: format!("field{index}"),
                ty,
            })
            .collect();
        let (struct_id, _) = pool.register_struct(
            interner.get_or_intern(name),
            StructDef {
                name: name.into(),
                fields: struct_fields,
                is_copy: true,
                is_linear: false,
                declared_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        (pool.freeze(), Type::new_struct(struct_id), interner)
    }

    fn diamond_fixture() -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::I32, 0, 0, "diamond".to_string(), vec![]);
        let entry = cfg.new_block();
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let join = cfg.new_block();
        cfg.entry = entry;
        let condition = cfg.append_inst(entry, inst(CfgInstData::BoolConst(true), Type::BOOL));
        let value = cfg.append_inst(entry, inst(CfgInstData::Const(7), Type::I32));
        let join_value = cfg.add_block_param(join, Type::I32);
        cfg.set_branch(entry, condition, then_block, [], else_block, []);
        cfg.set_goto(then_block, join, [value]);
        cfg.set_goto(else_block, join, [value]);
        cfg.set_return(join, Some(join_value));
        (cfg, pool, interner)
    }

    fn same_successor_fixture() -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::I32, 0, 0, "same_successor".to_string(), vec![]);
        let entry = cfg.new_block();
        let target = cfg.new_block();
        cfg.entry = entry;
        let condition = cfg.append_inst(entry, inst(CfgInstData::BoolConst(true), Type::BOOL));
        let then_value = cfg.append_inst(entry, inst(CfgInstData::Const(1), Type::I32));
        let else_value = cfg.append_inst(entry, inst(CfgInstData::Const(2), Type::I32));
        let target_value = cfg.add_block_param(target, Type::I32);
        cfg.set_branch(entry, condition, target, [then_value], target, [else_value]);
        cfg.set_return(target, Some(target_value));
        (cfg, pool, interner)
    }

    fn loop_fixture() -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::I32, 0, 0, "loop".to_string(), vec![]);
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = header;
        let condition = cfg.append_inst(header, inst(CfgInstData::BoolConst(true), Type::BOOL));
        let result = cfg.append_inst(header, inst(CfgInstData::Const(1), Type::I32));
        cfg.set_branch(header, condition, body, [], exit, []);
        cfg.set_goto(body, header, []);
        cfg.set_return(exit, Some(result));
        (cfg, pool, interner)
    }

    fn switch_fixture(ty: Type, name: &str) -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, name.to_string(), vec![]);
        let entry = cfg.new_block();
        let first = cfg.new_block();
        let second = cfg.new_block();
        let third = cfg.new_block();
        let default = cfg.new_block();
        cfg.entry = entry;
        let scrutinee = cfg.append_inst(entry, inst(CfgInstData::Const(u64::MAX), ty));
        cfg.set_switch(
            entry,
            scrutinee,
            [(-1, third), (7, first), (42, second)],
            default,
        );
        for block in [first, second, third, default] {
            cfg.set_return(block, None);
        }
        (cfg, pool, interner)
    }

    fn cleanup_edge_fixture() -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let (pool, aggregate_ty, interner) = struct_pool("CleanupPair", vec![Type::I32, Type::I32]);
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "cleanup_edge".to_string(), vec![]);
        let entry = cfg.new_block();
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let cleanup = cfg.new_block();
        cfg.entry = entry;
        let condition = cfg.append_inst(entry, inst(CfgInstData::BoolConst(true), Type::BOOL));
        let left = cfg.append_inst(entry, inst(CfgInstData::Const(3), Type::I32));
        let right = cfg.append_inst(entry, inst(CfgInstData::Const(4), Type::I32));
        let aggregate = cfg
            .append_struct_init(
                entry,
                match aggregate_ty.kind() {
                    rue_air::TypeKind::Struct(id) => id,
                    _ => panic!("fixture aggregate must be a struct"),
                },
                [left, right],
                aggregate_ty,
                Span::new(0, 0),
            )
            .unwrap();
        let _cleanup_param = cfg.add_block_param(cleanup, aggregate_ty);
        cfg.set_branch(entry, condition, then_block, [], else_block, []);
        cfg.set_goto(then_block, cleanup, [aggregate]);
        cfg.set_goto(else_block, cleanup, [aggregate]);
        cfg.set_return(cleanup, None);
        (cfg, pool, interner)
    }

    fn aggregate_return_fixture(
        fields: usize,
        name: &str,
    ) -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let (pool, aggregate_ty, interner) = struct_pool(name, vec![Type::I64; fields]);
        let mut cfg = Cfg::new(aggregate_ty, 0, 0, name.to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let values = (0..fields)
            .map(|value| cfg.append_inst(entry, inst(CfgInstData::Const(value as u64), Type::I64)))
            .collect::<Vec<_>>();
        let aggregate = cfg
            .append_struct_init(
                entry,
                match aggregate_ty.kind() {
                    rue_air::TypeKind::Struct(id) => id,
                    _ => panic!("fixture aggregate must be a struct"),
                },
                values,
                aggregate_ty,
                Span::new(0, 0),
            )
            .unwrap();
        cfg.set_return(entry, Some(aggregate));
        (cfg, pool, interner)
    }

    fn zero_slot_edge_fixture() -> (Cfg, FrozenTypeInternPool, ThreadedRodeo) {
        let pool = TypeInternPool::new();
        let array_id = pool.intern_array_from_type(Type::UNIT, 2);
        let pool = pool.freeze();
        let interner = ThreadedRodeo::new();
        let array_ty = Type::new_array(array_id);
        let mut cfg = Cfg::new(array_ty, 0, 0, "zero_slot_edge".to_string(), vec![]);
        let entry = cfg.new_block();
        let target = cfg.new_block();
        cfg.entry = entry;
        let zero = cfg
            .append_array_init(entry, [], array_ty, Span::new(0, 0))
            .unwrap();
        let target_param = cfg.add_block_param(target, array_ty);
        cfg.set_goto(entry, target, [zero]);
        cfg.set_return(target, Some(target_param));
        (cfg, pool, interner)
    }

    #[test]
    fn real_diamond_and_loop_fixtures_share_topology_edge_work_and_branch_mir() {
        for (cfg, pool, interner, expected_kinds) in [
            {
                let (cfg, pool, interner) = diamond_fixture();
                (
                    cfg,
                    pool,
                    interner,
                    vec![
                        TerminatorTraceKind::Branch,
                        TerminatorTraceKind::Goto,
                        TerminatorTraceKind::Goto,
                        TerminatorTraceKind::ReturnScalar,
                    ],
                )
            },
            {
                let (cfg, pool, interner) = loop_fixture();
                (
                    cfg,
                    pool,
                    interner,
                    vec![
                        TerminatorTraceKind::Branch,
                        TerminatorTraceKind::Goto,
                        TerminatorTraceKind::ReturnScalar,
                    ],
                )
            },
        ] {
            let (x86, x86_debug, arm, arm_debug) = lower_both(&cfg, &pool, &interner);
            let trace = assert_same_policy_trace(&cfg, &x86_debug, &arm_debug);
            assert_eq!(
                trace.iter().map(|trace| trace.kind).collect::<Vec<_>>(),
                expected_kinds
            );
            assert!(trace[0].successors.len() == 2);
            assert!(trace[0].fallthrough[0]);
            assert_eq!(trace[0].edge_work, vec![vec![], vec![]]);
            assert!(
                x86.instructions()
                    .iter()
                    .any(|inst| matches!(inst, X86Inst::Jz { .. } | X86Inst::Jnz { .. }))
            );
            assert!(arm.instructions().iter().any(|inst| {
                matches!(inst, Aarch64Inst::Cbz { .. } | Aarch64Inst::Cbnz { .. })
            }));
        }
    }

    #[test]
    fn real_same_successor_fixture_selects_one_fallthrough_layout() {
        let (cfg, pool, interner) = same_successor_fixture();
        let (x86, x86_debug, arm, arm_debug) = lower_both(&cfg, &pool, &interner);
        let trace = assert_same_policy_trace(&cfg, &x86_debug, &arm_debug);
        assert_eq!(trace[0].kind, TerminatorTraceKind::Branch);
        assert_eq!(trace[0].fallthrough, vec![true, false]);
        assert_eq!(trace[0].edge_work, vec![vec![(0, 0)], vec![(0, 0)]]);
        assert!(
            x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Jnz { .. }))
        );
        assert!(
            arm.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Cbnz { .. }))
        );
    }

    #[test]
    fn real_switch_fixtures_preserve_signed_64_bit_order_and_target_compare_facts() {
        for (ty, expected_signed) in [(Type::I64, true), (Type::U64, false)] {
            let (cfg, pool, interner) = switch_fixture(
                ty,
                if expected_signed {
                    "switch_i64"
                } else {
                    "switch_u64"
                },
            );
            let (x86, x86_debug, arm, arm_debug) = lower_both(&cfg, &pool, &interner);
            let trace = assert_same_policy_trace(&cfg, &x86_debug, &arm_debug);
            assert_eq!(trace[0].kind, TerminatorTraceKind::Switch);
            assert_eq!(
                trace[0].switch_width,
                Some(value_plan::IntegerWidth {
                    bits: 64,
                    signed: expected_signed
                })
            );
            assert_eq!(
                trace[0].switch_cases,
                vec![
                    (-1, BlockId::from_raw(3)),
                    (7, BlockId::from_raw(1)),
                    (42, BlockId::from_raw(2)),
                ]
            );
            assert_eq!(
                trace[0].successors,
                vec![
                    BlockId::from_raw(3),
                    BlockId::from_raw(1),
                    BlockId::from_raw(2),
                    BlockId::from_raw(4)
                ]
            );
            assert_eq!(
                x86.instructions()
                    .iter()
                    .filter(|inst| matches!(inst, X86Inst::Cmp64RR { .. }))
                    .count(),
                3
            );
            assert_eq!(
                arm.instructions()
                    .iter()
                    .filter(|inst| matches!(inst, Aarch64Inst::Cmp64RR { .. }))
                    .count(),
                3
            );
            assert_eq!(
                x86.instructions()
                    .iter()
                    .filter(|inst| matches!(inst, X86Inst::Jz { .. }))
                    .count(),
                3
            );
            assert_eq!(
                arm.instructions()
                    .iter()
                    .filter(|inst| matches!(inst, Aarch64Inst::BCond { .. }))
                    .count(),
                3
            );
        }
    }

    #[test]
    fn real_cleanup_fixture_preserves_ordered_aggregate_edge_work() {
        let (cfg, pool, interner) = cleanup_edge_fixture();
        let (x86, x86_debug, arm, arm_debug) = lower_both(&cfg, &pool, &interner);
        let trace = assert_same_policy_trace(&cfg, &x86_debug, &arm_debug);
        assert_eq!(trace[0].kind, TerminatorTraceKind::Branch);
        assert_eq!(trace[0].edge_move_counts, vec![0, 0]);
        assert_eq!(trace[0].edge_work, vec![vec![], vec![]]);
        assert_eq!(trace[1].edge_move_counts, vec![2]);
        assert_eq!(trace[2].edge_move_counts, vec![2]);
        assert_eq!(trace[1].edge_work, vec![vec![(0, 0), (0, 1)]]);
        assert_eq!(trace[2].edge_work, vec![vec![(0, 0), (0, 1)]]);
        assert_eq!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::MovRR { .. }))
                .count(),
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::MovRR { .. }))
                .count()
        );
    }

    #[test]
    fn real_unreachable_fixture_has_target_traps_and_shared_trap_policy() {
        let pool = FrozenTypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "trap".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_unreachable(entry);
        let (x86, x86_debug, arm, arm_debug) = lower_both(&cfg, &pool, &interner);
        let trace = assert_same_policy_trace(&cfg, &x86_debug, &arm_debug);
        assert_eq!(
            trace,
            vec![TerminatorTrace {
                kind: TerminatorTraceKind::Unreachable,
                successors: vec![],
                fallthrough: vec![],
                edge_move_counts: vec![],
                edge_work: vec![],
                switch_width: None,
                switch_cases: vec![],
            }]
        );
        assert!(
            x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Ud2))
        );
        assert!(
            arm.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Brk))
        );
    }

    #[test]
    fn real_return_fixture_matrix_covers_scalar_unit_main_multiple_and_aggregate_modes() {
        let scalar_pool = FrozenTypeInternPool::new();
        let scalar_interner = ThreadedRodeo::new();
        let mut scalar_cfg = Cfg::new(Type::I32, 0, 0, "scalar".to_string(), vec![]);
        let scalar_entry = scalar_cfg.new_block();
        scalar_cfg.entry = scalar_entry;
        let scalar = scalar_cfg.append_inst(scalar_entry, inst(CfgInstData::Const(9), Type::I32));
        scalar_cfg.set_return(scalar_entry, Some(scalar));

        let unit_pool = FrozenTypeInternPool::new();
        let unit_interner = ThreadedRodeo::new();
        let mut unit_cfg = Cfg::new(Type::UNIT, 0, 0, "unit".to_string(), vec![]);
        let unit_entry = unit_cfg.new_block();
        unit_cfg.entry = unit_entry;
        unit_cfg.set_return(unit_entry, None);

        let main_pool = FrozenTypeInternPool::new();
        let main_interner = ThreadedRodeo::new();
        let mut main_cfg = Cfg::new(Type::I32, 0, 0, "main".to_string(), vec![]);
        let main_entry = main_cfg.new_block();
        main_cfg.entry = main_entry;
        let main_value = main_cfg.append_inst(main_entry, inst(CfgInstData::Const(0), Type::I32));
        main_cfg.set_return(main_entry, Some(main_value));

        for (cfg, pool, interner, expected_kind) in [
            (
                &scalar_cfg,
                &scalar_pool,
                &scalar_interner,
                TerminatorTraceKind::ReturnScalar,
            ),
            (
                &unit_cfg,
                &unit_pool,
                &unit_interner,
                TerminatorTraceKind::ReturnUnit,
            ),
            (
                &main_cfg,
                &main_pool,
                &main_interner,
                TerminatorTraceKind::ReturnExit,
            ),
        ] {
            let (x86, x86_debug, arm, arm_debug) = lower_both(cfg, pool, interner);
            let trace = assert_same_policy_trace(cfg, &x86_debug, &arm_debug);
            assert_eq!(trace[0].kind, expected_kind);
            assert!(
                x86.instructions()
                    .iter()
                    .any(|inst| matches!(inst, X86Inst::Ret | X86Inst::CallRel { .. }))
            );
            assert!(
                arm.instructions()
                    .iter()
                    .any(|inst| matches!(inst, Aarch64Inst::Ret | Aarch64Inst::Bl { .. }))
            );
        }

        let (multi_pool, multi_interner) = (FrozenTypeInternPool::new(), ThreadedRodeo::new());
        let mut multi_cfg = Cfg::new(Type::I32, 0, 0, "multiple_returns".to_string(), vec![]);
        let multi_entry = multi_cfg.new_block();
        let first = multi_cfg.new_block();
        let second = multi_cfg.new_block();
        multi_cfg.entry = multi_entry;
        let condition =
            multi_cfg.append_inst(multi_entry, inst(CfgInstData::BoolConst(true), Type::BOOL));
        let first_value = multi_cfg.append_inst(first, inst(CfgInstData::Const(1), Type::I32));
        let second_value = multi_cfg.append_inst(second, inst(CfgInstData::Const(2), Type::I32));
        multi_cfg.set_branch(multi_entry, condition, first, [], second, []);
        multi_cfg.set_return(first, Some(first_value));
        multi_cfg.set_return(second, Some(second_value));
        let (x86, x86_debug, arm, arm_debug) = lower_both(&multi_cfg, &multi_pool, &multi_interner);
        let trace = assert_same_policy_trace(&multi_cfg, &x86_debug, &arm_debug);
        assert_eq!(
            trace
                .iter()
                .filter(|trace| trace.kind == TerminatorTraceKind::ReturnScalar)
                .count(),
            2
        );
        assert!(
            x86.instructions()
                .iter()
                .filter(|inst| matches!(inst, X86Inst::Ret))
                .count()
                >= 2
        );
        assert!(
            arm.instructions()
                .iter()
                .filter(|inst| matches!(inst, Aarch64Inst::Ret))
                .count()
                >= 2
        );
    }

    #[test]
    fn real_aggregate_return_and_zero_slot_edge_fixtures_share_abi_shape() {
        let (register_cfg, register_pool, register_interner) =
            aggregate_return_fixture(2, "aggregate_registers");
        let (x86, x86_debug, arm, arm_debug) =
            lower_both(&register_cfg, &register_pool, &register_interner);
        let trace = assert_same_policy_trace(&register_cfg, &x86_debug, &arm_debug);
        assert_eq!(trace[0].kind, TerminatorTraceKind::ReturnAggregateRegisters);
        assert!(
            x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Ret))
        );
        assert!(
            arm.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Ret))
        );

        let (sret_cfg, sret_pool, sret_interner) = aggregate_return_fixture(9, "aggregate_sret");
        let (x86, x86_debug, arm, arm_debug) = lower_both(&sret_cfg, &sret_pool, &sret_interner);
        let trace = assert_same_policy_trace(&sret_cfg, &x86_debug, &arm_debug);
        assert_eq!(trace[0].kind, TerminatorTraceKind::ReturnAggregateSret);
        assert!(
            x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Ret))
        );
        assert!(
            arm.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Ret))
        );

        let (zero_cfg, zero_pool, zero_interner) = zero_slot_edge_fixture();
        let (x86, x86_debug, arm, arm_debug) = lower_both(&zero_cfg, &zero_pool, &zero_interner);
        let trace = assert_same_policy_trace(&zero_cfg, &x86_debug, &arm_debug);
        assert_eq!(trace[0].kind, TerminatorTraceKind::Goto);
        assert_eq!(trace[0].edge_move_counts, vec![0]);
        assert_eq!(trace[0].edge_work, vec![vec![]]);
        assert_eq!(trace[1].kind, TerminatorTraceKind::ReturnAggregateZeroSized);
        assert!(
            x86.instructions()
                .iter()
                .any(|inst| matches!(inst, X86Inst::Ret))
        );
        assert!(
            arm.instructions()
                .iter()
                .any(|inst| matches!(inst, Aarch64Inst::Ret))
        );
    }

    #[test]
    fn edge_fallthrough_is_based_on_logical_block_order() {
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "ordered_blocks".to_string(), vec![]);
        let first = cfg.new_block();
        let second = cfg.new_block();
        let third = cfg.new_block();
        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);
        let order = BlockOrder::of(&ctx);
        assert_eq!(order.next(first), Some(second));
        assert_eq!(order.next(second), Some(third));
        assert_eq!(order.next(third), None);
    }

    /// A continuation block that an inline splice appends after the successors
    /// it dominates must still be lowered before them, or their on-demand
    /// materialization emits the continuation's definitions into themselves
    /// (RUE-1758).
    #[test]
    fn lowering_order_puts_a_dominating_continuation_before_its_successors() {
        let pool = FrozenTypeInternPool::new();
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "spliced_continuation".to_string(), vec![]);
        let entry = cfg.new_block();
        let some_arm = cfg.new_block();
        let none_arm = cfg.new_block();
        let join = cfg.new_block();
        // The splice appends its continuation last even though it dominates the
        // arms the original call block already branched to.
        let continuation = cfg.new_block();
        cfg.entry = entry;
        let condition = cfg.append_inst(entry, inst(CfgInstData::BoolConst(true), Type::BOOL));
        let scrutinee = cfg.append_inst(continuation, inst(CfgInstData::Const(0), Type::I64));
        cfg.set_branch(entry, condition, continuation, [], continuation, []);
        cfg.set_switch(continuation, scrutinee, [(0, some_arm)], none_arm);
        cfg.set_goto(some_arm, join, []);
        cfg.set_goto(none_arm, join, []);
        cfg.set_return(join, None);

        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);
        let order = BlockOrder::of(&ctx);
        assert_eq!(
            order.blocks(),
            [entry, continuation, some_arm, none_arm, join]
        );
    }

    /// Arena order is already a valid lowering order for an ordinary diamond,
    /// so the emitted layout must be left exactly as it was.
    #[test]
    fn lowering_order_keeps_a_valid_arena_order_unchanged() {
        let (cfg, pool, _interner) = diamond_fixture();
        let ctx = crate::cfg_lower::CfgLowerContext::new(&cfg, &pool);
        let order = BlockOrder::of(&ctx);
        let arena: Vec<BlockId> = cfg.blocks().iter().map(|block| block.id).collect();
        assert_eq!(order.blocks(), arena.as_slice());
    }

    #[test]
    fn plan_is_target_free() {
        let plan = TerminatorPlan::Switch {
            scrutinee: VReg::new(1),
            width: value_plan::IntegerWidth {
                bits: 64,
                signed: true,
            },
            cases: vec![SwitchCasePlan {
                value: -1,
                target: BlockId::from_raw(2),
            }],
            default: BlockId::from_raw(3),
        };
        let text = format!("{plan:?}");
        assert!(!text.contains("Reg::"));
        assert!(!text.contains("Inst::"));
    }

    #[test]
    fn trace_preserves_ordered_topology_and_switch_width() {
        let plan = TerminatorPlan::Switch {
            scrutinee: VReg::new(0),
            width: value_plan::IntegerWidth {
                bits: 64,
                signed: true,
            },
            cases: vec![
                SwitchCasePlan {
                    value: 7,
                    target: BlockId::from_raw(4),
                },
                SwitchCasePlan {
                    value: -1,
                    target: BlockId::from_raw(2),
                },
            ],
            default: BlockId::from_raw(9),
        };
        let trace = plan.trace();
        assert_eq!(trace.kind, TerminatorTraceKind::Switch);
        assert_eq!(
            trace.successors,
            vec![
                BlockId::from_raw(4),
                BlockId::from_raw(2),
                BlockId::from_raw(9)
            ]
        );
        assert_eq!(
            trace.switch_cases,
            vec![(7, BlockId::from_raw(4)), (-1, BlockId::from_raw(2))]
        );
        assert_eq!(trace.switch_width.unwrap().bits, 64);
    }

    #[test]
    fn trace_records_edge_work_and_fallthrough() {
        let plan = TerminatorPlan::Branch {
            condition: VReg::new(0),
            then_edge: EdgePlan {
                target: BlockId::from_raw(1),
                moves: vec![SlotMove {
                    argument_index: 0,
                    slot_index: 0,
                    source: VReg::new(1),
                    destination: VReg::new(2),
                    float_width: None,
                }],
                fallthrough: true,
            },
            else_edge: EdgePlan {
                target: BlockId::from_raw(8),
                moves: vec![],
                fallthrough: false,
            },
        };
        assert_eq!(
            plan.trace(),
            TerminatorTrace {
                kind: TerminatorTraceKind::Branch,
                successors: vec![BlockId::from_raw(1), BlockId::from_raw(8)],
                fallthrough: vec![true, false],
                edge_move_counts: vec![1, 0],
                edge_work: vec![vec![(0, 0)], vec![]],
                switch_width: None,
                switch_cases: vec![],
            }
        );
    }
}
