//! Natural-loop analysis and preheader materialization (RUE-926).
//!
//! This is ADR-0054 Phase 1: the shape infrastructure LICM (RUE-927) and
//! constant-trip unrolling (RUE-928) build on. It has two halves:
//!
//! 1. **Natural-loop detection** ([`loops`]). Built on the shared
//!    [`DominatorTree`] (RUE-914): a *back edge* is an edge `u -> h` where `h`
//!    dominates `u`, and the natural loop of that back edge is `h` plus every
//!    block that can reach `u` without passing through `h`. Back edges sharing
//!    a header are one loop with several latches. The result is a [`LoopForest`]
//!    of [`NaturalLoop`]s carrying header, latches, body, nesting parent, and
//!    exit edges. **Irreducible** control flow — a retreating edge whose target
//!    does not dominate its source, i.e. a multi-entry cycle — is refused
//!    outright: the forest is flagged [`LoopForest::is_irreducible`] and carries
//!    no loops, so no consumer ever guesses a body for a tangle the dominator
//!    tree cannot describe.
//!
//! 2. **Preheader materialization** ([`ensure_preheader`]). A loop's preheader
//!    is a dedicated block that dominates the header and runs exactly once per
//!    loop entry — the block LICM hoists *into*. The header's predecessors split
//!    into **back-edge predecessors** (latches, inside the body) and **outside
//!    predecessors** (loop entries). When exactly one outside predecessor `p`
//!    targets the header unconditionally as its only successor, `p` already is a
//!    preheader and is reused. Otherwise a fresh block `ph` is inserted: every
//!    outside edge is redirected to `ph`, `ph` forwards the header's block
//!    arguments on with a single `Goto`, and the back edges keep targeting the
//!    header directly. The edge split is required not only for several outside
//!    predecessors but **also when the single outside predecessor has other
//!    successors** (the header is one arm of its `Branch`/`Switch`) — hoisting
//!    into such a predecessor would run the hoisted work on the sibling arm's
//!    path too.
//!
//! LICM (RUE-927, `opt/licm.rs`) is the live pipeline consumer: it runs at
//! `-O3` from `opt/mod.rs` and drives [`loops`] and [`ensure_preheader`].
//! Constant-trip unrolling (RUE-928) is the tracked next consumer; the few
//! items only it will need carry targeted allows below. Dominators and loops
//! are **recomputed per pass, never cached** (ADR-0054): every mutation here
//! invalidates the dominator tree, and recompute-per-pass is O(blocks) and
//! trivially correct at Rue's scale.

use crate::dominators::DominatorTree;
use crate::{BlockId, Cfg, CfgEditError, Terminator};
use ahash::AHashSet;
use rue_air::FrozenTypeInternPool;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static PREHEADER_PRED_SCAN_COUNT: Cell<usize> = const { Cell::new(0) };
}

use super::CfgOptimizationError;

/// Index of a [`NaturalLoop`] within its [`LoopForest`].
pub(crate) type LoopId = usize;

/// One natural loop: a single header, its back-edge latches, the body it
/// dominates, its immediate enclosing loop, and the edges that leave it.
pub(crate) struct NaturalLoop {
    /// The loop header — the sole entry, dominating every body block.
    pub(crate) header: BlockId,
    /// Back-edge sources: every block `u` with an edge `u -> header` where the
    /// header dominates `u`. Sorted and deduplicated. Several latches sharing
    /// one header are merged into this single loop (ADR-0054).
    // Read today only by this module's tests; RUE-928 constant-trip unrolling
    // is the tracked consumer (it rewrites the back edges).
    #[allow(dead_code)]
    pub(crate) latches: Vec<BlockId>,
    /// Every block in the loop, including the header. Sorted by block index, so
    /// [`NaturalLoop::contains`] is a binary search.
    pub(crate) body: Vec<BlockId>,
    /// The immediately enclosing loop, or `None` for a top-level loop. In a
    /// reducible CFG natural loops are properly nested, so this containment
    /// forest is well-defined.
    pub(crate) parent: Option<LoopId>,
    /// Exit edges `(from, to)`: `from` is in the body, `to` is not. The header
    /// itself can be an exit source (a loop whose trip test branches straight
    /// out).
    // Read today only by this module's tests; RUE-928 constant-trip unrolling
    // is the tracked consumer (its trip-count shape check needs the exits).
    #[allow(dead_code)]
    pub(crate) exits: Vec<(BlockId, BlockId)>,
}

impl NaturalLoop {
    /// Whether `block` is in this loop's body (binary search over the sorted
    /// body).
    pub(crate) fn contains(&self, block: BlockId) -> bool {
        self.body
            .binary_search_by_key(&block.as_u32(), |b: &BlockId| b.as_u32())
            .is_ok()
    }
}

/// The natural loops of a function, or the explicit refusal for an irreducible
/// CFG.
pub(crate) struct LoopForest {
    loops: Vec<NaturalLoop>,
    irreducible: bool,
}

impl LoopForest {
    /// Whether the CFG contains irreducible control flow. When `true` the forest
    /// holds no loops: ADR-0054 refuses to guess a body for a multi-entry cycle.
    pub(crate) fn is_irreducible(&self) -> bool {
        self.irreducible
    }

    /// The analyzed loops. Empty when the CFG is loop-free or irreducible.
    // Called today only by this module's tests (LICM iterates by id via
    // `len`/`get`); kept as the forest's slice view for RUE-928 unrolling.
    #[allow(dead_code)]
    pub(crate) fn loops(&self) -> &[NaturalLoop] {
        &self.loops
    }

    /// Number of analyzed loops.
    pub(crate) fn len(&self) -> usize {
        self.loops.len()
    }

    /// Whether no loops were analyzed.
    pub(crate) fn is_empty(&self) -> bool {
        self.loops.is_empty()
    }

    /// Borrow a loop by id.
    pub(crate) fn get(&self, id: LoopId) -> &NaturalLoop {
        &self.loops[id]
    }
}

/// Bounded-work counters for one [`loops`] analysis (RUE-794 discipline).
///
/// Every field is proportional to either the input graph or the loop bodies the
/// result actually represents. The reachability DFS visits each reachable
/// block once and each edge once (`blocks_visited`, `edges_visited`); grouping
/// performs one slot/dedup probe per back edge; and body/parent work visits each
/// represented loop-body membership a bounded number of times.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Stats {
    /// Blocks reached by the reachability/irreducibility DFS (one visit each).
    pub blocks_visited: u64,
    /// Successor edges examined across the DFS and the back-edge scan.
    pub edges_visited: u64,
    /// Back edges found (edges `u -> h` with `h` dominating `u`).
    pub back_edges: u64,
    /// Block-indexed header-slot probes, one per back edge.
    pub header_slot_probes: u64,
    /// Last-latch deduplication probes, one per back edge.
    pub latch_dedup_probes: u64,
    /// Distinct loop headers, i.e. natural loops in the forest.
    pub loops_found: u64,
    /// Total block insertions across every loop-body worklist.
    pub body_pushes: u64,
    /// Existing loop-body memberships visited while assigning parents.
    pub parent_body_visits: u64,
}

