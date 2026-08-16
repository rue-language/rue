//! Iterative dominator-tree construction (RUE-914).
//!
//! Computes the immediate-dominator tree of a [`Cfg`] with the
//! Cooper–Harvey–Kennedy "A Simple, Fast Dominance Algorithm" fixpoint over a
//! reverse-postorder traversal. The graph and its predecessor lists come from
//! [`Cfg::compute_predecessors`], so the tree always reflects the current
//! terminators (no cached edges to fall out of sync after a pass rewrites
//! control flow).
//!
//! This is shared analysis infrastructure, not a pass: consumers recompute it
//! on demand rather than caching it across mutations. The value-forwarding pass
//! (RUE-914, `opt/forward.rs`) uses [`DominatorTree::dominates`] to turn sema's
//! definite-initialization argument into an always-on checked invariant,
//! natural-loop analysis (RUE-926, `opt/loops.rs`) and LICM (RUE-927,
//! `opt/licm.rs`, run at `-O3` from `opt/mod.rs`) build the loop forest and its
//! hoisting decisions on it, and the structural verifier (RUE-227,
//! `verify.rs`) uses it for both reachability and the defined-before-use
//! dominance check on every operand.
//!
//! ## Definitions
//!
//! Block `a` *dominates* block `b` when every path from the entry to `b` passes
//! through `a`; `a` is the *immediate dominator* (`idom`) of `b` when it is the
//! closest strict dominator on every such path. The entry block has no
//! immediate dominator, and an unreachable block has none either (no path
//! reaches it). Dominance is reflexive: a block dominates itself.
//!
//! ## Algorithm
//!
//! 1. Number the reachable blocks in postorder via a DFS from the entry. The
//!    entry finishes last, so it holds the largest postorder number — the
//!    property the intersection step relies on.
//! 2. Seed `idom[entry] = entry` (a self-loop sentinel) and leave every other
//!    reachable block undefined.
//! 3. Sweep the blocks in reverse postorder (entry first, excluding it from the
//!    update) until no `idom` changes. Each block's new immediate dominator is
//!    the running intersection of its already-processed predecessors. By RPO
//!    order at least one predecessor is always processed before a block is
//!    visited, so the fixpoint is reached in a handful of sweeps for reducible
//!    graphs.
//! 4. Number the finished tree in preorder and record, for each block, the
//!    largest preorder number in its subtree. `a` dominates `b` exactly when
//!    `b`'s number lies inside `a`'s interval, so [`DominatorTree::dominates`]
//!    is two integer comparisons rather than a walk up `b`'s `idom` chain.
//!
//! `intersect(a, b)` walks the two fingers up the partial tree, each time
//! advancing whichever finger has the smaller postorder number (i.e. is deeper,
//! farther from the entry), until they meet at the common dominator.
//!
//! Step 4 is what makes the query cheap enough to call once per operand. The
//! verifier does exactly that, so an O(depth) chain walk there would be
//! quadratic again on a deep CFG — the shape RUE-1544 removed from the
//! verifier's own dominance computation.
//!
//! The pipeline consumers are value forwarding's Rule 1 dominance check
//! (`opt/forward.rs`), natural-loop detection (`opt/loops.rs`), LICM
//! (`opt/licm.rs`), and the structural verifier (`verify.rs`); the `idom` query
//! itself is exercised only by this module's tests and carries a targeted allow
//! below.

use crate::{BlockId, Cfg, Terminator};

/// The immediate-dominator tree of a [`Cfg`].
///
/// Query it with [`DominatorTree::idom`], [`DominatorTree::dominates`], and
/// [`DominatorTree::is_reachable`].
pub(crate) struct DominatorTree {
    /// Immediate dominator of each block by raw block index.
    ///
    /// - The entry maps to itself (the CHK self-loop sentinel); [`Self::idom`]
    ///   reports `None` for it since the entry has no immediate dominator.
    /// - A reachable non-entry block maps to its immediate dominator.
    /// - An unreachable block maps to `None`.
    idom: Vec<Option<BlockId>>,
    /// Postorder number of each block by raw index; `None` when unreachable.
    /// The entry holds the maximum among reachable blocks.
    ///
    /// This is the DFS from the entry over the current terminators, so it is
    /// also the authority for [`Self::is_reachable`].
    post_num: Vec<Option<u32>>,
    /// Dominator-tree preorder number of each block by raw index; `None` when
    /// the block is unreachable and therefore absent from the tree.
    pre_num: Vec<Option<u32>>,
    /// Largest preorder number in each block's dominator subtree, so that `a`
    /// dominates `b` exactly when `pre_num[a] <= pre_num[b] <= subtree_last[a]`.
    /// Only meaningful where `pre_num` is `Some`.
    subtree_last: Vec<u32>,
}

