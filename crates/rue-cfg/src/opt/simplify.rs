//! CFG simplification: constant-condition terminator folding, empty-block
//! threading, and straight-line block merging.
//!
//! ## Terminator folding (RUE-910)
//!
//! Constant folding produces `BoolConst` branch conditions and `Const` switch
//! scrutinees — including fieldless enum comparisons like
//! `@target_arch() == Arch.X86_64`, whose fold exists specifically to enable
//! dead platform code elimination — but folding alone never changes control
//! flow: DCE's reachability walk treats every `Branch` and `Switch` as taking
//! all of its edges, so statically dead arms survive to codegen.
//!
//! * `Branch` on a `BoolConst` condition becomes a `Goto` to the taken block,
//!   carrying the taken edge's block arguments verbatim.
//! * `Switch` on a `Const`/`BoolConst` scrutinee becomes a `Goto` to the
//!   matching case (or the default when no case matches).
//!
//! Case selection must be exactly what the backends execute, or folding
//! changes behavior. Both backends materialize the case value as a full
//! 64-bit immediate and compare at 64-bit width when the scrutinee's type is
//! 64-bit, and at 32-bit width otherwise (RUE-27) — so this pass compares the
//! scrutinee's canonical constant the same way.
//!
//! ## Threading and merging (RUE-911)
//!
//! Folded terminators leave `Goto` chains and single-predecessor blocks
//! behind. Two structural cleanups run after folding, both restricted to
//! blocks reachable *after* folding so statically dead arms don't inhibit
//! them:
//!
//! * **Threading**: an edge into an empty, parameterless forwarding block
//!   (no instructions, terminator `Goto`) is retargeted at the forwarder's
//!   ultimate destination, carrying the final forwarder's block arguments.
//!   Any value those arguments reference dominates the forwarder, and
//!   nothing executes between a predecessor's terminator and the forwarder,
//!   so the values are equally available on the retargeted edge. `Switch`
//!   edges are not threaded: they cannot carry block arguments, so only an
//!   argument-free chain would qualify, and those are rare enough to leave
//!   for a later change.
//! * **Merging**: a block whose ONLY incoming edge is a `Goto` is absorbed
//!   into its predecessor: instructions are spliced onto the predecessor,
//!   the predecessor takes over the terminator, and each block parameter is
//!   substituted by the corresponding `Goto` argument. Substitutions from
//!   every merge in the pass are applied in one batched
//!   [`Cfg::rewrite_value_uses`] sweep, so the pass performs a single global
//!   use-rewrite regardless of how many blocks merge (RUE-794 work
//!   discipline). Malformed parameter substitution is the RUE-347 ill-typed
//!   edge class; `optimize()`'s post-pass verification checks every surviving
//!   edge's arity and types.
//!
//! Merged-away and threaded-around blocks become husks (no instructions,
//! `Unreachable` terminator) or drop out of reachability; DCE prunes them.
//! Husks are marked, not removed — renumbering dense `BlockId`s is the
//! separate compaction decision tracked by RUE-769.

use crate::{BlockId, Cfg, CfgInstData, CfgValue, Terminator};

use super::dce;

/// Work counters for one run (RUE-794 convention). Each phase makes one
/// bounded scan: folding visits each terminator once, threading resolves
/// each forwarder chain with memoization, merging consumes each block at
/// most once, and all parameter substitutions apply in one global
/// use-rewrite.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Terminators inspected by the folding phase (one per block).
    pub blocks_scanned: u64,
    /// `Branch` terminators rewritten to `Goto`.
    pub branches_folded: u64,
    /// `Switch` terminators rewritten to `Goto`.
    pub switches_folded: u64,
    /// Edges retargeted around empty forwarding blocks.
    pub edges_threaded: u64,
    /// Blocks absorbed into their unique predecessor.
    pub blocks_merged: u64,
}

