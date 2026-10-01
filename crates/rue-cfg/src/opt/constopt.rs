//! Sparse worklist driver for constant folding and constant propagation.
//!
//! The optimizer used to interleave full constant-folding and store-to-load
//! propagation scans in a loop until a fixpoint. A dependency chain
//! (`let x1 = x0 + 1; let x2 = x1 + 1; ...`) exposes only one new constant
//! per round, so ordinary straight-line programs made -O1 quadratic in
//! function size (RUE-794). This driver reaches the same fixpoint sparsely:
//! every instruction gets one seed fold attempt, and after that an
//! instruction is revisited only when one of its operands actually becomes
//! constant (or provably equal to another value, below). Each value becomes
//! constant at most once, so total work is bounded by the instruction count
//! plus the def-use edge count.
//!
//! The fold kernel (one instruction at a time) lives in [`super::constfold`];
//! this module owns the driver and the store-to-load propagation for local
//! slots.
//!
//! ## Store-to-load propagation
//!
//! CfgBuilder materializes every `let` as a stack slot: the initializer
//! becomes an `Alloc` and each later use a `Load`. Constant folding only
//! sees literal `Const` operands, so without propagation
//! `let a = -2147483648; let q = a / -1;` never folds even though `a` is a
//! compile-time constant (RUE-154).
//!
//! For each local slot with exactly ONE live whole-slot write (its `Alloc`,
//! or a single `Store`) whose value is or becomes a `Const`/`BoolConst`, and
//! that is never written through any other live channel, every `Load` of the
//! slot is replaced by that constant. Which slots qualify — and the full
//! safety argument for why (RUE-521 address escapes, by-ref call arguments,
//! projected writes, multi-write slots, dominance) — is owned by
//! [`super::slot_facts`], the classifier this pass shares with value
//! forwarding.
//!
//! ## Constant control flow (RUE-2545)
//!
//! A constant often reaches its use only across a branch the driver itself
//! makes constant: an inlined callee's result arrives as a continuation-block
//! parameter whose arms disagree until one is proven dead, and a slot's
//! competing write may sit in an arm that a folded condition kills. If the
//! driver stopped at the branch, every dependent branch would cost one more
//! round of the caller's cleanup loop (constopt, then simplify folding the
//! branch and merging the join, then constopt again), so the number of rounds
//! grew with the length of the chain rather than with anything the cleanup
//! loop could bound. The driver therefore tracks control flow as part of the
//! same fixpoint:
//!
//! - **Edge pruning.** Edges start live when their source block is reachable.
//!   When a `Branch` condition or `Switch` scrutinee becomes constant, every
//!   edge it cannot take dies, choosing exactly as
//!   [`super::simplify::constant_successor`] does. A non-entry block whose
//!   last live incoming edge dies is dead, and its outgoing edges die with it.
//!   When the worklist drains after edges died, one walk from the entry over
//!   live edges also retires dead cycles, which incoming-edge counts alone
//!   cannot see.
//! - **Block parameters.** A parameter whose live incoming edges all carry
//!   the same constant becomes that constant: its `BlockParam` is rewritten
//!   in place into the `Const`, so every use sees it without a use rewrite,
//!   and the parameter is removed from the block (and its argument from every
//!   incoming edge) when the run ends.
//! - **Dead writes.** A write in a dead block stops counting against its slot
//!   ([`super::slot_facts::LiveSlotWrites`]).
//! - **Equal values.** An identity operation (`x + 0`, see
//!   [`super::peephole::identity_target`]) and a parameter with exactly one
//!   live incoming edge are recorded as aliases of the value they always
//!   equal, and the kernel's equal-operand annihilators (`x - x`, `x ^ x`)
//!   consult those aliases. Nothing is rewritten for an alias: peephole and
//!   simplify still own rewiring identities and merging blocks.
//!
//! Every fact here only ever grows — edges and blocks die, values become
//! constant or aliased, slot write counts fall — so the driver reaches its
//! fixpoint in one run, with each edge, block, slot, and value changing state
//! at most once. The single-live-edge alias is sound because the block's only
//! executed predecessor dominates it on executed paths: the argument's
//! definition dominates that predecessor, so the argument cannot be
//! recomputed between entering the block and any use the block dominates
//! without re-entering the block through that edge.
//!
//! Pruning only decides which edges can run; it never rewrites a terminator.
//! Folding the terminator into a `Goto` stays [`super::simplify`]'s job, so a
//! dead edge here is always one simplify removes in the same cleanup round.

use std::collections::VecDeque;

use crate::{BlockId, Cfg, CfgInstData, CfgValue, Terminator};

use super::constfold;
use super::dce;
use super::peephole;
use super::simplify::{self, ConstantSuccessor};
use super::slot_facts::{self, LiveSlotWrites};
use super::use_index::CfgUseIndex;

/// Work counters for one run.
///
/// These exist so tests can prove the driver does bounded work (RUE-794):
/// `fold_attempts` is structurally O(values + def-use edges) — one seed
/// attempt per value plus one re-attempt per operand→user edge whose operand
/// became constant or aliased — where the old rescan loop performed
/// O(values × rounds) attempts.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Calls into the fold kernel.
    pub fold_attempts: u64,
    /// Instructions the fold kernel replaced with a constant.
    pub folded: u64,
    /// `Load`s replaced by their slot's single constant write.
    pub loads_rewritten: u64,
    /// Block parameters replaced by the constant every live edge passes.
    pub params_resolved: u64,
    /// Values recorded as always equal to another value.
    pub aliases_recorded: u64,
    /// Edges proven never taken.
    pub edges_pruned: u64,
    /// Blocks proven never executed.
    pub blocks_proven_dead: u64,
    /// Reachability walks over the live edges, which retire dead cycles.
    pub reachability_walks: u64,
}

impl Stats {
    /// Whether the run changed the CFG. Pruning and aliasing are analysis
    /// only; the rewrites they enable are counted by the other fields.
    pub fn made_progress(self) -> bool {
        self.folded > 0 || self.loads_rewritten > 0 || self.params_resolved > 0
    }
}

/// Run sparse constant folding and store-to-load constant propagation to a
/// fixpoint, including the constant control flow the folds expose (module
/// docs). Ownership-boundary values are always preserved: they describe a
/// semantic transfer across an inline boundary, not an optional optimization
/// policy.
///
/// Errors only when removing a resolved block parameter's arguments cannot
/// allocate the rewritten edge payload; the caller discards the editor then.
pub fn run(cfg: &mut Cfg) -> Result<Stats, crate::CfgEditError> {
    let (mut driver, use_index) = Driver::new(cfg);
    driver.seed(cfg);
    driver.drain(cfg, &use_index);
    driver.remove_resolved_params(cfg)?;
    Ok(driver.stats)
}

/// The constant payloads propagation understands. Strings and aggregates are
/// not propagated (matching the fold kernel, which only produces these two).
fn const_payload(cfg: &Cfg, value: CfgValue) -> Option<CfgInstData> {
    match &cfg.get_inst(value).data {
        CfgInstData::Const(v) => Some(CfgInstData::Const(*v)),
        CfgInstData::BoolConst(b) => Some(CfgInstData::BoolConst(*b)),
        _ => None,
    }
}

