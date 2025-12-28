//! Dead code elimination optimization pass.
//!
//! This pass removes:
//! - Unused instructions (values that are never used)
//! - Unreachable blocks (blocks with no predecessors, except entry)
//!
//! ## Algorithm
//!
//! 1. Mark all side-effecting instructions as live (calls, stores, intrinsics, drops)
//! 2. Mark all values used by terminators as live
//! 3. Transitively mark all values used by live instructions
//! 4. Remove dead instructions from basic blocks
//! 5. Remove unreachable blocks
//!
//! ## What counts as a side effect
//!
//! - Function calls (may have arbitrary effects)
//! - Intrinsic calls (e.g., @dbg)
//! - Store instructions (write to memory)
//! - Alloc instructions (initialize memory)
//! - FieldSet, IndexSet (write to memory)
//! - Drop instructions (run destructors)
//! - StorageLive, StorageDead (affect stack allocation)

use std::collections::HashSet;

use crate::{BlockId, Cfg, CfgInstData, CfgValue, Terminator};

/// Run dead code elimination on the CFG.
///
/// This marks live values and removes dead instructions from blocks.
/// It also removes unreachable blocks.
pub fn run(cfg: &mut Cfg) {
    // Phase 1: Compute liveness
    let live = compute_live_values(cfg);

    // Phase 2: Remove dead instructions from blocks
    eliminate_dead_instructions(cfg, &live);

    // Phase 3: Remove unreachable blocks
    eliminate_unreachable_blocks(cfg);
}

/// Compute the set of live values in the CFG.
///
/// A value is live if:
/// - It has side effects, or
/// - It's used by a terminator, or
/// - It's used by another live value
fn compute_live_values(cfg: &Cfg) -> HashSet<CfgValue> {
    let mut live = HashSet::new();
    let mut worklist = Vec::new();

    // Pass 1: Mark all side-effecting instructions as live
    for i in 0..cfg.value_count() {
        let value = CfgValue::from_raw(i as u32);
        if has_side_effects(cfg, value) {
            if live.insert(value) {
                worklist.push(value);
            }
        }
    }

    // Pass 2: Mark all values used by terminators as live
    for block in cfg.blocks() {
        for value in terminator_uses(&block.terminator) {
            if live.insert(value) {
                worklist.push(value);
            }
        }
        // Block parameters are also live if the block is reachable
        for (param_val, _) in &block.params {
            if live.insert(*param_val) {
                worklist.push(*param_val);
            }
        }
    }

    // Pass 3: Transitively mark all values used by live instructions
    while let Some(value) = worklist.pop() {
        for used_value in instruction_uses(cfg, value) {
            if live.insert(used_value) {
                worklist.push(used_value);
            }
        }
    }

    live
}

/// Check if an instruction has side effects.
fn has_side_effects(cfg: &Cfg, value: CfgValue) -> bool {
    match &cfg.get_inst(value).data {
        // Function calls can have arbitrary effects
        CfgInstData::Call { .. } => true,

        // Intrinsics (like @dbg) have effects
        CfgInstData::Intrinsic { .. } => true,

        // Memory writes
        CfgInstData::Alloc { .. } => true,
        CfgInstData::Store { .. } => true,
        CfgInstData::ParamStore { .. } => true,
        CfgInstData::FieldSet { .. } => true,
        CfgInstData::ParamFieldSet { .. } => true,
        CfgInstData::IndexSet { .. } => true,
        CfgInstData::ParamIndexSet { .. } => true,

        // Drop runs destructors
        CfgInstData::Drop { .. } => true,

        // Storage liveness affects stack allocation
        CfgInstData::StorageLive { .. } => true,
        CfgInstData::StorageDead { .. } => true,

        // IntCast can panic (range check), so it has side effects
        CfgInstData::IntCast { .. } => true,

        // Everything else is pure computation
        _ => false,
    }
}

/// Get values used by a terminator.
fn terminator_uses(term: &Terminator) -> Vec<CfgValue> {
    match term {
        Terminator::Goto { args, .. } => args.clone(),
        Terminator::Branch {
            cond,
            then_args,
            else_args,
            ..
        } => {
            let mut uses = vec![*cond];
            uses.extend(then_args);
            uses.extend(else_args);
            uses
        }
        Terminator::Switch { scrutinee, .. } => vec![*scrutinee],
        Terminator::Return { value } => value.into_iter().copied().collect(),
        Terminator::Unreachable | Terminator::None => vec![],
    }
}

