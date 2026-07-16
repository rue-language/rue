//! Constant-condition terminator simplification.
//!
//! Constant folding produces `BoolConst` branch conditions and `Const` switch
//! scrutinees — including fieldless enum comparisons like
//! `@target_arch() == Arch.X86_64`, whose fold exists specifically to enable
//! dead platform code elimination — but folding alone never changes control
//! flow: DCE's reachability walk treats every `Branch` and `Switch` as taking
//! all of its edges, so statically dead arms survive to codegen (RUE-910).
//!
//! This pass rewrites terminators whose outcome is statically known:
//!
//! * `Branch` on a `BoolConst` condition becomes a `Goto` to the taken block,
//!   carrying the taken edge's block arguments verbatim.
//! * `Switch` on a `Const`/`BoolConst` scrutinee becomes a `Goto` to the
//!   matching case (or the default when no case matches).
//!
//! Running before DCE, the rewrite makes the untaken edges disappear from
//! reachability, so DCE's existing unreachable-block elimination prunes the
//! dead arms and their instructions.
//!
//! ## Switch matching semantics
//!
//! Case selection must be exactly what the backends execute, or folding
//! changes behavior. Both backends materialize the case value as a full
//! 64-bit immediate and compare at 64-bit width when the scrutinee's type is
//! 64-bit, and at 32-bit width otherwise (RUE-27) — so this pass compares the
//! scrutinee's canonical constant the same way: full-width for 64-bit types,
//! low 32 bits for everything narrower.

use crate::{BlockId, Cfg, CfgInstData, Terminator};

/// Work counters for one run (RUE-794 convention): the pass visits each
/// block's terminator exactly once, so `blocks_scanned` is the block count
/// and the folded counts are bounded by it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Terminators inspected (one per block).
    pub blocks_scanned: u64,
    /// `Branch` terminators rewritten to `Goto`.
    pub branches_folded: u64,
    /// `Switch` terminators rewritten to `Goto`.
    pub switches_folded: u64,
}