fn same_payload(a: &CfgInstData, b: &CfgInstData) -> bool {
    match (a, b) {
        (CfgInstData::Const(a), CfgInstData::Const(b)) => a == b,
        (CfgInstData::BoolConst(a), CfgInstData::BoolConst(b)) => a == b,
        _ => false,
    }
}

/// Follow recorded aliases to the value a value always equals.
fn resolve(alias: &[Option<CfgValue>], mut value: CfgValue) -> CfgValue {
    while let Some(&Some(next)) = alias.get(value.as_u32() as usize) {
        value = next;
    }
    value
}

#[derive(Clone, Copy)]
enum Event {
    /// The value is now a `Const`/`BoolConst`.
    Const(CfgValue),
    /// The value now has a recorded alias.
    Alias(CfgValue),
}

/// One control-flow edge. Branch edges are stored then before else, and switch
/// edges case-by-case then default, in each block's contiguous range.
struct Edge {
    target: BlockId,
    args_start: u32,
    args_len: u32,
    live: bool,
}

/// What waits for a value to become constant.
#[derive(Clone, Copy)]
enum Watch {
    /// The slot whose single live write is this value.
    Slot(u32),
    /// The block whose terminator branches on this value.
    Terminator(BlockId),
    /// Argument `position` of `edge` is this value.
    Argument { edge: u32, position: u32 },
}

const NO_WATCH: u32 = u32::MAX;

/// Per-value watcher lists, as one head index per value into a shared
/// singly linked arena: one allocation sized by the value count, however
/// many kinds of watcher a value has.
struct Watchers {
    head: Vec<u32>,
    entries: Vec<(Watch, u32)>,
}

impl Watchers {
    fn new(value_count: usize) -> Self {
        Self {
            head: vec![NO_WATCH; value_count],
            entries: Vec::new(),
        }
    }

    fn add(&mut self, value: CfgValue, watch: Watch) {
        let head = &mut self.head[value.as_u32() as usize];
        self.entries.push((watch, *head));
        *head = (self.entries.len() - 1) as u32;
    }

    /// Detach and return the first watcher of `value`, so each fires once.
    fn pop(&mut self, value: CfgValue) -> Option<Watch> {
        let head = &mut self.head[value.as_u32() as usize];
        let (watch, next) = *self.entries.get(*head as usize)?;
        *head = next;
        Some(watch)
    }
}

struct Driver {
    stats: Stats,
    entry: BlockId,
    queue: VecDeque<Event>,
    /// `alias[v] = Some(w)`: v always holds w's value where both are used.
    /// Allocated on the first alias; most runs record none.
    alias: Vec<Option<CfgValue>>,
    value_count: usize,
    watchers: Watchers,

    loads_of_slot: Vec<Vec<CfgValue>>,
    slot_writes: LiveSlotWrites,
    /// The slot has been propagated, or is waiting on its write's value.
    slot_settled: Vec<bool>,

    block_live: Vec<bool>,
    edges: Vec<Edge>,
    edge_args: Vec<CfgValue>,
    /// Out-edges of block `b` are `edges[out_offsets[b]..out_offsets[b + 1]]`.
    out_offsets: Vec<u32>,
    /// In-edges of block `b` are `in_edges[in_offsets[b]..in_offsets[b + 1]]`.
    in_offsets: Vec<u32>,
    in_edges: Vec<u32>,
    live_in: Vec<u32>,
    edges_pruned_since_walk: bool,
    resolved_param_blocks: Vec<BlockId>,

    // Consequences of pruning, settled together.
    newly_dead: Vec<BlockId>,
    touched_blocks: Vec<BlockId>,
    touched_slots: Vec<u32>,
    recheck_every_slot: bool,
}

