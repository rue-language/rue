//! Conservative constant-trip full loop unrolling (ADR-0054 phase 3).
//!
//! The first implementation intentionally accepts one canonical shape: one
//! local slot is initialized by a constant outside the loop, the header loads
//! and compares it with a constant bound, and one latch updates it by a
//! constant non-zero stride.
//! A single header exit and a single latch make the transformation easy to
//! audit for trap, drop, and block-argument ordering. Everything else is
//! refused without changing the graph.

use super::CfgOptimizationError;
use super::loops::{NaturalLoop, loops};
use crate::dominators::DominatorTree;
use crate::{
    BlockId, Cfg, CfgCallArg, CfgEditError, CfgInst, CfgInstData, CfgValue, PlaceBase, Projection,
    Terminator, Type,
};
use ahash::{AHashMap, AHashSet};
use rue_span::Span;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub forest_computations: u64,
    pub loops_analyzed: u64,
    pub loops_unrolled: u64,
    pub budget_refusals: u64,
    pub shape_refusals: u64,
    pub blocks_cloned: u64,
    pub values_cloned: u64,
    pub instructions_cloned: u64,
}

#[derive(Debug, Clone, Copy)]
struct Trip {
    count: u64,
    initial: i128,
    stride: i128,
    ty: Type,
    slot: u32,
}

#[cfg(test)]
pub fn run(cfg: &mut Cfg) -> Result<Stats, CfgOptimizationError> {
    let mut budget = super::CodeGrowthBudget::o3();
    run_with_budget(cfg, &mut budget)
}

pub fn run_with_budget(
    cfg: &mut Cfg,
    budget: &mut super::CodeGrowthBudget,
) -> Result<Stats, CfgOptimizationError> {
    let mut stats = Stats::default();
    loop {
        stats.forest_computations += 1;
        let dom = DominatorTree::compute(cfg);
        let forest = loops(cfg, &dom);
        if forest.is_irreducible() || forest.is_empty() {
            break;
        }
        let mut changed = false;
        let entries: Vec<_> = forest
            .loops()
            .iter()
            .map(|lp| unique_outside_predecessor(cfg, lp))
            .collect();
        let mut unrolled_in_generation = Vec::new();
        for id in 0..forest.len() {
            // One edit invalidates its own loop id and every enclosing body's
            // membership. Sibling loops, however, come from the same forest
            // generation and remain valid when their bodies and entry edits
            // cannot touch one another. Batch only that bounded case; anything
            // nested or connected through an edited boundary waits for the
            // mandatory rebuild below.
            if unrolled_in_generation
                .iter()
                .any(|&other| !forest_generation_independent(&forest, &entries, id, other))
            {
                continue;
            }
            stats.loops_analyzed += 1;
            let lp = forest.get(id);
            // An enclosing loop still contains another induction protocol and
            // cannot be cloned. An innermost loop is self-contained even when
            // it has a parent, so it may use the ordinary canonical-shape and
            // budget checks below. A successful edit invalidates its own id
            // and enclosing bodies; only the independent sibling check above
            // permits further use of this forest generation.
            if forest.loops().iter().any(|other| other.parent == Some(id)) {
                stats.shape_refusals += 1;
                continue;
            }
            let Some(trip) = recognize(cfg, lp) else {
                stats.shape_refusals += 1;
                continue;
            };
            // Charge the same value axis used by inlining: every cloned
            // block parameter and instruction consumes one budget unit.
            let body_size: u64 = lp
                .body
                .iter()
                .map(|&b| {
                    let block = cfg.get_block(b);
                    u64::try_from(block.params.len())
                        .ok()
                        .and_then(|params| {
                            params.checked_add(u64::try_from(block.insts.len()).ok()?)
                        })
                        .unwrap_or(u64::MAX)
                })
                .try_fold(0u64, |a, b| a.checked_add(b))
                .unwrap_or(u64::MAX);
            let Some(growth_values) = trip.count.checked_mul(body_size) else {
                stats.budget_refusals += 1;
                continue;
            };
            let Some(growth_blocks) = trip
                .count
                .checked_mul(u64::try_from(lp.body.len()).unwrap_or(u64::MAX))
            else {
                stats.budget_refusals += 1;
                continue;
            };
            let growth = super::CodeGrowth {
                values: growth_values,
                blocks: growth_blocks,
            };
            if !budget.try_charge(growth) {
                stats.budget_refusals += 1;
                continue;
            }
            // Edited in place. The transactional clone this used to take
            // protected nothing (RUE-1842, following RUE-1663): the `Err` arm
            // propagates out of `run_with_budget`, through
            // `optimize_with_budget`'s `?`, into `publish_optimization`, whose
            // first statement is `pass_result?` — so a partially unrolled graph
            // is never validated or published, and the preserved original was
            // never read.
            match unroll_one(cfg, lp, trip) {
                Ok((blocks, values, instructions)) => {
                    stats.loops_unrolled += 1;
                    stats.blocks_cloned += blocks;
                    stats.values_cloned += values;
                    stats.instructions_cloned += instructions;
                    changed = true;
                    unrolled_in_generation.push(id);
                }
                Err(error) => return Err(error),
            }
        }
        if !changed {
            break;
        }
    }
    Ok(stats)
}

fn unique_outside_predecessor(cfg: &Cfg, lp: &NaturalLoop) -> Option<BlockId> {
    let mut outside = cfg
        .predecessors_of(lp.header)
        .into_iter()
        .filter(|predecessor| !lp.contains(*predecessor));
    let predecessor = outside.next()?;
    outside.next().is_none().then_some(predecessor)
}

/// Whether two loop ids from one immutable forest generation can be edited
/// without either edit invalidating the other's forest-local shape.
fn forest_generation_independent(
    forest: &super::loops::LoopForest,
    entries: &[Option<BlockId>],
    a_id: usize,
    b_id: usize,
) -> bool {
    let a = forest.get(a_id);
    let b = forest.get(b_id);

    // Ancestors contain descendants, so this also excludes every
    // ancestor/descendant pair while accepting disjoint sibling trees.
    if a.body.iter().any(|block| b.contains(*block)) {
        return false;
    }

    // `unroll_one` rewires the unique outside predecessor. Do not reuse a
    // loop whose body supplies that predecessor, nor clone an exit that enters
    // the other loop body and would create a new, unanalyzed predecessor.
    if entries[a_id].is_some_and(|entry| b.contains(entry))
        || entries[b_id].is_some_and(|entry| a.contains(entry))
    {
        return false;
    }
    !a.exits.iter().any(|(_, target)| b.contains(*target))
        && !b.exits.iter().any(|(_, target)| a.contains(*target))
}

fn const_value(cfg: &Cfg, value: CfgValue, ty: Type) -> Option<i128> {
    let CfgInstData::Const(raw) = cfg.get_inst(value).data else {
        return None;
    };
    let width = ty.int_bit_width()?;
    if ty.is_signed() {
        let shift = 128 - width;
        Some(((raw as i128) << shift) >> shift)
    } else {
        Some(i128::from(raw))
    }
}

fn encoded_const(value: i128, ty: Type) -> Option<u64> {
    let width = ty.int_bit_width()?;
    let min = if ty.is_signed() {
        -(1i128 << (width - 1))
    } else {
        0
    };
    let max = if ty.is_signed() {
        (1i128 << (width - 1)) - 1
    } else {
        (1i128 << width) - 1
    };
    if !(min..=max).contains(&value) {
        return None;
    }
    u64::try_from(value.rem_euclid(1i128 << width)).ok()
}

