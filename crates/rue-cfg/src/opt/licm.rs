//! Loop-invariant code motion (RUE-927) — ADR-0054 Phase 2.
//!
//! LICM hoists an operation that computes the same value on every iteration out
//! of the loop and into the loop's *preheader*, so it runs once per loop entry
//! instead of once per iteration. This pass implements the **trap-free-only**
//! variant ADR-0054 §2 specifies: it is the first `-O3`-only pass.
//!
//! ## The governing rule — never manufacture a trap
//!
//! Hoisting moves an op from "once per iteration" to "once in the preheader,
//! before the loop's entry test". If the loop runs **zero** iterations, a
//! hoisted op executes on a path the source never took. For a *trapping* op that
//! invents a trap out of thin air — the exact inverse of RUE-57, where DCE
//! *deleted* a mandatory trap. So **only [`classify::is_speculatable`] ops move**
//! (neither `may_trap` nor observable). Every trapping invariant op —
//! `Add`/`Sub`/`Mul`/`Div`/`Mod`/`Neg`, `IntCast`, an indexed `PlaceRead` — stays
//! in the loop body even when invariant. Guarded hoisting of trapping ops is
//! RUE-934, after loop rotation lands; there is no cleverness here.
//!
//! ## Invariance
//!
//! An instruction defined in a loop's body is loop-invariant when every operand
//! is either defined *outside* the body (so it dominates the preheader — see
//! below) or is itself an already-hoisted invariant op. This is iterated to a
//! fixpoint per loop. Values defined as header/body **block parameters** vary per
//! iteration, so they are never invariant; an instruction using one is therefore
//! not invariant either (its operand is defined inside the body and is not
//! itself hoisted).
//!
//! ### Memory reads — phase-2 conservatism
//!
//! A non-indexed `Load`/`PlaceRead` is speculatable per the classifier (it never
//! traps), but its *result* is only invariant if no store inside the loop can
//! change the memory it reads. Rue has no memory-versioning / alias analysis
//! today, so this phase is maximally conservative: a memory read is treated as
//! **non-invariant whenever the loop body contains any instruction with an
//! observable side effect** (`Call`, `Intrinsic`, `Store`, `ParamStore`,
//! `PlaceWrite`, `Alloc`, `Drop`) — everything
//! `classify::has_observable_side_effect` reports *except* the
//! `StorageLive`/`StorageDead` storage markers, which move no memory. When the
//! body is free of such effects, a memory read genuinely yields the same value
//! every iteration and may hoist. This is relaxable later with memory versioning
//! (RUE-914 territory); until then the whole-body effect gate is the safe rule.
//!
//! ## Why hoisting is verifier-clean
//!
//! Hoisting is a *move*: the instruction's `CfgValue` id is unchanged, so every
//! existing use stays valid — only its defining block changes. An operand defined
//! outside the loop and used inside must dominate the loop header (SSA), and the
//! preheader is the header's immediate non-loop dominator, so that operand also
//! dominates the preheader — it is available there. A value defined in the body
//! has all its uses dominated by the header (they are in the loop or reached only
//! through it), hence dominated by the preheader too, so relocating its
//! definition to the preheader keeps every use dominated. Hoisted ops are placed
//! in the preheader in discovery (dependency) order, so a hoisted operand always
//! precedes its hoisted user.
//!
//! ## Placement in the pipeline and the recompute rule
//!
//! LICM runs at `-O3` only, **after** the whole `-O1`/`-O2` sequence
//! (`constopt` → `peephole` → `simplify` → `forward` → `cse`), so it works on a
//! CFG whose constants are already folded and whose trivial control flow is
//! already threaded — the invariant operands it keys on are as exposed as the
//! earlier passes can make them — and **before** the final `dce`, which sweeps
//! anything the moves orphan. It recomputes the dominator tree and loop forest
//! from scratch (ADR-0054's recompute-per-pass rule) and, because a preheader
//! insertion mutates block structure, recomputes again after each loop it
//! changes rather than trusting a stale forest. Moving instructions between
//! blocks does not change CFG edges or dominators, but preheader materialization
//! does, so the conservative "recompute after any mutation" discipline is what
//! this pass follows. An **irreducible** forest makes the pass a no-op: the
//! analysis refuses to describe a multi-entry cycle, so there is nothing to hoist.
//!
//! Nested loops are processed **innermost first** (the forest's nesting orders
//! them by body size): an op invariant for an inner loop hoists to the inner
//! preheader, which sits in the outer loop's body, and a later sweep can hoist it
//! again to the outer preheader if it is invariant there too. An op that is not
//! invariant for the inner loop cannot be invariant for the outer one either (the
//! varying operand lives in the inner body, which is part of the outer body), so
//! innermost-first never misses a hoist.