/// Compute the natural-loop forest of `cfg` using the dominator tree `dom`.
pub(crate) fn loops(cfg: &Cfg, dom: &DominatorTree) -> LoopForest {
    loops_with_stats(cfg, dom).0
}

/// [`loops`] plus its work counters, for the bounded-work unit test.
pub(crate) fn loops_with_stats(cfg: &Cfg, dom: &DominatorTree) -> (LoopForest, Stats) {
    let mut stats = Stats::default();
    let n = cfg.block_count();

    // Successor lists, decoded once.
    let succs: Vec<Vec<BlockId>> = (0..n)
        .map(|i| successors_of(cfg, BlockId::from_raw(i as u32)))
        .collect();

    // ------------------------------------------------------------------
    // Reachability + irreducibility. A retreating edge `u -> v` (v is a DFS
    // ancestor, i.e. still on the stack) that is NOT a back edge — the header
    // `v` does not dominate the source `u` — means a cycle with two entries:
    // irreducible. The dominator tree cannot describe its body, so we refuse
    // the whole function rather than guess (ADR-0054).
    // ------------------------------------------------------------------
    let entry = cfg.entry;
    let mut visited = vec![false; n];
    let mut on_stack = vec![false; n];
    let mut irreducible = false;

    if (entry.as_u32() as usize) < n {
        visited[entry.as_u32() as usize] = true;
        on_stack[entry.as_u32() as usize] = true;
        stats.blocks_visited += 1;
        // (block, next successor index).
        let mut stack: Vec<(BlockId, usize)> = vec![(entry, 0)];
        while let Some(&(block, idx)) = stack.last() {
            let block_succs = &succs[block.as_u32() as usize];
            if idx < block_succs.len() {
                stack.last_mut().unwrap().1 += 1;
                stats.edges_visited += 1;
                let s = block_succs[idx];
                let si = s.as_u32() as usize;
                if on_stack[si] {
                    // Retreating edge. It is a back edge iff its target
                    // dominates its source; otherwise the graph is irreducible.
                    if !dom.dominates(s, block) {
                        irreducible = true;
                    }
                } else if !visited[si] {
                    visited[si] = true;
                    on_stack[si] = true;
                    stats.blocks_visited += 1;
                    stack.push((s, 0));
                }
            } else {
                on_stack[block.as_u32() as usize] = false;
                stack.pop();
            }
        }
    }

    if irreducible {
        return (
            LoopForest {
                loops: Vec::new(),
                irreducible: true,
            },
            stats,
        );
    }

    // ------------------------------------------------------------------
    // Back edges: every edge `u -> h` where `h` dominates `u`. This is the
    // dominance definition, independent of DFS order; on a reducible graph it
    // is exactly the retreating-edge set found above. Group by header — back
    // edges sharing a header are one loop with several latches.
    // ------------------------------------------------------------------
    let preds = cfg.compute_predecessors();
    // Headers, in first-seen order, each with its deduplicated latch set.
    let mut headers: Vec<BlockId> = Vec::new();
    let mut latch_sets: Vec<Vec<BlockId>> = Vec::new();
    let mut header_slots = vec![None; n];
    for u in 0..n {
        if !visited[u] {
            continue;
        }
        let ub = BlockId::from_raw(u as u32);
        for &s in &succs[u] {
            stats.edges_visited += 1;
            if dom.dominates(s, ub) {
                stats.back_edges += 1;
                stats.header_slot_probes += 1;
                let header_index = s.as_u32() as usize;
                let slot = match header_slots[header_index] {
                    Some(i) => i,
                    None => {
                        let slot = headers.len();
                        headers.push(s);
                        latch_sets.push(Vec::new());
                        header_slots[header_index] = Some(slot);
                        slot
                    }
                };
                stats.latch_dedup_probes += 1;
                // Source blocks are visited in index order, so every repeated
                // edge from one latch to this header is adjacent in this set.
                if latch_sets[slot].last() != Some(&ub) {
                    latch_sets[slot].push(ub);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Loop bodies. For header `h` with latches `L`, the body is `h` plus every
    // block that reaches a latch without passing through `h`: seed the worklist
    // with the latches and walk predecessors, stopping at `h` (already in the
    // set, never expanded). Classic Cooper/Torczon natural-loop body.
    // ------------------------------------------------------------------
    let mut loops: Vec<NaturalLoop> = Vec::with_capacity(headers.len());
    for (hi, &header) in headers.iter().enumerate() {
        let mut latches = latch_sets[hi].clone();
        latches.sort_by_key(|b: &BlockId| b.as_u32());

        let mut body: AHashSet<BlockId> = AHashSet::new();
        body.insert(header);
        let mut work: Vec<BlockId> = Vec::new();
        for &latch in &latches {
            if latch != header && body.insert(latch) {
                stats.body_pushes += 1;
                work.push(latch);
            }
        }
        while let Some(block) = work.pop() {
            for &p in &preds[block.as_u32() as usize] {
                if body.insert(p) {
                    stats.body_pushes += 1;
                    work.push(p);
                }
            }
        }

        let mut body: Vec<BlockId> = body.into_iter().collect();
        body.sort_by_key(|b: &BlockId| b.as_u32());

        // Exit edges: any body block's successor that leaves the body. The
        // header is a legitimate exit source (its trip test branching out).
        let mut exits: Vec<(BlockId, BlockId)> = Vec::new();
        for &b in &body {
            for &s in &succs[b.as_u32() as usize] {
                if body
                    .binary_search_by_key(&s.as_u32(), |b: &BlockId| b.as_u32())
                    .is_err()
                {
                    exits.push((b, s));
                }
            }
        }

        loops.push(NaturalLoop {
            header,
            latches,
            body,
            parent: None,
            exits,
        });
    }
    stats.loops_found = loops.len() as u64;

    // ------------------------------------------------------------------
    // Nesting forest. In a reducible CFG natural loops are properly nested, so
    // loop `i`'s immediate parent is the smallest *other* loop whose body
    // contains `i`'s header. Headers are unique (merged by header above), so
    // ties by size cannot arise between distinct loops.
    // ------------------------------------------------------------------
    assign_loop_parents(&mut loops, n, &mut stats);

    (
        LoopForest {
            loops,
            irreducible: false,
        },
        stats,
    )
}

/// Assign the smallest enclosing loop to every loop by visiting the body
/// memberships already materialized in the result. Reducible natural loops are
/// properly nested or disjoint, so the first enclosing loop encountered in
/// ascending body-size order is the immediate parent.
fn assign_loop_parents(loops: &mut [NaturalLoop], block_count: usize, stats: &mut Stats) {
    let mut loop_by_header = vec![None; block_count];
    for (id, lp) in loops.iter().enumerate() {
        loop_by_header[lp.header.as_u32() as usize] = Some(id);
    }

    let mut by_body_size: Vec<LoopId> = (0..loops.len()).collect();
    by_body_size.sort_by_key(|&id| (loops[id].body.len(), id));
    let mut parents = vec![None; loops.len()];
    for outer in by_body_size {
        for &block in &loops[outer].body {
            stats.parent_body_visits += 1;
            let Some(inner) = loop_by_header[block.as_u32() as usize] else {
                continue;
            };
            if inner != outer
                && loops[outer].body.len() > loops[inner].body.len()
                && parents[inner].is_none()
            {
                parents[inner] = Some(outer);
            }
        }
    }
    for (lp, parent) in loops.iter_mut().zip(parents) {
        lp.parent = parent;
    }
}

/// Materialize a dedicated preheader for `lp` and return its block id.
///
/// Reuses the single outside predecessor when it already targets the header
/// unconditionally as its only successor; otherwise inserts a fresh block,
/// redirecting every outside edge through it and forwarding the header's block
/// arguments (see the module docs and ADR-0054 §1). The back edges keep
/// targeting the header directly. Idempotent: a second call on a freshly
/// recomputed loop reuses the block the first call inserted, adding no block.
///
/// The result leaves the CFG verifier-clean: `ph`'s block parameters mirror the
/// header's, each redirected outside edge keeps its arity, and `ph`'s own
/// parameters (defined in `ph`) are what it forwards on.
pub(crate) fn ensure_preheader(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    type_pool: &FrozenTypeInternPool,
) -> Result<BlockId, CfgOptimizationError> {
    ensure_preheader_with_injection(cfg, lp, type_pool, PayloadFailureInjection::default())
}

#[derive(Default)]
struct PayloadFailureInjection {
    #[cfg(test)]
    fail_at: Option<usize>,
    #[cfg(test)]
    next: usize,
}

impl PayloadFailureInjection {
    fn before(&mut self, family: &'static str) -> Result<(), CfgEditError> {
        #[cfg(test)]
        {
            let stage = self.next;
            self.next += 1;
            if self.fail_at == Some(stage) {
                return Err(CfgEditError::CapacityFailure { family });
            }
        }
        #[cfg(not(test))]
        let _ = family;
        Ok(())
    }

    fn before_validation(&mut self, cfg: &mut Cfg, ph: BlockId) {
        #[cfg(test)]
        {
            let stage = self.next;
            self.next += 1;
            if self.fail_at == Some(stage) {
                // Deterministic corruption of the private optimizer editor.
                // Verification must reject it before owner publication.
                cfg.get_block_mut(ph).terminator = Terminator::None;
            }
        }
        #[cfg(not(test))]
        let _ = (cfg, ph);
    }
}

fn ensure_preheader_with_injection(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    type_pool: &FrozenTypeInternPool,
    mut injection: PayloadFailureInjection,
) -> Result<BlockId, CfgOptimizationError> {
    // Reusing an existing unconditional outside predecessor is a proof about
    // the owner we were given, not an edit. Do it before entering the
    // materialization path: no validation or test injection work is needed on
    // this no-mutation path.
    let classification = classify_preheader(cfg, lp);
    if let Some(ph) = classification.reusable {
        return Ok(ph);
    }

    // `cfg` is the optimizer's private editor. A failed edit can leave it
    // poisoned, but the error propagates to `publish_optimization`, whose
    // `pass_result?` discards the editor before it can become a `ValidatedCfg`.
    let ph = ensure_preheader_in(cfg, lp, &classification.outside, &mut injection)?;
    injection.before_validation(cfg, ph);
    // Validate the materialized preheader with the materialization verifier, NOT
    // the strict `verify_with_type_pool`. Preheader materialization runs
    // mid-pipeline during LICM (RUE-927), between `simplify` and the pipeline's
    // final DCE, so the CFG legitimately carries two kinds of transient debris
    // that DCE has not swept yet and that this edit did not introduce:
    //   * detached-but-in-arena dead values left by `forward`/`cse` — tolerated
    //     because the check does not require complete attachment; and
    //   * pre-DCE husks — unreachable blocks whose stale terminators still pass
    //     edge arguments to targets that `simplify` reparameterized (e.g. a
    //     folded const `if`'s dead `else` still holds `goto merge([6])` after
    //     `merge` lost its parameter). Skipping unreachable blocks keeps that
    //     husk from being mistaken for a defect in the preheader we just built.
    // The preheader and the loop it feeds are reachable, so a genuinely
    // malformed materialization (bad arity, ill-typed/dominance-violating edge,
    // missing terminator) is still rejected before the owner is published. The
    // pipeline's final `finish_after_optimization` re-verifies the whole graph
    // strictly once DCE has removed the husks.
    cfg.verify_materialization_with_type_pool(type_pool)
        .map_err(CfgOptimizationError::Verification)?;
    Ok(ph)
}

struct PreheaderClassification {
    outside: Vec<BlockId>,
    reusable: Option<BlockId>,
}

#[cfg(test)]
fn reset_test_preheader_pred_scan_count() {
    PREHEADER_PRED_SCAN_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn test_preheader_pred_scan_count() -> usize {
    PREHEADER_PRED_SCAN_COUNT.with(Cell::get)
}

/// Classify a loop's header predecessors once, retaining the outside set for
/// the materialization edit. Reuse is proved before materialization; the
/// in-place edit receives this same classification so it does not rescan the
/// owner.
fn classify_preheader(cfg: &Cfg, lp: &NaturalLoop) -> PreheaderClassification {
    let header = lp.header;
    #[cfg(test)]
    PREHEADER_PRED_SCAN_COUNT.with(|count| count.set(count.get() + 1));
    let outside: Vec<BlockId> = cfg
        .predecessors_of(header)
        .into_iter()
        .filter(|predecessor| !lp.contains(*predecessor))
        .collect();
    let reusable = (outside.len() == 1)
        .then(|| outside[0])
        .filter(|&predecessor| {
            matches!(
                cfg.get_block(predecessor).terminator,
                Terminator::Goto { target, .. } if target == header
            )
        });
    PreheaderClassification { outside, reusable }
}

fn ensure_preheader_in(
    cfg: &mut Cfg,
    lp: &NaturalLoop,
    outside: &[BlockId],
    injection: &mut PayloadFailureInjection,
) -> Result<BlockId, CfgEditError> {
    let header = lp.header;

    // Otherwise insert a dedicated preheader `ph`. It takes the header's
    // parameter list (so outside edges keep their arity) and forwards those
    // parameters to the header with a single `Goto`.
    let ph = cfg.new_block();
    let header_param_tys: Vec<crate::Type> = cfg
        .get_block(header)
        .params
        .iter()
        .map(|(_, ty)| *ty)
        .collect();
    let ph_params: Vec<crate::CfgValue> = header_param_tys
        .iter()
        .map(|&ty| cfg.add_block_param(ph, ty))
        .collect();
    injection.before("goto args")?;
    let args = cfg.push_goto_args(ph_params)?;
    cfg.get_block_mut(ph).terminator = Terminator::Goto {
        target: header,
        args,
    };

    // Redirect every outside edge from the header to `ph`, preserving each
    // edge's arguments (they now feed `ph`'s mirror parameters).
    for &p in outside {
        redirect_edges(cfg, p, header, ph, injection)?;
    }

    // If the header was the entry, the preheader becomes the new entry so it
    // still dominates the header.
    if header == cfg.entry {
        cfg.entry = ph;
    }

    Ok(ph)
}

/// Retarget every edge `from -> old_target` in `from`'s terminator to `new`,
/// keeping each edge's block arguments. Latch/back edges are never passed here.
fn redirect_edges(
    cfg: &mut Cfg,
    from: BlockId,
    old_target: BlockId,
    new: BlockId,
    injection: &mut PayloadFailureInjection,
) -> Result<(), CfgEditError> {
    let term = std::mem::replace(
        &mut cfg.get_block_mut(from).terminator,
        Terminator::Unreachable,
    );
    let new_term = match term {
        Terminator::Goto { target, args } => {
            if target == old_target {
                Terminator::Goto { target: new, args }
            } else {
                Terminator::Goto { target, args }
            }
        }
        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            let new_then = if then_block == old_target {
                new
            } else {
                then_block
            };
            let new_else = if else_block == old_target {
                new
            } else {
                else_block
            };
            Terminator::Branch {
                cond,
                then_block: new_then,
                then_args,
                else_block: new_else,
                else_args,
            }
        }
        Terminator::Switch {
            scrutinee,
            cases,
            default,
        } => {
            // Switch edges carry no block arguments, so retargeting only
            // rewrites case/default targets. The rewritten cases are appended
            // fresh; the old range is left as unreferenced arena slack (the
            // husk discipline simplify uses).
            let new_cases: Vec<(i64, BlockId)> = cfg
                .switch_cases(&cases)
                .iter()
                .map(|&(v, t)| (v, if t == old_target { new } else { t }))
                .collect();
            injection.before("switch cases")?;
            let new_cases = match cfg.push_switch_cases(new_cases) {
                Ok(new_cases) => new_cases,
                Err(error) => {
                    cfg.get_block_mut(from).terminator = Terminator::Switch {
                        scrutinee,
                        cases,
                        default,
                    };
                    return Err(error);
                }
            };
            Terminator::Switch {
                scrutinee,
                cases: new_cases,
                default: if default == old_target { new } else { default },
            }
        }
        term @ (Terminator::Return { .. } | Terminator::Unreachable | Terminator::None) => term,
    };
    cfg.get_block_mut(from).terminator = new_term;
    Ok(())
}

/// Successor blocks of `block` in control-flow order.
fn successors_of(cfg: &Cfg, block: BlockId) -> Vec<BlockId> {
    match &cfg.get_block(block).terminator {
        Terminator::Goto { target, .. } => vec![*target],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        Terminator::Switch { cases, default, .. } => {
            let mut out: Vec<BlockId> = cfg
                .switch_cases(cases)
                .iter()
                .map(|(_, target)| *target)
                .collect();
            out.push(*default);
            out
        }
        Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CfgInst, CfgInstData, CfgValue, Terminator, Type};
    use rue_span::Span;

    // A unit-returning function, so the `Return { value: None }` terminators
    // the loop shapes use are verifier-clean.
    fn make_cfg() -> Cfg {
        Cfg::new(Type::UNIT, 0, 0, "test".to_string(), vec![])
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

    fn bool_const(cfg: &mut Cfg, block: BlockId) -> CfgValue {
        cfg.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        )
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

    fn switch<I>(cfg: &mut Cfg, scrutinee: CfgValue, cases: I, default: BlockId) -> Terminator
    where
        I: IntoIterator<Item = (i64, BlockId)>,
        I::IntoIter: ExactSizeIterator,
    {
        Terminator::Switch {
            scrutinee,
            cases: cfg.push_switch_cases(cases).unwrap(),
            default,
        }
    }

    /// Find the loop whose header is `header`.
    fn loop_of(forest: &LoopForest, header: BlockId) -> &NaturalLoop {
        forest
            .loops()
            .iter()
            .find(|l| l.header == header)
            .unwrap_or_else(|| panic!("no loop with header {header}"))
    }

    fn analyze(cfg: &Cfg) -> LoopForest {
        let dom = DominatorTree::compute(cfg);
        loops(cfg, &dom)
    }

    // ------------------------------------------------------------------
    // Natural-loop detection
    // ------------------------------------------------------------------

    #[test]
    fn single_loop_header_latch_body_exit() {
        // entry -> header -> body -> header (back edge); header -> exit.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(header));
        let cond = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        assert!(!forest.is_irreducible());
        assert_eq!(forest.len(), 1);
        let lp = loop_of(&forest, header);
        assert_eq!(lp.header, header);
        assert_eq!(lp.latches, vec![body]);
        assert_eq!(lp.body, vec![header, body]);
        assert_eq!(lp.parent, None);
        // The header's branch to `exit` is the sole exit edge.
        assert_eq!(lp.exits, vec![(header, exit)]);
        assert!(lp.contains(header) && lp.contains(body));
        assert!(!lp.contains(exit) && !lp.contains(entry));
    }

    #[test]
    fn self_loop() {
        // entry -> h; h -> h (self back edge) and h -> exit.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let h = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(h));
        let cond = bool_const(&mut cfg, h);
        cfg.set_terminator(h, branch(cond, h, exit));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        assert_eq!(forest.len(), 1);
        let lp = loop_of(&forest, h);
        assert_eq!(lp.latches, vec![h], "the header is its own latch");
        assert_eq!(lp.body, vec![h], "a self-loop body is just the header");
        assert_eq!(lp.exits, vec![(h, exit)]);
    }

    #[test]
    fn header_that_is_also_an_exit() {
        // The single-loop shape already exercises a header whose own trip test
        // leaves the loop: assert the exit source is the header itself.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(header));
        let cond = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        let lp = loop_of(&forest, header);
        assert!(
            lp.exits.iter().any(|&(from, _)| from == header),
            "the header is an exit source"
        );
    }

    #[test]
    fn two_latches_one_header() {
        // entry -> h; h -> a and h -> b; a -> h and b -> h (two back edges to
        // one header); h -> exit is via `a`/`b` only, so add an explicit exit.
        // Shape: h branches to a or b, both loop back to h, and a also exits.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let h = cfg.new_block();
        let a = cfg.new_block();
        let b = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(h));
        let cond_h = bool_const(&mut cfg, h);
        cfg.set_terminator(h, branch(cond_h, a, b));
        // a -> h (back) or a -> exit.
        let cond_a = bool_const(&mut cfg, a);
        cfg.set_terminator(a, branch(cond_a, h, exit));
        // b -> h (back).
        cfg.set_terminator(b, goto(h));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        assert_eq!(
            forest.len(),
            1,
            "back edges sharing a header merge to one loop"
        );
        let lp = loop_of(&forest, h);
        assert_eq!(lp.latches, vec![a, b], "both latches recorded, sorted");
        assert_eq!(lp.body, vec![h, a, b]);
        assert_eq!(lp.exits, vec![(a, exit)]);
    }

    #[test]
    fn nested_loops_forest_parentage() {
        // Outer loop header `o`, inner loop header `i` nested inside it.
        // entry -> o -> i -> i_body -> i (inner back edge)
        // i -> o_body -> o (outer back edge); o -> exit.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let o = cfg.new_block();
        let i = cfg.new_block();
        let i_body = cfg.new_block();
        let o_body = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(o));
        let cond_o = bool_const(&mut cfg, o);
        cfg.set_terminator(o, branch(cond_o, i, exit));
        let cond_i = bool_const(&mut cfg, i);
        cfg.set_terminator(i, branch(cond_i, i_body, o_body));
        cfg.set_terminator(i_body, goto(i)); // inner back edge
        cfg.set_terminator(o_body, goto(o)); // outer back edge
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        assert_eq!(forest.len(), 2);
        let outer = loop_of(&forest, o);
        let inner = loop_of(&forest, i);

        // Outer body contains everything; inner body is the tight inner set.
        assert_eq!(outer.body, vec![o, i, i_body, o_body]);
        assert_eq!(inner.body, vec![i, i_body]);

        // Inner's parent is the outer loop; the outer loop is top-level.
        assert_eq!(outer.parent, None);
        let outer_id = forest.loops().iter().position(|l| l.header == o).unwrap();
        assert_eq!(inner.parent, Some(outer_id));
    }

    #[test]
    fn no_loops_when_acyclic() {
        // entry -> {t, e} -> merge: a diamond, no back edges.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let t = cfg.new_block();
        let e = cfg.new_block();
        let merge = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, t, e));
        cfg.set_terminator(t, goto(merge));
        cfg.set_terminator(e, goto(merge));
        cfg.set_terminator(merge, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        assert!(!forest.is_irreducible());
        assert!(forest.is_empty());
    }

    #[test]
    fn irreducible_graph_is_refused() {
        // Two blocks a, b form a cycle a <-> b, entered at BOTH a and b from
        // entry. Neither dominates the other, so no header dominates the cycle:
        // irreducible.
        // entry -> a and entry -> b; a -> b; b -> a.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a = cfg.new_block();
        let b = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, a, b));
        cfg.set_terminator(a, goto(b));
        // b -> a: with the a -> b edge this is a 2-entry cycle.
        cfg.set_terminator(b, goto(a));

        let forest = analyze(&cfg);
        assert!(
            forest.is_irreducible(),
            "a multi-entry cycle must be refused"
        );
        assert!(forest.is_empty(), "an irreducible forest carries no loops");
    }

    #[test]
    fn reducible_loop_with_inner_diamond_is_not_irreducible() {
        // Guards against the coarse "cycle edge that is not a back edge"
        // misclassification: a reducible loop whose body has an if-diamond has
        // edges that close the outer cycle without being back edges, but the
        // graph is still reducible.
        // entry -> h -> {x, y} -> latch -> h (back); h -> exit.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let h = cfg.new_block();
        let x = cfg.new_block();
        let y = cfg.new_block();
        let latch = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(h));
        let cond_h = bool_const(&mut cfg, h);
        cfg.set_terminator(h, branch(cond_h, x, exit));
        let cond_x = bool_const(&mut cfg, x);
        cfg.set_terminator(x, branch(cond_x, y, latch));
        cfg.set_terminator(y, goto(latch));
        cfg.set_terminator(latch, goto(h)); // back edge
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let forest = analyze(&cfg);
        assert!(!forest.is_irreducible());
        let lp = loop_of(&forest, h);
        assert_eq!(lp.latches, vec![latch]);
        assert_eq!(lp.body, vec![h, x, y, latch]);
    }

    // ------------------------------------------------------------------
    // Preheader materialization
    // ------------------------------------------------------------------

    /// A single loop whose sole outside predecessor is an unconditional `Goto`.
    fn single_loop_goto_entry() -> (Cfg, BlockId, BlockId, BlockId, BlockId) {
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        cfg.set_terminator(entry, goto(header));
        let cond = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        (cfg, entry, header, body, exit)
    }

    #[test]
    fn preheader_reuses_unconditional_single_entry() {
        // The sole outside predecessor is `entry`, a Goto whose only successor
        // is the header: it already is a preheader, so no block is added.
        let (mut cfg, entry, header, _body, _exit) = single_loop_goto_entry();
        let before = cfg.block_count();

        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let lp = loop_of(&forest, header);
        let ph = ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();

        assert_eq!(ph, entry, "the entry Goto is reused as the preheader");
        assert_eq!(cfg.block_count(), before, "no block inserted for reuse");
        cfg.verify().unwrap();
    }

    #[test]
    fn preheader_reuse_is_a_noop_before_materialization_and_verification() {
        let (mut cfg, entry, header, _body, _exit) = single_loop_goto_entry();
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let lp = loop_of(&forest, header);
        let before_debug = format!("{cfg:?}");
        let before_display = cfg.to_string();
        let before_payload = cfg.payload_storage_stats();
        let before_values = cfg.value_count();
        let before_blocks = cfg.block_count();
        let block_address = cfg.get_block(entry) as *const _;
        let value_address = cfg.get_inst(cfg.get_block(header).insts[0]) as *const _;

        // A stage-zero injection would poison the private editor if the
        // materialization path or its verifier were entered. Reuse must return
        // before either one is touched.
        Cfg::reset_test_clone_count();
        let ph = ensure_preheader_with_injection(
            &mut cfg,
            lp,
            &test_type_pool(),
            PayloadFailureInjection {
                fail_at: Some(0),
                next: 0,
            },
        )
        .unwrap();
        assert_eq!(ph, entry);
        assert_eq!(cfg.block_count(), before_blocks);
        assert_eq!(cfg.value_count(), before_values);
        assert_eq!(cfg.payload_storage_stats(), before_payload);
        assert_eq!(format!("{cfg:?}"), before_debug);
        assert_eq!(cfg.to_string(), before_display);
        assert_eq!(cfg.get_block(entry) as *const _, block_address);
        assert_eq!(
            cfg.get_inst(cfg.get_block(header).insts[0]) as *const _,
            value_address
        );
        assert_eq!(Cfg::test_clone_count(), 0);
    }

    #[test]
    fn preheader_splits_when_sole_pred_has_another_successor() {
        // The sole outside predecessor branches to the header on one arm and
        // elsewhere on the other. Hoisting into it would run on the sibling
        // path, so a dedicated preheader must be split out (ADR-0054 §1).
        // entry -> {header, side}; header loops; side returns.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let side = cfg.new_block();
        let exit = cfg.new_block();

        let cond_e = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond_e, header, side));
        let cond_h = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond_h, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(side, Terminator::Return { value: None });
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let before = cfg.block_count();
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let lp = loop_of(&forest, header);
        let ph = ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();

        assert_eq!(cfg.block_count(), before + 1, "a preheader is inserted");
        assert_ne!(ph, entry);
        // entry's then-arm now targets ph, not the header; the else-arm is
        // untouched.
        match cfg.get_block(entry).terminator {
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                assert_eq!(then_block, ph);
                assert_eq!(else_block, side);
            }
            ref other => panic!("expected Branch, got {other:?}"),
        }
        // ph is an unconditional Goto to the header; the back edge still targets
        // the header directly.
        assert!(matches!(
            cfg.get_block(ph).terminator,
            Terminator::Goto { target, .. } if target == header
        ));
        assert!(matches!(
            cfg.get_block(body).terminator,
            Terminator::Goto { target, .. } if target == header
        ));
        cfg.verify().unwrap();
    }

    #[test]
    fn preheader_splits_multiple_outside_predecessors() {
        // Two outside entries into the header from p1 and p2; the header also
        // has a back edge. A dedicated preheader merges the two entries.
        // entry -> {p1, p2}; p1 -> header; p2 -> header; header -> body ->
        // header (back); header -> exit.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let p1 = cfg.new_block();
        let p2 = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        let cond_e = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond_e, p1, p2));
        cfg.set_terminator(p1, goto(header));
        cfg.set_terminator(p2, goto(header));
        let cond_h = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond_h, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let before = cfg.block_count();
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let lp = loop_of(&forest, header);
        let ph = ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();

        assert_eq!(cfg.block_count(), before + 1);
        assert!(matches!(
            cfg.get_block(p1).terminator,
            Terminator::Goto { target, .. } if target == ph
        ));
        assert!(matches!(
            cfg.get_block(p2).terminator,
            Terminator::Goto { target, .. } if target == ph
        ));
        // Both outside entries now converge on ph; the back edge is unchanged.
        let preds = cfg.compute_predecessors();
        let mut ph_preds = preds[ph.as_u32() as usize].clone();
        ph_preds.sort_by_key(|b| b.as_u32());
        assert_eq!(ph_preds, vec![p1, p2]);
        let mut header_preds = preds[header.as_u32() as usize].clone();
        header_preds.sort_by_key(|b| b.as_u32());
        // The header is now entered only via ph and the back edge from body.
        let mut expected = vec![ph, body];
        expected.sort_by_key(|b| b.as_u32());
        assert_eq!(header_preds, expected);
        cfg.verify().unwrap();
    }

    #[test]
    fn preheader_failure_at_every_payload_stage_is_not_published() {
        // Three fallible payload stages are exercised: the preheader's Goto
        // arguments and one rewritten switch-case payload for each outside
        // predecessor. The fourth stage forces materialization verification to
        // reject the finished edit. Failures poison this private optimizer
        // editor; `publish_optimization` must propagate the exact failure before
        // publishing any of that partial state.
        let mut original = make_cfg();
        let entry = original.new_block();
        original.entry = entry;
        let p1 = original.new_block();
        let p2 = original.new_block();
        let side1 = original.new_block();
        let side2 = original.new_block();
        let header = original.new_block();
        let body = original.new_block();
        let exit = original.new_block();

        let entry_cond = bool_const(&mut original, entry);
        original.set_terminator(entry, branch(entry_cond, p1, p2));
        let p1_cond = bool_const(&mut original, p1);
        let p1_term = switch(&mut original, p1_cond, [(1, header)], side1);
        original.set_terminator(p1, p1_term);
        let p2_cond = bool_const(&mut original, p2);
        let p2_term = switch(&mut original, p2_cond, [(1, header)], side2);
        original.set_terminator(p2, p2_term);
        original.set_terminator(side1, Terminator::Return { value: None });
        original.set_terminator(side2, Terminator::Return { value: None });
        let header_cond = bool_const(&mut original, header);
        original.set_terminator(header, branch(header_cond, body, exit));
        original.set_terminator(body, goto(header));
        original.set_terminator(exit, Terminator::Return { value: None });
        original.verify().unwrap();

        let forest = analyze(&original);
        let lp = loop_of(&forest, header);
        let before_shape = (
            original.block_count(),
            original.value_count(),
            successors_of(&original, p1),
            successors_of(&original, p2),
        );

        for fail_at in 0..4 {
            let mut cfg = original.clone();
            let error = ensure_preheader_with_injection(
                &mut cfg,
                lp,
                &test_type_pool(),
                PayloadFailureInjection {
                    fail_at: Some(fail_at),
                    next: 0,
                },
            )
            .unwrap_err();
            assert_ne!(
                (
                    cfg.block_count(),
                    cfg.value_count(),
                    successors_of(&cfg, p1),
                    successors_of(&cfg, p2),
                ),
                before_shape,
                "the private editor must retain partial state at stage {fail_at}"
            );

            match (&error, fail_at) {
                (
                    CfgOptimizationError::Edit(CfgEditError::CapacityFailure {
                        family: "goto args",
                    }),
                    0,
                )
                | (
                    CfgOptimizationError::Edit(CfgEditError::CapacityFailure {
                        family: "switch cases",
                    }),
                    1 | 2,
                )
                | (CfgOptimizationError::Verification(_), 3) => {}
                (error, stage) => panic!("wrong failure at stage {stage}: {error:?}"),
            }

            let published =
                crate::opt::publish_optimization(cfg, Err(error), &test_type_pool()).unwrap_err();
            match (published, fail_at) {
                (
                    CfgOptimizationError::Edit(CfgEditError::CapacityFailure {
                        family: "goto args",
                    }),
                    0,
                )
                | (
                    CfgOptimizationError::Edit(CfgEditError::CapacityFailure {
                        family: "switch cases",
                    }),
                    1 | 2,
                )
                | (CfgOptimizationError::Verification(_), 3) => {}
                (error, stage) => panic!("publication changed stage {stage}: {error:?}"),
            }
        }
    }

    #[test]
    fn preheader_idempotent_adds_one_block_once() {
        // Calling ensure_preheader twice (recomputing the loop between calls)
        // inserts exactly one block the first time and none the second.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let body = cfg.new_block();
        let side = cfg.new_block();
        let exit = cfg.new_block();

        let cond_e = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond_e, header, side));
        let cond_h = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond_h, body, exit));
        cfg.set_terminator(body, goto(header));
        cfg.set_terminator(side, Terminator::Return { value: None });
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let base = cfg.block_count();

        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let ph1 = ensure_preheader(&mut cfg, loop_of(&forest, header), &test_type_pool()).unwrap();
        assert_eq!(cfg.block_count(), base + 1, "first call inserts one block");

        // Recompute from scratch (per-pass discipline) and call again.
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let ph2 = ensure_preheader(&mut cfg, loop_of(&forest, header), &test_type_pool()).unwrap();
        assert_eq!(ph2, ph1, "the existing preheader is reused");
        assert_eq!(cfg.block_count(), base + 1, "second call inserts nothing");
        cfg.verify().unwrap();
    }

    #[test]
    fn preheader_forwards_block_arguments() {
        // The header carries a block parameter. Two outside predecessors feed
        // it different values; the back edge feeds a third. After
        // materialization the preheader mirrors the parameter and the values
        // still flow to the header identically.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let p1 = cfg.new_block();
        let p2 = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let exit = cfg.new_block();

        // Header takes one i32 parameter.
        let hp = cfg.add_block_param(header, Type::I32);

        let cond_e = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond_e, p1, p2));

        // p1 -> header(10); p2 -> header(20).
        let v10 = cfg.add_inst_to_block(
            p1,
            CfgInst {
                data: CfgInstData::Const(10),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let args = cfg.push_goto_args(vec![v10]).unwrap();
        cfg.set_terminator(
            p1,
            Terminator::Goto {
                target: header,
                args,
            },
        );
        let v20 = cfg.add_inst_to_block(
            p2,
            CfgInst {
                data: CfgInstData::Const(20),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let args = cfg.push_goto_args(vec![v20]).unwrap();
        cfg.set_terminator(
            p2,
            Terminator::Goto {
                target: header,
                args,
            },
        );

        // header uses its param, then branches to body/exit.
        let cond_h = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(cond_h, body, exit));

        // body -> header(hp + 1): back edge feeds the incremented param.
        let one = cfg.add_inst_to_block(
            body,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let next = cfg.add_inst_to_block(
            body,
            CfgInst {
                data: CfgInstData::Add(hp, one),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let args = cfg.push_goto_args(vec![next]).unwrap();
        cfg.set_terminator(
            body,
            Terminator::Goto {
                target: header,
                args,
            },
        );
        cfg.set_terminator(exit, Terminator::Return { value: None });

        cfg.verify().unwrap();

        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let lp = loop_of(&forest, header);
        let ph = ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();

        // ph mirrors the header's single parameter.
        assert_eq!(cfg.get_block(ph).params.len(), 1);
        let (php, php_ty) = cfg.get_block(ph).params[0];
        assert_eq!(php_ty, Type::I32);

        // The two outside edges now feed ph their original values.
        let p1_args = cfg.get_goto_args(&cfg.get_block(p1).terminator).to_vec();
        let p2_args = cfg.get_goto_args(&cfg.get_block(p2).terminator).to_vec();
        assert_eq!(p1_args, vec![v10]);
        assert_eq!(p2_args, vec![v20]);
        assert!(matches!(
            cfg.get_block(p1).terminator,
            Terminator::Goto { target, .. } if target == ph
        ));

        // ph forwards its own mirror parameter to the header.
        let ph_args = cfg.get_goto_args(&cfg.get_block(ph).terminator).to_vec();
        assert_eq!(ph_args, vec![php]);

        // The back edge still feeds the header directly with the incremented
        // value.
        let body_args = cfg.get_goto_args(&cfg.get_block(body).terminator).to_vec();
        assert_eq!(body_args, vec![next]);
        assert!(matches!(
            cfg.get_block(body).terminator,
            Terminator::Goto { target, .. } if target == header
        ));

        // The whole rewrite is verifier-clean: arity and types agree on every
        // edge, and ph dominates the header.
        cfg.verify().unwrap();
        let dom = DominatorTree::compute(&cfg);
        assert!(dom.dominates(ph, header));
    }

    #[test]
    fn preheader_of_entry_header_becomes_new_entry() {
        // The entry block is itself a loop header (entry -> body -> entry).
        // Its only predecessor is the back edge, so a dedicated preheader is
        // inserted and becomes the new entry.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let body = cfg.new_block();
        let exit = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, body, exit));
        cfg.set_terminator(body, goto(entry)); // back edge to the entry header
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        let lp = loop_of(&forest, entry);
        let ph = ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();

        assert_eq!(cfg.entry, ph, "the preheader is the new entry");
        assert!(matches!(
            cfg.get_block(ph).terminator,
            Terminator::Goto { target, .. } if target == entry
        ));
        cfg.verify().unwrap();
        let dom = DominatorTree::compute(&cfg);
        assert!(dom.dominates(ph, entry));
    }

    // ------------------------------------------------------------------
    // Work-counter bound (RUE-794 discipline)
    // ------------------------------------------------------------------

    #[test]
    fn analysis_work_is_bounded() {
        // A chain of `k` sequential loops must stay near-linear: the DFS
        // visits each reachable block once and each edge once, and body
        // worklists and parent assignment visit only represented memberships.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;

        // Build k independent sequential loops off a spine.
        let k = 64u32;
        let mut prev = entry;
        let mut total_edges = 0u64;
        for _ in 0..k {
            let header = cfg.new_block();
            let body = cfg.new_block();
            let next = cfg.new_block();
            cfg.set_terminator(prev, goto(header)); // 1 edge
            let cond = bool_const(&mut cfg, header);
            cfg.set_terminator(header, branch(cond, body, next)); // 2 edges
            cfg.set_terminator(body, goto(header)); // 1 edge (back)
            total_edges += 4;
            prev = next;
        }
        cfg.set_terminator(prev, Terminator::Return { value: None });

        let dom = DominatorTree::compute(&cfg);
        let (forest, stats) = loops_with_stats(&cfg, &dom);
        assert_eq!(forest.len(), k as usize);

        let blocks = cfg.block_count() as u64;
        // Each reachable block is DFS-visited exactly once.
        assert_eq!(stats.blocks_visited, blocks);
        // Edges are examined by the DFS and again by the back-edge scan: two
        // linear sweeps, never a quadratic rescan.
        assert!(
            stats.edges_visited <= 2 * total_edges,
            "edges_visited {} exceeded 2*E {}",
            stats.edges_visited,
            2 * total_edges
        );
        assert_eq!(stats.back_edges, k as u64);
        assert_eq!(stats.header_slot_probes, stats.back_edges);
        assert_eq!(stats.latch_dedup_probes, stats.back_edges);
        // Body inserts are bounded by the total body size (2 blocks per loop:
        // header seeded separately, so 1 push per loop for the latch).
        assert!(
            stats.body_pushes <= blocks,
            "body_pushes {} exceeded block count {}",
            stats.body_pushes,
            blocks
        );
        let represented_body_memberships = forest
            .loops()
            .iter()
            .map(|lp| lp.body.len() as u64)
            .sum::<u64>();
        assert_eq!(
            stats.parent_body_visits, represented_body_memberships,
            "parentage must visit represented bodies, not every loop pair"
        );
    }

    #[test]
    fn many_reusable_preheaders_do_not_clone_or_materialize() {
        // Every loop has a distinct unconditional spine predecessor. Reuse is
        // a pure owner query, so exercising many loops must not perform one
        // private CFG clone per loop.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let mut previous = entry;
        const LOOPS: usize = 64;
        for _ in 0..LOOPS {
            let header = cfg.new_block();
            let body = cfg.new_block();
            let next = cfg.new_block();
            cfg.set_terminator(previous, goto(header));
            let condition = bool_const(&mut cfg, header);
            cfg.set_terminator(header, branch(condition, body, next));
            cfg.set_terminator(body, goto(header));
            previous = next;
        }
        cfg.set_terminator(previous, Terminator::Return { value: None });
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        assert_eq!(forest.len(), LOOPS);
        let blocks_before = cfg.block_count();
        Cfg::reset_test_clone_count();
        for lp in forest.loops() {
            ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();
        }
        assert_eq!(cfg.block_count(), blocks_before);
        assert_eq!(Cfg::test_clone_count(), 0);
    }

    #[test]
    fn many_materialized_preheaders_classify_once_per_loop() {
        // Each loop has two outside entries, forcing materialization. The
        // classification carries that already-computed set into the in-place
        // edit, so one loop means one predecessor scan.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let mut previous = entry;
        const LOOPS: usize = 32;
        for _ in 0..LOOPS {
            let header = cfg.new_block();
            let body = cfg.new_block();
            let alternate = cfg.new_block();
            let next = cfg.new_block();
            let condition = bool_const(&mut cfg, previous);
            cfg.set_terminator(previous, branch(condition, header, alternate));
            cfg.set_terminator(alternate, goto(header));
            let header_condition = bool_const(&mut cfg, header);
            cfg.set_terminator(header, branch(header_condition, body, next));
            cfg.set_terminator(body, goto(header));
            previous = next;
        }
        cfg.set_terminator(previous, Terminator::Return { value: None });
        let dom = DominatorTree::compute(&cfg);
        let forest = loops(&cfg, &dom);
        assert_eq!(forest.len(), LOOPS);
        let blocks_before = cfg.block_count();
        Cfg::reset_test_clone_count();
        reset_test_preheader_pred_scan_count();
        for lp in forest.loops() {
            ensure_preheader(&mut cfg, lp, &test_type_pool()).unwrap();
        }
        assert_eq!(cfg.block_count(), blocks_before + LOOPS);
        assert_eq!(Cfg::test_clone_count(), 0);
        assert_eq!(test_preheader_pred_scan_count(), LOOPS);
    }

    #[test]
    fn many_latches_use_one_grouping_probe_per_back_edge() {
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        cfg.set_terminator(entry, goto(header));

        let latch_count = 64u32;
        let latches = (0..latch_count)
            .map(|_| cfg.new_block())
            .collect::<Vec<_>>();
        let selectors = (0..latch_count)
            .map(|_| cfg.new_block())
            .collect::<Vec<_>>();
        let exit = cfg.new_block();
        cfg.set_terminator(header, goto(selectors[0]));
        for index in 0..latch_count as usize {
            let selector = selectors[index];
            let otherwise = selectors.get(index + 1).copied().unwrap_or(exit);
            let cond = bool_const(&mut cfg, selector);
            cfg.set_terminator(selector, branch(cond, latches[index], otherwise));
            cfg.set_terminator(latches[index], goto(header));
        }
        cfg.set_terminator(exit, Terminator::Return { value: None });

        let dom = DominatorTree::compute(&cfg);
        let (forest, stats) = loops_with_stats(&cfg, &dom);
        let lp = loop_of(&forest, header);
        assert_eq!(lp.latches, latches);
        assert_eq!(stats.back_edges, latch_count as u64);
        assert_eq!(stats.header_slot_probes, latch_count as u64);
        assert_eq!(stats.latch_dedup_probes, latch_count as u64);
    }

    #[test]
    fn repeated_edges_from_one_latch_are_deduplicated() {
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let header = cfg.new_block();
        let latch = cfg.new_block();
        cfg.set_terminator(entry, goto(header));
        cfg.set_terminator(header, goto(latch));
        let cond = bool_const(&mut cfg, latch);
        cfg.set_terminator(latch, branch(cond, header, header));

        let dom = DominatorTree::compute(&cfg);
        let (forest, stats) = loops_with_stats(&cfg, &dom);
        assert_eq!(loop_of(&forest, header).latches, vec![latch]);
        assert_eq!(stats.back_edges, 2);
        assert_eq!(stats.header_slot_probes, 2);
        assert_eq!(stats.latch_dedup_probes, 2);
    }
}