impl DominatorTree {
    /// Build the dominator tree of `cfg`.
    pub(crate) fn compute(cfg: &Cfg) -> Self {
        let n = cfg.block_count();
        let entry = cfg.entry;

        // Precompute successor lists once so the DFS and the fixpoint do not
        // re-decode terminators repeatedly.
        let succs: Vec<Vec<BlockId>> = (0..n)
            .map(|i| successors_of(cfg, BlockId::from_raw(i as u32)))
            .collect();

        // ------------------------------------------------------------------
        // Postorder DFS from the entry. `post_order` lists reachable blocks in
        // the order their DFS finishes; the entry is last.
        // ------------------------------------------------------------------
        let mut post_order: Vec<BlockId> = Vec::new();
        let mut post_num: Vec<Option<u32>> = vec![None; n];
        let mut visited = vec![false; n];

        if (entry.as_u32() as usize) < n {
            visited[entry.as_u32() as usize] = true;
            // Stack of (block, next successor index to explore).
            let mut stack: Vec<(BlockId, usize)> = vec![(entry, 0)];
            while let Some(&(block, idx)) = stack.last() {
                let block_succs = &succs[block.as_u32() as usize];
                if idx < block_succs.len() {
                    stack.last_mut().unwrap().1 += 1;
                    let s = block_succs[idx];
                    let si = s.as_u32() as usize;
                    if !visited[si] {
                        visited[si] = true;
                        stack.push((s, 0));
                    }
                } else {
                    stack.pop();
                    post_num[block.as_u32() as usize] = Some(post_order.len() as u32);
                    post_order.push(block);
                }
            }
        }

        // Reverse postorder: entry first.
        let rpo: Vec<BlockId> = post_order.iter().rev().copied().collect();

        // ------------------------------------------------------------------
        // CHK fixpoint. idom[entry] = entry seeds the self-loop; all other
        // reachable blocks start undefined and converge.
        // ------------------------------------------------------------------
        let mut idom: Vec<Option<BlockId>> = vec![None; n];
        if (entry.as_u32() as usize) < n {
            idom[entry.as_u32() as usize] = Some(entry);
        }
        let preds = cfg.compute_predecessors();

        let mut changed = true;
        while changed {
            changed = false;
            for &b in &rpo {
                if b == entry {
                    continue;
                }
                let mut new_idom: Option<BlockId> = None;
                for &p in &preds[b.as_u32() as usize] {
                    // Skip predecessors the DFS never reached and predecessors
                    // whose idom is not yet computed in this sweep.
                    if post_num[p.as_u32() as usize].is_none() {
                        continue;
                    }
                    if idom[p.as_u32() as usize].is_none() {
                        continue;
                    }
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect(p, cur, &idom, &post_num),
                    });
                }
                if let Some(ni) = new_idom {
                    if idom[b.as_u32() as usize] != Some(ni) {
                        idom[b.as_u32() as usize] = Some(ni);
                        changed = true;
                    }
                }
            }
        }

        let (pre_num, subtree_last) = preorder_intervals(&idom, entry, n);

        DominatorTree {
            idom,
            post_num,
            pre_num,
            subtree_last,
        }
    }

    /// Whether `block` is reachable from the entry along the current
    /// terminators.
    ///
    /// This is the tree's own DFS, so a consumer that needs both reachability
    /// and dominance pays for one traversal rather than two.
    pub(crate) fn is_reachable(&self, block: BlockId) -> bool {
        self.post_num
            .get(block.as_u32() as usize)
            .copied()
            .flatten()
            .is_some()
    }

    /// The immediate dominator of `block`, or `None` when `block` is the entry
    /// or is unreachable.
    // Every call site is this module's own tests; the query is kept as the
    // tree's basic inspection API alongside `dominates`.
    #[allow(dead_code)]
    pub(crate) fn idom(&self, block: BlockId) -> Option<BlockId> {
        let i = block.as_u32() as usize;
        match self.idom.get(i).copied().flatten() {
            // The entry's self-loop sentinel is reported as "no immediate
            // dominator".
            Some(d) if d == block => None,
            other => other,
        }
    }

    /// Whether `a` dominates `b` (every path from the entry to `b` passes
    /// through `a`). Reflexive: a block dominates itself. An unreachable `b` is
    /// dominated only by itself.
    ///
    /// Constant time: `a` dominates `b` iff `b` sits in `a`'s dominator-tree
    /// subtree, and the preorder intervals make that a containment test.
    pub(crate) fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        let (ai, bi) = (a.as_u32() as usize, b.as_u32() as usize);
        match (
            self.pre_num.get(ai).copied().flatten(),
            self.pre_num.get(bi).copied().flatten(),
        ) {
            (Some(pre_a), Some(pre_b)) => pre_a <= pre_b && pre_b <= self.subtree_last[ai],
            // A block outside the tree — unreachable, or an out-of-range id —
            // dominates nothing but itself and is dominated by nothing else.
            _ => a == b,
        }
    }
}