use std::collections::HashSet;

use rue_air::FrozenTypeInternPool;

use super::CfgOptimizationError;
use super::classify;
use super::loops::{LoopId, NaturalLoop, ensure_preheader, loops};
use crate::dominators::DominatorTree;
use crate::{BlockId, Cfg, CfgInstData, CfgValue};

/// Bounded-work counters for one LICM run (RUE-794 discipline).
///
/// Every field is monotone and structurally bounded: each productive sweep moves
/// at least one instruction permanently out of a loop body, so the number of
/// sweeps — and hence `loops_analyzed` — is bounded by the total instruction
/// count times loop-nesting depth, and `invariants_hoisted` by the instruction
/// count.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Per-loop invariance analyses performed (one per loop visited in a sweep).
    pub loops_analyzed: u64,
    /// Instructions moved into a preheader across all loops.
    pub invariants_hoisted: u64,
    /// Dedicated preheader blocks materialized (an existing unconditional
    /// single-entry predecessor is reused instead of counted here).
    pub preheaders_materialized: u64,
}

/// Run loop-invariant code motion. Call at `-O3` only, after the `-O2` passes
/// and before DCE (see the module docs). Returns its work counters. The type
/// pool feeds preheader materialization's typed block-param payloads (RUE-840),
/// whose capacity failures propagate as the pass error.
pub fn run(
    cfg: &mut Cfg,
    type_pool: &FrozenTypeInternPool,
) -> Result<Stats, CfgOptimizationError> {
    let mut stats = Stats::default();

    // Recompute dominators + loops from scratch each sweep (ADR-0054). A sweep
    // hoists from at most one loop, then breaks to recompute, because a
    // preheader insertion mutates block structure and would invalidate the
    // forest we are iterating.
    loop {
        let dom = DominatorTree::compute(cfg);
        let forest = loops(cfg, &dom);
        // An irreducible forest carries no loops, so this is a natural no-op;
        // an empty (loop-free) forest likewise.
        if forest.is_irreducible() || forest.is_empty() {
            break;
        }

        // Innermost first: a smaller body is nested more deeply. Hoisting from
        // the innermost loop first lets its results bubble outward on later
        // sweeps.
        let mut order: Vec<LoopId> = (0..forest.len()).collect();
        order.sort_by_key(|&id| forest.get(id).body.len());

        let mut changed = false;
        for id in order {
            stats.loops_analyzed += 1;
            if hoist_loop(cfg, forest.get(id), type_pool, &mut stats)? {
                // Structure may have changed (a preheader was inserted); the
                // forest is now stale. Recompute before touching another loop.
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }

    Ok(stats)
}

/// Hoist every trap-free invariant instruction out of `lp` into its preheader.
/// Returns `true` if anything was hoisted (the caller must then recompute).
fn hoist_loop(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    type_pool: &FrozenTypeInternPool,
    stats: &mut Stats,
) -> Result<bool, CfgOptimizationError> {
    let def_block = compute_def_blocks(cfg);
    // Phase-2 conservatism: any memory read is non-invariant when the loop body
    // contains any observable-effect op other than the storage markers.
    let body_has_effect = body_has_memory_effect(cfg, lp);

    // Fixpoint: mark invariant instructions, appending them to `order` in
    // discovery order so a hoisted operand always precedes its hoisted user.
    let mut invariant: HashSet<CfgValue> = HashSet::new();
    let mut order: Vec<CfgValue> = Vec::new();
    loop {
        let mut added = false;
        for &block in &lp.body {
            // Clone the id list so the CFG stays borrowable during the operand
            // walk below.
            let insts = cfg.get_block(block).insts.clone();
            for value in insts {
                if invariant.contains(&value) {
                    continue;
                }
                if !is_hoist_candidate(cfg, value, body_has_effect) {
                    continue;
                }
                if operands_available(cfg, value, lp, &def_block, &invariant) {
                    invariant.insert(value);
                    order.push(value);
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }

    if order.is_empty() {
        return Ok(false);
    }

    // Materialize the preheader once, then relocate each invariant instruction
    // into it. `ensure_preheader` reuses a suitable existing predecessor when it
    // can, inserting a block only when it must.
    let before = cfg.block_count();
    let ph = ensure_preheader(cfg, lp, type_pool)?;
    if cfg.block_count() > before {
        stats.preheaders_materialized += 1;
    }

    let hoisted: HashSet<CfgValue> = order.iter().copied().collect();
    for &block in &lp.body {
        cfg.get_block_mut(block)
            .insts
            .retain(|v| !hoisted.contains(v));
    }
    for &value in &order {
        cfg.get_block_mut(ph).insts.push(value);
    }

    stats.invariants_hoisted += order.len() as u64;
    Ok(true)
}

/// Whether `value` is eligible to hoist on its own merits, independent of its
/// operands: speculatable (never traps, no observable effect), not a block
/// parameter, and — for a memory read — only when the loop body is effect-free.
fn is_hoist_candidate(cfg: &Cfg, value: CfgValue, body_has_effect: bool) -> bool {
    // Trapping or observable ops never move (ADR-0054 §2). This already excludes
    // arithmetic, IntCast, indexed PlaceRead, calls, stores, allocs, drops, and
    // the storage markers.
    if !classify::is_speculatable(cfg, value) {
        return false;
    }
    match cfg.get_inst(value).data {
        // Block parameters vary per iteration; they are not instructions in the
        // body's `insts` list, but guard defensively.
        CfgInstData::BlockParam { .. } => false,
        // Memory reads yield the same value each iteration only when nothing in
        // the loop writes memory (phase-2 conservatism; see the module docs).
        CfgInstData::PlaceRead { .. } | CfgInstData::Load { .. } => !body_has_effect,
        _ => true,
    }
}

/// Whether every operand of `value` is available at the preheader: defined
/// outside the loop body (hence dominating the preheader) or itself already
/// marked invariant (and so hoisted ahead of `value`).
fn operands_available(
    cfg: &Cfg,
    value: CfgValue,
    lp: &NaturalLoop,
    def_block: &[Option<BlockId>],
    invariant: &HashSet<CfgValue>,
) -> bool {
    let mut ok = true;
    super::dce::visit_instruction_uses(cfg, value, |operand| {
        match def_block[operand.as_u32() as usize] {
            // Defined inside the body: available only if it is itself invariant
            // (a header/body block param is defined in the body and never
            // invariant, so an op reading one fails here — as intended).
            Some(b) if lp.contains(b) => {
                if !invariant.contains(&operand) {
                    ok = false;
                }
            }
            // Defined outside the body (or a detached value with no block): it
            // dominates the header and therefore the preheader.
            _ => {}
        }
    });
    ok
}

/// Whether the loop body contains any observable-effect instruction other than
/// the `StorageLive`/`StorageDead` markers — the gate that makes memory reads
/// non-invariant this phase (ADR-0054 §3 conservatism).
fn body_has_memory_effect(cfg: &Cfg, lp: &NaturalLoop) -> bool {
    lp.body.iter().any(|&block| {
        cfg.get_block(block).insts.iter().any(|&value| {
            classify::has_observable_side_effect(cfg, value)
                && !matches!(
                    cfg.get_inst(value).data,
                    CfgInstData::StorageLive { .. } | CfgInstData::StorageDead { .. }
                )
        })
    })
}

/// Map each `CfgValue` to the block that defines it (as a block parameter or as
/// a block-attached instruction). Values with no defining block — detached or
/// dead arena entries — map to `None` and are treated as available (outside the
/// loop) by [`operands_available`].
fn compute_def_blocks(cfg: &Cfg) -> Vec<Option<BlockId>> {
    let mut def = vec![None; cfg.value_count()];
    for i in 0..cfg.block_count() {
        let block_id = BlockId::from_raw(i as u32);
        let block = cfg.get_block(block_id);
        for &(param, _) in &block.params {
            def[param.as_u32() as usize] = Some(block_id);
        }
        for &value in &block.insts {
            def[value.as_u32() as usize] = Some(block_id);
        }
    }
    def
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, Terminator, Type};
    use rue_span::Span;

    // Two local slots so the memory-read and indexed-place tests are
    // verifier-clean; loop-shape tests that use no slots ignore them.
    fn make_cfg() -> Cfg {
        Cfg::new(Type::UNIT, 2, 0, "test".to_string(), vec![])
    }

    fn test_type_pool() -> FrozenTypeInternPool {
        rue_air::TypeInternPool::new().freeze()
    }

    fn goto(target: BlockId) -> Terminator {
        Terminator::Goto {
            target,
            args: crate::payload::CfgGotoArgs::EMPTY,
        }
    }

    fn branch(cond: CfgValue, then_block: BlockId, else_block: BlockId) -> Terminator {
        Terminator::Branch {
            cond,
            then_block,
            then_args: crate::payload::CfgThenArgs::EMPTY,
            else_block,
            else_args: crate::payload::CfgElseArgs::EMPTY,
        }
    }

    fn push(cfg: &mut Cfg, block: BlockId, data: CfgInstData, ty: Type) -> CfgValue {
        cfg.add_inst_to_block(
            block,
            CfgInst {
                data,
                ty,
                span: Span::new(0, 0),
            },
        )
    }

    fn bool_const(cfg: &mut Cfg, block: BlockId) -> CfgValue {
        push(cfg, block, CfgInstData::BoolConst(true), Type::BOOL)
    }

    /// Which block currently holds `value` in its `insts` list.
    fn block_of(cfg: &Cfg, value: CfgValue) -> Option<BlockId> {
        (0..cfg.block_count())
            .map(|i| BlockId::from_raw(i as u32))
            .find(|&b| cfg.get_block(b).insts.contains(&value))
    }

    // A canonical single loop: entry(preheader, Goto header) -> header
    // -> body -> header (back edge); header -> exit. The two outside-defined
    // integers `a`, `b` live in the entry block.
    struct LoopShape {
        cfg: Cfg,
        entry: BlockId,
        body: BlockId,
        a: CfgValue,
        b: CfgValue,
    }

    fn single_loop() -> LoopShape {
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        // Two invariant operands, defined outside the loop. The loop condition
        // is also defined in the entry (a dominating block), NOT in the header,
        // so it is not itself a body instruction competing to be hoisted — the
        // body then contains exactly the op each test adds.
        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, goto(header));

        cfg.set_terminator(header, branch(cond, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        LoopShape {
            cfg,
            entry,
            body,
            a,
            b,
        }
    }

    #[test]
    fn hoists_pure_invariant_op() {
        // A bitwise op (speculatable, never traps) over two outside-defined
        // operands, computed in the loop body, must hoist to the preheader.
        let mut s = single_loop();
        let inv = push(&mut s.cfg, s.body, CfgInstData::BitOr(s.a, s.b), Type::I32);
        s.cfg.verify().unwrap();

        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1);
        // The op no longer lives in the body; it lives in the entry preheader
        // (reused, since entry is an unconditional Goto to the header).
        assert_eq!(block_of(&s.cfg, inv), Some(s.entry));
        assert!(!s.cfg.get_block(s.body).insts.contains(&inv));
        s.cfg.verify().unwrap();
    }

    #[test]
    fn refuses_trapping_invariant_add() {
        // Add of two outside-defined values is invariant but *may trap*
        // (overflow check). It must stay in the loop body: hoisting it into a
        // zero-trip preheader would manufacture an overflow trap the source
        // never runs. This is the core ADR-0054 §2 obligation.
        let mut s = single_loop();
        let trap = push(&mut s.cfg, s.body, CfgInstData::Add(s.a, s.b), Type::I32);
        s.cfg.verify().unwrap();

        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0, "a trapping Add must not hoist");
        assert_eq!(block_of(&s.cfg, trap), Some(s.body));
        s.cfg.verify().unwrap();
    }

    #[test]
    fn refuses_intcast_and_div() {
        // IntCast (range check) and Div (divide-by-zero / INT_MIN/-1 check) are
        // both may_trap and must never hoist, even though otherwise invariant.
        // Together with `refuses_trapping_invariant_add` these cover the
        // non-array trapping classes; the indexed-`PlaceRead` bounds-check path
        // is pinned by classify's tests and the differential trap-safety case.
        let mut s = single_loop();
        let cast = push(
            &mut s.cfg,
            s.body,
            CfgInstData::IntCast {
                value: s.a,
                from_ty: Type::I32,
            },
            Type::I8,
        );
        let div = push(&mut s.cfg, s.body, CfgInstData::Div(s.a, s.b), Type::I32);
        s.cfg.verify().unwrap();

        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(block_of(&s.cfg, cast), Some(s.body));
        assert_eq!(block_of(&s.cfg, div), Some(s.body));
    }

    #[test]
    fn refuses_op_depending_on_block_param() {
        // The header carries a block parameter (a loop-varying induction value).
        // A bitwise op reading it is NOT invariant and must not move; a sibling
        // op over only outside operands still hoists.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let init = push(&mut cfg, entry, CfgInstData::Const(0), Type::I32);
        // Loop condition defined in the dominating entry, not the header.
        let cond = bool_const(&mut cfg, entry);
        let hp = cfg.add_block_param(header, Type::I32);
        let args = cfg.push_goto_args([init]).unwrap();
        cfg.set_terminator(entry, Terminator::Goto { target: header, args });

        cfg.set_terminator(header, branch(cond, body, exit));

        // Depends on the header block param: varies, must NOT hoist.
        let varying = push(&mut cfg, body, CfgInstData::BitOr(hp, a), Type::I32);
        // Pure invariant sibling: must hoist.
        let invariant = push(&mut cfg, body, CfgInstData::BitXor(a, a), Type::I32);
        // Feed the block param back on the loop's back edge so it is well-formed.
        let args = cfg.push_goto_args([varying]).unwrap();
        cfg.set_terminator(body, Terminator::Goto { target: header, args });
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1, "only the invariant op hoists");
        assert_eq!(block_of(&cfg, varying), Some(body), "block-param op stays");
        assert_eq!(block_of(&cfg, invariant), Some(entry), "invariant op moved");
        cfg.verify().unwrap();
    }

    #[test]
    fn nested_loops_hoist_to_innermost_enclosing_preheader() {
        // Outer header o, inner header i nested inside. An op invariant for both
        // loops must end up hoisted all the way to the OUTER preheader; an op
        // that depends on the outer loop's block param stops at the inner
        // preheader (invariant for the inner loop only).
        // entry -> o -> i -> i_body -> i (inner back); i -> o_body -> o (outer
        // back); o -> exit.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let o = cfg.new_block();
        let i = cfg.new_block();
        let i_body = cfg.new_block();
        let o_body = cfg.new_block();
        let exit = cfg.new_block();

        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        let o_init = push(&mut cfg, entry, CfgInstData::Const(0), Type::I32);
        // Both loop conditions defined in the dominating entry, not the headers,
        // so the loop bodies contain only the test ops.
        let cond_o = bool_const(&mut cfg, entry);
        let cond_i = bool_const(&mut cfg, entry);
        let op = cfg.add_block_param(o, Type::I32); // outer induction param
        let args = cfg.push_goto_args([o_init]).unwrap();
        cfg.set_terminator(entry, Terminator::Goto { target: o, args });

        cfg.set_terminator(o, branch(cond_o, i, exit));
        cfg.set_terminator(i, branch(cond_i, i_body, o_body));

        // Fully invariant (a ^ b): hoists past the inner loop to the outer
        // preheader.
        let fully = push(&mut cfg, i_body, CfgInstData::BitXor(a, b), Type::I32);
        // Depends on the outer loop param: invariant for the INNER loop only, so
        // it lands in the inner preheader and stays there.
        let inner_only = push(&mut cfg, i_body, CfgInstData::BitOr(op, a), Type::I32);
        cfg.set_terminator(i_body, goto(i));

        let next_o = push(&mut cfg, o_body, CfgInstData::BitAnd(op, b), Type::I32);
        let args = cfg.push_goto_args([next_o]).unwrap();
        cfg.set_terminator(o_body, Terminator::Goto { target: o, args });
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        cfg.verify().unwrap();
        assert!(stats.invariants_hoisted >= 2);

        // `fully` must have left the inner body entirely and no longer live in
        // any loop body block (it reached an outer preheader).
        let fully_block = block_of(&cfg, fully).unwrap();
        assert!(
            fully_block != i_body && fully_block != o_body && fully_block != i && fully_block != o,
            "fully-invariant op should be hoisted out of every loop body, found in {fully_block}"
        );
        // `inner_only` must have left the inner body but still sit inside the
        // outer loop (it depends on the outer induction param).
        let inner_block = block_of(&cfg, inner_only).unwrap();
        assert!(
            inner_block != i_body,
            "inner-only invariant op should leave the inner body"
        );

        // Reachable-value invariant: every operand use is still dominated by its
        // definition (verify() enforces this).
    }

    #[test]
    fn irreducible_forest_is_a_no_op() {
        // A multi-entry cycle is irreducible; the analysis refuses it, so LICM
        // must hoist nothing even though `a ^ b` looks invariant.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = cfg.new_block();
        let y = cfg.new_block();

        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, x, y));
        // x <-> y two-entry cycle.
        let _inv = push(&mut cfg, x, CfgInstData::BitXor(a, b), Type::I32);
        cfg.set_terminator(x, goto(y));
        cfg.set_terminator(y, goto(x));

        let before = cfg.block_count();
        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(stats.preheaders_materialized, 0);
        assert_eq!(cfg.block_count(), before, "no preheader inserted");
    }

    #[test]
    fn memory_read_hoisted_only_when_body_effect_free() {
        // A non-indexed Load is speculatable. With no store/call in the loop it
        // is invariant and hoists; add an observable effect (a Store) to the
        // body and the same Load must stay put (phase-2 conservatism).
        let mut s = single_loop();
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        s.cfg.verify().unwrap();
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1, "effect-free body: Load hoists");
        assert_eq!(block_of(&s.cfg, load), Some(s.entry));

        // Now a body with a store: the Load must not move.
        let mut s = single_loop();
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::Store {
                slot: 1,
                value: s.a,
            },
            Type::UNIT,
        );
        s.cfg.verify().unwrap();
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0, "body has a store: Load stays");
        assert_eq!(block_of(&s.cfg, load), Some(s.body));
    }

    #[test]
    fn no_loops_is_a_no_op() {
        // A straight-line function has no loops; LICM must do nothing.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        let _c = push(&mut cfg, entry, CfgInstData::BitOr(a, b), Type::I32);
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.loops_analyzed, 0);
        assert_eq!(stats.invariants_hoisted, 0);
    }

    #[test]
    fn work_is_bounded() {
        // A single loop with one hoistable op: the pass converges in a bounded
        // number of sweeps. The productive sweep hoists the op and recomputes;
        // the next sweep finds nothing and stops. So loops_analyzed stays small
        // (one productive visit + one draining visit) and never spins.
        let mut s = single_loop();
        let _inv = push(&mut s.cfg, s.body, CfgInstData::BitOr(s.a, s.b), Type::I32);

        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1);
        // One loop, so each sweep analyzes at most one loop; convergence takes
        // two sweeps (hoist, then confirm-empty). Assert a tight bound so a
        // regression into a non-terminating rescan trips the test.
        assert!(
            stats.loops_analyzed <= 2,
            "loops_analyzed {} should be bounded (≤ 2 for one loop)",
            stats.loops_analyzed
        );
    }
}