fn recognize(cfg: &Cfg, lp: &NaturalLoop) -> Option<Trip> {
    if lp.latches.len() != 1 || lp.exits.len() != 1 {
        return None;
    }
    let header = lp.header;
    let latch = lp.latches[0];
    let outside: Vec<_> = cfg
        .predecessors_of(header)
        .into_iter()
        .filter(|p| !lp.contains(*p))
        .collect();
    if outside.len() != 1 || !cfg.get_block(header).params.is_empty() {
        return None;
    }
    if !matches!(&cfg.get_block(outside[0]).terminator,
        Terminator::Goto { target, .. } if *target == header)
    {
        return None;
    }
    let (cond, body_target, exit_target) = match &cfg.get_block(header).terminator {
        Terminator::Branch {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let then_inside = lp.contains(*then_block);
            let else_inside = lp.contains(*else_block);
            if then_inside == else_inside {
                return None;
            }
            (
                *cond,
                if then_inside {
                    *then_block
                } else {
                    *else_block
                },
                if then_inside {
                    *else_block
                } else {
                    *then_block
                },
            )
        }
        _ => return None,
    };
    if lp.exits[0] != (header, exit_target) || !lp.contains(body_target) {
        return None;
    }
    // Exit arguments require a value map from the final iteration. The first
    // phase keeps that wiring explicit by accepting only a parameterless exit.
    if let Terminator::Branch { then_block, .. } = &cfg.get_block(header).terminator {
        let exit_args_len = if *then_block == exit_target {
            cfg.get_branch_then_args(&cfg.get_block(header).terminator)
                .len()
        } else {
            cfg.get_branch_else_args(&cfg.get_block(header).terminator)
                .len()
        };
        if exit_args_len != 0 || !cfg.get_block(exit_target).params.is_empty() {
            return None;
        }
    }
    let (mut cmp, left, right) = match cfg.get_inst(cond).data {
        CfgInstData::Lt(a, b) => (0u8, a, b),
        CfgInstData::Le(a, b) => (1, a, b),
        CfgInstData::Gt(a, b) => (2, a, b),
        CfgInstData::Ge(a, b) => (3, a, b),
        _ => return None,
    };
    // Trip arithmetic describes the iterations for which the predicate is
    // true. If the loop body is the false arm, normalize the predicate before
    // counting (the CFG shape is otherwise equivalent).
    let body_is_true_arm = match cfg.get_block(header).terminator {
        Terminator::Branch { then_block, .. } => then_block == body_target,
        _ => false,
    };
    if !body_is_true_arm {
        cmp = match cmp {
            0 => 3,
            1 => 2,
            2 => 1,
            _ => 0,
        };
    }
    let (iv_load, bound_v, slot) = if matches!(cfg.get_inst(right).data, CfgInstData::Load { .. }) {
        cmp = match cmp {
            0 => 2,
            1 => 3,
            2 => 0,
            _ => 1,
        };
        let CfgInstData::Load { slot } = cfg.get_inst(right).data else {
            return None;
        };
        (right, left, slot)
    } else if matches!(cfg.get_inst(left).data, CfgInstData::Load { .. }) {
        let CfgInstData::Load { slot } = cfg.get_inst(left).data else {
            return None;
        };
        (left, right, slot)
    } else {
        return None;
    };
    let ty = cfg.get_inst(iv_load).ty;
    let bound = const_value(cfg, bound_v, ty)?;
    // Removing the final failed test is equivalent only for this pure,
    // canonical three-instruction header.
    let header_insts = &cfg.get_block(header).insts;
    if (header_insts.len() != 2 && header_insts.len() != 3)
        || !header_insts.contains(&cond)
        || !header_insts.contains(&iv_load)
        || header_insts
            .iter()
            .any(|value| *value != cond && *value != iv_load && *value != bound_v)
    {
        return None;
    }
    let mut initial = None;
    let mut alloc_count = 0;
    for &v in &cfg.get_block(outside[0]).insts {
        if let CfgInstData::Alloc { slot: s, init } = cfg.get_inst(v).data {
            if s == slot {
                alloc_count += 1;
                initial = const_value(cfg, init, ty);
            }
        }
    }
    // An inlined droppable parameter is a one-write slot too, but cloning its
    // loop body would preserve the wrong caller-owned value if its ownership
    // transfer were treated as an ordinary induction-variable spill. The
    // slot marker is separate from the per-value forwarding marker because
    // unrolling clones the storage region rather than an individual Load.
    if alloc_count != 1
        || initial.is_none()
        || cfg.is_address_taken(slot)
        || cfg.is_ownership_boundary(slot)
    {
        return None;
    }
    let initial = initial?;
    // A constant Alloc is only a valid initial value when no reaching write
    // in the preheader can replace it. The first phase rejects that shape
    // rather than trying to reconstruct a full reaching-definitions analysis.
    if cfg.get_block(outside[0]).insts.iter().any(|value| {
        matches!(
            cfg.get_inst(*value).data,
            CfgInstData::Store { slot: s, .. } if s == slot
        ) || matches!(
            cfg.get_inst(*value).data,
            CfgInstData::PlaceWrite { ref place, .. }
                if matches!(place.base, PlaceBase::Local(s) if s == slot)
        )
    }) {
        return None;
    }
    // Do not clone a loop whose SSA result is consumed by an external block.
    // In particular, a parameterless exit can still directly use a value
    // defined in the loop; zero-trip and final-iteration paths need different
    // mappings for that case.
    let loop_values = lp
        .body
        .iter()
        .flat_map(|block| {
            cfg.get_block(*block)
                .params
                .iter()
                .map(|(value, _)| *value)
                .chain(cfg.get_block(*block).insts.iter().copied())
        })
        .collect::<AHashSet<_>>();
    let mut external_use = false;
    for block in cfg.block_ids().filter(|block| !lp.contains(*block)) {
        for &value in &cfg.get_block(block).insts {
            super::dce::visit_instruction_uses(cfg, value, |used| {
                external_use |= loop_values.contains(&used);
            });
        }
        super::dce::visit_terminator_uses(cfg, &cfg.get_block(block).terminator, |used| {
            external_use |= loop_values.contains(&used);
        });
    }
    if external_use {
        return None;
    }
    let (target, args) = match &cfg.get_block(latch).terminator {
        Terminator::Goto { target, .. } => {
            (*target, cfg.get_goto_args(&cfg.get_block(latch).terminator))
        }
        _ => return None,
    };
    if target != header || !args.is_empty() {
        return None;
    }
    let mut latch_load = None;
    let mut update = None;
    let mut store_count = 0;
    let mut load_values = AHashSet::new();
    for &block in &lp.body {
        for &v in &cfg.get_block(block).insts {
            if matches!(cfg.get_inst(v).data, CfgInstData::Load { slot: s } if s == slot) {
                load_values.insert(v);
            }
        }
    }
    // Any second write, place write, allocation, or by-reference escape makes
    // the slot sequence ambiguous, so refuse the whole loop.
    for &block in &lp.body {
        for &v in &cfg.get_block(block).insts {
            match &cfg.get_inst(v).data {
                CfgInstData::Load { slot: s } if *s == slot => {
                    if block == latch {
                        latch_load = Some(v);
                    }
                }
                CfgInstData::Store { slot: s, value } if *s == slot => {
                    store_count += 1;
                    if block == latch {
                        update = Some(*value);
                    }
                }
                CfgInstData::Alloc { slot: s, .. } if *s == slot => return None,
                CfgInstData::PlaceWrite { place, .. } if matches!(place.base, PlaceBase::Local(s) if s == slot) =>
                {
                    return None;
                }
                CfgInstData::Call { args, .. } | CfgInstData::AccessorCall { args, .. }
                    if cfg
                        .call_args(args)
                        .iter()
                        .any(|arg| arg.is_by_ref() && load_values.contains(&arg.value)) =>
                {
                    return None;
                }
                _ => {}
            }
        }
    }
    if store_count != 1 || latch_load.is_none() {
        return None;
    }
    let update = update?;
    let latch_load = latch_load?;
    let (step, direction) = match cfg.get_inst(update).data {
        CfgInstData::Add(a, b) if a == latch_load => (const_value(cfg, b, ty)?, 1),
        CfgInstData::Add(a, b) if b == latch_load => (const_value(cfg, a, ty)?, 1),
        CfgInstData::Sub(a, b) if a == latch_load => (-const_value(cfg, b, ty)?, -1),
        _ => return None,
    };
    if step == 0 || (direction > 0 && step <= 0) || (direction < 0 && step >= 0) {
        return None;
    }
    let count = checked_trip_count(initial, bound, step, cmp, ty)?;
    Some(Trip {
        count,
        initial,
        stride: step,
        ty,
        slot,
    })
}

/// Compute a finite trip count and prove every checked IV update, including
/// the latch update after the final body iteration, without iterating by the
/// trip count.
fn checked_trip_count(initial: i128, bound: i128, step: i128, cmp: u8, ty: Type) -> Option<u64> {
    if step == 0 {
        return None;
    }
    let count = if step > 0 {
        if cmp == 2 || cmp == 3 {
            return None;
        }
        if (cmp == 0 && initial >= bound) || (cmp == 1 && initial > bound) {
            0
        } else {
            let delta = bound.checked_sub(initial)?;
            let n = delta.checked_add(step.checked_sub(1)?)?.checked_div(step)?;
            if cmp == 1 {
                (delta / step).checked_add(1)?.try_into().ok()?
            } else {
                n.try_into().ok()?
            }
        }
    } else {
        if cmp == 0 || cmp == 1 {
            return None;
        }
        if (cmp == 2 && initial <= bound) || (cmp == 3 && initial < bound) {
            0
        } else {
            let delta = initial.checked_sub(bound)?;
            let magnitude = step.checked_neg()?;
            let n = if cmp == 3 {
                (delta / magnitude).checked_add(1)?
            } else {
                delta
                    .checked_add(magnitude.checked_sub(1)?)?
                    .checked_div(magnitude)?
            };
            n.try_into().ok()?
        }
    };
    // The update executes after the final body iteration. Refuse a sequence
    // that would overflow a checked induction update (which must still trap).
    let width = ty.int_bit_width()?;
    let (min, max) = if ty.is_signed() {
        (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
    } else {
        (0, (1i128 << width) - 1)
    };
    if initial < min || initial > max {
        return None;
    }
    let count_i = i128::from(count);
    let final_value = step.checked_mul(count_i)?.checked_add(initial)?;
    if final_value < min || final_value > max {
        return None;
    }
    Some(count)
}

fn map_value(map: &AHashMap<CfgValue, CfgValue>, value: CfgValue) -> CfgValue {
    map.get(&value).copied().unwrap_or(value)
}

/// The variable-length operands of one instruction, read out of the original
/// graph before any cloning begins.
///
/// At most one field is ever non-empty; which one follows from the
/// instruction's own shape. The arms filled by [`capture_operands`] mirror the
/// payload-reading arms of [`remap_data`] one for one — a payload kind added to
/// either belongs in both.
#[derive(Debug, Default)]
struct SourceOperands {
    /// `Intrinsic` arguments, `StructInit` fields, `ArrayInit` elements and
    /// `EnumVariant` payloads: flat value lists.
    values: Vec<CfgValue>,
    /// `Call` and `AccessorCall` arguments.
    call_args: Vec<CfgCallArg>,
    /// `PlaceRead` and `PlaceWrite` projections.
    projections: Vec<Projection>,
}

fn capture_operands(cfg: &Cfg, data: &CfgInstData) -> SourceOperands {
    let mut operands = SourceOperands::default();
    match data {
        CfgInstData::Call { .. } => operands.call_args = cfg.get_call_args(data).to_vec(),
        CfgInstData::AccessorCall { args, .. } => operands.call_args = cfg.call_args(args).to_vec(),
        CfgInstData::Intrinsic { .. } => operands.values = cfg.get_intrinsic_args(data).to_vec(),
        CfgInstData::StructInit { .. } => operands.values = cfg.get_struct_fields(data).to_vec(),
        CfgInstData::ArrayInit { elements } => {
            operands.values = cfg.array_elements(elements).to_vec();
        }
        CfgInstData::EnumVariant { payload, .. } => {
            operands.values = cfg.enum_payload(payload).to_vec();
        }
        CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
            operands.projections = cfg.get_place_projections(place).to_vec();
        }
        _ => {}
    }
    operands
}