/// Get values used by an instruction.
fn instruction_uses(cfg: &Cfg, value: CfgValue) -> Vec<CfgValue> {
    match &cfg.get_inst(value).data {
        // Constants and parameters have no uses
        CfgInstData::Const(_)
        | CfgInstData::BoolConst(_)
        | CfgInstData::StringConst(_)
        | CfgInstData::Param { .. }
        | CfgInstData::BlockParam { .. } => vec![],

        // Binary operations
        CfgInstData::Add(lhs, rhs)
        | CfgInstData::Sub(lhs, rhs)
        | CfgInstData::Mul(lhs, rhs)
        | CfgInstData::Div(lhs, rhs)
        | CfgInstData::Mod(lhs, rhs)
        | CfgInstData::Eq(lhs, rhs)
        | CfgInstData::Ne(lhs, rhs)
        | CfgInstData::Lt(lhs, rhs)
        | CfgInstData::Gt(lhs, rhs)
        | CfgInstData::Le(lhs, rhs)
        | CfgInstData::Ge(lhs, rhs)
        | CfgInstData::BitAnd(lhs, rhs)
        | CfgInstData::BitOr(lhs, rhs)
        | CfgInstData::BitXor(lhs, rhs)
        | CfgInstData::Shl(lhs, rhs)
        | CfgInstData::Shr(lhs, rhs) => vec![*lhs, *rhs],

        // Unary operations
        CfgInstData::Neg(v) | CfgInstData::Not(v) | CfgInstData::BitNot(v) => vec![*v],

        // Variable operations
        CfgInstData::Alloc { init, .. } => vec![*init],
        CfgInstData::Load { .. } => vec![],
        CfgInstData::Store { value, .. } => vec![*value],
        CfgInstData::ParamStore { value, .. } => vec![*value],

        // Function calls
        CfgInstData::Call { args, .. } => args.iter().map(|a| a.value).collect(),
        CfgInstData::Intrinsic { args, .. } => args.clone(),

        // Struct operations
        CfgInstData::StructInit { fields, .. } => fields.clone(),
        CfgInstData::FieldGet { base, .. } => vec![*base],
        CfgInstData::FieldSet { value, .. } => vec![*value],
        CfgInstData::ParamFieldSet { value, .. } => vec![*value],

        // Array operations
        CfgInstData::ArrayInit { elements, .. } => elements.clone(),
        CfgInstData::IndexGet { base, index, .. } => vec![*base, *index],
        CfgInstData::IndexSet { index, value, .. } => vec![*index, *value],
        CfgInstData::ParamIndexSet { index, value, .. } => vec![*index, *value],

        // Enum operations
        CfgInstData::EnumVariant { .. } => vec![],

        // Type conversion
        CfgInstData::IntCast { value, .. } => vec![*value],

        // Drop
        CfgInstData::Drop { value } => vec![*value],

        // Storage liveness
        CfgInstData::StorageLive { .. } | CfgInstData::StorageDead { .. } => vec![],
    }
}

/// Remove dead instructions from basic blocks.
///
/// This filters the instruction list of each block to only include live
/// instructions. Dead instructions are removed from the block's instruction
/// list but remain in the CFG's value pool (as they may still be referenced
/// by live instructions through their operands).
fn eliminate_dead_instructions(cfg: &mut Cfg, live: &HashSet<CfgValue>) {
    // Collect block IDs to avoid borrow issues
    let block_ids: Vec<BlockId> = cfg.block_ids().collect();

    for block_id in block_ids {
        let block = cfg.get_block_mut(block_id);
        // Retain only live instructions in this block
        block.insts.retain(|value| live.contains(value));
    }
}

/// Remove unreachable blocks (blocks with no predecessors except entry).
fn eliminate_unreachable_blocks(cfg: &mut Cfg) {
    // First, compute which blocks are reachable from entry
    let reachable = compute_reachable_blocks(cfg);

    // Collect the block IDs to process (to avoid borrowing issues)
    let block_ids: Vec<BlockId> = cfg.block_ids().collect();

    // For now, we don't actually remove blocks from the vector
    // (that would require renumbering all BlockIds).
    // Instead, we mark unreachable blocks with Unreachable terminator
    // and empty their instruction lists.
    for block_id in block_ids {
        if block_id != cfg.entry && !reachable.contains(&block_id) {
            let block = cfg.get_block_mut(block_id);
            block.insts.clear();
            block.terminator = Terminator::Unreachable;
        }
    }
}

