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
//! itself hoisted). Discovery builds the candidate def-use graph once and
//! propagates availability through a worklist, so dependency order in the CFG's
//! block numbering cannot turn the analysis into repeated full-body scans.
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
//! from scratch (ADR-0054's recompute-per-pass rule). Moving instructions
//! between existing blocks does not change CFG edges, dominators, or loop
//! membership, so the same forest remains authoritative. Creating a dedicated
//! preheader does change edges; only then does LICM stop consuming the current
//! forest and recompute before continuing. An **irreducible** forest makes the
//! pass a no-op: the analysis refuses to describe a multi-entry cycle, so there
//! is nothing to hoist.
//!
//! Nested loops are processed **innermost first** (the forest's nesting orders
//! them by body size): an op invariant for an inner loop hoists to the inner
//! preheader, which sits in the outer loop's body, and a later sweep can hoist it
//! again to the outer preheader if it is invariant there too. An op that is not
//! invariant for the inner loop cannot be invariant for the outer one either (the
//! varying operand lives in the inner body, which is part of the outer body), so
//! innermost-first never misses a hoist.

use std::collections::VecDeque;

use rue_air::FrozenTypeInternPool;

use super::CfgOptimizationError;
use super::classify;
use super::loops::{LoopId, NaturalLoop, ensure_preheader, loops};
use crate::dominators::DominatorTree;
use crate::{BlockId, Cfg, CfgInstData, CfgValue, Terminator};

/// Bounded-work counters for one LICM run (RUE-794 discipline).
///
/// Every field is monotone and structurally bounded. One forest computation
/// visits each loop until a dedicated preheader changes CFG edges. Within a loop,
/// each instruction is classified once, each candidate dependency is recorded
/// once, and each discovered invariant leaves the worklist once.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Dominator-tree and loop-forest computations, including the initial one.
    pub forest_computations: u64,
    /// Per-loop invariance analyses performed (one per loop visited in a sweep).
    pub loops_analyzed: u64,
    /// Loop-body instructions tested for hoist eligibility.
    pub instructions_examined: u64,
    /// Def-use edges between hoist candidates in the same loop body.
    pub candidate_dependencies: u64,
    /// Invariant candidates removed from the discovery worklist.
    pub worklist_pops: u64,
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
pub fn run(cfg: &mut Cfg, type_pool: &FrozenTypeInternPool) -> Result<Stats, CfgOptimizationError> {
    // Every directed cycle contains an edge whose target is no later than its
    // source in the total block-id order. Proving that no such edge exists is
    // therefore enough to skip both dominator construction and loop discovery.
    // This summary is computed from the post-O2 CFG itself, so earlier edge
    // rewrites cannot leave behind a falsely cleared bit. A non-forward edge in
    // an acyclic graph is only a conservative false positive: LICM runs as
    // before.
    if !may_have_cycle_by_block_order(cfg) {
        return Ok(Stats::default());
    }

    let mut stats = Stats::default();

    // Recompute dominators + loops from scratch after each actual CFG-edge
    // change (ADR-0054). Instruction-only motion leaves the forest valid, so
    // one computation can serve every loop in that sweep.
    loop {
        stats.forest_computations += 1;
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

        let mut cfg_changed = false;
        for id in order {
            stats.loops_analyzed += 1;
            if hoist_loop(cfg, forest.get(id), type_pool, &mut stats)?.cfg_changed {
                // A dedicated preheader changed edges, so the forest is stale.
                cfg_changed = true;
                break;
            }
        }
        if !cfg_changed {
            break;
        }
    }

    Ok(stats)
}

fn may_have_cycle_by_block_order(cfg: &Cfg) -> bool {
    for raw in 0..cfg.block_count() {
        let block = BlockId::from_raw(raw as u32);
        let observe = |target: BlockId| target.as_u32() <= block.as_u32();
        let may_cycle = match &cfg.get_block(block).terminator {
            Terminator::Goto { target, .. } => observe(*target),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => observe(*then_block) || observe(*else_block),
            Terminator::Switch { cases, default, .. } => {
                cfg.switch_cases(cases)
                    .iter()
                    .any(|(_, target)| observe(*target))
                    || observe(*default)
            }
            Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => false,
        };
        if may_cycle {
            return true;
        }
    }
    false
}

/// Hoist every trap-free invariant instruction out of `lp` into its preheader.
/// Reports whether materializing the destination changed CFG edges.
#[derive(Default)]
struct HoistResult {
    cfg_changed: bool,
}

