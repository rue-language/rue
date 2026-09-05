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
//! `Add`/`Sub`/`Mul`/`Div`/`Mod`/`Neg`, `IntCast`, an indirect or indexed
//! `PlaceRead` — stays in the loop body even when invariant. Guarded hoisting of
//! trapping ops is RUE-934, after loop rotation lands; there is no cleverness
//! here.
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
//! ### Memory reads
//!
//! A direct, non-indexed `Load`/`PlaceRead` is speculatable per the classifier,
//! but its result is invariant only when its storage is unchanged by the loop.
//! [`super::slot_facts`] classifies direct local and parameter roots per loop.
//! A read may move only when its exact slot is not written or reached in the
//! reachable loop body and its address has not escaped. Thus an allocation or
//! write of slot B does not kill a read of slot A. Calls, intrinsics, drops, and
//! indirect writes still block memory-read motion because their targets cannot
//! be bounded without general alias analysis. Indirect and indexed reads remain
//! non-speculatable because they can fault.
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
//! ## Placement in the pipeline and the canonical preheader contract
//!
//! LICM runs at `-O3` only, **after** the whole `-O1`/`-O2` sequence
//! (`constopt` → `peephole` → `simplify` → `forward` → `cse`), so it works on a
//! CFG whose constants are already folded and whose trivial control flow is
//! already threaded — the invariant operands it keys on are as exposed as the
//! earlier passes can make them — and **before** the final `dce`, which sweeps
//! anything the moves orphan. The `-O3` driver first establishes canonical
//! preheaders for every natural loop. LICM then computes the dominator tree and
//! loop forest exactly once and only moves instructions between existing
//! blocks, so that forest remains authoritative for the whole pass. An
//! **irreducible** forest makes the pass a no-op: the analysis refuses to
//! describe a multi-entry cycle, so there is nothing to hoist.
//!
//! Nested loops are processed **innermost first** (the forest's nesting orders
//! them by body size): an op invariant for an inner loop hoists to the inner
//! preheader, which sits in the outer loop's body, and the later outer-loop
//! visit can hoist it again to the outer preheader if it is invariant there too.
//! An op that is not invariant for the inner loop cannot be invariant for the
//! outer one either (the varying operand lives in the inner body, which is part
//! of the outer body), so innermost-first never misses a hoist.

use std::collections::VecDeque;

use rue_air::FrozenTypeInternPool;

use super::CfgOptimizationError;
use super::classify;
use super::loops::{LoopId, NaturalLoop, loops, may_have_cycle_by_block_order, preheader};
use super::slot_facts::{self, LoopSlotFacts};
use crate::dominators::DominatorTree;
use crate::{BlockId, Cfg, CfgInstData, CfgValue};