/// Number the dominator tree in preorder and record where each subtree ends.
///
/// Returns `(pre_num, subtree_last)`: a block's descendants are exactly the
/// blocks whose preorder number lies in `pre_num[block] ..= subtree_last[block]`,
/// which is the standard constant-time ancestor test.
///
/// The walk starts at the entry and follows `idom` edges outward, so it numbers
/// exactly the reachable blocks: the fixpoint above gives every reachable block
/// an immediate dominator, and leaves unreachable blocks with `None`.
fn preorder_intervals(
    idom: &[Option<BlockId>],
    entry: BlockId,
    n: usize,
) -> (Vec<Option<u32>>, Vec<u32>) {
    // Invert `idom` into child lists. The entry's self-loop sentinel is not an
    // edge, so it is skipped rather than made a child of itself.
    let mut children: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    for (index, parent) in idom.iter().enumerate() {
        let block = BlockId::from_raw(index as u32);
        match *parent {
            Some(parent) if parent != block => children[parent.as_u32() as usize].push(block),
            _ => {}
        }
    }

    let mut pre_num: Vec<Option<u32>> = vec![None; n];
    let mut subtree_last: Vec<u32> = vec![0; n];
    if (entry.as_u32() as usize) >= n {
        return (pre_num, subtree_last);
    }

    let mut next = 0_u32;
    pre_num[entry.as_u32() as usize] = Some(next);
    next += 1;
    // Stack of (block, next child index to descend into).
    let mut stack: Vec<(BlockId, usize)> = vec![(entry, 0)];
    while let Some(&(block, cursor)) = stack.last() {
        let block_children = &children[block.as_u32() as usize];
        if cursor < block_children.len() {
            let child = block_children[cursor];
            stack.last_mut().unwrap().1 += 1;
            pre_num[child.as_u32() as usize] = Some(next);
            next += 1;
            stack.push((child, 0));
        } else {
            stack.pop();
            // Everything numbered since `block` was pushed is in its subtree,
            // and `block` itself took a number, so `next` is never zero here.
            subtree_last[block.as_u32() as usize] = next - 1;
        }
    }

    (pre_num, subtree_last)
}