/// One loop-body instruction as it was before unrolling started.
#[derive(Debug)]
struct SourceInst {
    ty: Type,
    span: Span,
    /// The original instruction shape. Its variable-length payload ranges still
    /// address the *live* graph's arenas — which is where they came from, and
    /// where they stay valid, since this pass only ever appends to them. Read
    /// the operands out of `operands`, never by resolving these ranges: after
    /// unrolling begins, resolving them against the graph yields the original
    /// operands rather than the mapped ones. Nothing here can: `remap_data`
    /// takes no read handle on the source graph at all.
    data: CfgInstData,
    operands: SourceOperands,
}

/// One loop-body block as it was before unrolling started, with its
/// terminator's block-argument lists already resolved.
#[derive(Debug)]
struct SourceBlock {
    params: Vec<(CfgValue, Type)>,
    insts: Vec<CfgValue>,
    /// Carries payload ranges under the same rule as [`SourceInst::data`]: the
    /// resolved lists below are what `remap_term` reads.
    terminator: Terminator,
    goto_args: Vec<CfgValue>,
    then_args: Vec<CfgValue>,
    else_args: Vec<CfgValue>,
    switch_cases: Vec<(i64, BlockId)>,
}

/// An immutable copy of the loop subgraph, taken once per unroll.
///
/// [`unroll_one`] appends blocks and instructions and rewires terminators while
/// it reads the original shape, so those reads cannot borrow the graph it is
/// editing. They only ever touch the loop body, so this copies the loop body
/// and nothing else — where a whole-`Cfg` clone used to stand in, at a cost
/// proportional to the entire function (RUE-1842).
#[derive(Debug)]
struct LoopSource {
    blocks: AHashMap<BlockId, SourceBlock>,
    insts: AHashMap<CfgValue, SourceInst>,
    ownership_boundaries: AHashSet<CfgValue>,
}