/// Bounded-work counters for one LICM run (RUE-794 discipline).
///
/// Every field is monotone and structurally bounded. One forest computation
/// visits each loop once. Within a loop, each instruction is scanned once for
/// slot facts and once for hoist eligibility, each candidate dependency is
/// recorded once, and each discovered invariant leaves the worklist once.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Dominator-tree and loop-forest computations, including the initial one.
    pub forest_computations: u64,
    /// Whole-function definition-block scans. One per run, not one per loop
    /// analyzed (RUE-1843): hoisting only relocates instructions to a known
    /// preheader, so the table is patched in place instead of rebuilt.
    pub def_block_scans: u64,
    /// Per-loop invariance analyses performed (one per loop visited).
    pub loops_analyzed: u64,
    /// Loop-body instructions tested for hoist eligibility.
    pub instructions_examined: u64,
    /// Loop-body instructions scanned by the shared slot-fact classifier.
    /// This is a separate physical scan from hoist eligibility and therefore
    /// stays separately visible even though both have the same structural
    /// bound.
    pub slot_fact_instructions_scanned: u64,
    /// Local/parameter generation-stamp entries initialized while growing (or
    /// after the theoretical generation-counter wrap). In the steady state
    /// this is at most the function's slot count, not slots times loops.
    pub slot_fact_entries_initialized: u64,
    /// Times the reusable slot-fact workspace grew. One initial growth covers
    /// every loop unless the CFG's slot domain itself grows.
    pub slot_fact_workspace_growths: u64,
    /// Def-use edges between hoist candidates in the same loop body.
    pub candidate_dependencies: u64,
    /// Candidate user instructions physically visited by sparse use-index CSR
    /// refills. Two visits per candidate are the count/fill passes; unrelated
    /// whole-function values do not contribute after amortized domain growth.
    pub use_index_users_visited: u64,
    /// Candidate operand edges physically visited by CSR count/fill passes.
    pub use_index_edges_visited: u64,
    /// Dense value-domain entries initialized while the reusable generation
    /// maps grow. Bounded by the maximum value domain, not loops times values.
    pub use_index_domain_entries_initialized: u64,
    /// Invariant candidates removed from the discovery worklist.
    pub worklist_pops: u64,
    /// Instructions moved into a preheader across all loops.
    pub invariants_hoisted: u64,
    /// Times the shared discovery workspace grew its whole-function-sized
    /// tables. One per run in the steady state, not one per loop analyzed
    /// (RUE-1843) — a regression to per-loop allocation shows up here.
    pub hoist_workspace_growths: u64,
}

/// Run loop-invariant code motion. Call at `-O3` only, after the `-O2` passes
/// and before DCE (see the module docs). The caller must first establish
/// canonical preheaders with `loops::normalize_preheaders`. Returns its work
/// counters. The type-pool parameter is retained for the pass API contract.
pub fn run(
    cfg: &mut Cfg,
    _type_pool: &FrozenTypeInternPool,
) -> Result<Stats, CfgOptimizationError> {
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
    // One discovery workspace for the whole run, reset per loop rather than
    // reallocated (RUE-1843).
    let mut workspace = HoistWorkspace::default();

    stats.forest_computations += 1;
    let dom = DominatorTree::compute(cfg);
    let forest = loops(cfg, &dom);
    // An irreducible forest carries no loops, so this is a natural no-op;
    // an empty (loop-free) forest likewise.
    if forest.is_irreducible() || forest.is_empty() {
        return Ok(stats);
    }

    // Innermost first: a smaller body is nested more deeply. Hoisting from
    // the innermost loop first lets its results bubble outward when the later
    // outer-loop visit sees them in the inner preheader.
    let mut order: Vec<LoopId> = (0..forest.len()).collect();
    order.sort_by_key(|&id| forest.get(id).body.len());
    // The forest contains only reachable natural loops. Keep an explicit
    // shared reachability set for slot-fact scans so disconnected
    // counterfeit blocks can never become memory barriers, and compute it
    // once per run rather than once per loop.
    let reachable = super::dce::compute_reachable_blocks(cfg);

    // One whole-function def-block scan per run. Instruction-only motion keeps
    // it accurate as long as each hoist patches the entries it moved
    // (RUE-1843).
    let mut def_block = compute_def_blocks(cfg, &mut stats);

    for id in order {
        stats.loops_analyzed += 1;
        hoist_loop(
            cfg,
            forest.get(id),
            &mut def_block,
            &reachable,
            &mut workspace,
            &mut stats,
        );
    }

    Ok(stats)
}