fn hoist_loop(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    type_pool: &FrozenTypeInternPool,
    stats: &mut Stats,
) -> Result<HoistResult, CfgOptimizationError> {
    let def_block = compute_def_blocks(cfg);
    // Phase-2 conservatism: any memory read is non-invariant when the loop body
    // contains any observable-effect op other than the storage markers.
    let body_has_effect = body_has_memory_effect(cfg, lp);

    // Classify each body instruction once. The classifier remains the sole
    // authority for trap/effect eligibility; the worklist below only answers
    // whether eligible operands become available at the preheader.
    let value_count = cfg.value_count();
    let mut in_loop = vec![false; cfg.block_count()];
    let mut candidate = vec![false; value_count];
    let mut candidate_values = Vec::new();
    for &block in &lp.body {
        in_loop[block.as_u32() as usize] = true;
        for &value in &cfg.get_block(block).insts {
            stats.instructions_examined += 1;
            if is_hoist_candidate(cfg, value, body_has_effect) {
                candidate[value.as_u32() as usize] = true;
                candidate_values.push(value);
            }
        }
    }

    // Build the candidate def-use graph. A candidate is permanently blocked
    // when any in-loop operand is not itself a candidate (including block
    // parameters). Otherwise its pending count reaches zero as invariant
    // operands leave the worklist.
    let mut pending = vec![0usize; value_count];
    let mut blocked = vec![false; value_count];
    let mut dependents: Vec<Vec<CfgValue>> = vec![Vec::new(); value_count];
    for &value in &candidate_values {
        let value_idx = value.as_u32() as usize;
        super::dce::visit_instruction_uses(cfg, value, |operand| {
            let operand_idx = operand.as_u32() as usize;
            if def_block[operand_idx].is_some_and(|block| in_loop[block.as_u32() as usize]) {
                if candidate[operand_idx] {
                    pending[value_idx] += 1;
                    dependents[operand_idx].push(value);
                    stats.candidate_dependencies += 1;
                } else {
                    blocked[value_idx] = true;
                }
            }
        });
    }

    let mut worklist = VecDeque::new();
    for &value in &candidate_values {
        let idx = value.as_u32() as usize;
        if !blocked[idx] && pending[idx] == 0 {
            worklist.push_back(value);
        }
    }

    // Dependency order falls out of the worklist: a user is enqueued only
    // after every in-loop candidate operand has been discovered invariant.
    let mut invariant = vec![false; value_count];
    let mut order = Vec::new();
    while let Some(value) = worklist.pop_front() {
        let idx = value.as_u32() as usize;
        invariant[idx] = true;
        order.push(value);
        stats.worklist_pops += 1;

        for &user in &dependents[idx] {
            let user_idx = user.as_u32() as usize;
            pending[user_idx] = pending[user_idx]
                .checked_sub(1)
                .expect("each candidate dependency is resolved exactly once");
            if pending[user_idx] == 0 && !blocked[user_idx] {
                worklist.push_back(user);
            }
        }
    }

    if order.is_empty() {
        return Ok(HoistResult::default());
    }

    // Materialize the preheader once, then relocate each invariant instruction
    // into it. `ensure_preheader` reuses a suitable existing predecessor when it
    // can, inserting a block only when it must.
    let before = cfg.block_count();
    let ph = ensure_preheader(cfg, lp, type_pool)?;
    let cfg_changed = cfg.block_count() > before;
    if cfg_changed {
        stats.preheaders_materialized += 1;
    }

    for &block in &lp.body {
        cfg.get_block_mut(block)
            .insts
            .retain(|value| !invariant[value.as_u32() as usize]);
    }
    for &value in &order {
        cfg.get_block_mut(ph).insts.push(value);
    }

    stats.invariants_hoisted += order.len() as u64;
    Ok(HoistResult { cfg_changed })
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
        // A `Param` re-reads its parameter slot; it has no operands, so the
        // operand walk trivially reports it invariant — but its *value* is only
        // stable across iterations when the parameter cannot be mutated inside
        // the loop. A writable (`inout`) or address-taken parameter can be
        // written by a body `ParamStore` or a raw-pointer `@ptr_write`, so its
        // read is invariant only when the body has no observable effect, exactly
        // like a memory read. A by-value, non-address-taken parameter never
        // changes and hoists freely. (Mirrors CSE's `never_written_params`
        // guard, RUE-914 — without this a hoisted `inout` read would freeze the
        // parameter at its entry value and miscompile the loop.)
        CfgInstData::Param { index } => {
            !body_has_effect
                || (!cfg.is_param_writable(index) && !cfg.is_param_address_taken(index))
        }
        _ => true,
    }
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
/// loop) by invariant discovery.
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
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target: header,
                args,
            },
        );

        cfg.set_terminator(header, branch(cond, body, exit));

        // Depends on the header block param: varies, must NOT hoist.
        let varying = push(&mut cfg, body, CfgInstData::BitOr(hp, a), Type::I32);
        // Pure invariant sibling: must hoist.
        let invariant = push(&mut cfg, body, CfgInstData::BitXor(a, a), Type::I32);
        // Feed the block param back on the loop's back edge so it is well-formed.
        let args = cfg.push_goto_args([varying]).unwrap();
        cfg.set_terminator(
            body,
            Terminator::Goto {
                target: header,
                args,
            },
        );
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
    fn mutable_param_read_hoisted_only_when_body_effect_free() {
        // A `Param` read re-reads its parameter slot and has no operands, so the
        // operand walk trivially calls it invariant. But a writable (`inout`)
        // parameter that the loop body mutates (a `ParamStore`) varies per
        // iteration: hoisting the read would freeze the parameter at its entry
        // value and miscompile the loop (the same hazard the Load gate guards,
        // and CSE's `never_written_params`). It must stay in the body.
        let mut cfg = Cfg::new(
            Type::UNIT,
            1,
            2,
            "test".to_string(),
            rue_air::ParamSlotModes::new(vec![true, false], vec![true, false]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, goto(header));
        cfg.set_terminator(header, branch(cond, body, exit));

        // Read the writable inout param 0 and store back to it in the body.
        let read = push(&mut cfg, body, CfgInstData::Param { index: 0 }, Type::I32);
        push(
            &mut cfg,
            body,
            CfgInstData::ParamStore {
                param_slot: 0,
                value: read,
            },
            Type::UNIT,
        );
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(
            stats.invariants_hoisted, 0,
            "a mutated inout param read must not hoist"
        );
        assert_eq!(block_of(&cfg, read), Some(body), "the inout read stays");
        cfg.verify().unwrap();

        // A by-value, non-address-taken parameter (slot 1) never changes, so its
        // read hoists even when the body has an unrelated effect (a Store to a
        // local): the refinement must not over-restrict.
        let mut cfg = Cfg::new(
            Type::UNIT,
            1,
            2,
            "test".to_string(),
            rue_air::ParamSlotModes::new(vec![true, false], vec![true, false]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        let zero = push(&mut cfg, entry, CfgInstData::Const(0), Type::I32);
        cfg.set_terminator(entry, goto(header));
        cfg.set_terminator(header, branch(cond, body, exit));

        let pure_read = push(&mut cfg, body, CfgInstData::Param { index: 1 }, Type::I32);
        // An unrelated observable effect in the body (a local store).
        push(
            &mut cfg,
            body,
            CfgInstData::Store {
                slot: 0,
                value: zero,
            },
            Type::UNIT,
        );
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(
            stats.invariants_hoisted, 1,
            "an immutable by-value param read hoists despite an unrelated effect"
        );
        assert_eq!(
            block_of(&cfg, pure_read),
            Some(entry),
            "the pure read moved"
        );
        cfg.verify().unwrap();
    }

    #[test]
    fn no_loops_is_a_no_op() {
        // A straight-line function has no loops; the one-sided cycle summary
        // proves this without constructing dominators or a loop forest.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        let _c = push(&mut cfg, entry, CfgInstData::BitOr(a, b), Type::I32);
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.forest_computations, 0);
        assert_eq!(stats.loops_analyzed, 0);
        assert_eq!(stats.invariants_hoisted, 0);
    }

    #[test]
    fn cycle_summary_covers_branch_switch_and_self_edges() {
        // Forward-only branch and switch edges form a DAG. Block ids were
        // minted in topological order, so the summary can prove it acyclic.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let left = cfg.new_block();
        let right = cfg.new_block();
        let exit = cfg.new_block();
        let cond = bool_const(&mut cfg, entry);
        let scrutinee = push(&mut cfg, left, CfgInstData::Const(0), Type::I32);
        cfg.set_terminator(entry, branch(cond, left, right));
        let cases = cfg.push_switch_cases([(0, exit)]).unwrap();
        cfg.set_terminator(
            left,
            Terminator::Switch {
                scrutinee,
                cases,
                default: exit,
            },
        );
        cfg.set_terminator(right, goto(exit));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        assert!(!may_have_cycle_by_block_order(&cfg));

        // Every directed cycle has a non-forward edge in any total ordering.
        // Exercise both a switch back edge and the equality case (self-loop),
        // which are enough to prevent the one-sided proof from false-clearing.
        let cases = cfg.push_switch_cases([(0, entry)]).unwrap();
        cfg.get_block_mut(left).terminator = Terminator::Switch {
            scrutinee,
            cases,
            default: exit,
        };
        assert!(may_have_cycle_by_block_order(&cfg));

        cfg.get_block_mut(left).terminator = goto(exit);
        cfg.get_block_mut(right).terminator = goto(right);
        assert!(may_have_cycle_by_block_order(&cfg));
    }

    #[test]
    fn cycle_summary_allows_conservative_acyclic_false_positive() {
        // Block ids need not be a topological ordering. This entry-to-earlier
        // edge is acyclic, but deliberately retains the old LICM analysis path
        // instead of risking a false proof.
        let mut cfg = make_cfg();
        let exit = cfg.new_block();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, goto(exit));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        assert!(may_have_cycle_by_block_order(&cfg));
        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.forest_computations, 1);
        assert_eq!(stats.loops_analyzed, 0);
    }

    #[test]
    fn reverse_ordered_dependency_chains_have_linear_work() {
        // Block ids increase in the opposite direction from execution:
        // header -> highest id -> ... -> lowest id -> header. Definitions
        // therefore dominate their users, but the loop body's sorted block
        // order presents every user before its invariant dependency. The old
        // full-body fixpoint needed one complete rescan per chain link.
        for len in [64usize, 128, 256] {
            let mut cfg = make_cfg();
            let entry = cfg.new_block();
            cfg.entry = entry;
            let header = cfg.new_block();
            let exit = cfg.new_block();
            let blocks: Vec<BlockId> = (0..len).map(|_| cfg.new_block()).collect();

            let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
            let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
            let cond = bool_const(&mut cfg, entry);
            cfg.set_terminator(entry, goto(header));
            cfg.set_terminator(header, branch(cond, blocks[len - 1], exit));

            let mut dependency = a;
            let mut values = Vec::with_capacity(len);
            for &block in blocks.iter().rev() {
                dependency = push(
                    &mut cfg,
                    block,
                    CfgInstData::BitXor(dependency, b),
                    Type::I32,
                );
                values.push(dependency);
            }
            for index in (1..len).rev() {
                cfg.set_terminator(blocks[index], goto(blocks[index - 1]));
            }
            cfg.set_terminator(blocks[0], goto(header));
            cfg.set_terminator(exit, Terminator::Return { value: None });
            cfg.verify().unwrap();

            let stats = run(&mut cfg, &test_type_pool()).unwrap();
            assert_eq!(stats.forest_computations, 1);
            assert_eq!(stats.loops_analyzed, 1);
            assert_eq!(stats.instructions_examined, len as u64);
            assert_eq!(stats.candidate_dependencies, len as u64 - 1);
            assert_eq!(stats.worklist_pops, len as u64);
            assert_eq!(stats.invariants_hoisted, len as u64);
            assert!(
                values
                    .iter()
                    .all(|&value| block_of(&cfg, value) == Some(entry))
            );
            cfg.verify().unwrap();
        }
    }

    #[test]
    fn many_independent_invariants_have_linear_work() {
        for len in [128usize, 256, 512] {
            let mut s = single_loop();
            for _ in 0..len {
                push(&mut s.cfg, s.body, CfgInstData::BitXor(s.a, s.b), Type::I32);
            }

            let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
            assert_eq!(stats.forest_computations, 1);
            assert_eq!(stats.loops_analyzed, 1);
            assert_eq!(stats.instructions_examined, len as u64);
            assert_eq!(stats.candidate_dependencies, 0);
            assert_eq!(stats.worklist_pops, len as u64);
            assert_eq!(stats.invariants_hoisted, len as u64);
            s.cfg.verify().unwrap();
        }
    }

    #[test]
    fn recomputes_forest_only_after_materializing_preheader() {
        // Entry has a sibling successor, so it cannot serve as the loop
        // preheader. Hoisting inserts one block and triggers exactly one
        // structural recomputation.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        let a = push(&mut cfg, entry, CfgInstData::Const(6), Type::I32);
        let b = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, header, exit));
        cfg.set_terminator(header, branch(cond, body, exit));
        let invariant = push(&mut cfg, body, CfgInstData::BitOr(a, b), Type::I32);
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1);
        assert_eq!(stats.preheaders_materialized, 1);
        assert_eq!(stats.forest_computations, 2);
        assert_ne!(block_of(&cfg, invariant), Some(body));
        cfg.verify().unwrap();
    }

    #[test]
    fn instruction_only_motion_does_not_recompute_the_forest() {
        // A single loop with one hoistable op reuses its existing entry
        // preheader. The worklist reaches the invariance fixed point in one
        // analysis, and instruction motion changes no CFG edge.
        let mut s = single_loop();
        let _inv = push(&mut s.cfg, s.body, CfgInstData::BitOr(s.a, s.b), Type::I32);

        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1);
        assert_eq!(stats.forest_computations, 1);
        assert_eq!(stats.loops_analyzed, 1);
        assert_eq!(stats.instructions_examined, 1);
        assert_eq!(stats.worklist_pops, 1);
    }
}