/// Rewrite constant-condition `Branch`/`Switch` terminators into `Goto`s.
///
/// Terminators are rewritten in place (not via `Cfg::set_terminator`, whose
/// already-set assertion guards *construction*; replacing a real terminator
/// is exactly this pass's job). Instructions are never touched.
pub fn run(cfg: &mut Cfg) -> Stats {
    let mut stats = Stats::default();

    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        stats.blocks_scanned += 1;

        match cfg.get_block(block_id).terminator {
            Terminator::Branch {
                cond,
                then_block,
                then_args_start,
                then_args_len,
                else_block,
                else_args_start,
                else_args_len,
            } => {
                let CfgInstData::BoolConst(taken) = cfg.get_inst(cond).data else {
                    continue;
                };
                let (target, args_start, args_len) = if taken {
                    (then_block, then_args_start, then_args_len)
                } else {
                    (else_block, else_args_start, else_args_len)
                };
                cfg.get_block_mut(block_id).terminator = Terminator::Goto {
                    target,
                    args_start,
                    args_len,
                };
                stats.branches_folded += 1;
            }
            Terminator::Switch {
                scrutinee,
                cases_start,
                cases_len,
                default,
            } => {
                let inst = cfg.get_inst(scrutinee);
                let scrut_val = match inst.data {
                    CfgInstData::Const(v) => v,
                    // Bool match arms lower to cases 0/1 (see CfgBuilder's
                    // AirPattern::Bool handling).
                    CfgInstData::BoolConst(b) => b as u64,
                    _ => continue,
                };
                // Match at the width the backends compare at (see module
                // docs): 64-bit types compare all 64 bits of the canonical
                // constant, narrower types compare the low 32 bits.
                let wide = inst.ty.is_64_bit();
                let matches = |case: i64| {
                    if wide {
                        scrut_val == case as u64
                    } else {
                        scrut_val as u32 == case as u32
                    }
                };
                let target = cfg
                    .get_switch_cases(cases_start, cases_len)
                    .iter()
                    .find(|(case, _)| matches(*case))
                    .map(|(_, target)| *target)
                    .unwrap_or(default);
                // Switch edges carry no block arguments, so the Goto is
                // argument-free.
                cfg.get_block_mut(block_id).terminator = Terminator::Goto {
                    target,
                    args_start: 0,
                    args_len: 0,
                };
                stats.switches_folded += 1;
            }
            Terminator::Goto { .. }
            | Terminator::Return { .. }
            | Terminator::Unreachable
            | Terminator::None => {}
        }
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, CfgValue, Type};
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg
    }

    fn push(cfg: &mut Cfg, data: CfgInstData, ty: Type) -> CfgValue {
        let entry = cfg.entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data,
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn ret_const(cfg: &mut Cfg, block: BlockId, val: u64) {
        let value = cfg.add_inst(CfgInst {
            data: CfgInstData::Const(val),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        cfg.get_block_mut(block).insts.push(value);
        cfg.set_terminator(block, Terminator::Return { value: Some(value) });
    }

    #[test]
    fn test_branch_true_folds_to_then_edge_with_args() {
        let mut cfg = make_cfg();
        let cond = push(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let then_arg = push(&mut cfg, CfgInstData::Const(10), Type::I32);
        let else_arg = push(&mut cfg, CfgInstData::Const(20), Type::I32);
        let (then_args_start, then_args_len) = cfg.push_extra(vec![then_arg]);
        let (else_args_start, else_args_len) = cfg.push_extra(vec![else_arg]);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Branch {
                cond,
                then_block,
                then_args_start,
                then_args_len,
                else_block,
                else_args_start,
                else_args_len,
            },
        );
        ret_const(&mut cfg, then_block, 1);
        ret_const(&mut cfg, else_block, 2);

        let stats = run(&mut cfg);
        assert_eq!(stats.branches_folded, 1);
        match cfg.get_block(cfg.entry).terminator {
            Terminator::Goto { target, .. } => {
                assert_eq!(target, then_block);
                let args = cfg.get_goto_args(&cfg.get_block(cfg.entry).terminator);
                assert_eq!(args, &[then_arg], "taken edge's args preserved verbatim");
            }
            ref other => panic!("Expected Goto, got {:?}", other),
        }
    }

    #[test]
    fn test_branch_false_folds_to_else_edge() {
        let mut cfg = make_cfg();
        let cond = push(&mut cfg, CfgInstData::BoolConst(false), Type::BOOL);
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Branch {
                cond,
                then_block,
                then_args_start: 0,
                then_args_len: 0,
                else_block,
                else_args_start: 0,
                else_args_len: 0,
            },
        );
        ret_const(&mut cfg, then_block, 1);
        ret_const(&mut cfg, else_block, 2);

        let stats = run(&mut cfg);
        assert_eq!(stats.branches_folded, 1);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == else_block
        ));
    }

    #[test]
    fn test_non_const_branch_untouched() {
        let mut cfg = make_cfg();
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::BOOL);
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Branch {
                cond: x,
                then_block,
                then_args_start: 0,
                then_args_len: 0,
                else_block,
                else_args_start: 0,
                else_args_len: 0,
            },
        );
        ret_const(&mut cfg, then_block, 1);
        ret_const(&mut cfg, else_block, 2);

        let stats = run(&mut cfg);
        assert_eq!(stats.branches_folded, 0);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Branch { .. }
        ));
    }

    fn switch_cfg(scrut_val: u64, scrut_ty: Type, cases: Vec<i64>) -> (Cfg, Vec<BlockId>, BlockId) {
        let mut cfg = make_cfg();
        let scrutinee = push(&mut cfg, CfgInstData::Const(scrut_val), scrut_ty);
        let case_blocks: Vec<BlockId> = cases.iter().map(|_| cfg.new_block()).collect();
        let default = cfg.new_block();
        let (cases_start, cases_len) = cfg.push_switch_cases(
            cases
                .iter()
                .zip(&case_blocks)
                .map(|(v, b)| (*v, *b))
                .collect::<Vec<_>>(),
        );
        cfg.set_terminator(
            cfg.entry,
            Terminator::Switch {
                scrutinee,
                cases_start,
                cases_len,
                default,
            },
        );
        for (i, block) in case_blocks.iter().enumerate() {
            ret_const(&mut cfg, *block, i as u64);
        }
        ret_const(&mut cfg, default, 99);
        (cfg, case_blocks, default)
    }

    #[test]
    fn test_switch_const_folds_to_matching_case() {
        let (mut cfg, case_blocks, _default) = switch_cfg(7, Type::I32, vec![3, 7, 11]);
        let stats = run(&mut cfg);
        assert_eq!(stats.switches_folded, 1);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == case_blocks[1]
        ));
    }

    #[test]
    fn test_switch_no_match_folds_to_default() {
        let (mut cfg, _case_blocks, default) = switch_cfg(5, Type::I32, vec![3, 7]);
        run(&mut cfg);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == default
        ));
    }

    #[test]
    fn test_switch_negative_case_matches_signed_scrutinee() {
        // -1 as i32: canonical constant is sign-extended; the case stores
        // -1i64. Low-32 comparison must match (mirrors the backends' 32-bit
        // compare for sub-64-bit scrutinees).
        let (mut cfg, case_blocks, _default) = switch_cfg((-1i64) as u64, Type::I32, vec![-1, 0]);
        run(&mut cfg);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == case_blocks[0]
        ));
    }

    #[test]
    fn test_switch_64bit_scrutinee_compares_full_width() {
        // 0x1_0000_0001 as i64 vs case 1: a 32-bit compare would match on the
        // low word, but the backends compare 64-bit scrutinees at full width
        // (RUE-27), so the fold must go to default.
        let (mut cfg, _case_blocks, default) = switch_cfg(0x1_0000_0001, Type::I64, vec![1]);
        run(&mut cfg);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == default
        ));
    }

    #[test]
    fn test_switch_bool_scrutinee() {
        let mut cfg = make_cfg();
        let scrutinee = push(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
        let false_block = cfg.new_block();
        let true_block = cfg.new_block();
        let (cases_start, cases_len) =
            cfg.push_switch_cases(vec![(0, false_block), (1, true_block)]);
        // Exhaustive bool match: builder would pop the last case into the
        // default; keep both as cases here — the fold must pick case 1.
        let default = false_block;
        cfg.set_terminator(
            cfg.entry,
            Terminator::Switch {
                scrutinee,
                cases_start,
                cases_len,
                default,
            },
        );
        ret_const(&mut cfg, false_block, 0);
        ret_const(&mut cfg, true_block, 1);

        run(&mut cfg);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == true_block
        ));
    }

    #[test]
    fn test_work_is_one_scan() {
        // RUE-794 convention: the pass makes exactly one pass over the
        // blocks, independent of how many terminators fold.
        let mut cfg = make_cfg();
        let mut prev = cfg.entry;
        for _ in 0..50 {
            let cond = push(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
            let then_block = cfg.new_block();
            let else_block = cfg.new_block();
            cfg.set_terminator(
                prev,
                Terminator::Branch {
                    cond,
                    then_block,
                    then_args_start: 0,
                    then_args_len: 0,
                    else_block,
                    else_args_start: 0,
                    else_args_len: 0,
                },
            );
            ret_const(&mut cfg, else_block, 0);
            prev = then_block;
        }
        ret_const(&mut cfg, prev, 1);

        let stats = run(&mut cfg);
        assert_eq!(stats.blocks_scanned, cfg.block_count() as u64);
        assert_eq!(stats.branches_folded, 50);
    }

    /// End-to-end through the pass pipeline (constopt -> simplify -> dce):
    /// a branch whose condition is a propagated constant loses its dead arm
    /// entirely — the RUE-910 acceptance shape.
    #[test]
    fn test_dead_arm_eliminated_through_pipeline() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        // let flag = false; if flag { 1 } else { 2 }
        let init = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::BoolConst(false),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Alloc { slot: 0, init },
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        let cond = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Load { slot: 0 },
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        );
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block,
                then_args_start: 0,
                then_args_len: 0,
                else_block,
                else_args_start: 0,
                else_args_len: 0,
            },
        );
        ret_const(&mut cfg, then_block, 1);
        ret_const(&mut cfg, else_block, 2);

        super::super::constopt::run(&mut cfg);
        let stats = run(&mut cfg);
        super::super::dce::run(&mut cfg);

        assert_eq!(stats.branches_folded, 1);
        // The dead then-arm is fully eliminated: no instructions, an
        // Unreachable terminator (DCE's block-husk form).
        let dead = cfg.get_block(then_block);
        assert!(dead.insts.is_empty(), "dead arm instructions eliminated");
        assert!(matches!(dead.terminator, Terminator::Unreachable));
        // The live arm survives.
        assert!(matches!(
            cfg.get_block(else_block).terminator,
            Terminator::Return { .. }
        ));
    }
}