/// Whole-function-sized scratch for invariant discovery, owned by the run
/// rather than by each loop (RUE-1843).
///
/// Every table is indexed by `CfgValue` or `BlockId`, so each is sized by the
/// *function* — but only the current loop's candidates are ever written. A
/// fresh set per loop meant six allocations proportional to the whole function
/// for every loop in the run, paid in full even by a loop with no
/// candidates at all, since they are built before the zero-hoist early return.
/// `pending` alone is 8 bytes per value. Candidate adjacency is kept in one
/// reusable CSR [`super::use_index::CfgUseIndex`] rather than a `Vec` header
/// per value before a single edge is pushed.
///
/// Reuse is only sound because every table's written set is bounded by
/// `candidate_values` (and `in_loop` by `in_loop_blocks`), so resetting just
/// those entries restores the all-default state each loop expects, in time
/// proportional to the loop rather than the function. `invariant` is the one
/// that would bite if this were wrong: it is read for *every* body instruction,
/// not just candidates, so a stale `true` would delete a live instruction.
#[derive(Default)]
struct HoistWorkspace {
    slot_facts: slot_facts::LoopSlotFactsWorkspace,
    in_loop: Vec<bool>,
    /// Blocks currently set in `in_loop`.
    in_loop_blocks: Vec<BlockId>,
    candidate: Vec<bool>,
    /// Values currently set in `candidate`. Every other value-indexed table's
    /// written set is a subset of this.
    candidate_values: Vec<CfgValue>,
    pending: Vec<usize>,
    blocked: Vec<bool>,
    use_index: super::use_index::CfgUseIndex,
    invariant: Vec<bool>,
    order: Vec<CfgValue>,
    worklist: VecDeque<CfgValue>,
}

impl HoistWorkspace {
    /// Reset what the previous loop dirtied, grow to cover the graph, and mark
    /// `body` as the loop now under analysis.
    fn prepare(&mut self, cfg: &Cfg, body: &[BlockId], stats: &mut Stats) {
        for &value in &self.candidate_values {
            let idx = value.as_u32() as usize;
            self.candidate[idx] = false;
            self.pending[idx] = 0;
            self.blocked[idx] = false;
            self.invariant[idx] = false;
        }
        self.candidate_values.clear();
        for &block in &self.in_loop_blocks {
            self.in_loop[block.as_u32() as usize] = false;
        }
        self.in_loop_blocks.clear();
        self.order.clear();
        self.worklist.clear();

        // Grow only. Canonical normalization has already established every
        // preheader, so the graph cannot grow during this LICM run; retaining
        // capacity across loop visits avoids per-loop allocation.
        let values = cfg.value_count();
        let blocks = cfg.block_count();
        let grow_values = self.candidate.len() < values;
        let grow_blocks = self.in_loop.len() < blocks;
        if grow_values || grow_blocks {
            stats.hoist_workspace_growths += 1;
        }
        if grow_values {
            self.candidate.resize(values, false);
            self.pending.resize(values, 0);
            self.blocked.resize(values, false);
            self.invariant.resize(values, false);
        }
        if grow_blocks {
            self.in_loop.resize(blocks, false);
        }

        for &block in body {
            self.in_loop[block.as_u32() as usize] = true;
            self.in_loop_blocks.push(block);
        }
    }
}