impl Driver {
    /// Build the driver, and the use index its events consult. The index is
    /// kept apart so a users slice can stay borrowed while an attempt edits
    /// the CFG and the driver.
    fn new(cfg: &Cfg) -> (Self, CfgUseIndex) {
        let value_count = cfg.value_count();
        let num_locals = cfg.num_locals() as usize;
        let block_count = cfg.block_count();

        // This index intentionally remains the pre-fold snapshot while the
        // event loop replaces instructions with constants. That is the
        // established sparse algorithm: every original foldable edge produces
        // one re-attempt per event on its operand. Rebuilding after each fold
        // would both change the RUE-1868 counter meaning and turn the pass
        // into an incremental-use-list project.
        let mut use_index = CfgUseIndex::default();
        use_index
            .rebuild(
                cfg,
                (0..value_count)
                    .map(|i| CfgValue::from_raw(i as u32))
                    .filter(|&value| constfold::is_foldable_instruction(cfg, value)),
            )
            .expect("verified CFG operands belong to this value domain");

        let mut loads_of_slot: Vec<Vec<CfgValue>> = vec![Vec::new(); num_locals];
        for i in 0..value_count {
            let value = CfgValue::from_raw(i as u32);
            if let CfgInstData::Load { slot } = &cfg.get_inst(value).data
                && let Some(loads) = loads_of_slot.get_mut(*slot as usize)
            {
                loads.push(value);
            }
        }

        // Blocks the entry cannot reach never execute; edges out of them are
        // dead from the start, and their writes never count.
        let reachable = dce::compute_reachable_blocks(cfg);
        let block_live: Vec<bool> = (0..block_count)
            .map(|index| reachable.contains(index as u32))
            .collect();
        let slot_writes =
            slot_facts::LiveSlotWrites::new(cfg, |block| block_live[block.as_u32() as usize]);

        let mut edges = Vec::with_capacity(block_count * 2);
        let mut edge_args = Vec::new();
        let mut out_offsets = Vec::with_capacity(block_count + 1);
        let mut live_in = vec![0u32; block_count];
        let mut watchers = Watchers::new(value_count);
        let mut push_edge =
            |edges: &mut Vec<Edge>, live: bool, target: BlockId, args: &[CfgValue]| {
                let args_start = edge_args.len() as u32;
                edge_args.extend_from_slice(args);
                if live {
                    live_in[target.as_u32() as usize] += 1;
                }
                edges.push(Edge {
                    target,
                    args_start,
                    args_len: args.len() as u32,
                    live,
                });
            };
        for block in cfg.blocks() {
            out_offsets.push(edges.len() as u32);
            let live = block_live[block.id.as_u32() as usize];
            match &block.terminator {
                Terminator::Goto { target, args } => {
                    push_edge(&mut edges, live, *target, cfg.goto_args(args));
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                } => {
                    push_edge(&mut edges, live, *then_block, cfg.then_args(then_args));
                    push_edge(&mut edges, live, *else_block, cfg.else_args(else_args));
                    if live {
                        watchers.add(*cond, Watch::Terminator(block.id));
                    }
                }
                Terminator::Switch {
                    scrutinee,
                    cases,
                    default,
                } => {
                    for (_, target) in cfg.switch_cases(cases) {
                        push_edge(&mut edges, live, *target, &[]);
                    }
                    push_edge(&mut edges, live, *default, &[]);
                    if live {
                        watchers.add(*scrutinee, Watch::Terminator(block.id));
                    }
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
        }
        out_offsets.push(edges.len() as u32);

        // Incoming edges matter only to block parameters; a graph whose
        // non-entry blocks take none skips the index and the argument
        // watchers entirely.
        let any_params = cfg
            .blocks()
            .iter()
            .any(|block| block.id != cfg.entry && !block.params.is_empty());
        let (in_offsets, in_edges) = if any_params {
            // Counting sort of edges by target: after the backwards fill,
            // `in_offsets[b]` is the start of block b's in-edges.
            let mut in_offsets = vec![0u32; block_count + 1];
            for edge in &edges {
                in_offsets[edge.target.as_u32() as usize + 1] += 1;
            }
            for index in 0..block_count {
                in_offsets[index + 1] += in_offsets[index];
            }
            let mut in_edges = vec![0u32; edges.len()];
            for (index, edge) in edges.iter().enumerate().rev() {
                let at = &mut in_offsets[edge.target.as_u32() as usize + 1];
                *at -= 1;
                in_edges[*at as usize] = index as u32;
            }
            in_offsets.rotate_left(1);
            in_offsets[block_count] = edges.len() as u32;
            for (index, edge) in edges.iter().enumerate() {
                if edge.live {
                    for position in 0..edge.args_len {
                        let arg = edge_args[(edge.args_start + position) as usize];
                        watchers.add(
                            arg,
                            Watch::Argument {
                                edge: index as u32,
                                position,
                            },
                        );
                    }
                }
            }
            (in_offsets, in_edges)
        } else {
            (Vec::new(), Vec::new())
        };

        let driver = Self {
            stats: Stats::default(),
            entry: cfg.entry,
            queue: VecDeque::new(),
            alias: Vec::new(),
            value_count,
            loads_of_slot,
            slot_writes,
            slot_settled: vec![false; num_locals],
            block_live,
            edges,
            edge_args,
            out_offsets,
            in_offsets,
            in_edges,
            live_in,
            watchers,
            edges_pruned_since_walk: false,
            resolved_param_blocks: Vec::new(),
            newly_dead: Vec::new(),
            touched_blocks: Vec::new(),
            touched_slots: Vec::new(),
            recheck_every_slot: false,
        };
        (driver, use_index)
    }

    /// One fold attempt per instruction, in index order. Every value that is
    /// constant — originally or by folding — enters the event queue exactly
    /// once. Then every slot and block parameter is checked once against the
    /// initial facts; later checks are triggered by events.
    fn seed(&mut self, cfg: &mut Cfg) {
        for i in 0..cfg.value_count() {
            let value = CfgValue::from_raw(i as u32);
            if const_payload(cfg, value).is_some() {
                self.queue.push_back(Event::Const(value));
            } else {
                self.attempt(cfg, value);
            }
        }
        for slot in 0..self.slot_settled.len() as u32 {
            self.check_slot(cfg, slot);
        }
        for index in 0..cfg.block_count() {
            let block = BlockId::from_raw(index as u32);
            self.check_params(cfg, block);
        }
    }

    /// Process events until nothing changes, retiring dead cycles whenever the
    /// queue drains after edges were pruned.
    fn drain(&mut self, cfg: &mut Cfg, use_index: &CfgUseIndex) {
        loop {
            while let Some(event) = self.queue.pop_front() {
                match event {
                    Event::Const(value) => self.on_const(cfg, use_index, value),
                    Event::Alias(value) => self.retry_users(cfg, use_index, value),
                }
            }
            if !self.edges_pruned_since_walk || !self.retire_unreachable(cfg) {
                return;
            }
        }
    }

    /// Re-attempt the foldable users of a value that became constant or
    /// aliased.
    fn retry_users(&mut self, cfg: &mut Cfg, use_index: &CfgUseIndex, value: CfgValue) {
        for &user in use_index
            .users(cfg, value)
            .expect("constopt owns the indexed CFG and preserves value types")
        {
            self.attempt(cfg, user);
        }
    }

    /// Try to fold `value`; failing that, record it as an identity alias.
    fn attempt(&mut self, cfg: &mut Cfg, value: CfgValue) {
        let alias = &self.alias;
        let same = |a: CfgValue, b: CfgValue| resolve(alias, a) == resolve(alias, b);
        self.stats.fold_attempts += 1;
        if constfold::fold_instruction(cfg, value, same) {
            self.stats.folded += 1;
            self.queue.push_back(Event::Const(value));
            return;
        }
        // Every identity shape is a foldable operation, so the cheap shape
        // test screens out loads, calls, and the rest first.
        if self.alias_of(value).is_none()
            && constfold::is_foldable_instruction(cfg, value)
            && let Some(target) = peephole::identity_target(cfg, value, same)
        {
            self.record_alias(value, target);
        }
    }

    fn alias_of(&self, value: CfgValue) -> Option<CfgValue> {
        self.alias.get(value.as_u32() as usize).copied().flatten()
    }

    /// Record `value` as always equal to `target`, unless that would close a
    /// cycle (possible only through the parameters of a not-yet-retired dead
    /// loop).
    fn record_alias(&mut self, value: CfgValue, target: CfgValue) {
        let root = resolve(&self.alias, target);
        if root == value {
            return;
        }
        if self.alias.is_empty() {
            self.alias = vec![None; self.value_count];
        }
        self.alias[value.as_u32() as usize] = Some(root);
        self.stats.aliases_recorded += 1;
        self.queue.push_back(Event::Alias(value));
    }

    fn on_const(&mut self, cfg: &mut Cfg, use_index: &CfgUseIndex, value: CfgValue) {
        self.retry_users(cfg, use_index, value);

        while let Some(watch) = self.watchers.pop(value) {
            match watch {
                Watch::Slot(slot) => self.propagate_slot(cfg, slot, value),
                Watch::Terminator(block) => {
                    if self.block_live[block.as_u32() as usize] {
                        self.prune_terminator(cfg, block);
                    }
                }
                Watch::Argument { edge, position } => {
                    let edge = &self.edges[edge as usize];
                    if edge.live {
                        let target = edge.target;
                        self.check_param(cfg, target, position as usize);
                    }
                }
            }
        }
    }

    /// Replace every `Load` of `slot` with the constant `write` holds.
    fn propagate_slot(&mut self, cfg: &mut Cfg, slot: u32, write: CfgValue) {
        let payload =
            const_payload(cfg, write).expect("slot propagation waits for a constant write");
        for index in 0..self.loads_of_slot[slot as usize].len() {
            let load = self.loads_of_slot[slot as usize][index];
            if cfg.is_ownership_boundary_value(load) {
                continue;
            }
            cfg.get_inst_mut(load).data = payload.duplicate_with_owner();
            self.stats.loads_rewritten += 1;
            self.queue.push_back(Event::Const(load));
        }
    }

    /// Settle a slot that may now have a single live write: propagate it at
    /// once if the write is already constant, else wait for the write.
    fn check_slot(&mut self, cfg: &mut Cfg, slot: u32) {
        if self.slot_settled[slot as usize] {
            return;
        }
        let Some(write) = self.slot_writes.single_live_write(cfg, slot) else {
            return;
        };
        self.slot_settled[slot as usize] = true;
        if const_payload(cfg, write).is_some() {
            self.propagate_slot(cfg, slot, write);
        } else {
            self.watchers.add(write, Watch::Slot(slot));
        }
    }

    fn check_params(&mut self, cfg: &mut Cfg, block: BlockId) {
        for position in 0..cfg.get_block(block).params.len() {
            self.check_param(cfg, block, position);
        }
    }

    /// Resolve parameter `position` of `block` to the constant every live
    /// incoming edge passes, or alias it to the value its only live incoming
    /// edge passes.
    fn check_param(&mut self, cfg: &mut Cfg, block: BlockId, position: usize) {
        let index = block.as_u32() as usize;
        if !self.block_live[index] || block == self.entry {
            return;
        }
        let param = cfg.get_block(block).params[position].0;
        if const_payload(cfg, param).is_some() || cfg.is_ownership_boundary_value(param) {
            return;
        }
        let mut payload: Option<CfgInstData> = None;
        let mut all_constant = true;
        let mut live_edges = 0u32;
        let mut only_arg = None;
        for &edge in
            &self.in_edges[self.in_offsets[index] as usize..self.in_offsets[index + 1] as usize]
        {
            let edge = &self.edges[edge as usize];
            if !edge.live {
                continue;
            }
            live_edges += 1;
            let arg = self.edge_args[(edge.args_start as usize) + position];
            only_arg = Some(arg);
            if !all_constant {
                continue;
            }
            match (const_payload(cfg, arg), &payload) {
                (Some(constant), None) => payload = Some(constant),
                (Some(constant), Some(seen)) if same_payload(&constant, seen) => {}
                _ => all_constant = false,
            }
        }
        if live_edges == 0 {
            return;
        }
        if all_constant && let Some(constant) = payload {
            cfg.get_inst_mut(param).data = constant;
            self.stats.params_resolved += 1;
            if self.resolved_param_blocks.last() != Some(&block) {
                self.resolved_param_blocks.push(block);
            }
            self.queue.push_back(Event::Const(param));
            return;
        }
        if live_edges == 1
            && let Some(arg) = only_arg
            && self.alias_of(param).is_none()
        {
            self.record_alias(param, arg);
        }
    }

    /// Kill the edges a now-constant terminator cannot take.
    fn prune_terminator(&mut self, cfg: &mut Cfg, block: BlockId) {
        let index = block.as_u32() as usize;
        let range = self.out_offsets[index]..self.out_offsets[index + 1];
        let terminator = &cfg.get_block(block).terminator;
        match simplify::constant_successor(cfg, terminator) {
            Some(ConstantSuccessor::Then) => self.kill_edge(range.start + 1),
            Some(ConstantSuccessor::Else) => self.kill_edge(range.start),
            Some(ConstantSuccessor::Switch(taken)) => {
                for edge in range {
                    if self.edges[edge as usize].target != taken {
                        self.kill_edge(edge);
                    }
                }
            }
            None => return,
        }
        self.settle(cfg);
    }

    fn kill_edge(&mut self, edge: u32) {
        let edge = &mut self.edges[edge as usize];
        if !edge.live {
            return;
        }
        edge.live = false;
        let target = edge.target;
        self.stats.edges_pruned += 1;
        self.edges_pruned_since_walk = true;
        let index = target.as_u32() as usize;
        self.live_in[index] -= 1;
        if self.live_in[index] == 0 && target != self.entry && self.block_live[index] {
            self.kill_block(target);
        } else {
            self.touched_blocks.push(target);
        }
    }

    fn kill_block(&mut self, block: BlockId) {
        self.block_live[block.as_u32() as usize] = false;
        self.stats.blocks_proven_dead += 1;
        self.newly_dead.push(block);
    }

    /// Propagate dead blocks to their out-edges, then recheck what the deaths
    /// may have unblocked.
    fn settle(&mut self, cfg: &mut Cfg) {
        while let Some(block) = self.newly_dead.pop() {
            self.recheck_every_slot |=
                self.slot_writes
                    .retire_block(cfg, block, &mut self.touched_slots);
            let index = block.as_u32() as usize;
            for edge in self.out_offsets[index]..self.out_offsets[index + 1] {
                self.kill_edge(edge);
            }
        }
        while let Some(block) = self.touched_blocks.pop() {
            self.check_params(cfg, block);
        }
        if std::mem::take(&mut self.recheck_every_slot) {
            self.touched_slots.clear();
            for slot in 0..self.slot_settled.len() as u32 {
                self.check_slot(cfg, slot);
            }
        }
        while let Some(slot) = self.touched_slots.pop() {
            self.check_slot(cfg, slot);
        }
    }

    /// Kill every live block the entry cannot reach over live edges: a dead
    /// cycle keeps its own incoming count above zero. Returns whether any
    /// block died.
    fn retire_unreachable(&mut self, cfg: &mut Cfg) -> bool {
        self.edges_pruned_since_walk = false;
        self.stats.reachability_walks += 1;
        let block_count = self.block_live.len();
        let mut reached = vec![false; block_count];
        let mut stack = vec![self.entry];
        reached[self.entry.as_u32() as usize] = true;
        while let Some(block) = stack.pop() {
            let index = block.as_u32() as usize;
            for edge in
                &self.edges[self.out_offsets[index] as usize..self.out_offsets[index + 1] as usize]
            {
                let target = edge.target.as_u32() as usize;
                if edge.live && !reached[target] {
                    reached[target] = true;
                    stack.push(edge.target);
                }
            }
        }
        let mut any = false;
        for (index, reached) in reached.into_iter().enumerate() {
            if self.block_live[index] && !reached {
                self.kill_block(BlockId::from_raw(index as u32));
                any = true;
            }
        }
        if any {
            self.settle(cfg);
        }
        any
    }

    /// Remove every resolved parameter from its block, and its argument from
    /// every edge into that block. Each resolved parameter's value already
    /// holds its constant, so it moves to the head of the block's
    /// instructions and every use keeps its value identity.
    fn remove_resolved_params(&mut self, cfg: &mut Cfg) -> Result<(), crate::CfgEditError> {
        if self.resolved_param_blocks.is_empty() {
            return Ok(());
        }
        let block_count = cfg.block_count();
        let mut kept: Vec<Option<Vec<bool>>> = vec![None; block_count];
        for &block in &self.resolved_param_blocks {
            if kept[block.as_u32() as usize].is_some() {
                continue;
            }
            let params = std::mem::take(&mut cfg.get_block_mut(block).params);
            let mut keep = Vec::with_capacity(params.len());
            let mut remaining = Vec::with_capacity(params.len());
            let mut constants = Vec::new();
            for (value, ty) in params {
                if const_payload(cfg, value).is_some() {
                    keep.push(false);
                    constants.push(value);
                } else {
                    keep.push(true);
                    cfg.get_inst_mut(value).data = CfgInstData::BlockParam {
                        index: remaining.len() as u32,
                    };
                    remaining.push((value, ty));
                }
            }
            let block_data = cfg.get_block_mut(block);
            block_data.params = remaining;
            constants.append(&mut block_data.insts);
            block_data.insts = constants;
            kept[block.as_u32() as usize] = Some(keep);
        }

        let retained = |kept: &[Option<Vec<bool>>], target: BlockId, args: &[CfgValue]| {
            kept[target.as_u32() as usize].as_ref().map(|keep| {
                args.iter()
                    .zip(keep)
                    .filter_map(|(arg, keep)| keep.then_some(*arg))
                    .collect::<Vec<_>>()
            })
        };
        for index in 0..block_count {
            let block = BlockId::from_raw(index as u32);
            match &cfg.get_block(block).terminator {
                Terminator::Goto { target, args } => {
                    let target = *target;
                    if let Some(values) = retained(&kept, target, cfg.goto_args(args)) {
                        let args = cfg.push_goto_args(values)?;
                        cfg.get_block_mut(block).terminator = Terminator::Goto { target, args };
                    }
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                } => {
                    let (cond, then_block, else_block) = (*cond, *then_block, *else_block);
                    let then_values = retained(&kept, then_block, cfg.then_args(then_args));
                    let else_values = retained(&kept, else_block, cfg.else_args(else_args));
                    if then_values.is_none() && else_values.is_none() {
                        continue;
                    }
                    let then_values =
                        then_values.unwrap_or_else(|| cfg.then_args(then_args).to_vec());
                    let else_values =
                        else_values.unwrap_or_else(|| cfg.else_args(else_args).to_vec());
                    let then_args = cfg.push_then_args(then_values)?;
                    let else_args = cfg.push_else_args(else_values)?;
                    cfg.get_block_mut(block).terminator = Terminator::Branch {
                        cond,
                        then_block,
                        then_args,
                        else_block,
                        else_args,
                    };
                }
                Terminator::Switch { .. }
                | Terminator::Return { .. }
                | Terminator::Unreachable
                | Terminator::None => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgArgMode, CfgCallArg, CfgInst, Terminator, Type};
    use lasso::{Key, Spur};
    use rue_span::Span;

    fn make_cfg(num_locals: u32) -> Cfg {
        let mut cfg = Cfg::new(Type::I32, num_locals, 0, "test".to_string(), vec![]);
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

    #[test]
    fn test_propagates_single_const_alloc() {
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(7), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 1);
        match &cfg.get_inst(load).data {
            CfgInstData::Const(7) => {}
            other => panic!("Expected Const(7), got {:?}", other),
        }
    }

    #[test]
    fn test_propagates_bool_const() {
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::BoolConst(true), Type::BOOL);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::BOOL);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::BoolConst(true)
        ));
    }