/// Compute the set of blocks reachable from the entry block.
fn compute_reachable_blocks(cfg: &Cfg) -> HashSet<BlockId> {
    let mut reachable = HashSet::new();
    let mut worklist = vec![cfg.entry];

    while let Some(block_id) = worklist.pop() {
        if !reachable.insert(block_id) {
            continue;
        }

        let block = cfg.get_block(block_id);
        match &block.terminator {
            Terminator::Goto { target, .. } => {
                worklist.push(*target);
            }
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                worklist.push(*then_block);
                worklist.push(*else_block);
            }
            Terminator::Switch { cases, default, .. } => {
                for (_, target) in cases {
                    worklist.push(*target);
                }
                worklist.push(*default);
            }
            Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
        }
    }

    reachable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, CfgInstData};
    use rue_air::Type;
    use rue_intern::Interner;
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg
    }

    fn add_const(cfg: &mut Cfg, val: u64, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(val),
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn add_add(cfg: &mut Cfg, lhs: CfgValue, rhs: CfgValue, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Add(lhs, rhs),
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn finalize_cfg(cfg: &mut Cfg, ret_val: CfgValue) {
        let entry = cfg.entry;
        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(ret_val),
            },
        );
    }

    #[test]
    fn test_dce_removes_unused_instructions() {
        let mut cfg = make_cfg();
        let entry = cfg.entry;

        // Create: c1 = 10, c2 = 20, add = c1 + c2, c3 = 42, return c3
        // c1, c2, and add should be removed since they're unused
        let _c1 = add_const(&mut cfg, 10, Type::I32);
        let _c2 = add_const(&mut cfg, 20, Type::I32);
        let _add = add_add(&mut cfg, _c1, _c2, Type::I32);
        let c3 = add_const(&mut cfg, 42, Type::I32);
        finalize_cfg(&mut cfg, c3);

        // Before DCE: block has 4 instructions
        assert_eq!(cfg.get_block(entry).insts.len(), 4);

        run(&mut cfg);

        // After DCE: block should have only 1 instruction (c3)
        let block = cfg.get_block(entry);
        assert_eq!(
            block.insts.len(),
            1,
            "Expected 1 instruction, got {}",
            block.insts.len()
        );
        assert_eq!(block.insts[0], c3, "Expected c3 to be the only instruction");

        // Verify c3 still has the correct value
        match &cfg.get_inst(c3).data {
            CfgInstData::Const(42) => {}
            other => panic!("Expected Const(42), got {:?}", other),
        }
    }

    #[test]
    fn test_dce_preserves_used_instructions() {
        let mut cfg = make_cfg();
        let entry = cfg.entry;

        // Create: c1 = 10, c2 = 20, add = c1 + c2, return add
        // All should be preserved since they're used
        let c1 = add_const(&mut cfg, 10, Type::I32);
        let c2 = add_const(&mut cfg, 20, Type::I32);
        let add = add_add(&mut cfg, c1, c2, Type::I32);
        finalize_cfg(&mut cfg, add);

        run(&mut cfg);

        // All 3 instructions should still be in the block
        let block = cfg.get_block(entry);
        assert_eq!(
            block.insts.len(),
            3,
            "Expected 3 instructions, got {}",
            block.insts.len()
        );

        // Verify the instructions are preserved with correct data
        match &cfg.get_inst(c1).data {
            CfgInstData::Const(10) => {}
            other => panic!("Expected Const(10), got {:?}", other),
        }
        match &cfg.get_inst(c2).data {
            CfgInstData::Const(20) => {}
            other => panic!("Expected Const(20), got {:?}", other),
        }
        match &cfg.get_inst(add).data {
            CfgInstData::Add(_, _) => {}
            other => panic!("Expected Add, got {:?}", other),
        }
    }

    #[test]
    fn test_dce_unreachable_block() {
        let mut cfg = make_cfg();
        let entry = cfg.entry;

        // Add a constant and return in entry
        let c1 = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(c1) });

        // Add an unreachable block
        let unreachable_block = cfg.new_block();
        let c2 = cfg.add_inst_to_block(
            unreachable_block,
            CfgInst {
                data: CfgInstData::Const(100),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(unreachable_block, Terminator::Return { value: Some(c2) });

        run(&mut cfg);

        // Unreachable block should have Unreachable terminator and no instructions
        let block = cfg.get_block(unreachable_block);
        assert!(
            block.insts.is_empty(),
            "Unreachable block should have no instructions"
        );
        assert!(
            matches!(block.terminator, Terminator::Unreachable),
            "Unreachable block should have Unreachable terminator"
        );
    }

    #[test]
    fn test_dce_preserves_side_effects() {
        let mut cfg = make_cfg();
        let entry = cfg.entry;

        // Create a call (side effect) that's not used by return
        let interner = Interner::new();
        let side_effect_sym = interner.intern("side_effect");
        let call = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    name: side_effect_sym,
                    args: vec![],
                },
                ty: Type::Unit,
                span: Span::new(0, 0),
            },
        );

        let ret_val = add_const(&mut cfg, 0, Type::I32);
        finalize_cfg(&mut cfg, ret_val);

        run(&mut cfg);

        // Block should still have 2 instructions (call and ret_val)
        let block = cfg.get_block(entry);
        assert_eq!(
            block.insts.len(),
            2,
            "Expected 2 instructions, got {}",
            block.insts.len()
        );

        // Call should be preserved (side effect)
        match &cfg.get_inst(call).data {
            CfgInstData::Call { name, .. } if *name == side_effect_sym => {}
            other => panic!("Expected Call, got {:?}", other),
        }
    }
}