fn hoist_loop(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    def_block: &mut Vec<Option<BlockId>>,
    reachable: &super::dce::BitSet,
    workspace: &mut HoistWorkspace,
    stats: &mut Stats,
) {
    // Classify each body instruction once. The classifier remains the sole
    // authority for trap/effect eligibility; the worklist below only answers
    // whether eligible operands become available at the preheader.
    workspace.prepare(cfg, &lp.body, stats);
    let HoistWorkspace {
        slot_facts: slot_fact_workspace,
        in_loop,
        in_loop_blocks: _,
        candidate,
        candidate_values,
        pending,
        blocked,
        use_index,
        invariant,
        order,
        worklist,
    } = workspace;

    // Slot writes and escape channels are classified once by the shared
    // RUE-521/RUE-1869 authority. Its generation-stamped storage is reused by
    // this run workspace, and every scan/growth cost is published in Stats.
    let (slot_facts, slot_work) =
        slot_fact_workspace.classify_loop_slot_invariance(cfg, &lp.body, reachable);
    stats.slot_fact_instructions_scanned += slot_work.instructions_scanned;
    stats.slot_fact_entries_initialized += slot_work.entries_initialized;
    stats.slot_fact_workspace_growths += slot_work.workspace_growths;
    for &block in &lp.body {
        for &value in &cfg.get_block(block).insts {
            stats.instructions_examined += 1;
            if is_hoist_candidate(cfg, value, &slot_facts) {
                candidate[value.as_u32() as usize] = true;
                candidate_values.push(value);
            }
        }
    }

    // Refill adjacency in candidate discovery order. That preserves the old
    // dependents-list order (and duplicate operands) exactly while reusing one
    // compact allocation across loop visits. LICM only moves instructions
    // between blocks below; it never rewrites operands, so this snapshot stays
    // valid until the next loop explicitly rebuilds it.
    let use_index_work = use_index
        .rebuild(cfg, candidate_values.iter().copied())
        .expect("verified CFG operands belong to this value domain");
    stats.use_index_users_visited += use_index_work.users_visited;
    stats.use_index_edges_visited += use_index_work.edges_visited;
    stats.use_index_domain_entries_initialized += use_index_work.domain_entries_initialized;

    // Build the candidate def-use graph. A candidate is permanently blocked
    // when any in-loop operand is not itself a candidate (including block
    // parameters). Otherwise its pending count reaches zero as invariant
    // operands leave the worklist.
    for &value in candidate_values.iter() {
        let value_idx = value.as_u32() as usize;
        super::dce::visit_instruction_uses(cfg, value, |operand| {
            let operand_idx = operand.as_u32() as usize;
            if def_block[operand_idx].is_some_and(|block| in_loop[block.as_u32() as usize]) {
                if candidate[operand_idx] {
                    pending[value_idx] += 1;
                    stats.candidate_dependencies += 1;
                } else {
                    blocked[value_idx] = true;
                }
            }
        });
    }

    for &value in candidate_values.iter() {
        let idx = value.as_u32() as usize;
        if !blocked[idx] && pending[idx] == 0 {
            worklist.push_back(value);
        }
    }

    // Dependency order falls out of the worklist: a user is enqueued only
    // after every in-loop candidate operand has been discovered invariant.
    while let Some(value) = worklist.pop_front() {
        let idx = value.as_u32() as usize;
        invariant[idx] = true;
        order.push(value);
        stats.worklist_pops += 1;

        for &user in use_index
            .users(cfg, value)
            .expect("LICM only moves indexed instructions between blocks")
        {
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
        return;
    }

    // Canonical O3 normalization has already established this destination.
    // Merely looking it up cannot change CFG edges or invalidate the forest.
    let ph = preheader(cfg, lp).expect("LICM requires canonical loop preheaders");

    for &block in &lp.body {
        cfg.get_block_mut(block)
            .insts
            .retain(|value| !invariant[value.as_u32() as usize]);
    }
    for &value in order.iter() {
        cfg.get_block_mut(ph).insts.push(value);
        // The shared table is now stale for exactly these values, and their new
        // defining block is known: the preheader they just moved into.
        def_block[value.as_u32() as usize] = Some(ph);
    }

    stats.invariants_hoisted += order.len() as u64;
}

/// Whether `value` is eligible to hoist on its own merits, independent of its
/// operands: speculatable (never traps, no observable effect), not a block
/// parameter, and — for a memory read — only when its direct root is invariant.
fn is_hoist_candidate(cfg: &Cfg, value: CfgValue, slot_facts: &LoopSlotFacts) -> bool {
    // Trapping or observable ops never move (ADR-0054 §2). This already excludes
    // arithmetic, IntCast, indirect/indexed PlaceRead, calls, stores, allocs,
    // drops, and the storage markers.
    if !classify::is_speculatable(cfg, value) {
        return false;
    }
    match &cfg.get_inst(value).data {
        // Block parameters vary per iteration; they are not instructions in the
        // body's `insts` list, but guard defensively.
        CfgInstData::BlockParam { .. } => false,
        CfgInstData::Load { slot } => slot_facts.local_is_invariant(*slot),
        CfgInstData::PlaceRead { place } => match place.base {
            crate::PlaceBase::Local(slot) => slot_facts.local_is_invariant(slot),
            crate::PlaceBase::Param(slot) => slot_facts.param_is_invariant(slot),
            crate::PlaceBase::Accessor(_) | crate::PlaceBase::Indirect(_) => false,
        },
        // A `Param` re-reads its ABI slot and has no operands, so its memory
        // root must be checked explicitly just like a local Load.
        CfgInstData::Param { index } => slot_facts.param_is_invariant(*index),
        _ => true,
    }
}

/// Map each `CfgValue` to the block that defines it (as a block parameter or as
/// a block-attached instruction). Values with no defining block — detached or
/// dead arena entries — map to `None` and are treated as available (outside the
/// loop) by invariant discovery.
fn compute_def_blocks(cfg: &Cfg, stats: &mut Stats) -> Vec<Option<BlockId>> {
    // Counted here rather than at the call site so the counter cannot drift
    // from the number of scans actually performed (RUE-1843).
    stats.def_block_scans += 1;
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
    use crate::{CfgArgMode, CfgCallArg, CfgInst, Terminator, Type};
    use lasso::{Key, Spur};
    use rue_span::Span;

    // Two local slots so the memory-read and indexed-place tests are
    // verifier-clean; loop-shape tests that use no slots ignore them.
    fn make_cfg() -> Cfg {
        Cfg::new(Type::UNIT, 2, 0, "test".to_string(), vec![])
    }

    fn test_type_pool() -> FrozenTypeInternPool {
        rue_air::TypeInternPool::new().freeze()
    }

    /// Exercise LICM through the same canonical-preheader boundary as O3.
    fn run(cfg: &mut Cfg, type_pool: &FrozenTypeInternPool) -> Result<Stats, CfgOptimizationError> {
        super::super::loops::normalize_preheaders(cfg, type_pool)?;
        super::run(cfg, type_pool)
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
        single_loop_with_condition(true)
    }

    fn single_loop_with_condition(condition: bool) -> LoopShape {
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
        let cond = push(
            &mut cfg,
            entry,
            CfgInstData::BoolConst(condition),
            Type::BOOL,
        );
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
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init: a },
            Type::UNIT,
        );
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
        // Slot 0 is unchanged in the inner loop, so this read may leave it. The
        // outer loop writes slot 0 below, so it must stop in the inner preheader.
        let inner_slot_read = push(&mut cfg, i_body, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(i_body, goto(i));

        let next_o = push(&mut cfg, o_body, CfgInstData::BitAnd(op, b), Type::I32);
        push(
            &mut cfg,
            o_body,
            CfgInstData::Store { slot: 0, value: b },
            Type::UNIT,
        );
        let args = cfg.push_goto_args([next_o]).unwrap();
        cfg.set_terminator(o_body, Terminator::Goto { target: o, args });
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        cfg.verify().unwrap();
        assert!(stats.invariants_hoisted >= 2);

        // RUE-1843: the whole-function definition-block table is built once per
        // run, not once per loop analyzed. This fixture has two nested loops,
        // so it analyzes strictly more loops than whole-function scans — which
        // is what makes the second assertion bite: restoring the per-loop scan
        // makes the scan count track `loops_analyzed` instead. The counter is
        // incremented inside `compute_def_blocks` itself, so it cannot drift
        // from the number of scans actually performed.
        assert_eq!(
            stats.def_block_scans, stats.forest_computations,
            "one definition-block scan per run, alongside the forest"
        );
        assert!(
            stats.loops_analyzed > stats.def_block_scans,
            "fixture must analyze more loops ({}) than it takes scans ({}) for \
             this to be a meaningful bound",
            stats.loops_analyzed,
            stats.def_block_scans,
        );

        // Same bound for the discovery workspace (RUE-1843): its six tables are
        // sized by the function, so allocating a set per loop cost the whole
        // function's size for every loop in the run — paid even by a loop
        // with no candidates, since they are built before the zero-hoist early
        // return. One workspace is reset per loop instead; canonical
        // normalization has already finished every graph edit, so it grows at
        // most once to the final graph's dimensions.
        assert!(
            stats.hoist_workspace_growths <= stats.forest_computations,
            "workspace grew {} times during one {}-forest run; it should grow \
             only to the final graph dimensions",
            stats.hoist_workspace_growths,
            stats.forest_computations,
        );
        assert!(
            stats.loops_analyzed > stats.hoist_workspace_growths,
            "fixture must analyze more loops ({}) than the workspace takes \
             growths ({}) for this to be a meaningful bound",
            stats.loops_analyzed,
            stats.hoist_workspace_growths,
        );

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
        let read_block = block_of(&cfg, inner_slot_read).unwrap();
        assert_ne!(read_block, i_body, "slot read should leave the inner loop");
        assert_ne!(
            read_block, entry,
            "outer same-slot write must stop the read"
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
        assert_eq!(cfg.block_count(), before, "no preheader inserted");
    }

    #[test]
    fn local_load_uses_per_slot_loop_invariance() {
        // A non-indexed Load is speculatable. With no write to its slot it is
        // invariant and hoists.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        s.cfg.verify().unwrap();
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1, "effect-free body: Load hoists");
        assert_eq!(block_of(&s.cfg, load), Some(s.entry));

        // A store to the same slot makes the Load variant.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::Store {
                slot: 0,
                value: s.a,
            },
            Type::UNIT,
        );
        s.cfg.verify().unwrap();
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0, "same-slot store: Load stays");
        assert_eq!(block_of(&s.cfg, load), Some(s.body));

        // A store to slot B cannot change slot A and must not retain the old
        // loop-wide memory-effect veto.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
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
        assert_eq!(stats.invariants_hoisted, 1, "unrelated store: Load hoists");
        assert_eq!(stats.instructions_examined, 2);
        assert_eq!(stats.worklist_pops, 1);
        assert_eq!(block_of(&s.cfg, load), Some(s.entry));

        // Allocation is a write too, but only to the allocated root.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::Alloc { slot: 1, init: s.b },
            Type::UNIT,
        );
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1, "unrelated alloc: Load hoists");
        assert_eq!(block_of(&s.cfg, load), Some(s.entry));
    }

    #[test]
    fn opaque_effects_and_by_ref_calls_block_loads() {
        // Calls remain an unknown-memory barrier even when their explicit
        // arguments do not identify the loaded root.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = s.cfg.push_call_args(std::iter::empty()).unwrap();
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::Call {
                runtime: None,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(block_of(&s.cfg, load), Some(s.body));

        // A by-ref argument explicitly reaches its root and is also an opaque
        // call boundary.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = s
            .cfg
            .push_call_args([CfgCallArg {
                value: load,
                mode: CfgArgMode::Inout,
            }])
            .unwrap();
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::Call {
                runtime: None,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(block_of(&s.cfg, load), Some(s.body));

        // Intrinsics likewise retain the conservative barrier.
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = s.cfg.push_intrinsic_args(std::iter::empty()).unwrap();
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::Intrinsic {
                operation: rue_air::IntrinsicOperation::PanicNoMessage,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(block_of(&s.cfg, load), Some(s.body));
    }

    #[test]
    fn escaped_slot_and_indirect_write_block_loads() {
        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        s.cfg.mark_address_taken(0);
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(block_of(&s.cfg, load), Some(s.body));

        let mut s = single_loop();
        push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Alloc { slot: 0, init: s.a },
            Type::UNIT,
        );
        let load = push(&mut s.cfg, s.body, CfgInstData::Load { slot: 0 }, Type::I32);
        let pool = rue_air::TypeInternPool::new();
        let pointer = push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Const(0),
            Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32)),
        );
        let place = s
            .cfg
            .make_place(
                crate::PlaceBase::Indirect(pointer),
                Type::I32,
                std::iter::empty(),
            )
            .unwrap();
        push(
            &mut s.cfg,
            s.body,
            CfgInstData::PlaceWrite { place, value: s.b },
            Type::UNIT,
        );
        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 0);
        assert_eq!(block_of(&s.cfg, load), Some(s.body));
    }

    #[test]
    fn indirect_place_read_stays_in_loop_but_direct_read_hoists() {
        // A direct local read remains safe to speculate. An indirect read with
        // no projections can fault on a bad pointer, so it must stay behind the
        // explicitly false loop condition and cannot manufacture a zero-trip
        // fault.
        let mut s = single_loop_with_condition(false);
        let direct_place = s
            .cfg
            .make_place(crate::PlaceBase::Local(0), Type::I32, std::iter::empty())
            .unwrap();
        let direct = push(
            &mut s.cfg,
            s.body,
            CfgInstData::PlaceRead {
                place: direct_place,
            },
            Type::I32,
        );
        let type_pool = rue_air::TypeInternPool::new();
        let pointer = push(
            &mut s.cfg,
            s.entry,
            CfgInstData::Const(0),
            Type::new_ptr_const(type_pool.intern_ptr_const_from_type(Type::I32)),
        );
        let indirect_place = s
            .cfg
            .make_place(
                crate::PlaceBase::Indirect(pointer),
                Type::I32,
                std::iter::empty(),
            )
            .unwrap();
        let indirect = push(
            &mut s.cfg,
            s.body,
            CfgInstData::PlaceRead {
                place: indirect_place,
            },
            Type::I32,
        );
        s.cfg.verify().unwrap();

        let stats = run(&mut s.cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1, "only the direct read hoists");
        assert_eq!(block_of(&s.cfg, direct), Some(s.entry));
        assert_eq!(block_of(&s.cfg, indirect), Some(s.body));
        s.cfg.verify().unwrap();
    }

    #[test]
    fn param_read_uses_per_slot_loop_invariance() {
        // A `Param` read re-reads its parameter slot and has no operands, so the
        // operand walk trivially calls it invariant. But a writable (`inout`)
        // parameter that the loop body mutates (a `ParamStore`) varies per
        // iteration: hoisting the read would freeze the parameter at its entry
        // value and miscompile the loop (the same hazard the Load gate guards,
        // and CSE through `slot_facts::classify_never_written_params`). It must stay in the body.
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

        // A write to parameter slot 0 cannot change parameter slot 1.
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
        push(
            &mut cfg,
            body,
            CfgInstData::ParamStore {
                param_slot: 0,
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

        // Writability is a permission, not itself a loop write: an inout read
        // with no reachable writer in the loop is invariant.
        let mut cfg = Cfg::new(
            Type::UNIT,
            0,
            1,
            "test".to_string(),
            rue_air::ParamSlotModes::new(vec![true], vec![true]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();
        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, goto(header));
        cfg.set_terminator(header, branch(cond, body, exit));
        let read = push(&mut cfg, body, CfgInstData::Param { index: 0 }, Type::I32);
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.invariants_hoisted, 1);
        assert_eq!(block_of(&cfg, read), Some(entry));
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
            assert_eq!(stats.slot_fact_instructions_scanned, len as u64);
            assert_eq!(stats.slot_fact_entries_initialized, 2);
            assert_eq!(stats.slot_fact_workspace_growths, 1);
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
            assert_eq!(stats.slot_fact_instructions_scanned, len as u64);
            assert_eq!(stats.slot_fact_entries_initialized, 2);
            assert_eq!(stats.slot_fact_workspace_growths, 1);
            assert_eq!(stats.candidate_dependencies, 0);
            assert_eq!(stats.worklist_pops, len as u64);
            assert_eq!(stats.invariants_hoisted, len as u64);
            s.cfg.verify().unwrap();
        }
    }

    #[test]
    fn many_small_loops_reuse_large_slot_fact_tables() {
        const LOOP_COUNT: usize = 24;
        const LOCAL_COUNT: usize = 4096;
        const PARAM_COUNT: usize = 2048;
        const UNRELATED_VALUE_COUNT: usize = 4096;

        let mut cfg = Cfg::new(
            Type::UNIT,
            LOCAL_COUNT as u32,
            PARAM_COUNT as u32,
            "test".to_string(),
            rue_air::ParamSlotModes::new(vec![false; PARAM_COUNT], vec![false; PARAM_COUNT]),
        );
        let entry = cfg.new_block();
        cfg.entry = entry;
        let cond = bool_const(&mut cfg, entry);
        let init = push(&mut cfg, entry, CfgInstData::Const(7), Type::I32);
        push(
            &mut cfg,
            entry,
            CfgInstData::Alloc { slot: 0, init },
            Type::UNIT,
        );
        for i in 0..UNRELATED_VALUE_COUNT {
            push(&mut cfg, entry, CfgInstData::Const(i as u64), Type::I32);
        }

        let mut predecessor = entry;
        let mut loads = Vec::with_capacity(LOOP_COUNT);
        let mut bodies = Vec::with_capacity(LOOP_COUNT);
        for _ in 0..LOOP_COUNT {
            let header = cfg.new_block();
            let body = cfg.new_block();
            let next = cfg.new_block();
            bodies.push(body);
            cfg.set_terminator(predecessor, goto(header));
            cfg.set_terminator(header, branch(cond, body, next));
            loads.push(push(
                &mut cfg,
                body,
                CfgInstData::Load { slot: 0 },
                Type::I32,
            ));
            push(
                &mut cfg,
                body,
                CfgInstData::Store {
                    slot: 1,
                    value: init,
                },
                Type::UNIT,
            );
            cfg.set_terminator(body, goto(header));
            predecessor = next;
        }
        cfg.set_terminator(predecessor, Terminator::Return { value: None });
        cfg.verify().unwrap();

        let stats = run(&mut cfg, &test_type_pool()).unwrap();
        assert_eq!(stats.forest_computations, 1);
        assert_eq!(stats.def_block_scans, 1);
        assert_eq!(stats.loops_analyzed, LOOP_COUNT as u64);
        assert_eq!(stats.instructions_examined, (LOOP_COUNT * 2) as u64);
        assert_eq!(
            stats.slot_fact_instructions_scanned,
            (LOOP_COUNT * 2) as u64
        );
        assert_eq!(
            stats.slot_fact_entries_initialized,
            (LOCAL_COUNT + PARAM_COUNT) as u64,
            "generation tables initialize once, not once per loop"
        );
        assert_eq!(stats.slot_fact_workspace_growths, 1);
        assert_eq!(stats.hoist_workspace_growths, 1);
        assert_eq!(stats.candidate_dependencies, 0);
        assert_eq!(stats.use_index_users_visited, (LOOP_COUNT * 2) as u64);
        assert_eq!(stats.use_index_edges_visited, 0);
        assert_eq!(
            stats.use_index_domain_entries_initialized,
            cfg.value_count() as u64,
            "sparse refills initialize the large value domain once, not once per loop"
        );
        assert_eq!(stats.worklist_pops, LOOP_COUNT as u64);
        assert_eq!(stats.invariants_hoisted, LOOP_COUNT as u64);
        assert!(
            loads
                .iter()
                .zip(&bodies)
                .all(|(&load, &body)| block_of(&cfg, load).is_some_and(|block| block != body))
        );
        cfg.verify().unwrap();
    }

    #[test]
    fn canonical_normalization_keeps_licm_to_one_forest() {
        // Entry has a sibling successor, so it cannot serve as the loop
        // preheader. Canonical normalization inserts it before LICM; the pass
        // itself performs only instruction motion and computes one forest.
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
        assert_eq!(stats.forest_computations, 1);
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
