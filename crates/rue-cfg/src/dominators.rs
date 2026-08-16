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
//! definite-initialization argument into an always-on checked invariant, and
//! natural-loop analysis (RUE-926, `opt/loops.rs`) and LICM (RUE-927,
//! `opt/licm.rs`, run at `-O3` from `opt/mod.rs`) build the loop forest and its
//! hoisting decisions on it.
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
//!
//! `intersect(a, b)` walks the two fingers up the partial tree, each time
//! advancing whichever finger has the smaller postorder number (i.e. is deeper,
//! farther from the entry), until they meet at the common dominator.
//!
//! The pipeline consumers are value forwarding's Rule 1 dominance check
//! (`opt/forward.rs`), natural-loop detection (`opt/loops.rs`), and LICM
//! (`opt/licm.rs`); the `idom` query itself is exercised only by this module's
//! tests and carries a targeted allow below.

use crate::{BlockId, Cfg, Terminator};

/// The immediate-dominator tree of a [`Cfg`].
///
/// Query it with [`DominatorTree::idom`] and [`DominatorTree::dominates`].
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
    post_num: Vec<Option<u32>>,
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

        DominatorTree { idom, post_num }
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
    pub(crate) fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        let bi = b.as_u32() as usize;
        if bi >= self.idom.len() {
            return a == b;
        }
        // Unreachable blocks have no idom entry; only self-dominance holds.
        if self.post_num[bi].is_none() {
            return a == b;
        }
        // Walk up b's idom chain to the entry. The entry's self-loop terminates
        // the walk.
        let mut cur = b;
        loop {
            if cur == a {
                return true;
            }
            match self.idom[cur.as_u32() as usize] {
                Some(d) if d != cur => cur = d,
                // Reached the entry sentinel (d == cur) without finding `a`.
                _ => return false,
            }
        }
    }
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
