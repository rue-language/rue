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

/// Shared future code-growth policy (ADR-0049). Inlining can consume this same
/// representation when its growth budget lands; this pass is the first user.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodeGrowthBudget {
    pub max_total_instructions: u64,
}

pub(crate) const O3_CODE_GROWTH_BUDGET: CodeGrowthBudget = CodeGrowthBudget {
    max_total_instructions: 256,
};

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

pub fn run(cfg: &mut Cfg) -> Result<Stats, CfgOptimizationError> {
    let mut stats = Stats::default();
    let mut budget_used = 0u64;
    loop {
        stats.forest_computations += 1;
        let dom = DominatorTree::compute(cfg);
        let forest = loops(cfg, &dom);
        if forest.is_irreducible() || forest.is_empty() {
            break;
        }
        let mut changed = false;
        for id in 0..forest.len() {
            stats.loops_analyzed += 1;
            let lp = forest.get(id);
            // Nested loops are intentionally not guessed at. Their outer body
            // contains another back edge and cloning it would duplicate a
            // second induction protocol with no canonical order.
            if lp.parent.is_some() || forest.loops().iter().any(|other| other.parent == Some(id)) {
                stats.shape_refusals += 1;
                continue;
            }
            let Some(trip) = recognize(cfg, lp) else {
                stats.shape_refusals += 1;
                continue;
            };
            let body_size: u64 = lp
                .body
                .iter()
                .map(|&b| u64::try_from(cfg.get_block(b).insts.len()).unwrap_or(u64::MAX))
                .try_fold(0u64, |a, b| a.checked_add(b))
                .unwrap_or(u64::MAX);
            let Some(growth) = trip.count.checked_mul(body_size) else {
                stats.budget_refusals += 1;
                continue;
            };
            let Some(new_used) = budget_used.checked_add(growth) else {
                stats.budget_refusals += 1;
                continue;
            };
            if new_used > O3_CODE_GROWTH_BUDGET.max_total_instructions {
                stats.budget_refusals += 1;
                continue;
            }
            let mut edited = cfg.clone();
            match unroll_one(&mut edited, lp, trip) {
                Ok((blocks, values, instructions)) => {
                    *cfg = edited;
                    budget_used = new_used;
                    stats.loops_unrolled += 1;
                    stats.blocks_cloned += blocks;
                    stats.values_cloned += values;
                    stats.instructions_cloned += instructions;
                    changed = true;
                    break;
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
    if alloc_count != 1 || initial.is_none() || cfg.is_address_taken(slot) {
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

fn remap_data(
    source: &Cfg,
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
            let ps = source
                .get_place_projections(place)
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
            let ps = source
                .get_place_projections(place)
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
            args: cfg.push_call_args(source.get_call_args(data).iter().map(|a| CfgCallArg {
                value: m(a.value),
                mode: a.mode,
            }))?,
        },
        CfgInstData::AccessorCall { name, args } => CfgInstData::AccessorCall {
            name: *name,
            args: cfg.push_call_args(source.call_args(args).iter().map(|a| CfgCallArg {
                value: m(a.value),
                mode: a.mode,
            }))?,
        },
        CfgInstData::Intrinsic { runtime, name, .. } => CfgInstData::Intrinsic {
            runtime: *runtime,
            name: *name,
            args: cfg.push_intrinsic_args(source.get_intrinsic_args(data).iter().map(|v| m(*v)))?,
        },
        CfgInstData::StructInit { struct_id, .. } => CfgInstData::StructInit {
            struct_id: *struct_id,
            fields: cfg.push_struct_fields(source.get_struct_fields(data).iter().map(|v| m(*v)))?,
        },
        CfgInstData::ArrayInit { elements } => CfgInstData::ArrayInit {
            elements: cfg
                .push_array_elements(source.array_elements(elements).iter().map(|v| m(*v)))?,
        },
        CfgInstData::EnumVariant {
            enum_id,
            variant_index,
            payload,
        } => CfgInstData::EnumVariant {
            enum_id: *enum_id,
            variant_index: *variant_index,
            payload: cfg.push_enum_payload(source.enum_payload(payload).iter().map(|v| m(*v)))?,
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
    // Read every original instruction, terminator, and owner-bound payload
    // from one immutable snapshot. This keeps work proportional to the
    // cloned subgraph and prevents later iterations observing earlier clones.
    let source = cfg.clone();
    let original_values: u64 = original_blocks
        .iter()
        .map(|b| {
            u64::try_from(cfg.get_block(*b).params.len())
                .ok()
                .and_then(|params| {
                    params.checked_add(u64::try_from(cfg.get_block(*b).insts.len()).ok()?)
                })
                .unwrap_or(u64::MAX)
        })
        .try_fold(0u64, |a, b| a.checked_add(b))
        .unwrap_or(u64::MAX);
    let original_instructions: u64 = original_blocks
        .iter()
        .map(|b| u64::try_from(cfg.get_block(*b).insts.len()).unwrap_or(u64::MAX))
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
            let params = cfg.get_block(b).params.clone();
            for &(v, ty) in &params {
                let nv = cfg.add_block_param(block_map[&b], ty);
                map.insert(v, nv);
            }
        }
        let insts: Vec<(CfgValue, BlockId)> = original_blocks
            .iter()
            .flat_map(|&b| {
                source
                    .get_block(b)
                    .insts
                    .iter()
                    .copied()
                    .map(move |v| (v, b))
            })
            .collect();
        // Reserve every cloned instruction value before translating any
        // operands. Accessor splicing can leave a later-appended definition
        // with a larger numeric ID than its user; numeric sorting alone would
        // then silently fall back to the original value.
        for &(v, b) in &insts {
            let i = source.get_inst(v);
            let nv = cfg.append_inst(
                block_map[&b],
                CfgInst {
                    data: CfgInstData::Const(0),
                    ty: i.ty,
                    span: i.span,
                },
            );
            map.insert(v, nv);
        }
        let internal_values = original_blocks
            .iter()
            .flat_map(|block| {
                source
                    .get_block(*block)
                    .params
                    .iter()
                    .map(|(value, _)| *value)
                    .chain(source.get_block(*block).insts.iter().copied())
            })
            .collect::<AHashSet<_>>();
        let mut missing_mapping = false;
        for &(v, _) in &insts {
            super::dce::visit_instruction_uses(&source, v, |used| {
                missing_mapping |= internal_values.contains(&used) && !map.contains_key(&used);
            });
        }
        for &block in &original_blocks {
            super::dce::visit_terminator_uses(
                &source,
                &source.get_block(block).terminator,
                |used| {
                    missing_mapping |= internal_values.contains(&used) && !map.contains_key(&used);
                },
            );
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
            let i = source.get_inst(v);
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
            let data = remap_data(&source, cfg, &i.data, &map, Some((trip.slot, raw)))?;
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
            let source_term = &source.get_block(b).terminator;
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
                    Terminator::Branch { then_block, .. } if *then_block == body => source
                        .get_branch_then_args(source_term)
                        .iter()
                        .map(|v| map_value(map, *v))
                        .collect(),
                    Terminator::Branch { .. } => source
                        .get_branch_else_args(source_term)
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
                remap_term(&source, cfg, source_term, map, block_map)?
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
    source: &Cfg,
    cfg: &mut Cfg,
    t: &Terminator,
    map: &AHashMap<CfgValue, CfgValue>,
    blocks: &AHashMap<BlockId, BlockId>,
) -> Result<Terminator, CfgOptimizationError> {
    Ok(match t {
        Terminator::Goto { target, .. } => Terminator::Goto {
            target: blocks.get(target).copied().unwrap_or(*target),
            args: cfg.push_goto_args(source.get_goto_args(t).iter().map(|v| map_value(map, *v)))?,
        },
        Terminator::Branch {
            cond,
            then_block,
            else_block,
            ..
        } => Terminator::Branch {
            cond: map_value(map, *cond),
            then_block: blocks.get(then_block).copied().unwrap_or(*then_block),
            then_args: cfg.push_then_args(
                source
                    .get_branch_then_args(t)
                    .iter()
                    .map(|v| map_value(map, *v)),
            )?,
            else_block: blocks.get(else_block).copied().unwrap_or(*else_block),
            else_args: cfg.push_else_args(
                source
                    .get_branch_else_args(t)
                    .iter()
                    .map(|v| map_value(map, *v)),
            )?,
        },
        Terminator::Switch {
            scrutinee,
            cases: _,
            default,
        } => Terminator::Switch {
            scrutinee: map_value(map, *scrutinee),
            cases: cfg.push_switch_cases(
                source
                    .get_switch_cases(t)
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
        assert_eq!(O3_CODE_GROWTH_BUDGET.max_total_instructions, 256);
        let used = 12u64.checked_add(200).unwrap();
        assert!(used <= O3_CODE_GROWTH_BUDGET.max_total_instructions);
        assert!(used.checked_add(100).unwrap() > O3_CODE_GROWTH_BUDGET.max_total_instructions);
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
    fn run_refuses_growth_before_cloning() {
        let mut cfg = slot_loop(0, 100, false);
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(stats.budget_refusals, 1);
        assert_eq!(stats.values_cloned, 0);
    }

    #[test]
    fn instruction_budget_boundary_excludes_block_parameters() {
        // The fixture has ten body instructions and no block parameters:
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
    fn run_refuses_nested_loops_without_mutating_the_cfg() {
        let mut cfg = slot_loop(0, 2, false);
        let body = BlockId::from_raw(2);
        let outer_latch = BlockId::from_raw(3);
        let inner_header = cfg.new_block();
        let inner_latch = cfg.new_block();
        let span = Span::new(5, 6);
        let always = cfg.append_inst(
            inner_header,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span,
            },
        );
        cfg.set_goto(inner_latch, inner_header, []);
        cfg.get_block_mut(body).terminator = Terminator::None;
        cfg.set_goto(body, inner_header, []);
        cfg.set_branch(inner_header, always, inner_latch, [], outer_latch, []);
        cfg.verify().unwrap();
        let before = cfg.clone();
        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loops_unrolled, 0);
        assert_eq!(format!("{:?}", cfg), format!("{:?}", before));
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