impl LoopSource {
    fn capture(cfg: &Cfg, body: &[BlockId]) -> Self {
        let mut blocks = AHashMap::with_capacity(body.len());
        let mut insts = AHashMap::new();
        let mut ownership_boundaries = AHashSet::new();
        for &id in body {
            let block = cfg.get_block(id);
            let terminator = block.terminator.duplicate_with_owner();
            let (mut goto_args, mut then_args, mut else_args, mut switch_cases) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            // Each accessor panics on the wrong terminator shape, so ask only
            // for the lists this terminator actually has.
            match &terminator {
                Terminator::Goto { .. } => goto_args = cfg.get_goto_args(&terminator).to_vec(),
                Terminator::Branch { .. } => {
                    then_args = cfg.get_branch_then_args(&terminator).to_vec();
                    else_args = cfg.get_branch_else_args(&terminator).to_vec();
                }
                Terminator::Switch { .. } => {
                    switch_cases = cfg.get_switch_cases(&terminator).to_vec();
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
            for &(value, _) in &block.params {
                if cfg.is_ownership_boundary_value(value) {
                    ownership_boundaries.insert(value);
                }
            }
            for &value in &block.insts {
                let inst = cfg.get_inst(value);
                insts.insert(
                    value,
                    SourceInst {
                        ty: inst.ty,
                        span: inst.span,
                        data: inst.data.duplicate_with_owner(),
                        operands: capture_operands(cfg, &inst.data),
                    },
                );
                if cfg.is_ownership_boundary_value(value) {
                    ownership_boundaries.insert(value);
                }
            }
            blocks.insert(
                id,
                SourceBlock {
                    params: block.params.clone(),
                    insts: block.insts.clone(),
                    terminator,
                    goto_args,
                    then_args,
                    else_args,
                    switch_cases,
                },
            );
        }
        Self {
            blocks,
            insts,
            ownership_boundaries,
        }
    }

    /// A captured loop-body block. Callers only ever ask for blocks they took
    /// from the same `body` slice this was captured from.
    fn block(&self, id: BlockId) -> &SourceBlock {
        &self.blocks[&id]
    }

    /// A captured loop-body instruction, keyed by its original value.
    fn inst(&self, value: CfgValue) -> &SourceInst {
        &self.insts[&value]
    }

    fn is_ownership_boundary_value(&self, value: CfgValue) -> bool {
        self.ownership_boundaries.contains(&value)
    }
}

fn remap_data(
    operands: &SourceOperands,
    cfg: &mut Cfg,
    data: &CfgInstData,
    map: &AHashMap<CfgValue, CfgValue>,
    specialize_slot: Option<(u32, u64)>,
) -> Result<CfgInstData, CfgEditError> {
    let m = |v| map_value(map, v);
    Ok(match data {
        CfgInstData::Const(v) => CfgInstData::Const(*v),
        CfgInstData::BoolConst(v) => CfgInstData::BoolConst(*v),
        CfgInstData::StringConst(v) => CfgInstData::StringConst(*v),
        CfgInstData::Param { index } => CfgInstData::Param { index: *index },
        CfgInstData::BlockParam { index } => CfgInstData::BlockParam { index: *index },
        CfgInstData::Add(a, b) => CfgInstData::Add(m(*a), m(*b)),
        CfgInstData::Sub(a, b) => CfgInstData::Sub(m(*a), m(*b)),
        CfgInstData::Mul(a, b) => CfgInstData::Mul(m(*a), m(*b)),
        CfgInstData::WrappingAdd(a, b) => CfgInstData::WrappingAdd(m(*a), m(*b)),
        CfgInstData::WrappingSub(a, b) => CfgInstData::WrappingSub(m(*a), m(*b)),
        CfgInstData::WrappingMul(a, b) => CfgInstData::WrappingMul(m(*a), m(*b)),
        CfgInstData::Div(a, b) => CfgInstData::Div(m(*a), m(*b)),
        CfgInstData::Mod(a, b) => CfgInstData::Mod(m(*a), m(*b)),
        CfgInstData::Eq(a, b) => CfgInstData::Eq(m(*a), m(*b)),
        CfgInstData::Ne(a, b) => CfgInstData::Ne(m(*a), m(*b)),
        CfgInstData::Lt(a, b) => CfgInstData::Lt(m(*a), m(*b)),
        CfgInstData::Gt(a, b) => CfgInstData::Gt(m(*a), m(*b)),
        CfgInstData::Le(a, b) => CfgInstData::Le(m(*a), m(*b)),
        CfgInstData::Ge(a, b) => CfgInstData::Ge(m(*a), m(*b)),
        CfgInstData::BitAnd(a, b) => CfgInstData::BitAnd(m(*a), m(*b)),
        CfgInstData::BitOr(a, b) => CfgInstData::BitOr(m(*a), m(*b)),
        CfgInstData::BitXor(a, b) => CfgInstData::BitXor(m(*a), m(*b)),
        CfgInstData::Shl(a, b) => CfgInstData::Shl(m(*a), m(*b)),
        CfgInstData::Shr(a, b) => CfgInstData::Shr(m(*a), m(*b)),
        CfgInstData::Neg(v) => CfgInstData::Neg(m(*v)),
        CfgInstData::Not(v) => CfgInstData::Not(m(*v)),
        CfgInstData::BitNot(v) => CfgInstData::BitNot(m(*v)),
        CfgInstData::Alloc { slot, init } => CfgInstData::Alloc {
            slot: *slot,
            init: m(*init),
        },
        CfgInstData::Load { slot } => {
            if let Some((iv_slot, raw)) = specialize_slot {
                if *slot == iv_slot {
                    CfgInstData::Const(raw)
                } else {
                    CfgInstData::Load { slot: *slot }
                }
            } else {
                CfgInstData::Load { slot: *slot }
            }
        }
        CfgInstData::Store { slot, value } => CfgInstData::Store {
            slot: *slot,
            value: m(*value),
        },
        CfgInstData::ParamStore { param_slot, value } => CfgInstData::ParamStore {
            param_slot: *param_slot,
            value: m(*value),
        },
        CfgInstData::PlaceRead { place } => {
            let base = match place.base {
                PlaceBase::Accessor(v) => PlaceBase::Accessor(m(v)),
                PlaceBase::Indirect(v) => PlaceBase::Indirect(m(v)),
                b => b,
            };
            let ps = operands
                .projections
                .iter()
                .map(|p| match p {
                    Projection::Field {
                        struct_id,
                        field_index,
                    } => Projection::Field {
                        struct_id: *struct_id,
                        field_index: *field_index,
                    },
                    Projection::Index { array_type, index } => Projection::Index {
                        array_type: *array_type,
                        index: m(*index),
                    },
                })
                .collect::<Vec<_>>();
            CfgInstData::PlaceRead {
                place: cfg.make_place(base, place.base_type, ps)?,
            }
        }
        CfgInstData::PlaceWrite { place, value } => {
            let base = match place.base {
                PlaceBase::Accessor(v) => PlaceBase::Accessor(m(v)),
                PlaceBase::Indirect(v) => PlaceBase::Indirect(m(v)),
                b => b,
            };
            let ps = operands
                .projections
                .iter()
                .map(|p| match p {
                    Projection::Field {
                        struct_id,
                        field_index,
                    } => Projection::Field {
                        struct_id: *struct_id,
                        field_index: *field_index,
                    },
                    Projection::Index { array_type, index } => Projection::Index {
                        array_type: *array_type,
                        index: m(*index),
                    },
                })
                .collect::<Vec<_>>();
            CfgInstData::PlaceWrite {
                place: cfg.make_place(base, place.base_type, ps)?,
                value: m(*value),
            }
        }
        CfgInstData::Call { runtime, name, .. } => CfgInstData::Call {
            runtime: *runtime,
            name: *name,
            args: cfg.push_call_args(operands.call_args.iter().map(|a| CfgCallArg {
                value: m(a.value),
                mode: a.mode,
            }))?,
        },
        CfgInstData::AccessorCall { name, .. } => CfgInstData::AccessorCall {
            name: *name,
            args: cfg.push_call_args(operands.call_args.iter().map(|a| CfgCallArg {
                value: m(a.value),
                mode: a.mode,
            }))?,
        },
        CfgInstData::Intrinsic {
            operation, name, ..
        } => CfgInstData::Intrinsic {
            operation: *operation,
            name: *name,
            args: cfg.push_intrinsic_args(operands.values.iter().map(|v| m(*v)))?,
        },
        CfgInstData::StructInit { struct_id, .. } => CfgInstData::StructInit {
            struct_id: *struct_id,
            fields: cfg.push_struct_fields(operands.values.iter().map(|v| m(*v)))?,
        },
        CfgInstData::ArrayInit { .. } => CfgInstData::ArrayInit {
            elements: cfg.push_array_elements(operands.values.iter().map(|v| m(*v)))?,
        },
        CfgInstData::EnumVariant {
            enum_id,
            variant_index,
            ..
        } => CfgInstData::EnumVariant {
            enum_id: *enum_id,
            variant_index: *variant_index,
            payload: cfg.push_enum_payload(operands.values.iter().map(|v| m(*v)))?,
        },
        CfgInstData::EnumPayloadGet {
            base,
            enum_id,
            variant_index,
            field_index,
        } => CfgInstData::EnumPayloadGet {
            base: m(*base),
            enum_id: *enum_id,
            variant_index: *variant_index,
            field_index: *field_index,
        },
        CfgInstData::IntCast { value, from_ty } => CfgInstData::IntCast {
            value: m(*value),
            from_ty: *from_ty,
        },
        CfgInstData::Drop { value } => CfgInstData::Drop { value: m(*value) },
        CfgInstData::StorageLive { slot, local_ty } => CfgInstData::StorageLive {
            slot: *slot,
            local_ty: *local_ty,
        },
        CfgInstData::StorageDead { slot, local_ty } => CfgInstData::StorageDead {
            slot: *slot,
            local_ty: *local_ty,
        },
    })
}

fn unroll_one(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    trip: Trip,
) -> Result<(u64, u64, u64), CfgOptimizationError> {
    let outside = cfg
        .predecessors_of(lp.header)
        .into_iter()
        .find(|p| !lp.contains(*p))
        .ok_or({
            CfgOptimizationError::Edit(CfgEditError::InvalidBuilderInput {
                operation: "unroll",
                detail: "missing loop entry",
            })
        })?;
    let exit = lp.exits[0].1;
    if trip.count == 0 {
        redirect_zero_trip(cfg, outside, lp.header, exit)?;
        return Ok((0, 0, 0));
    }
    let original_blocks = lp.body.clone();
    // Read every original instruction, terminator, and owner-bound payload from
    // one immutable copy: the pass appends blocks and rewires terminators while
    // it reads, so these reads cannot borrow the graph being edited, and later
    // iterations must not observe earlier clones. Only the loop body is ever
    // read, so only the loop body is copied (RUE-1842).
    let source = LoopSource::capture(cfg, &original_blocks);
    let original_values: u64 = original_blocks
        .iter()
        .map(|b| {
            u64::try_from(source.block(*b).params.len())
                .ok()
                .and_then(|params| {
                    params.checked_add(u64::try_from(source.block(*b).insts.len()).ok()?)
                })
                .unwrap_or(u64::MAX)
        })
        .try_fold(0u64, |a, b| a.checked_add(b))
        .unwrap_or(u64::MAX);
    let original_instructions: u64 = original_blocks
        .iter()
        .map(|b| u64::try_from(source.block(*b).insts.len()).unwrap_or(u64::MAX))
        .try_fold(0u64, |a, b| a.checked_add(b))
        .unwrap_or(u64::MAX);
    let mut iterations: Vec<AHashMap<BlockId, BlockId>> = Vec::new();
    let mut all_maps = Vec::new();
    for iteration in 0..trip.count {
        let block_map: AHashMap<_, _> = original_blocks
            .iter()
            .map(|&b| (b, cfg.new_block()))
            .collect();
        let mut map = AHashMap::new();
        for &b in &original_blocks {
            for &(v, ty) in &source.block(b).params {
                let nv = cfg.add_block_param(block_map[&b], ty);
                map.insert(v, nv);
                // Unrolling duplicates the value arena. Preserve the
                // per-value ownership fact alongside each value mapping;
                // otherwise a protected transfer Load in the loop body can
                // become forwardable only in the cloned iteration.
                if source.is_ownership_boundary_value(v) {
                    cfg.mark_ownership_boundary_value(nv);
                }
            }
        }
        let insts: Vec<(CfgValue, BlockId)> = original_blocks
            .iter()
            .flat_map(|&b| source.block(b).insts.iter().copied().map(move |v| (v, b)))
            .collect();
        // Reserve every cloned instruction value before translating any
        // operands. Accessor splicing can leave a later-appended definition
        // with a larger numeric ID than its user; numeric sorting alone would
        // then silently fall back to the original value.
        for &(v, b) in &insts {
            let i = source.inst(v);
            let nv = cfg.append_inst(
                block_map[&b],
                CfgInst {
                    data: CfgInstData::Const(0),
                    ty: i.ty,
                    span: i.span,
                },
            );
            map.insert(v, nv);
            if source.is_ownership_boundary_value(v) {
                cfg.mark_ownership_boundary_value(nv);
            }
        }
        let internal_values = original_blocks
            .iter()
            .flat_map(|block| {
                source
                    .block(*block)
                    .params
                    .iter()
                    .map(|(value, _)| *value)
                    .chain(source.block(*block).insts.iter().copied())
            })
            .collect::<AHashSet<_>>();
        let mut missing_mapping = false;
        // These read original loop values, whose instruction data and payload
        // handles this pass never rewrites — it only appends — so reading them
        // off the graph gives the same answer the copy would.
        for &(v, _) in &insts {
            super::dce::visit_instruction_uses(cfg, v, |used| {
                missing_mapping |= internal_values.contains(&used) && !map.contains_key(&used);
            });
        }
        for &block in &original_blocks {
            super::dce::visit_terminator_uses(cfg, &cfg.get_block(block).terminator, |used| {
                missing_mapping |= internal_values.contains(&used) && !map.contains_key(&used);
            });
        }
        if missing_mapping {
            return Err(CfgOptimizationError::Edit(
                CfgEditError::InvalidBuilderInput {
                    operation: "unroll",
                    detail: "loop-internal value has no clone mapping",
                },
            ));
        }
        for (v, _) in insts {
            let i = source.inst(v);
            let iteration_i = i128::from(iteration);
            let raw = trip
                .initial
                .checked_add(trip.stride.checked_mul(iteration_i).ok_or(
                    CfgOptimizationError::Edit(CfgEditError::ResourceLimitExceeded {
                        family: "unrolled IV arithmetic",
                    }),
                )?)
                .ok_or(CfgOptimizationError::Edit(
                    CfgEditError::ResourceLimitExceeded {
                        family: "unrolled IV arithmetic",
                    },
                ))?;
            let raw = encoded_const(raw, trip.ty).ok_or({
                CfgOptimizationError::Edit(CfgEditError::ResourceLimitExceeded {
                    family: "unrolled IV representation",
                })
            })?;
            let data = remap_data(&i.operands, cfg, &i.data, &map, Some((trip.slot, raw)))?;
            let nv = map[&v];
            cfg.get_inst_mut(nv).data = data;
        }
        iterations.push(block_map);
        all_maps.push(map);
    }
    let iteration_count = usize::try_from(trip.count).map_err(|_| {
        CfgOptimizationError::Edit(CfgEditError::ResourceLimitExceeded {
            family: "unrolled iteration index",
        })
    })?;
    for i in 0..iteration_count {
        let block_map = &iterations[i];
        let map = &all_maps[i];
        for &b in &original_blocks {
            let source_block = source.block(b);
            let source_term = &source_block.terminator;
            let target = block_map[&b];
            let term = if b == lp.header {
                let body = match source_term {
                    Terminator::Branch {
                        then_block,
                        else_block,
                        ..
                    } if lp.contains(*then_block) => *then_block,
                    Terminator::Branch {
                        then_block: _,
                        else_block,
                        ..
                    } => *else_block,
                    _ => {
                        return Err(CfgOptimizationError::Edit(
                            CfgEditError::InvalidBuilderInput {
                                operation: "unroll",
                                detail: "header shape changed",
                            },
                        ));
                    }
                };
                let header_args: Vec<_> = match source_term {
                    Terminator::Branch { then_block, .. } if *then_block == body => source_block
                        .then_args
                        .iter()
                        .map(|v| map_value(map, *v))
                        .collect(),
                    Terminator::Branch { .. } => source_block
                        .else_args
                        .iter()
                        .map(|v| map_value(map, *v))
                        .collect(),
                    _ => Vec::new(),
                };
                Terminator::Goto {
                    target: block_map[&body],
                    args: cfg.push_goto_args(header_args)?,
                }
            } else if b == lp.latches[0] {
                let next = if i + 1 < iterations.len() {
                    iterations[i + 1][&lp.header]
                } else {
                    exit
                };
                Terminator::Goto {
                    target: next,
                    // The accepted slot-IV shape has no header parameters;
                    // the latch edge therefore remains argument-free.
                    args: cfg.push_goto_args(Vec::<CfgValue>::new())?,
                }
            } else {
                remap_term(source_block, cfg, map, block_map)?
            };
            cfg.get_block_mut(target).terminator = term;
        }
    }
    redirect(cfg, outside, lp.header, iterations[0][&lp.header]);
    Ok((
        trip.count
            .checked_mul(u64::try_from(original_blocks.len()).map_err(|_| {
                CfgOptimizationError::Edit(CfgEditError::ResourceLimitExceeded {
                    family: "unrolled blocks",
                })
            })?)
            .ok_or(CfgOptimizationError::Edit(
                CfgEditError::ResourceLimitExceeded {
                    family: "unrolled blocks",
                },
            ))?,
        trip.count
            .checked_mul(original_values)
            .ok_or(CfgOptimizationError::Edit(
                CfgEditError::ResourceLimitExceeded {
                    family: "unrolled values",
                },
            ))?,
        trip.count
            .checked_mul(original_instructions)
            .ok_or(CfgOptimizationError::Edit(
                CfgEditError::ResourceLimitExceeded {
                    family: "unrolled instructions",
                },
            ))?,
    ))
}

fn remap_term(
    source: &SourceBlock,
    cfg: &mut Cfg,
    map: &AHashMap<CfgValue, CfgValue>,
    blocks: &AHashMap<BlockId, BlockId>,
) -> Result<Terminator, CfgOptimizationError> {
    Ok(match &source.terminator {
        Terminator::Goto { target, .. } => Terminator::Goto {
            target: blocks.get(target).copied().unwrap_or(*target),
            args: cfg.push_goto_args(source.goto_args.iter().map(|v| map_value(map, *v)))?,
        },
        Terminator::Branch {
            cond,
            then_block,
            else_block,
            ..
        } => Terminator::Branch {
            cond: map_value(map, *cond),
            then_block: blocks.get(then_block).copied().unwrap_or(*then_block),
            then_args: cfg.push_then_args(source.then_args.iter().map(|v| map_value(map, *v)))?,
            else_block: blocks.get(else_block).copied().unwrap_or(*else_block),
            else_args: cfg.push_else_args(source.else_args.iter().map(|v| map_value(map, *v)))?,
        },
        Terminator::Switch {
            scrutinee,
            cases: _,
            default,
        } => Terminator::Switch {
            scrutinee: map_value(map, *scrutinee),
            cases: cfg.push_switch_cases(
                source
                    .switch_cases
                    .iter()
                    .map(|(v, b)| (*v, blocks.get(b).copied().unwrap_or(*b))),
            )?,
            default: blocks.get(default).copied().unwrap_or(*default),
        },
        Terminator::Return { value } => Terminator::Return {
            value: value.map(|v| map_value(map, v)),
        },
        Terminator::Unreachable => Terminator::Unreachable,
        Terminator::None => Terminator::None,
    })
}

fn redirect_zero_trip(
    cfg: &mut Cfg,
    from: BlockId,
    old: BlockId,
    new: BlockId,
) -> Result<(), CfgOptimizationError> {
    let term = std::mem::replace(
        &mut cfg.get_block_mut(from).terminator,
        Terminator::Unreachable,
    );
    let Terminator::Goto { target, .. } = term else {
        return Err(CfgOptimizationError::Edit(
            CfgEditError::InvalidBuilderInput {
                operation: "unroll",
                detail: "zero-trip entry is not a goto",
            },
        ));
    };
    let args = cfg.push_goto_args(Vec::<CfgValue>::new())?;
    cfg.get_block_mut(from).terminator = Terminator::Goto {
        target: if target == old { new } else { target },
        args,
    };
    Ok(())
}

fn redirect(cfg: &mut Cfg, from: BlockId, old: BlockId, new: BlockId) {
    let term = std::mem::replace(
        &mut cfg.get_block_mut(from).terminator,
        Terminator::Unreachable,
    );
    cfg.get_block_mut(from).terminator = match term {
        Terminator::Goto { target, args } => Terminator::Goto {
            target: if target == old { new } else { target },
            args,
        },
        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => Terminator::Branch {
            cond,
            then_block: if then_block == old { new } else { then_block },
            then_args,
            else_block: if else_block == old { new } else { else_block },
            else_args,
        },
        x => x,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use rue_span::Span;

    fn slot_loop(initial: u64, bound: u64, descending: bool) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "slot_loop".into(), Vec::<bool>::new());
        let entry = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let latch = cfg.new_block();
        let exit = cfg.new_block();
        let span = Span::new(1, 2);
        let init = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(initial),
                ty: Type::I32,
                span,
            },
        );
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Alloc { slot: 0, init },
                ty: Type::I32,
                span,
            },
        );
        cfg.set_goto(entry, header, []);
        let iv = cfg.append_inst(
            header,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::I32,
                span,
            },
        );
        let limit = cfg.append_inst(
            header,
            CfgInst {
                data: CfgInstData::Const(bound),
                ty: Type::I32,
                span,
            },
        );
        let cond = cfg.append_inst(
            header,
            CfgInst {
                data: if descending {
                    CfgInstData::Gt(iv, limit)
                } else {
                    CfgInstData::Lt(iv, limit)
                },
                ty: Type::BOOL,
                span,
            },
        );
        cfg.set_branch(header, cond, body, [], exit, []);
        let body_iv = cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::I32,
                span,
            },
        );
        let one = cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span,
            },
        );
        cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::Add(body_iv, one),
                ty: Type::I32,
                span,
            },
        );
        cfg.set_goto(body, latch, []);
        let latch_iv = cfg.append_inst(
            latch,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::I32,
                span,
            },
        );
        let step = cfg.append_inst(
            latch,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span,
            },
        );
        let update = cfg.append_inst(
            latch,
            CfgInst {
                data: if descending {
                    CfgInstData::Sub(latch_iv, step)
                } else {
                    CfgInstData::Add(latch_iv, step)
                },
                ty: Type::I32,
                span,
            },
        );
        cfg.append_inst(
            latch,
            CfgInst {
                data: CfgInstData::Store {
                    slot: 0,
                    value: update,
                },
                ty: Type::UNIT,
                span,
            },
        );
        cfg.set_goto(latch, header, []);
        let result = cfg.append_inst(
            exit,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span,
            },
        );
        cfg.set_return(exit, Some(result));
        cfg
    }

    fn independent_slot_loops(loop_count: usize, bound: u64) -> Cfg {
        assert!(loop_count > 0);
        let mut cfg = Cfg::new(
            Type::I32,
            u32::try_from(loop_count).unwrap(),
            0,
            "independent_slot_loops".into(),
            Vec::<bool>::new(),
        );
        let span = Span::new(10, 11);
        let mut preheader = cfg.new_block();

        for slot in 0..loop_count {
            let header = cfg.new_block();
            let body = cfg.new_block();
            let latch = cfg.new_block();
            let exit = cfg.new_block();
            let slot = u32::try_from(slot).unwrap();

            let init = cfg.append_inst(
                preheader,
                CfgInst {
                    data: CfgInstData::Const(0),
                    ty: Type::I32,
                    span,
                },
            );
            cfg.append_inst(
                preheader,
                CfgInst {
                    data: CfgInstData::Alloc { slot, init },
                    ty: Type::I32,
                    span,
                },
            );
            cfg.set_goto(preheader, header, []);

            let iv = cfg.append_inst(
                header,
                CfgInst {
                    data: CfgInstData::Load { slot },
                    ty: Type::I32,
                    span,
                },
            );
            let limit = cfg.append_inst(
                header,
                CfgInst {
                    data: CfgInstData::Const(bound),
                    ty: Type::I32,
                    span,
                },
            );
            let cond = cfg.append_inst(
                header,
                CfgInst {
                    data: CfgInstData::Lt(iv, limit),
                    ty: Type::BOOL,
                    span,
                },
            );
            cfg.set_branch(header, cond, body, [], exit, []);

            let body_iv = cfg.append_inst(
                body,
                CfgInst {
                    data: CfgInstData::Load { slot },
                    ty: Type::I32,
                    span,
                },
            );
            let body_one = cfg.append_inst(
                body,
                CfgInst {
                    data: CfgInstData::Const(1),
                    ty: Type::I32,
                    span,
                },
            );
            cfg.append_inst(
                body,
                CfgInst {
                    data: CfgInstData::Add(body_iv, body_one),
                    ty: Type::I32,
                    span,
                },
            );
            cfg.set_goto(body, latch, []);

            let latch_iv = cfg.append_inst(
                latch,
                CfgInst {
                    data: CfgInstData::Load { slot },
                    ty: Type::I32,
                    span,
                },
            );
            let step = cfg.append_inst(
                latch,
                CfgInst {
                    data: CfgInstData::Const(1),
                    ty: Type::I32,
                    span,
                },
            );
            let update = cfg.append_inst(
                latch,
                CfgInst {
                    data: CfgInstData::Add(latch_iv, step),
                    ty: Type::I32,
                    span,
                },
            );
            cfg.append_inst(
                latch,
                CfgInst {
                    data: CfgInstData::Store {
                        slot,
                        value: update,
                    },
                    ty: Type::UNIT,
                    span,
                },
            );
            cfg.set_goto(latch, header, []);
            preheader = exit;
        }

        let result = cfg.append_inst(
            preheader,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span,
            },
        );
        cfg.set_return(preheader, Some(result));
        cfg
    }

    fn nested_slot_loops(depth: usize, bound: u64) -> Cfg {
        nested_slot_loops_with_order(depth, bound, true)
    }

    fn nested_slot_loops_with_order(depth: usize, bound: u64, outer_latch_first: bool) -> Cfg {
        assert!(depth > 0);
        let mut cfg = Cfg::new(
            Type::I32,
            u32::try_from(depth).unwrap(),
            0,
            "nested_slot_loops".into(),
            Vec::<bool>::new(),
        );
        let entry = cfg.new_block();
        // Loop-forest order follows the first-seen latch. Most tests put the
        // outer latch first so the has-child refusal is observed before the
        // accepted inner loop. The alternate order makes a stale loop id name
        // a different header after the forest is rebuilt.
        let mut latches: Vec<_> = (0..depth).map(|_| cfg.new_block()).collect();
        if !outer_latch_first {
            latches.reverse();
        }
        let headers: Vec<_> = (0..depth).map(|_| cfg.new_block()).collect();
        let inner_preheaders: Vec<_> = (1..depth).map(|_| cfg.new_block()).collect();
        let leaf = cfg.new_block();
        let exit = cfg.new_block();
        let span = Span::new(20, 21);

        for level in 0..depth {
            let preheader = if level == 0 {
                entry
            } else {
                inner_preheaders[level - 1]
            };
            let init = cfg.append_inst(
                preheader,
                CfgInst {
                    data: CfgInstData::Const(0),
                    ty: Type::I32,
                    span,
                },
            );
            cfg.append_inst(
                preheader,
                CfgInst {
                    data: CfgInstData::Alloc {
                        slot: u32::try_from(level).unwrap(),
                        init,
                    },
                    ty: Type::I32,
                    span,
                },
            );
            cfg.set_goto(preheader, headers[level], []);

            let iv = cfg.append_inst(
                headers[level],
                CfgInst {
                    data: CfgInstData::Load {
                        slot: u32::try_from(level).unwrap(),
                    },
                    ty: Type::I32,
                    span,
                },
            );
            let limit = cfg.append_inst(
                headers[level],
                CfgInst {
                    data: CfgInstData::Const(bound),
                    ty: Type::I32,
                    span,
                },
            );
            let cond = cfg.append_inst(
                headers[level],
                CfgInst {
                    data: CfgInstData::Lt(iv, limit),
                    ty: Type::BOOL,
                    span,
                },
            );
            let body = if level + 1 == depth {
                leaf
            } else {
                inner_preheaders[level]
            };
            let loop_exit = if level == 0 { exit } else { latches[level - 1] };
            cfg.set_branch(headers[level], cond, body, [], loop_exit, []);

            let latch_iv = cfg.append_inst(
                latches[level],
                CfgInst {
                    data: CfgInstData::Load {
                        slot: u32::try_from(level).unwrap(),
                    },
                    ty: Type::I32,
                    span,
                },
            );
            let one = cfg.append_inst(
                latches[level],
                CfgInst {
                    data: CfgInstData::Const(1),
                    ty: Type::I32,
                    span,
                },
            );
            let update = cfg.append_inst(
                latches[level],
                CfgInst {
                    data: CfgInstData::Add(latch_iv, one),
                    ty: Type::I32,
                    span,
                },
            );
            cfg.append_inst(
                latches[level],
                CfgInst {
                    data: CfgInstData::Store {
                        slot: u32::try_from(level).unwrap(),
                        value: update,
                    },
                    ty: Type::UNIT,
                    span,
                },
            );
            cfg.set_goto(latches[level], headers[level], []);
        }

        let leaf_iv = cfg.append_inst(
            leaf,
            CfgInst {
                data: CfgInstData::Load {
                    slot: u32::try_from(depth - 1).unwrap(),
                },
                ty: Type::I32,
                span,
            },
        );
        let leaf_one = cfg.append_inst(
            leaf,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span,
            },
        );
        cfg.append_inst(
            leaf,
            CfgInst {
                data: CfgInstData::Add(leaf_iv, leaf_one),
                ty: Type::I32,
                span,
            },
        );
        cfg.set_goto(leaf, latches[depth - 1], []);
        let result = cfg.append_inst(
            exit,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span,
            },
        );
        cfg.set_return(exit, Some(result));
        cfg
    }

    #[test]
    fn trip_counts_cover_strict_and_inclusive_boundaries() {
        assert_eq!(checked_trip_count(0, 3, 1, 0, Type::I64), Some(3));
        assert_eq!(checked_trip_count(0, 3, 1, 1, Type::I64), Some(4));
        assert_eq!(checked_trip_count(3, 3, 1, 0, Type::I64), Some(0));
        assert_eq!(checked_trip_count(3, 3, 1, 1, Type::I64), Some(1));
        assert_eq!(checked_trip_count(3, 0, -1, 2, Type::I64), Some(3));
        assert_eq!(checked_trip_count(3, 0, -1, 3, Type::I64), Some(4));
        assert_eq!(checked_trip_count(3, 3, -1, 2, Type::I64), Some(0));
        assert_eq!(checked_trip_count(3, 3, -1, 3, Type::I64), Some(1));
    }

    #[test]
    fn trip_counts_refuse_wrong_direction_zero_stride_and_overflow() {
        assert_eq!(checked_trip_count(0, 3, -1, 0, Type::I64), None);
        assert_eq!(checked_trip_count(3, 0, 1, 2, Type::I64), None);
        assert_eq!(checked_trip_count(0, 3, 0, 0, Type::I64), None);
        assert_eq!(checked_trip_count(127, 127, 1, 1, Type::I8), None);
        assert_eq!(
            checked_trip_count(i128::MAX, i128::MAX, 1, 1, Type::I64),
            None
        );
    }

    #[test]
    fn budget_is_bounded_and_consumer_neutral() {
        let mut budget = super::super::CodeGrowthBudget::o3();
        assert!(budget.try_charge(super::super::CodeGrowth {
            values: 12,
            blocks: 4,
        }));
        assert!(budget.try_charge(super::super::CodeGrowth {
            values: 200,
            blocks: 20,
        }));
        assert_eq!(budget.used(), 212);
        assert_eq!(budget.remaining(), 44);
        assert!(!budget.try_charge(super::super::CodeGrowth {
            values: 100,
            blocks: 1,
        }));
    }

    #[test]
    fn run_unrolls_slot_iv_for_zero_one_n_and_descending() {
        for (initial, bound, descending, expected) in [
            (3, 3, false, 1),
            (0, 1, false, 1),
            (0, 4, false, 1),
            (3, 0, true, 1),
        ] {
            let mut cfg = slot_loop(initial, bound, descending);
            cfg.verify().unwrap();
            let stats = run(&mut cfg).unwrap();
            assert_eq!(stats.loops_unrolled, expected);
            cfg.verify().unwrap();
            let dom = DominatorTree::compute(&cfg);
            assert!(loops(&cfg, &dom).is_empty());
        }
    }

    #[test]
    fn run_normalizes_a_false_body_branch_arm() {
        let mut cfg = slot_loop(3, 0, true);
        let header = BlockId::from_raw(1);
        let body = BlockId::from_raw(2);
        let exit = BlockId::from_raw(4);
        let cond = cfg
            .get_block(header)
            .insts
            .iter()
            .copied()
            .find(|value| matches!(cfg.get_inst(*value).data, CfgInstData::Gt(_, _)))
            .unwrap();
        let (iv, bound) = match cfg.get_inst(cond).data {
            CfgInstData::Gt(iv, bound) => (iv, bound),
            _ => unreachable!(),
        };
        cfg.get_inst_mut(cond).data = CfgInstData::Le(iv, bound);
        cfg.get_block_mut(header).terminator = Terminator::None;
        cfg.set_branch(header, cond, exit, [], body, []);
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 1);
        cfg.verify().unwrap();
    }

    #[test]
    fn run_tracks_bounded_analysis_and_recomputation() {
        let mut cfg = slot_loop(0, 3, false);
        let stats = run(&mut cfg).unwrap();
        assert!(stats.forest_computations <= 3);
        assert!(stats.loops_analyzed <= 2);
        assert!(stats.values_cloned > 0);
    }

    #[test]
    fn run_batches_independent_loops_in_one_forest_generation() {
        let mut cfg = independent_slot_loops(3, 2);
        let mut repeated = independent_slot_loops(3, 2);
        cfg.verify().unwrap();
        let stats = run(&mut cfg).unwrap();
        assert_eq!(
            stats,
            Stats {
                forest_computations: 2,
                loops_analyzed: 3,
                loops_unrolled: 3,
                budget_refusals: 0,
                shape_refusals: 0,
                blocks_cloned: 18,
                values_cloned: 60,
                instructions_cloned: 60,
            }
        );
        assert_eq!(run(&mut repeated).unwrap(), stats);
        assert_eq!(format!("{repeated:?}"), format!("{cfg:?}"));
        cfg.verify().unwrap();
        let dom = DominatorTree::compute(&cfg);
        assert!(loops(&cfg, &dom).is_empty());
    }

    #[test]
    fn refused_independent_loop_does_not_invalidate_the_batch() {
        let mut cfg = independent_slot_loops(3, 2);
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let refused_header = forest.get(1).header;
        let term = std::mem::replace(
            &mut cfg.get_block_mut(refused_header).terminator,
            Terminator::None,
        );
        cfg.append_inst(
            refused_header,
            CfgInst {
                data: CfgInstData::Const(99),
                ty: Type::I32,
                span: Span::new(12, 13),
            },
        );
        cfg.get_block_mut(refused_header).terminator = term;

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.forest_computations, 2);
        assert_eq!(stats.loops_analyzed, 4);
        assert_eq!(stats.loops_unrolled, 2);
        assert_eq!(stats.shape_refusals, 2);
        assert_eq!(stats.blocks_cloned, 12);
        assert_eq!(stats.values_cloned, 40);
        cfg.verify().unwrap();
        let dom = DominatorTree::compute(&cfg);
        let remaining = loops(&cfg, &dom);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining.get(0).header, refused_header);
    }

    #[test]
    fn independent_batch_charges_the_shared_budget_in_forest_order() {
        let mut cfg = independent_slot_loops(3, 2);
        // Each loop charges exactly 20 values. Forty remaining units accept
        // the first two forest-ordered loops and refuse the third both before
        // and after the rebuild driven by those successful edits.
        let mut budget = super::super::CodeGrowthBudget::with_used(216, 0);
        let stats = run_with_budget(&mut cfg, &mut budget).unwrap();
        assert_eq!(stats.forest_computations, 2);
        assert_eq!(stats.loops_analyzed, 4);
        assert_eq!(stats.loops_unrolled, 2);
        assert_eq!(stats.budget_refusals, 2);
        assert_eq!(stats.shape_refusals, 0);
        assert_eq!(stats.blocks_cloned, 12);
        assert_eq!(stats.values_cloned, 40);
        assert_eq!(budget.used(), 256);
        cfg.verify().unwrap();
    }

    #[test]
    fn run_refuses_growth_before_cloning() {
        let mut cfg = slot_loop(0, 100, false);
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.budget_refusals, 1);
        assert_eq!(stats.values_cloned, 0);
    }

    #[test]
    fn run_respects_budget_carried_from_another_growth_pass() {
        let mut cfg = slot_loop(0, 3, false);
        let mut budget = super::super::CodeGrowthBudget::with_used(250, 0);
        let stats = run_with_budget(&mut cfg, &mut budget).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.budget_refusals, 1);
        assert_eq!(budget.used(), 250);
    }

    #[test]
    fn run_refuses_block_growth_before_cloning() {
        let mut cfg = slot_loop(0, 3, false);
        let mut budget = super::super::CodeGrowthBudget::with_used(0, 256);
        let stats = run_with_budget(&mut cfg, &mut budget).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.budget_refusals, 1);
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.used_blocks(), 256);
    }

    #[test]
    fn value_budget_boundary_includes_block_parameters() {
        // The fixture has ten body values and no block parameters:
        // 25 iterations fit under 256, while 26 do not.
        let mut accepted = slot_loop(0, 25, false);
        let accepted_stats = run(&mut accepted).unwrap();
        assert_eq!(accepted_stats.loops_unrolled, 1);
        let mut refused = slot_loop(0, 26, false);
        let refused_stats = run(&mut refused).unwrap();
        assert_eq!(refused_stats.loops_unrolled, 0);
        assert_eq!(refused_stats.budget_refusals, 1);
    }

    #[test]
    fn run_refuses_effecting_or_extra_header_work() {
        let mut cfg = slot_loop(0, 2, false);
        let header = BlockId::from_raw(1);
        let term = std::mem::replace(&mut cfg.get_block_mut(header).terminator, Terminator::None);
        cfg.append_inst(
            header,
            CfgInst {
                data: CfgInstData::Const(99),
                ty: Type::I32,
                span: Span::new(3, 4),
            },
        );
        cfg.get_block_mut(header).terminator = term;
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.shape_refusals, 1);
    }

    #[test]
    fn run_unrolls_a_constant_trip_inner_loop_then_its_containing_loop() {
        let mut cfg = nested_slot_loops(2, 2);
        cfg.verify().unwrap();
        let stats = run(&mut cfg).unwrap();
        assert_eq!(
            stats,
            Stats {
                forest_computations: 3,
                loops_analyzed: 3,
                loops_unrolled: 2,
                budget_refusals: 0,
                shape_refusals: 1,
                blocks_cloned: 30,
                values_cloned: 98,
                instructions_cloned: 98,
            }
        );
        cfg.verify().unwrap();
        let dom = DominatorTree::compute(&cfg);
        assert!(loops(&cfg, &dom).is_empty());
    }

    #[test]
    fn run_unrolls_deeper_nests_from_the_inside_out() {
        let mut cfg = nested_slot_loops(3, 1);
        cfg.verify().unwrap();
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.forest_computations, 4);
        assert_eq!(stats.loops_analyzed, 6);
        assert_eq!(stats.loops_unrolled, 3);
        assert_eq!(stats.shape_refusals, 3);
        assert_eq!(stats.budget_refusals, 0);
        cfg.verify().unwrap();
        let dom = DominatorTree::compute(&cfg);
        assert!(loops(&cfg, &dom).is_empty());
    }

    #[test]
    fn run_refuses_a_noncanonical_inner_loop_and_its_containing_loop() {
        let mut cfg = nested_slot_loops(2, 2);
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let inner_header = forest
            .loops()
            .iter()
            .find(|lp| lp.parent.is_some())
            .unwrap()
            .header;
        let term = std::mem::replace(
            &mut cfg.get_block_mut(inner_header).terminator,
            Terminator::None,
        );
        cfg.append_inst(
            inner_header,
            CfgInst {
                data: CfgInstData::Const(99),
                ty: Type::I32,
                span: Span::new(22, 23),
            },
        );
        cfg.get_block_mut(inner_header).terminator = term;
        cfg.verify().unwrap();
        let before = format!("{:?}", cfg);
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.forest_computations, 1);
        assert_eq!(stats.loops_analyzed, 2);
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.shape_refusals, 2);
        assert_eq!(stats.budget_refusals, 0);
        assert_eq!(format!("{:?}", cfg), before);
    }

    #[test]
    fn run_applies_the_existing_budget_to_an_innermost_loop() {
        let mut cfg = nested_slot_loops(2, 26);
        let before = format!("{:?}", cfg);
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.forest_computations, 1);
        assert_eq!(stats.loops_analyzed, 2);
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.shape_refusals, 1);
        assert_eq!(stats.budget_refusals, 1);
        assert_eq!(stats.blocks_cloned, 0);
        assert_eq!(stats.values_cloned, 0);
        assert_eq!(format!("{:?}", cfg), before);
    }

    #[test]
    fn run_rebuilds_a_counterfeit_loop_id_and_stale_enclosing_body() {
        let mut cfg = nested_slot_loops_with_order(2, 2, false);
        cfg.verify().unwrap();
        let old_block_count = cfg.block_count();
        let dom = DominatorTree::compute(&cfg);
        let stale_forest = loops(&cfg, &dom);
        assert_eq!(stale_forest.len(), 2);
        let stale_zero = stale_forest.get(0);
        assert!(
            stale_zero.parent.is_some(),
            "loop zero starts as the inner loop"
        );
        let stale_inner_header = stale_zero.header;
        let stale_outer = stale_forest
            .loops()
            .iter()
            .find(|lp| lp.parent.is_none())
            .unwrap();
        assert_eq!(stale_outer.body.len(), 6);

        let trip = recognize(&cfg, stale_zero).expect("the inner loop is canonical");
        let (blocks, values, instructions) = unroll_one(&mut cfg, stale_zero, trip).unwrap();
        assert_eq!((blocks, values, instructions), (6, 20, 20));
        cfg.verify().unwrap();

        let dom = DominatorTree::compute(&cfg);
        let fresh_forest = loops(&cfg, &dom);
        assert_eq!(fresh_forest.len(), 1);
        let fresh_zero = fresh_forest.get(0);
        assert_ne!(fresh_zero.header, stale_inner_header);
        assert_eq!(fresh_zero.header, stale_outer.header);
        assert_eq!(fresh_zero.body.len(), 12);
        assert!(
            fresh_zero
                .body
                .iter()
                .any(|block| block.as_u32() as usize >= old_block_count),
            "the rebuilt enclosing body must contain the cloned inner blocks"
        );

        // `run` must consume the fresh 12-block body. Reusing the stale
        // six-block body (or treating stale loop id zero as the outer loop)
        // would report only 12 cloned blocks here.
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.forest_computations, 2);
        assert_eq!(stats.loops_analyzed, 1);
        assert_eq!(stats.loops_unrolled, 1);
        assert_eq!(stats.blocks_cloned, 24);
        assert_eq!(stats.values_cloned, 78);
        cfg.verify().unwrap();
    }

    #[test]
    fn unrolling_never_clones_the_whole_graph() {
        // RUE-1842: `unroll_one` read the original loop out of a whole-`Cfg`
        // clone, once per accepted unroll. The snapshot is load-bearing — the
        // pass appends blocks and rewires terminators while it reads the
        // original shape — but it only ever reads the loop body, so copying
        // the function was pure waste. `Cfg::clone` counts itself, which makes
        // this checkable directly rather than by matching source text.
        let mut cfg = slot_loop(0, 3, false);
        Cfg::reset_test_clone_count();
        let stats = run(&mut cfg).unwrap();
        let clones = Cfg::test_clone_count();
        assert_eq!(stats.loops_unrolled, 1, "the fixture must actually unroll");
        assert_eq!(
            clones, 0,
            "unrolling cloned the whole graph {clones} time(s)"
        );
        cfg.verify().unwrap();
    }

    #[test]
    fn the_loop_snapshot_copies_only_the_loop_body() {
        // The other half of RUE-1842: a copy that avoids `Cfg::clone` but still
        // walks every block would count zero clones and cost just as much. The
        // snapshot must be bounded by the loop it was asked for.
        let cfg = slot_loop(0, 3, false);
        let entry = BlockId::from_raw(0);
        let exit = BlockId::from_raw(4);
        let body = vec![
            BlockId::from_raw(1),
            BlockId::from_raw(2),
            BlockId::from_raw(3),
        ];
        let source = LoopSource::capture(&cfg, &body);

        assert_eq!(
            source.blocks.len(),
            body.len(),
            "the snapshot copied blocks it was not asked for"
        );
        for outside in [entry, exit] {
            assert!(
                !source.blocks.contains_key(&outside),
                "the snapshot reached outside the loop body"
            );
        }
        let expected: usize = body.iter().map(|b| cfg.get_block(*b).insts.len()).sum();
        assert_eq!(
            source.insts.len(),
            expected,
            "the snapshot copied instructions outside the loop body"
        );
        assert!(
            expected < cfg.get_block(entry).insts.len() + expected,
            "the fixture must have work outside the loop for this to mean anything"
        );
    }

    #[test]
    fn run_refuses_an_extra_loop_exit() {
        let mut cfg = slot_loop(0, 2, false);
        let body = BlockId::from_raw(2);
        let latch = BlockId::from_raw(3);
        let exit = BlockId::from_raw(4);
        let cond = cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::BoolConst(false),
                ty: Type::BOOL,
                span: Span::new(7, 8),
            },
        );
        cfg.get_block_mut(body).terminator = Terminator::None;
        cfg.set_branch(body, cond, latch, [], exit, []);
        cfg.verify().unwrap();
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.shape_refusals, 1);
    }

    #[test]
    fn run_clones_internal_join_parameters_and_edges() {
        let mut cfg = slot_loop(0, 2, false);
        let body = BlockId::from_raw(2);
        let latch = BlockId::from_raw(3);
        let span = Span::new(1, 2);
        let choose = cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span,
            },
        );
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let join = cfg.new_block();
        let join_param = cfg.add_block_param(join, Type::I32);
        let arg = cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::Const(7),
                ty: Type::I32,
                span,
            },
        );
        cfg.get_block_mut(body).terminator = Terminator::None;
        cfg.set_branch(body, choose, then_block, [], else_block, []);
        cfg.set_goto(then_block, join, [arg]);
        cfg.set_goto(else_block, join, [arg]);
        let join_one = cfg.append_inst(
            join,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span,
            },
        );
        cfg.append_inst(
            join,
            CfgInst {
                data: CfgInstData::Add(join_param, join_one),
                ty: Type::I32,
                span,
            },
        );
        cfg.set_goto(join, latch, []);
        cfg.verify().unwrap();
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 1);
        assert!(stats.values_cloned > 0);
        cfg.verify().unwrap();
    }

    #[test]
    fn run_copies_boundary_markers_to_each_unrolled_value() {
        let mut cfg = slot_loop(0, 2, false);
        let body = BlockId::from_raw(2);
        let marked = cfg.get_block(body).insts[0];
        cfg.mark_ownership_boundary_value(marked);

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 1);
        let marked_attached = cfg
            .blocks()
            .iter()
            .flat_map(|block| block.insts.iter().copied())
            .filter(|&value| cfg.is_ownership_boundary_value(value))
            .count();
        // The original body value and both unrolled clones must carry the
        // fact. If clone metadata propagation is removed, this remains 1.
        assert_eq!(marked_attached, 3);
    }

    #[test]
    fn run_refuses_a_preheader_store_after_alloc() {
        let mut cfg = slot_loop(0, 4, false);
        let entry = BlockId::from_raw(0);
        let term = std::mem::replace(&mut cfg.get_block_mut(entry).terminator, Terminator::None);
        let reassigned = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(3),
                ty: Type::I32,
                span: Span::new(9, 10),
            },
        );
        cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Store {
                    slot: 0,
                    value: reassigned,
                },
                ty: Type::UNIT,
                span: Span::new(9, 10),
            },
        );
        cfg.get_block_mut(entry).terminator = term;
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
    }

    #[test]
    fn run_refuses_external_use_of_a_loop_value() {
        for bound in [2, 0] {
            let mut cfg = slot_loop(0, bound, false);
            let body = BlockId::from_raw(2);
            let exit = BlockId::from_raw(4);
            let value = *cfg.get_block(body).insts.last().unwrap();
            cfg.get_block_mut(exit).terminator = Terminator::Return { value: Some(value) };
            let stats = run(&mut cfg).unwrap();
            assert_eq!(stats.loops_unrolled, 0);
        }
    }

    #[test]
    fn run_clones_accessor_calls_without_call_payload_panics() {
        let mut cfg = slot_loop(0, 2, false);
        let body = BlockId::from_raw(2);
        let term = std::mem::replace(&mut cfg.get_block_mut(body).terminator, Terminator::None);
        let arg = *cfg.get_block(body).insts.first().unwrap();
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("accessor");
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg,
                mode: crate::CfgArgMode::Normal,
            }])
            .unwrap();
        cfg.append_inst(
            body,
            CfgInst {
                data: CfgInstData::AccessorCall { name, args },
                ty: Type::I32,
                span: Span::new(11, 12),
            },
        );
        cfg.get_block_mut(body).terminator = term;
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 1);
    }
}