    #[test]
    fn test_mutated_slot_not_propagated() {
        // let a = 1; a = 2; a -> two writes, must not propagate either.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(1), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c1 },
            Type::UNIT,
        );
        let c2 = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Store { slot: 0, value: c2 },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_address_taken_slot_not_propagated() {
        // let x = 42; @raw(x) — CfgBuilder marks x's slot address-taken, so
        // its Load must survive as a place even though the slot has exactly
        // one constant write. Rewriting it to Const(42) makes codegen
        // dereference 42 as an address (RUE-521 O1+ segfault).
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(42), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg.push_intrinsic_args(vec![load]).unwrap();
        let raw_sym = Spur::try_from_usize(0).unwrap();
        let ptr = push(
            &mut cfg,
            CfgInstData::Intrinsic {
                operation: rue_air::IntrinsicOperation::Raw,
                name: raw_sym,
                args,
            },
            Type::I32,
        );
        cfg.mark_address_taken(0);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(ptr) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_ownership_boundary_slot_not_propagated() {
        // An inlined by-value parameter is initialized once, but the Load is
        // the callee-owned read and must not be folded back to the caller's
        // initializer during reoptimization.
        let mut cfg = make_cfg(1);
        let value = push(&mut cfg, CfgInstData::Const(7), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: value,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.mark_ownership_boundary(0);
        cfg.mark_ownership_boundary_value(load);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_mixed_boundary_and_copy_slots_only_protect_the_boundary_value() {
        let mut cfg = make_cfg(2);
        let owned_init = push(&mut cfg, CfgInstData::Const(7), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: owned_init,
            },
            Type::UNIT,
        );
        let owned_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.mark_ownership_boundary(0);
        cfg.mark_ownership_boundary_value(owned_load);

        let copy_init = push(&mut cfg, CfgInstData::Const(9), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 1,
                init: copy_init,
            },
            Type::UNIT,
        );
        let copy_load = push(&mut cfg, CfgInstData::Load { slot: 1 }, Type::I32);
        cfg.set_terminator(
            cfg.entry,
            Terminator::Return {
                value: Some(copy_load),
            },
        );

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(
            cfg.get_inst(owned_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(copy_load).data,
            CfgInstData::Const(9)
        ));
    }

    #[test]
    fn test_non_const_init_not_propagated() {
        // The slot's single write is a Param — never constant, never
        // propagated.
        let mut cfg = make_cfg(1);
        let p = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: p },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_folded_init_propagates_after_seed() {
        // let a = 3 + 4; a — at classification time the init is a non-const
        // Add. The old driver caught this on its second rescan round; the
        // sparse driver must catch it when the Add's fold event resolves the
        // watching slot.
        let mut cfg = make_cfg(1);
        let c1 = push(&mut cfg, CfgInstData::Const(3), Type::I32);
        let c2 = push(&mut cfg, CfgInstData::Const(4), Type::I32);
        let sum = push(&mut cfg, CfgInstData::Add(c1, c2), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: sum },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.folded, 1);
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(cfg.get_inst(load).data, CfgInstData::Const(7)));
    }

    #[test]
    fn test_byref_call_arg_disqualifies_slot() {
        // f(inout a): the callee may rewrite a, and the by-ref lowering
        // needs the Load to remain a place. Nothing may be propagated.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Inout,
            }])
            .unwrap();
        push(
            &mut cfg,
            CfgInstData::Call {
                runtime: None,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_byref_accessor_call_arg_disqualifies_slot() {
        // An accessor can write through a by-ref argument too, so constant
        // propagation must not substitute either the place argument or a
        // later load with the pre-accessor constant.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Inout,
            }])
            .unwrap();
        push(
            &mut cfg,
            CfgInstData::AccessorCall {
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(arg_load).data,
            CfgInstData::Load { slot: 0 }
        ));
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_zero_slot_local_out_of_range_is_ignored() {
        // Historically a zero-sized local ([T; 0], unit) occupied 0 slots, so a
        // trailing one was assigned slot index == num_locals — out of range
        // for the slot table (source locals have their own slot since
        // RUE-2453). Its Alloc/Load must be skipped, not panic (RUE-194).
        let mut cfg = make_cfg(0);
        let c = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::UNIT);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_normal_call_arg_still_propagates() {
        // f(a) by value: the slot is only read; propagation is fine.
        let mut cfg = make_cfg(1);
        let c = push(&mut cfg, CfgInstData::Const(5), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc { slot: 0, init: c },
            Type::UNIT,
        );
        let arg_load = push(&mut cfg, CfgInstData::Load { slot: 0 }, Type::I32);
        let args = cfg
            .push_call_args([CfgCallArg {
                value: arg_load,
                mode: CfgArgMode::Normal,
            }])
            .unwrap();
        push(
            &mut cfg,
            CfgInstData::Call {
                runtime: None,
                name: Spur::try_from_usize(0).unwrap(),
                args,
            },
            Type::UNIT,
        );
        cfg.set_terminator(cfg.entry, Terminator::Return { value: None });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(cfg.get_inst(arg_load).data, CfgInstData::Const(5)));
    }

    /// Build the CFG shape CfgBuilder produces for a `let x{i} = x{i-1} + 1`
    /// chain of the given length: Alloc slot 0 with a constant, then each
    /// binding Loads the previous slot, adds a constant, and Allocs the next
    /// slot. Returns the final Load.
    fn build_let_chain(cfg: &mut Cfg, len: u32) -> CfgValue {
        let zero = push(cfg, CfgInstData::Const(0), Type::I64);
        push(
            cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        for i in 1..len {
            let prev = push(cfg, CfgInstData::Load { slot: i - 1 }, Type::I64);
            let one = push(cfg, CfgInstData::Const(1), Type::I64);
            let sum = push(cfg, CfgInstData::Add(prev, one), Type::I64);
            push(cfg, CfgInstData::Alloc { slot: i, init: sum }, Type::UNIT);
        }
        push(cfg, CfgInstData::Load { slot: len - 1 }, Type::I64)
    }

    fn build_wrapping_let_chain(cfg: &mut Cfg, len: u32) -> CfgValue {
        let zero = push(cfg, CfgInstData::Const(0), Type::U64);
        push(
            cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        for i in 1..len {
            let prev = push(cfg, CfgInstData::Load { slot: i - 1 }, Type::U64);
            let one = push(cfg, CfgInstData::Const(1), Type::U64);
            let sum = push(cfg, CfgInstData::WrappingAdd(prev, one), Type::U64);
            push(cfg, CfgInstData::Alloc { slot: i, init: sum }, Type::UNIT);
        }
        push(cfg, CfgInstData::Load { slot: len - 1 }, Type::U64)
    }

    #[test]
    fn test_let_chain_fully_propagates() {
        // The RUE-794 workload: every binding's constant is exposed only by
        // propagating the previous one. The whole chain must still fold.
        let len = 100;
        let mut cfg = make_cfg(len);
        let last = build_let_chain(&mut cfg, len);
        cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(last) });

        run(&mut cfg).unwrap();
        match &cfg.get_inst(last).data {
            CfgInstData::Const(v) => assert_eq!(*v, (len - 1) as u64),
            other => panic!("Expected Const({}), got {:?}", len - 1, other),
        }
    }

    #[test]
    fn test_let_chain_work_is_linear() {
        // Structural work bound (RUE-794 acceptance criteria): the driver
        // performs one seed fold attempt per value plus at most one
        // re-attempt per operand→user edge (≤ 2 per foldable instruction),
        // regardless of chain depth. The old rescan loop needed O(len)
        // full-CFG rounds here — ~len²/8 attempts — so this bound fails
        // loudly on any regression to global rescanning.
        for len in [100, 200, 400] {
            let mut cfg = make_cfg(len);
            let last = build_let_chain(&mut cfg, len);
            cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(last) });

            let stats = run(&mut cfg).unwrap();
            let values = cfg.value_count() as u64;
            assert!(
                stats.fold_attempts <= 3 * values,
                "chain len {}: {} fold attempts for {} values exceeds the \
                 sparse bound",
                len,
                stats.fold_attempts,
                values
            );
            // Every Add folded and every Load was rewritten.
            assert_eq!(stats.folded, (len - 1) as u64);
            assert_eq!(stats.loads_rewritten, len as u64);
        }
    }

    #[test]
    fn test_wrapping_let_chain_work_is_linear() {
        for len in [100, 200, 400] {
            let mut cfg = make_cfg(len);
            let last = build_wrapping_let_chain(&mut cfg, len);
            cfg.set_terminator(cfg.entry, Terminator::Return { value: Some(last) });

            let stats = run(&mut cfg).unwrap();
            let values = cfg.value_count() as u64;
            assert!(
                stats.fold_attempts <= 3 * values,
                "wrapping chain len {}: {} fold attempts for {} values exceeds the sparse bound",
                len,
                stats.fold_attempts,
                values
            );
            assert_eq!(stats.folded, (len - 1) as u64);
            assert_eq!(stats.loads_rewritten, len as u64);
            assert!(matches!(
                cfg.get_inst(last).data,
                CfgInstData::Const(v) if v == (len - 1) as u64
            ));
        }
    }

    // ---------------------------------------------------------------
    // Constant control flow (RUE-2545)
    // ---------------------------------------------------------------

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

    fn goto(cfg: &mut Cfg, from: BlockId, target: BlockId, args: Vec<CfgValue>) {
        let args = cfg.push_goto_args(args).unwrap();
        cfg.set_terminator(from, Terminator::Goto { target, args });
    }

    fn branch(
        cfg: &mut Cfg,
        from: BlockId,
        cond: CfgValue,
        (then_block, then_args): (BlockId, Vec<CfgValue>),
        (else_block, else_args): (BlockId, Vec<CfgValue>),
    ) {
        let then_args = cfg.push_then_args(then_args).unwrap();
        let else_args = cfg.push_else_args(else_args).unwrap();
        cfg.set_terminator(
            from,
            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            },
        );
    }

    /// The structure CFG verification checks must survive parameter removal.
    fn assert_verifies(cfg: Cfg) {
        let pool = rue_air::TypeInternPool::new().freeze();
        cfg.finish(&pool)
            .expect("constopt leaves edge arity and parameter numbering valid");
    }

    /// `len` dependent diamonds: diamond i branches on whether the previous
    /// join's parameter is zero; its then-arm passes zero on and its else-arm
    /// passes the function parameter. Only the first join's argument is
    /// constant before folding, so each join is constant only once the branch
    /// before it is.
    fn build_diamond_chain(len: usize) -> (Cfg, CfgValue) {
        let mut cfg = Cfg::new(Type::I64, 0, 1, "diamonds".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = push_in(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I64);
        let zero = push_in(&mut cfg, entry, CfgInstData::Const(0), Type::I64);
        let mut head = cfg.new_block();
        let mut param = cfg.add_block_param(head, Type::I64);
        goto(&mut cfg, entry, head, vec![zero]);
        for _ in 0..len {
            let then_block = cfg.new_block();
            let else_block = cfg.new_block();
            let next = cfg.new_block();
            let next_param = cfg.add_block_param(next, Type::I64);
            let local_zero = push_in(&mut cfg, head, CfgInstData::Const(0), Type::I64);
            let cond = push_in(
                &mut cfg,
                head,
                CfgInstData::Eq(param, local_zero),
                Type::BOOL,
            );
            branch(
                &mut cfg,
                head,
                cond,
                (then_block, vec![]),
                (else_block, vec![]),
            );
            let passed_zero = push_in(&mut cfg, then_block, CfgInstData::Const(0), Type::I64);
            goto(&mut cfg, then_block, next, vec![passed_zero]);
            goto(&mut cfg, else_block, next, vec![x]);
            head = next;
            param = next_param;
        }
        cfg.set_terminator(head, Terminator::Return { value: Some(param) });
        (cfg, param)
    }

    #[test]
    fn test_join_param_chain_resolves_in_one_run() {
        // The RUE-2545 shape: before, every diamond cost one cleanup round
        // (constopt, then simplify folding the branch and merging the join).
        for len in [100, 200, 400] {
            let (mut cfg, last) = build_diamond_chain(len);
            let stats = run(&mut cfg).unwrap();
            let values = cfg.value_count() as u64;
            assert_eq!(stats.params_resolved, len as u64 + 1);
            // Each untaken branch edge, and the dead arm's own edge to the join.
            assert_eq!(stats.edges_pruned, 2 * len as u64);
            assert_eq!(stats.blocks_proven_dead, len as u64);
            assert!(matches!(cfg.get_inst(last).data, CfgInstData::Const(0)));
            assert!(
                stats.fold_attempts <= 3 * values,
                "chain len {len}: {} fold attempts for {values} values",
                stats.fold_attempts
            );
            assert!(cfg.blocks().iter().all(|block| block.params.is_empty()));
            assert_verifies(cfg);
        }
    }

    #[test]
    fn test_join_with_two_live_edges_keeps_its_param() {
        // A runtime condition keeps both arms live, and they pass different
        // constants: the join parameter stays a parameter.
        let (mut cfg, _) = build_diamond_chain(0);
        let entry = cfg.entry;
        let x = cfg.get_block(entry).insts[0];
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let join = cfg.new_block();
        let param = cfg.add_block_param(join, Type::I64);
        // Rebuild the entry terminator as a runtime branch.
        let zero = push_in(&mut cfg, entry, CfgInstData::Const(0), Type::I64);
        let cond = push_in(&mut cfg, entry, CfgInstData::Gt(x, zero), Type::BOOL);
        cfg.get_block_mut(entry).terminator = Terminator::Unreachable;
        branch(
            &mut cfg,
            entry,
            cond,
            (then_block, vec![]),
            (else_block, vec![]),
        );
        let one = push_in(&mut cfg, then_block, CfgInstData::Const(1), Type::I64);
        goto(&mut cfg, then_block, join, vec![one]);
        let two = push_in(&mut cfg, else_block, CfgInstData::Const(2), Type::I64);
        goto(&mut cfg, else_block, join, vec![two]);
        let diff = push_in(&mut cfg, join, CfgInstData::Sub(param, x), Type::I64);
        cfg.set_terminator(join, Terminator::Return { value: Some(diff) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.params_resolved, 0);
        assert_eq!(stats.aliases_recorded, 0);
        assert_eq!(stats.edges_pruned, 0);
        assert!(matches!(
            cfg.get_inst(param).data,
            CfgInstData::BlockParam { index: 0 }
        ));
        assert!(matches!(cfg.get_inst(diff).data, CfgInstData::Sub(..)));
    }

    #[test]
    fn test_loop_carried_param_is_not_resolved() {
        // header(p): p is 0 on entry and p + 1 around the back edge. Both
        // edges stay live, and only one of them is constant.
        let mut cfg = Cfg::new(Type::I64, 0, 1, "loop".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = push_in(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I64);
        let zero = push_in(&mut cfg, entry, CfgInstData::Const(0), Type::I64);
        let header = cfg.new_block();
        let exit = cfg.new_block();
        let p = cfg.add_block_param(header, Type::I64);
        goto(&mut cfg, entry, header, vec![zero]);
        let one = push_in(&mut cfg, header, CfgInstData::Const(1), Type::I64);
        let next = push_in(&mut cfg, header, CfgInstData::Add(p, one), Type::I64);
        let more = push_in(&mut cfg, header, CfgInstData::Lt(next, x), Type::BOOL);
        branch(&mut cfg, header, more, (header, vec![next]), (exit, vec![]));
        cfg.set_terminator(exit, Terminator::Return { value: Some(p) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.params_resolved, 0);
        assert!(matches!(
            cfg.get_inst(p).data,
            CfgInstData::BlockParam { index: 0 }
        ));
        assert_verifies(cfg);
    }

    #[test]
    fn test_partially_resolved_params_are_renumbered() {
        // join(a, b): a is constant on the only live edge, b is the runtime
        // parameter. Removing a renumbers b and drops a's argument from
        // every edge into the join, including the dead one.
        let mut cfg = Cfg::new(Type::I64, 0, 1, "renumber".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = push_in(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I64);
        let yes = push_in(&mut cfg, entry, CfgInstData::BoolConst(true), Type::BOOL);
        let three = push_in(&mut cfg, entry, CfgInstData::Const(3), Type::I64);
        let four = push_in(&mut cfg, entry, CfgInstData::Const(4), Type::I64);
        let join = cfg.new_block();
        let a = cfg.add_block_param(join, Type::I64);
        let b = cfg.add_block_param(join, Type::I64);
        branch(
            &mut cfg,
            entry,
            yes,
            (join, vec![three, x]),
            (join, vec![four, x]),
        );
        let sum = push_in(&mut cfg, join, CfgInstData::Add(a, b), Type::I64);
        cfg.set_terminator(join, Terminator::Return { value: Some(sum) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.params_resolved, 1);
        assert!(matches!(cfg.get_inst(a).data, CfgInstData::Const(3)));
        assert!(matches!(
            cfg.get_inst(b).data,
            CfgInstData::BlockParam { index: 0 }
        ));
        assert_eq!(cfg.get_block(join).params.len(), 1);
        assert_eq!(cfg.get_block(join).insts[0], a);
        let terminator = &cfg.get_block(entry).terminator;
        assert_eq!(cfg.get_branch_then_args(terminator), &[x]);
        assert_eq!(cfg.get_branch_else_args(terminator), &[x]);
        assert_verifies(cfg);
    }

    #[test]
    fn test_single_live_edge_param_folds_equal_operand_annihilator() {
        // join(p) is entered only from the then-arm, which passes x, so
        // p - x is zero even though x is not constant.
        let mut cfg = Cfg::new(Type::I64, 0, 2, "alias".to_string(), vec![false, false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = push_in(&mut cfg, entry, CfgInstData::Param { index: 0 }, Type::I64);
        let z = push_in(&mut cfg, entry, CfgInstData::Param { index: 1 }, Type::I64);
        let yes = push_in(&mut cfg, entry, CfgInstData::BoolConst(true), Type::BOOL);
        let join = cfg.new_block();
        let p = cfg.add_block_param(join, Type::I64);
        branch(&mut cfg, entry, yes, (join, vec![x]), (join, vec![z]));
        let diff = push_in(&mut cfg, join, CfgInstData::Sub(p, x), Type::I64);
        cfg.set_terminator(join, Terminator::Return { value: Some(diff) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.aliases_recorded, 1);
        assert!(matches!(cfg.get_inst(diff).data, CfgInstData::Const(0)));
        // The alias rewrites nothing: p stays a parameter for simplify.
        assert!(matches!(
            cfg.get_inst(p).data,
            CfgInstData::BlockParam { index: 0 }
        ));
        assert_verifies(cfg);
    }

    #[test]
    fn test_identity_alias_chain_folds_in_one_run() {
        // a0 = x - x; b_i = x + a_{i-1}; a_i = b_i - x. Each b_i is an
        // identity only once a_{i-1} folds to zero, and each a_i folds only
        // through that identity: the peephole pass used to expose one link
        // per cleanup round.
        let len = 200;
        let mut cfg = Cfg::new(Type::I64, 0, 1, "identities".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::I64);
        let mut a = push(&mut cfg, CfgInstData::Sub(x, x), Type::I64);
        for _ in 0..len {
            let b = push(&mut cfg, CfgInstData::Add(x, a), Type::I64);
            a = push(&mut cfg, CfgInstData::Sub(b, x), Type::I64);
        }
        cfg.set_terminator(entry, Terminator::Return { value: Some(a) });

        let stats = run(&mut cfg).unwrap();
        assert!(matches!(cfg.get_inst(a).data, CfgInstData::Const(0)));
        assert_eq!(stats.folded, len + 1);
        assert_eq!(stats.aliases_recorded, len);
        assert!(stats.fold_attempts <= 3 * cfg.value_count() as u64);
    }

    #[test]
    fn test_float_identity_is_not_an_alias() {
        // `x + 0.0` is not x for x = -0.0, so it records no alias and
        // `(x + 0.0) - x` stays.
        let mut cfg = Cfg::new(Type::F64, 0, 1, "float".to_string(), vec![false]);
        let entry = cfg.new_block();
        cfg.entry = entry;
        let x = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::F64);
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::F64);
        let sum = push(&mut cfg, CfgInstData::Add(x, zero), Type::F64);
        let diff = push(&mut cfg, CfgInstData::Sub(sum, x), Type::F64);
        cfg.set_terminator(entry, Terminator::Return { value: Some(diff) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.aliases_recorded, 0);
        assert!(matches!(cfg.get_inst(diff).data, CfgInstData::Sub(..)));
    }

    #[test]
    fn test_dead_arm_write_stops_disqualifying_its_slot() {
        // let mut y = 0; if false { y = 1 }; y -- the competing Store sits in
        // an arm a constant branch never takes.
        let mut cfg = make_cfg(1);
        let entry = cfg.entry;
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        let no = push(&mut cfg, CfgInstData::BoolConst(false), Type::BOOL);
        let dead = cfg.new_block();
        let join = cfg.new_block();
        branch(&mut cfg, entry, no, (dead, vec![]), (join, vec![]));
        let one = push_in(&mut cfg, dead, CfgInstData::Const(1), Type::I32);
        push_in(
            &mut cfg,
            dead,
            CfgInstData::Store {
                slot: 0,
                value: one,
            },
            Type::UNIT,
        );
        goto(&mut cfg, dead, join, vec![]);
        let load = push_in(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.blocks_proven_dead, 1);
        assert_eq!(stats.loads_rewritten, 1);
        assert!(matches!(cfg.get_inst(load).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_live_second_write_still_disqualifies_its_slot() {
        // The same shape with a runtime condition: both writes stay live.
        let mut cfg = make_cfg(1);
        let entry = cfg.entry;
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        let flag = push(&mut cfg, CfgInstData::Param { index: 0 }, Type::BOOL);
        let arm = cfg.new_block();
        let join = cfg.new_block();
        branch(&mut cfg, entry, flag, (arm, vec![]), (join, vec![]));
        let one = push_in(&mut cfg, arm, CfgInstData::Const(1), Type::I32);
        push_in(
            &mut cfg,
            arm,
            CfgInstData::Store {
                slot: 0,
                value: one,
            },
            Type::UNIT,
        );
        goto(&mut cfg, arm, join, vec![]);
        let load = push_in(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.loads_rewritten, 0);
        assert!(matches!(
            cfg.get_inst(load).data,
            CfgInstData::Load { slot: 0 }
        ));
    }

    #[test]
    fn test_dead_cycle_is_retired_by_the_reachability_walk() {
        // if false { loop { y = 1 } }: the loop's back edge keeps its header's
        // incoming count above zero, so only the walk from the entry proves
        // the loop dead and lets y's initializer propagate.
        let mut cfg = make_cfg(1);
        let entry = cfg.entry;
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        let no = push(&mut cfg, CfgInstData::BoolConst(false), Type::BOOL);
        let header = cfg.new_block();
        let join = cfg.new_block();
        branch(&mut cfg, entry, no, (header, vec![]), (join, vec![]));
        let one = push_in(&mut cfg, header, CfgInstData::Const(1), Type::I32);
        push_in(
            &mut cfg,
            header,
            CfgInstData::Store {
                slot: 0,
                value: one,
            },
            Type::UNIT,
        );
        goto(&mut cfg, header, header, vec![]);
        let load = push_in(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.reachability_walks, 2);
        assert_eq!(stats.blocks_proven_dead, 1);
        assert!(matches!(cfg.get_inst(load).data, CfgInstData::Const(0)));
    }

    #[test]
    fn test_constant_switch_prunes_every_other_case() {
        // switch 2 { 1 => a, 2 => b, _ => c }: writes in a and c are dead.
        let mut cfg = make_cfg(1);
        let entry = cfg.entry;
        let zero = push(&mut cfg, CfgInstData::Const(0), Type::I32);
        push(
            &mut cfg,
            CfgInstData::Alloc {
                slot: 0,
                init: zero,
            },
            Type::UNIT,
        );
        let scrutinee = push(&mut cfg, CfgInstData::Const(2), Type::I32);
        let [a, b, c, join] = [(); 4].map(|_| cfg.new_block());
        let cases = cfg.push_switch_cases([(1, a), (2, b)]).unwrap();
        cfg.set_terminator(
            entry,
            Terminator::Switch {
                scrutinee,
                cases,
                default: c,
            },
        );
        for (block, value) in [(a, 1), (c, 3)] {
            let value = push_in(&mut cfg, block, CfgInstData::Const(value), Type::I32);
            push_in(
                &mut cfg,
                block,
                CfgInstData::Store { slot: 0, value },
                Type::UNIT,
            );
            goto(&mut cfg, block, join, vec![]);
        }
        goto(&mut cfg, b, join, vec![]);
        let load = push_in(&mut cfg, join, CfgInstData::Load { slot: 0 }, Type::I32);
        cfg.set_terminator(join, Terminator::Return { value: Some(load) });

        let stats = run(&mut cfg).unwrap();
        assert_eq!(stats.blocks_proven_dead, 2);
        assert!(matches!(cfg.get_inst(load).data, CfgInstData::Const(0)));
    }
}