/// Run all simplification phases: fold constant-condition terminators, then
/// thread empty forwarders, then merge single-predecessor `Goto` chains.
pub fn run(cfg: &mut Cfg) -> Result<Stats, crate::CfgEditError> {
    let mut stats = Stats::default();
    fold_terminators(cfg, &mut stats)?;
    thread_forwarders(cfg, &mut stats)?;
    merge_chains(cfg, &mut stats)?;
    Ok(stats)
}

/// Rewrite constant-condition `Branch`/`Switch` terminators into `Goto`s.
///
/// Terminators are rewritten in place (not via `Cfg::set_terminator`, whose
/// already-set assertion guards *construction*; replacing a real terminator
/// is exactly this pass's job). Instructions are never touched.
fn fold_terminators(cfg: &mut Cfg, stats: &mut Stats) -> Result<(), crate::CfgEditError> {
    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        stats.blocks_scanned += 1;

        match &cfg.get_block(block_id).terminator {
            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            } => {
                let CfgInstData::BoolConst(taken) = cfg.get_inst(*cond).data else {
                    continue;
                };
                let (target, values) = if taken {
                    (*then_block, cfg.then_args(then_args).to_vec())
                } else {
                    (*else_block, cfg.else_args(else_args).to_vec())
                };
                let args = cfg.push_goto_args(values)?;
                cfg.get_block_mut(block_id).terminator = Terminator::Goto { target, args };
                stats.branches_folded += 1;
            }
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            } => {
                let inst = cfg.get_inst(*scrutinee);
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
                    .switch_cases(cases)
                    .iter()
                    .find(|(case, _)| matches(*case))
                    .map(|(_, target)| *target)
                    .unwrap_or(*default);
                // Switch edges carry no block arguments, so the Goto is
                // argument-free.
                cfg.get_block_mut(block_id).terminator = Terminator::Goto {
                    target,
                    args: crate::payload::CfgGotoArgs::EMPTY,
                };
                stats.switches_folded += 1;
            }
            Terminator::Goto { .. }
            | Terminator::Return { .. }
            | Terminator::Unreachable
            | Terminator::None => {}
        }
    }
    Ok(())
}

/// A decoded `Goto` edge payload. Keeping values here prevents a range from
/// being detached from its owning CFG while the forwarding graph is solved.
#[derive(Clone, PartialEq, Eq)]
struct Edge {
    target: BlockId,
    args: Vec<CfgValue>,
}