/// Intersect two nodes of the partial dominator tree, per Cooper–Harvey–Kennedy:
/// advance whichever finger is deeper (smaller postorder number) until the two
/// fingers meet. Every node visited here is reachable, so the `idom`/`post_num`
/// lookups are always populated.
fn intersect(
    mut a: BlockId,
    mut b: BlockId,
    idom: &[Option<BlockId>],
    post_num: &[Option<u32>],
) -> BlockId {
    while a != b {
        let mut pa = post_num[a.as_u32() as usize].expect("reachable finger");
        let mut pb = post_num[b.as_u32() as usize].expect("reachable finger");
        while pa < pb {
            a = idom[a.as_u32() as usize].expect("processed finger has an idom");
            pa = post_num[a.as_u32() as usize].expect("reachable finger");
        }
        while pb < pa {
            b = idom[b.as_u32() as usize].expect("processed finger has an idom");
            pb = post_num[b.as_u32() as usize].expect("reachable finger");
        }
    }
    a
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
    use crate::{CfgInst, CfgInstData, Terminator, Type};
    use rue_span::Span;

    fn make_cfg() -> Cfg {
        Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![])
    }

    fn goto(target: BlockId) -> Terminator {
        Terminator::Goto {
            target,
            args: crate::payload::CfgGotoArgs::EMPTY,
        }
    }

    fn bool_const(cfg: &mut Cfg, block: BlockId) -> crate::CfgValue {
        cfg.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::BoolConst(true),
                ty: Type::BOOL,
                span: Span::new(0, 0),
            },
        )
    }

    fn branch(cond: crate::CfgValue, then_block: BlockId, else_block: BlockId) -> Terminator {
        Terminator::Branch {
            cond,
            then_block,
            then_args: crate::payload::CfgThenArgs::EMPTY,
            else_block,
            else_args: crate::payload::CfgElseArgs::EMPTY,
        }
    }

    #[test]
    fn test_entry_has_no_idom_and_dominates_all() {
        // entry -> a -> b (straight line).
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a = cfg.new_block();
        let b = cfg.new_block();
        cfg.set_terminator(entry, goto(a));
        cfg.set_terminator(a, goto(b));
        cfg.set_terminator(b, Terminator::Return { value: None });

        let dom = DominatorTree::compute(&cfg);
        assert_eq!(dom.idom(entry), None);
        assert_eq!(dom.idom(a), Some(entry));
        assert_eq!(dom.idom(b), Some(a));

        for block in [entry, a, b] {
            assert!(dom.dominates(entry, block), "entry must dominate {block}");
            assert!(dom.dominates(block, block), "dominance is reflexive");
        }
        assert!(dom.dominates(a, b));
        assert!(!dom.dominates(b, a));
    }

    #[test]
    fn test_diamond() {
        // entry -> {t, e} -> merge. entry is the only dominator of merge; the
        // arms dominate only themselves.
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

        let dom = DominatorTree::compute(&cfg);
        assert_eq!(dom.idom(t), Some(entry));
        assert_eq!(dom.idom(e), Some(entry));
        // merge is reachable from both arms, so its immediate dominator is the
        // entry, not either arm.
        assert_eq!(dom.idom(merge), Some(entry));

        assert!(dom.dominates(entry, merge));
        assert!(!dom.dominates(t, merge));
        assert!(!dom.dominates(e, merge));
        assert!(!dom.dominates(t, e));
    }

    #[test]
    fn test_loop_with_back_edge() {
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

        let dom = DominatorTree::compute(&cfg);
        assert_eq!(dom.idom(header), Some(entry));
        assert_eq!(dom.idom(body), Some(header));
        assert_eq!(dom.idom(exit), Some(header));

        // The header dominates everything in and after the loop.
        assert!(dom.dominates(header, body));
        assert!(dom.dominates(header, exit));
        // The body is inside the loop; it does not dominate the exit (the
        // header can branch straight to exit without entering the body).
        assert!(!dom.dominates(body, exit));
    }

    /// Whether `target` is reachable from the entry, optionally with `removed`
    /// deleted from the graph.
    fn reaches(cfg: &Cfg, target: BlockId, removed: Option<BlockId>) -> bool {
        let entry = cfg.entry;
        if removed == Some(entry) {
            return false;
        }
        let mut seen = vec![false; cfg.block_count()];
        seen[entry.as_u32() as usize] = true;
        let mut stack = vec![entry];
        while let Some(block) = stack.pop() {
            if block == target {
                return true;
            }
            for successor in successors_of(cfg, block) {
                if removed == Some(successor) {
                    continue;
                }
                let index = successor.as_u32() as usize;
                if !seen[index] {
                    seen[index] = true;
                    stack.push(successor);
                }
            }
        }
        false
    }

    /// Dominance straight from the definition: `b` must stop being reachable
    /// once `a` is deleted from the graph. This shares no code with the CHK
    /// fixpoint or the preorder intervals, so it is a real oracle for both.
    fn dominates_by_definition(cfg: &Cfg, a: BlockId, b: BlockId) -> bool {
        if !reaches(cfg, b, None) {
            // An unreachable block is dominated only by itself, which is the
            // convention `DominatorTree::dominates` documents.
            return a == b;
        }
        a == b || !reaches(cfg, b, Some(a))
    }

    /// Check every block pair of `cfg` against the definition, plus every
    /// block's reachability.
    fn assert_matches_definition(cfg: &Cfg) {
        let dom = DominatorTree::compute(cfg);
        for i in 0..cfg.block_count() {
            let a = BlockId::from_raw(i as u32);
            assert_eq!(
                dom.is_reachable(a),
                reaches(cfg, a, None),
                "reachability of {a}"
            );
            for j in 0..cfg.block_count() {
                let b = BlockId::from_raw(j as u32);
                assert_eq!(
                    dom.dominates(a, b),
                    dominates_by_definition(cfg, a, b),
                    "dominates({a}, {b})"
                );
            }
        }
    }

    #[test]
    fn test_dominance_matches_the_definition_on_reducible_graphs() {
        // Straight line with a diamond hanging off it, then a loop whose body
        // has its own branch, then a dead island.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let t = cfg.new_block();
        let e = cfg.new_block();
        let header = cfg.new_block();
        let body = cfg.new_block();
        let body_alt = cfg.new_block();
        let exit = cfg.new_block();
        let island = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, t, e));
        cfg.set_terminator(t, goto(header));
        cfg.set_terminator(e, goto(header));
        let loop_cond = bool_const(&mut cfg, header);
        cfg.set_terminator(header, branch(loop_cond, body, exit));
        let body_cond = bool_const(&mut cfg, body);
        cfg.set_terminator(body, branch(body_cond, body_alt, header));
        cfg.set_terminator(body_alt, goto(header));
        cfg.set_terminator(exit, Terminator::Return { value: None });
        cfg.set_terminator(island, goto(exit));

        assert_matches_definition(&cfg);
    }

    #[test]
    fn test_dominance_matches_the_definition_on_an_irreducible_graph() {
        // Two loop headers entered from outside, each branching into the other:
        // no single back edge, so the CHK fixpoint needs more than one sweep.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let left = cfg.new_block();
        let right = cfg.new_block();
        let exit = cfg.new_block();

        let cond = bool_const(&mut cfg, entry);
        cfg.set_terminator(entry, branch(cond, left, right));
        let left_cond = bool_const(&mut cfg, left);
        cfg.set_terminator(left, branch(left_cond, right, exit));
        let right_cond = bool_const(&mut cfg, right);
        cfg.set_terminator(right, branch(right_cond, left, exit));
        cfg.set_terminator(exit, Terminator::Return { value: None });

        assert_matches_definition(&cfg);
    }

    #[test]
    fn test_dominance_matches_the_definition_on_a_switch_fan_out() {
        // The verifier's motivating shape (RUE-1544): one Switch over many arm
        // blocks that all rejoin. Every arm is a child of the switch block, so
        // the preorder intervals here are all singletons but the join's is not.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let join = cfg.new_block();
        let arms: Vec<BlockId> = (0..6).map(|_| cfg.new_block()).collect();

        let scrutinee = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            },
        );
        let cases = cfg
            .push_switch_cases(
                arms.iter()
                    .enumerate()
                    .skip(1)
                    .map(|(index, &arm)| (index as i64, arm))
                    .collect::<Vec<_>>(),
            )
            .expect("switch cases fit");
        cfg.set_terminator(
            entry,
            Terminator::Switch {
                scrutinee,
                cases,
                default: arms[0],
            },
        );
        for &arm in &arms {
            cfg.set_terminator(arm, goto(join));
        }
        cfg.set_terminator(join, Terminator::Return { value: None });

        assert_matches_definition(&cfg);
    }

    #[test]
    fn test_unreachable_block_has_no_idom() {
        // entry -> a; `island` has no path from entry.
        let mut cfg = make_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let a = cfg.new_block();
        let island = cfg.new_block();
        cfg.set_terminator(entry, goto(a));
        cfg.set_terminator(a, Terminator::Return { value: None });
        cfg.set_terminator(island, Terminator::Return { value: None });

        let dom = DominatorTree::compute(&cfg);
        assert_eq!(dom.idom(island), None);
        // Nothing reachable dominates the unreachable block, and it dominates
        // nothing but itself.
        assert!(!dom.dominates(entry, island));
        assert!(dom.dominates(island, island));
        assert!(!dom.dominates(island, a));
    }
}