/// Retarget edges that point at empty, parameterless forwarding blocks.
fn thread_forwarders(cfg: &mut Cfg, stats: &mut Stats) -> Result<(), crate::CfgEditError> {
    let reachable = dce::compute_reachable_blocks(cfg);

    // A forwarder holds no instructions and no parameters, so jumping to it
    // does nothing but jump again. The entry block is never treated as one:
    // it has no incoming edges to retarget and must remain the entry.
    let forward_edge: Vec<Option<Edge>> = (0..cfg.block_count())
        .map(|idx| {
            let block_id = BlockId::from_raw(idx as u32);
            if block_id == cfg.entry || !reachable.contains(idx as u32) {
                return None;
            }
            let block = cfg.get_block(block_id);
            if !block.insts.is_empty() || !block.params.is_empty() {
                return None;
            }
            match &block.terminator {
                Terminator::Goto { target, args } => Some(Edge {
                    target: *target,
                    args: cfg.goto_args(args).to_vec(),
                }),
                _ => None,
            }
        })
        .collect();

    // Resolve each forwarder to its ultimate destination with memoization,
    // leaving forwarder cycles (empty infinite loops) alone. Every block is
    // resolved at most once, so the whole phase is linear in blocks.
    let mut resolved: Vec<Option<Edge>> = vec![None; cfg.block_count()];
    let resolve = |resolved: &mut Vec<Option<Edge>>, start: BlockId| -> Option<Edge> {
        forward_edge[start.as_u32() as usize].as_ref()?;
        if let Some(edge) = &resolved[start.as_u32() as usize] {
            return Some(edge.clone());
        }
        // Walk the chain, tracking visited forwarders to stop on cycles.
        let mut chain = vec![start];
        let mut edge = forward_edge[start.as_u32() as usize]
            .as_ref()
            .unwrap()
            .clone();
        while let Some(next) = &forward_edge[edge.target.as_u32() as usize] {
            if chain.contains(&edge.target) {
                break; // forwarder cycle: an empty infinite loop, keep as-is
            }
            chain.push(edge.target);
            edge = next.clone();
        }
        for block in chain {
            resolved[block.as_u32() as usize] = Some(edge.clone());
        }
        Some(edge)
    };

    for block_idx in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(block_idx as u32);
        if !reachable.contains(block_idx as u32) {
            continue;
        }
        // Only Goto and Branch edges are threaded; Switch edges cannot carry
        // the forwarder's block arguments (see module docs).
        match &cfg.get_block(block_id).terminator {
            Terminator::Goto { target, .. } => {
                if *target == block_id {
                    continue;
                }
                if let Some(edge) = resolve(&mut resolved, *target) {
                    // An edge into a parameterless block carries no args, so
                    // the forwarder chain's final args replace them whole.
                    let args = cfg.push_goto_args(edge.args)?;
                    cfg.get_block_mut(block_id).terminator = Terminator::Goto {
                        target: edge.target,
                        args,
                    };
                    stats.edges_threaded += 1;
                }
            }
            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            } => {
                let cond = *cond;
                let thread_side =
                    |resolved: &mut Vec<Option<Edge>>, side: BlockId, stats: &mut Stats| {
                        match resolve(resolved, side) {
                            Some(edge) => {
                                stats.edges_threaded += 1;
                                (edge.target, Some(edge.args))
                            }
                            None => (side, None),
                        }
                    };
                let (new_then, forwarded_then) = thread_side(&mut resolved, *then_block, stats);
                let (new_else, forwarded_else) = thread_side(&mut resolved, *else_block, stats);
                if (new_then, new_else) != (*then_block, *else_block) {
                    let then_values =
                        forwarded_then.unwrap_or_else(|| cfg.then_args(then_args).to_vec());
                    let else_values =
                        forwarded_else.unwrap_or_else(|| cfg.else_args(else_args).to_vec());
                    let then_args = cfg.push_then_args(then_values)?;
                    let else_args = cfg.push_else_args(else_values)?;
                    cfg.get_block_mut(block_id).terminator = Terminator::Branch {
                        cond,
                        then_block: new_then,
                        then_args,
                        else_block: new_else,
                        else_args,
                    };
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Absorb blocks whose only incoming edge is a `Goto` into their
/// predecessor, substituting block parameters with the edge's arguments.
fn merge_chains(cfg: &mut Cfg, stats: &mut Stats) -> Result<(), crate::CfgEditError> {
    let reachable = dce::compute_reachable_blocks(cfg);

    // Count incoming EDGES (not predecessor blocks: a Branch with both sides
    // on one target contributes two) among reachable blocks only, so arms
    // already killed by terminator folding don't inhibit merging.
    let mut incoming = vec![0u32; cfg.block_count()];
    for block_idx in 0..cfg.block_count() {
        if !reachable.contains(block_idx as u32) {
            continue;
        }
        let block_id = BlockId::from_raw(block_idx as u32);
        match &cfg.get_block(block_id).terminator {
            Terminator::Goto { target, .. } => incoming[target.as_u32() as usize] += 1,
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                incoming[then_block.as_u32() as usize] += 1;
                incoming[else_block.as_u32() as usize] += 1;
            }
            Terminator::Switch { cases, default, .. } => {
                for (_, target) in cfg.switch_cases(cases) {
                    incoming[target.as_u32() as usize] += 1;
                }
                incoming[default.as_u32() as usize] += 1;
            }
            _ => {}
        }
    }

    // Parameter -> argument substitutions accumulated across every merge,
    // applied in ONE global use-rewrite afterwards.
    let mut subst: Vec<Option<CfgValue>> = vec![None; cfg.value_count()];
    let mut consumed = vec![false; cfg.block_count()];

    for block_idx in 0..cfg.block_count() {
        if !reachable.contains(block_idx as u32) || consumed[block_idx] {
            continue;
        }
        let head = BlockId::from_raw(block_idx as u32);

        // Absorb the whole downstream chain into `head`. Each absorbed block
        // is marked consumed, so total splice work is linear.
        loop {
            let Terminator::Goto { target, args } = &cfg.get_block(head).terminator else {
                break;
            };
            let target = *target;
            let target_idx = target.as_u32() as usize;
            if target == head
                || target == cfg.entry
                || consumed[target_idx]
                || incoming[target_idx] != 1
            {
                break;
            }

            // Record param -> arg substitutions for the edge being erased.
            let args: Vec<CfgValue> = cfg.goto_args(args).to_vec();
            let params = std::mem::take(&mut cfg.get_block_mut(target).params);
            assert_eq!(
                params.len(),
                args.len(),
                "edge arity must remain valid while merging blocks"
            );
            for ((param, _ty), arg) in params.into_iter().zip(args) {
                subst[param.as_u32() as usize] = Some(arg);
            }

            // Splice the target into the head and husk the target.
            let target_insts = std::mem::take(&mut cfg.get_block_mut(target).insts);
            let target_term = std::mem::replace(
                &mut cfg.get_block_mut(target).terminator,
                Terminator::Unreachable,
            );
            let head_block = cfg.get_block_mut(head);
            head_block.insts.extend(target_insts);
            head_block.terminator = target_term;

            consumed[target_idx] = true;
            stats.blocks_merged += 1;
        }
    }

    if stats.blocks_merged == 0 {
        return Ok(());
    }

    // Resolve substitution chains (a merged block's argument can itself be a
    // parameter that a later merge substituted) iteratively, then rewrite
    // every use once.
    let resolve = |subst: &[Option<CfgValue>], mut v: CfgValue| -> CfgValue {
        while let Some(next) = subst[v.as_u32() as usize] {
            v = next;
        }
        v
    };
    let resolved: Vec<CfgValue> = (0..cfg.value_count())
        .map(|i| resolve(&subst, CfgValue::from_raw(i as u32)))
        .collect();
    cfg.rewrite_value_uses(|v| resolved[v.as_u32() as usize])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, Type};
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

    fn push_in(cfg: &mut Cfg, block: BlockId, data: CfgInstData, ty: Type) -> CfgValue {
        cfg.add_inst_to_block(
            block,
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

    fn fold_only(cfg: &mut Cfg) -> Stats {
        let mut stats = Stats::default();
        fold_terminators(cfg, &mut stats).unwrap();
        stats
    }

    // ------------------------------------------------------------------
    // Terminator folding (RUE-910)
    // ------------------------------------------------------------------

    #[test]
    fn test_branch_true_folds_to_then_edge_with_args() {
        let mut cfg = make_cfg();
        let cond = push(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let then_param = cfg.add_block_param(then_block, Type::I32);
        let _else_param = cfg.add_block_param(else_block, Type::I32);
        let then_arg = push(&mut cfg, CfgInstData::Const(10), Type::I32);
        let else_arg = push(&mut cfg, CfgInstData::Const(20), Type::I32);
        let then_args = cfg.push_then_args(vec![then_arg]).unwrap();
        let else_args = cfg.push_else_args(vec![else_arg]).unwrap();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            },
        );
        cfg.set_terminator(
            then_block,
            Terminator::Return {
                value: Some(then_param),
            },
        );
        ret_const(&mut cfg, else_block, 2);

        let stats = fold_only(&mut cfg);
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
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        ret_const(&mut cfg, then_block, 1);
        ret_const(&mut cfg, else_block, 2);

        let stats = fold_only(&mut cfg);
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
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        ret_const(&mut cfg, then_block, 1);
        ret_const(&mut cfg, else_block, 2);

        let stats = fold_only(&mut cfg);
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
        let case_payload = cfg
            .push_switch_cases(
                cases
                    .iter()
                    .zip(&case_blocks)
                    .map(|(v, b)| (*v, *b))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Switch {
                scrutinee,
                cases: case_payload,
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
        let stats = fold_only(&mut cfg);
        assert_eq!(stats.switches_folded, 1);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == case_blocks[1]
        ));
    }

    #[test]
    fn test_switch_no_match_folds_to_default() {
        let (mut cfg, _case_blocks, default) = switch_cfg(5, Type::I32, vec![3, 7]);
        fold_only(&mut cfg);
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
        fold_only(&mut cfg);
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
        fold_only(&mut cfg);
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
        let cases = cfg
            .push_switch_cases(vec![(0, false_block), (1, true_block)])
            .unwrap();
        let default = false_block;
        cfg.set_terminator(
            cfg.entry,
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            },
        );
        ret_const(&mut cfg, false_block, 0);
        ret_const(&mut cfg, true_block, 1);

        fold_only(&mut cfg);
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Goto { target, .. } if target == true_block
        ));
    }

    #[test]
    fn test_fold_work_is_one_scan() {
        // RUE-794 convention: the folding phase makes exactly one pass over
        // the blocks, independent of how many terminators fold.
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
                    then_args: crate::payload::CfgThenArgs::EMPTY,
                    else_block,
                    else_args: crate::payload::CfgElseArgs::EMPTY,
                },
            );
            ret_const(&mut cfg, else_block, 0);
            prev = then_block;
        }
        ret_const(&mut cfg, prev, 1);

        let stats = fold_only(&mut cfg);
        assert_eq!(stats.blocks_scanned, cfg.block_count() as u64);
        assert_eq!(stats.branches_folded, 50);
    }

    // ------------------------------------------------------------------
    // Threading and merging (RUE-911)
    // ------------------------------------------------------------------

    /// entry -> f1 -> f2 -> tail, where f1/f2 are empty forwarders and f2's
    /// edge into tail carries an argument. Threading retargets entry
    /// directly at tail with that argument; merging then absorbs tail.
    #[test]
    fn test_threads_forwarder_chain_and_carries_final_args() {
        let mut cfg = make_cfg();
        let arg = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        let f1 = cfg.new_block();
        let f2 = cfg.new_block();
        let tail = cfg.new_block();
        let tail_param = cfg.add_block_param(tail, Type::I32);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Goto {
                target: f1,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.set_terminator(
            f1,
            Terminator::Goto {
                target: f2,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let args = cfg.push_goto_args(vec![arg]).unwrap();
        cfg.set_terminator(f2, Terminator::Goto { target: tail, args });
        cfg.set_terminator(
            tail,
            Terminator::Return {
                value: Some(tail_param),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert!(stats.edges_threaded >= 1);
        // After threading, entry pointed at tail with f2's arg; merging then
        // absorbed tail into entry, substituting the param with the arg.
        assert!(matches!(
            cfg.get_block(cfg.entry).terminator,
            Terminator::Return { value: Some(v) } if v == arg
        ));
    }

    #[test]
    fn test_forwarder_cycle_left_alone() {
        // Two empty blocks forwarding to each other: an empty infinite loop.
        // Threading must not "skip past" it and change semantics.
        let mut cfg = make_cfg();
        let a = cfg.new_block();
        let b = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Goto {
                target: a,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.set_terminator(
            a,
            Terminator::Goto {
                target: b,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.set_terminator(
            b,
            Terminator::Goto {
                target: a,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );

        run(&mut cfg).unwrap();
        // Control still enters the a<->b cycle; whichever block entry now
        // targets, it must be part of the cycle, and the cycle must persist.
        let Terminator::Goto { target, .. } = cfg.get_block(cfg.entry).terminator else {
            panic!("entry must still be a Goto");
        };
        assert!(target == a || target == b);
        let Terminator::Goto { target: next, .. } = cfg.get_block(target).terminator else {
            panic!("cycle block must still be a Goto");
        };
        assert!(next == a || next == b);
    }

    /// A goto chain of blocks that each carry an instruction merges into one
    /// straight-line block, with each hop's param substituted by its arg.
    #[test]
    fn test_merges_goto_chain_with_param_substitution() {
        let mut cfg = make_cfg();
        let start = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        let mut prev_out = start;
        let mut blocks = Vec::new();
        for _ in 0..3 {
            let block = cfg.new_block();
            let param = cfg.add_block_param(block, Type::I32);
            let out = push_in(&mut cfg, block, CfgInstData::Add(param, param), Type::I32);
            blocks.push((block, out));
            prev_out = out;
        }
        // Wire: entry -> b0(start), b0 -> b1(out0), b1 -> b2(out1), b2 returns out2.
        let mut source = cfg.entry;
        let mut carried = start;
        for &(block, out) in &blocks {
            let args = cfg.push_goto_args(vec![carried]).unwrap();
            cfg.set_terminator(
                source,
                Terminator::Goto {
                    target: block,
                    args,
                },
            );
            source = block;
            carried = out;
        }
        cfg.set_terminator(
            source,
            Terminator::Return {
                value: Some(prev_out),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.blocks_merged, 3);
        // Everything lives in entry now: 1 const + 3 adds, ending in Return.
        let entry = cfg.get_block(cfg.entry);
        assert_eq!(entry.insts.len(), 4);
        assert!(matches!(entry.terminator, Terminator::Return { .. }));
        // The adds now operate on substituted values: first add reads the
        // const directly (its param was substituted).
        let first_add = cfg.get_inst(entry.insts[1]);
        assert!(matches!(first_add.data, CfgInstData::Add(a, b) if a == start && b == start));
        // Merged blocks are husks.
        for &(block, _) in &blocks {
            let husk = cfg.get_block(block);
            assert!(husk.insts.is_empty());
            assert!(husk.params.is_empty());
            assert!(matches!(husk.terminator, Terminator::Unreachable));
        }
    }

    #[test]
    fn test_two_predecessor_block_not_merged() {
        // A join block with two live incoming edges must not be absorbed.
        let mut cfg = make_cfg();
        let cond = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::BOOL);
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let join = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Branch {
                cond,
                then_block,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        let t = push_in(&mut cfg, then_block, CfgInstData::Const(1), Type::I32);
        let _ = t;
        cfg.set_terminator(
            then_block,
            Terminator::Goto {
                target: join,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        let e = push_in(&mut cfg, else_block, CfgInstData::Const(2), Type::I32);
        let _ = e;
        cfg.set_terminator(
            else_block,
            Terminator::Goto {
                target: join,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        ret_const(&mut cfg, join, 3);

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.blocks_merged, 0);
        assert!(matches!(
            cfg.get_block(join).terminator,
            Terminator::Return { .. }
        ));
    }

    #[test]
    fn test_loop_back_edge_not_merged() {
        // entry -> header; header branches to body or exit; body -> header.
        // The header has two incoming edges (entry + back edge), so nothing
        // merges into it and the loop survives intact.
        let mut cfg = make_cfg();
        let cond = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::BOOL);
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        cfg.set_terminator(
            cfg.entry,
            Terminator::Goto {
                target: header,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        cfg.set_terminator(
            header,
            Terminator::Branch {
                cond,
                then_block: body,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block: exit,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        let work = push_in(&mut cfg, body, CfgInstData::Const(1), Type::I32);
        let _ = work;
        cfg.set_terminator(
            body,
            Terminator::Goto {
                target: header,
                args: crate::payload::CfgGotoArgs::EMPTY,
            },
        );
        ret_const(&mut cfg, exit, 0);

        let stats = run(&mut cfg).unwrap();
        // body merges into nothing (header has 2 incoming edges); body itself
        // has 1 incoming edge and CAN be absorbed into header's then-side?
        // No: absorption requires the predecessor to end in a Goto, and
        // header ends in a Branch. The loop shape is untouched.
        assert_eq!(stats.blocks_merged, 0);
        assert!(matches!(
            cfg.get_block(header).terminator,
            Terminator::Branch { .. }
        ));
        assert!(matches!(
            cfg.get_block(body).terminator,
            Terminator::Goto { target, .. } if target == header
        ));
    }

    #[test]
    fn test_merge_work_is_bounded() {
        // A 100-block goto chain merges in one pass: every block is consumed
        // exactly once and blocks_merged equals the chain length.
        let mut cfg = make_cfg();
        let mut source = cfg.entry;
        let mut blocks = Vec::new();
        for _ in 0..100 {
            let block = cfg.new_block();
            push_in(&mut cfg, block, CfgInstData::Const(7), Type::I32);
            cfg.set_terminator(
                source,
                Terminator::Goto {
                    target: block,
                    args: crate::payload::CfgGotoArgs::EMPTY,
                },
            );
            blocks.push(block);
            source = block;
        }
        ret_const(&mut cfg, source, 0);

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.blocks_merged, 100);
        assert_eq!(cfg.get_block(cfg.entry).insts.len(), 101);
    }

    /// End-to-end through the pass pipeline (constopt -> simplify -> dce):
    /// a diamond over a propagated-constant condition collapses to a single
    /// straight-line block — the RUE-910 + RUE-911 acceptance shape.
    #[test]
    fn test_diamond_collapses_to_straight_line() {
        let mut cfg = Cfg::new(Type::I32, 1, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        // let flag = false; if flag { 1 } else { 2 } (join carries the value)
        let init = push_in(&mut cfg, entry, CfgInstData::BoolConst(false), Type::BOOL);
        push_in(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        let cond = push_in(&mut cfg, entry, CfgInstData::Load { slot: 0 }, Type::BOOL);
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let join = cfg.new_block();
        let join_param = cfg.add_block_param(join, Type::I32);
        cfg.set_terminator(
            entry,
            Terminator::Branch {
                cond,
                then_block,
                then_args: crate::payload::CfgThenArgs::EMPTY,
                else_block,
                else_args: crate::payload::CfgElseArgs::EMPTY,
            },
        );
        let t = push_in(&mut cfg, then_block, CfgInstData::Const(1), Type::I32);
        let t_args = cfg.push_goto_args(vec![t]).unwrap();
        cfg.set_terminator(
            then_block,
            Terminator::Goto {
                target: join,
                args: t_args,
            },
        );
        let e = push_in(&mut cfg, else_block, CfgInstData::Const(2), Type::I32);
        let e_args = cfg.push_goto_args(vec![e]).unwrap();
        cfg.set_terminator(
            else_block,
            Terminator::Goto {
                target: join,
                args: e_args,
            },
        );
        cfg.set_terminator(
            join,
            Terminator::Return {
                value: Some(join_param),
            },
        );

        super::super::constopt::run(&mut cfg);
        let stats = run(&mut cfg).unwrap();
        super::super::dce::run(&mut cfg);

        assert_eq!(stats.branches_folded, 1);
        // else (the taken arm) and join both merged into entry, and the join
        // param was substituted by the else arm's value.
        assert_eq!(stats.blocks_merged, 2);
        assert!(
            matches!(
                cfg.get_block(cfg.entry).terminator,
                Terminator::Return { value: Some(v) } if v == e
            ),
            "entry must return the else-arm value directly"
        );
        // The dead then-arm is a husk.
        let dead = cfg.get_block(then_block);
        assert!(dead.insts.is_empty());
        assert!(matches!(dead.terminator, Terminator::Unreachable));
    }
}
