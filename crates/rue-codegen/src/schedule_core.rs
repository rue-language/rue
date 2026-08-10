//! Shared instruction scheduling core.
//!
//! Backends supply target-specific instruction facts. This module owns the
//! dependency DAG, priority calculation, list scheduling, and basic-block
//! traversal used by both machine backends.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::Hash;

/// Backend-specific facts required by the shared scheduler.
pub trait SchedulerAdapter {
    /// Backend MIR instruction type.
    type Inst: Clone;
    /// Backend physical register type.
    type Reg: Copy + Eq + Hash;

    /// Latency in cycles until an instruction's result is ready.
    fn latency(&self, inst: &Self::Inst) -> u32;

    /// Whether this instruction prevents scheduling across it.
    fn is_barrier(&self, inst: &Self::Inst) -> bool;

    /// Whether this instruction accesses memory. Memory accesses are ordered
    /// conservatively.
    fn accesses_memory(&self, inst: &Self::Inst) -> bool;

    /// Physical registers read by this instruction.
    fn regs_read(&self, inst: &Self::Inst) -> Vec<Self::Reg>;

    /// Physical registers written by this instruction.
    fn regs_written(&self, inst: &Self::Inst) -> Vec<Self::Reg>;

    /// Physical registers clobbered by this instruction.
    fn clobbers(&self, inst: &Self::Inst) -> Vec<Self::Reg>;

    /// Whether this instruction writes the target's condition flags.
    fn writes_flags(&self, inst: &Self::Inst) -> bool;

    /// Whether this instruction reads the target's condition flags.
    fn reads_flags(&self, inst: &Self::Inst) -> bool;
}

/// A node in the scheduling dependency graph.
#[derive(Debug)]
pub(crate) struct SchedNode {
    /// Instructions this depends on (must execute before this).
    pub(crate) deps: Vec<usize>,
    /// Instructions that depend on this (must execute after this).
    pub(crate) users: Vec<usize>,
    /// Scheduling priority (higher = schedule earlier).
    pub(crate) priority: u32,
    /// Latency in cycles until result is ready.
    latency: u32,
}

impl SchedNode {
    fn new(latency: u32) -> Self {
        Self {
            deps: Vec::new(),
            users: Vec::new(),
            priority: 0,
            latency,
        }
    }
}

/// A ready instruction with its priority, for the scheduling queue.
#[derive(Debug, Eq, PartialEq)]
struct ReadyInst {
    priority: u32,
    idx: usize,
}

impl Ord for ReadyInst {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, break ties by lower index (original order).
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.idx.cmp(&self.idx))
    }
}

impl PartialOrd for ReadyInst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Schedule instructions in place.
///
/// This reorders instructions within basic blocks only. Control-flow boundaries
/// and other target-defined barriers are preserved.
pub fn schedule_instructions<A>(instructions: &mut Vec<A::Inst>, adapter: &A)
where
    A: SchedulerAdapter,
{
    if instructions.len() < 3 {
        // Not worth scheduling very small functions.
        return;
    }

    // Find basic block boundaries.
    let mut block_starts = vec![0usize];
    for (i, inst) in instructions.iter().enumerate() {
        if adapter.is_barrier(inst) {
            // The barrier is the last instruction of the current block. The
            // next block starts after the barrier.
            if i + 1 < instructions.len() {
                block_starts.push(i + 1);
            }
        }
    }
    block_starts.push(instructions.len());

    let mut new_instructions = Vec::with_capacity(instructions.len());

    for window in block_starts.windows(2) {
        let start = window[0];
        let end = window[1];

        if end - start <= 2 {
            new_instructions.extend_from_slice(&instructions[start..end]);
            continue;
        }

        // If the block ends with a barrier, exclude it from scheduling and
        // re-emit it after the scheduled interior.
        let last_is_barrier = adapter.is_barrier(&instructions[end - 1]);
        let sched_end = if last_is_barrier { end - 1 } else { end };

        if sched_end - start <= 2 {
            new_instructions.extend_from_slice(&instructions[start..end]);
            continue;
        }

        let mut nodes = build_dep_graph(instructions, start, sched_end, adapter);
        calculate_priorities(&mut nodes);
        let order = schedule_block(&nodes);

        for &idx in &order {
            new_instructions.push(instructions[start + idx].clone());
        }

        if last_is_barrier {
            new_instructions.push(instructions[end - 1].clone());
        }
    }

    *instructions = new_instructions;
}

/// Build the dependency graph for a basic block of instructions.
pub(crate) fn build_dep_graph<A>(
    instructions: &[A::Inst],
    start: usize,
    end: usize,
    adapter: &A,
) -> Vec<SchedNode>
where
    A: SchedulerAdapter,
{
    let block_len = end - start;
    let mut nodes: Vec<SchedNode> = instructions[start..end]
        .iter()
        .map(|inst| SchedNode::new(adapter.latency(inst)))
        .collect();
    let mut edges = HashSet::new();

    // Track last writer of each register.
    let mut last_writer: HashMap<A::Reg, usize> = HashMap::new();
    // Track last readers of each register (for WAR dependencies).
    let mut last_readers: HashMap<A::Reg, Vec<usize>> = HashMap::new();
    // Track last memory access (conservative).
    let mut last_memory_access: Option<usize> = None;
    // Track last FLAGS writer and readers.
    let mut last_flags_writer: Option<usize> = None;
    let mut last_flags_readers: Vec<usize> = Vec::new();

    for i in 0..block_len {
        let inst = &instructions[start + i];
        let reads = adapter.regs_read(inst);
        let writes = adapter.regs_written(inst);

        // RAW (Read After Write): this instruction reads what another wrote.
        for reg in &reads {
            if let Some(&writer) = last_writer.get(reg) {
                add_edge(&mut nodes, &mut edges, writer, i);
            }
        }

        // WAW (Write After Write): this instruction writes what another wrote.
        for reg in &writes {
            if let Some(&prev_writer) = last_writer.get(reg) {
                add_edge(&mut nodes, &mut edges, prev_writer, i);
            }
        }

        // WAR (Write After Read): this instruction writes what another read.
        for reg in &writes {
            if let Some(readers) = last_readers.get(reg) {
                for &reader in readers {
                    if reader != i {
                        add_edge(&mut nodes, &mut edges, reader, i);
                    }
                }
            }
        }

        // FLAGS dependencies.
        if adapter.reads_flags(inst)
            && let Some(writer) = last_flags_writer
        {
            add_edge(&mut nodes, &mut edges, writer, i);
        }

        if adapter.writes_flags(inst)
            && let Some(prev_writer) = last_flags_writer
        {
            add_edge(&mut nodes, &mut edges, prev_writer, i);
        }

        if adapter.writes_flags(inst) {
            for &reader in &last_flags_readers {
                if reader != i {
                    add_edge(&mut nodes, &mut edges, reader, i);
                }
            }
        }

        // Memory dependencies (conservative: order all memory accesses).
        if adapter.accesses_memory(inst) {
            if let Some(prev) = last_memory_access {
                add_edge(&mut nodes, &mut edges, prev, i);
            }
            last_memory_access = Some(i);
        }

        let clobbers = adapter.clobbers(inst);
        // Clobber dependencies.
        for &clobbered in &clobbers {
            // This instruction clobbers the register, so it must come after
            // any readers.
            if let Some(readers) = last_readers.get(&clobbered) {
                for &reader in readers {
                    if reader != i {
                        add_edge(&mut nodes, &mut edges, reader, i);
                    }
                }
            }
            // And after the last writer.
            if let Some(&writer) = last_writer.get(&clobbered) {
                add_edge(&mut nodes, &mut edges, writer, i);
            }
        }

        // Update tracking. Clobbers count as writes here: a later instruction
        // that writes (WAW) or reads (RAW) a clobbered register must not be
        // scheduled above the clobberer, or the clobber destroys its value.
        for clobbered in clobbers {
            last_writer.insert(clobbered, i);
            last_readers.remove(&clobbered);
        }
        for reg in writes {
            last_writer.insert(reg, i);
            last_readers.remove(&reg);
        }
        for reg in reads {
            last_readers.entry(reg).or_default().push(i);
        }

        if adapter.writes_flags(inst) {
            last_flags_writer = Some(i);
            last_flags_readers.clear();
        }
        if adapter.reads_flags(inst) {
            last_flags_readers.push(i);
        }
    }

    nodes
}

fn add_edge(nodes: &mut [SchedNode], edges: &mut HashSet<(usize, usize)>, from: usize, to: usize) {
    if edges.insert((from, to)) {
        nodes[to].deps.push(from);
        nodes[from].users.push(to);
    }
}

/// Calculate priority for each node (critical path length to exit).
///
/// `priority[idx] = latency[idx] + max(priority[u] for u in users[idx])`, i.e.
/// the longest path from `idx` to any exit node in the dependency DAG. This is
/// computed with an explicit-stack, memoized post-order traversal rather than
/// recursion.
pub(crate) fn calculate_priorities(nodes: &mut [SchedNode]) {
    let mut memo: HashMap<usize, u32> = HashMap::new();
    let mut stack: Vec<(usize, bool)> = Vec::new();

    for start in 0..nodes.len() {
        if memo.contains_key(&start) {
            continue;
        }
        stack.push((start, false));
        while let Some((idx, expanded)) = stack.pop() {
            if memo.contains_key(&idx) {
                continue;
            }
            if expanded {
                let max_user = nodes[idx]
                    .users
                    .iter()
                    .map(|&u| memo[&u])
                    .max()
                    .unwrap_or(0);
                memo.insert(idx, nodes[idx].latency + max_user);
            } else {
                stack.push((idx, true));
                for &u in &nodes[idx].users {
                    if !memo.contains_key(&u) {
                        stack.push((u, false));
                    }
                }
            }
        }
    }

    for i in 0..nodes.len() {
        nodes[i].priority = memo[&i];
    }
}

/// Schedule instructions within a basic block using list scheduling.
pub(crate) fn schedule_block(nodes: &[SchedNode]) -> Vec<usize> {
    schedule_block_with_work(nodes).0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScheduleWork {
    #[cfg(test)]
    nodes_enqueued: usize,
    #[cfg(test)]
    ready_heap_pops: usize,
    #[cfg(test)]
    dependency_edges_completed: usize,
}

impl ScheduleWork {
    #[inline(always)]
    fn node_enqueued(&mut self) {
        #[cfg(test)]
        {
            self.nodes_enqueued += 1;
        }
    }

    #[inline(always)]
    fn ready_heap_popped(&mut self) {
        #[cfg(test)]
        {
            self.ready_heap_pops += 1;
        }
    }

    #[inline(always)]
    fn dependency_edge_completed(&mut self) {
        #[cfg(test)]
        {
            self.dependency_edges_completed += 1;
        }
    }
}

fn schedule_block_with_work(nodes: &[SchedNode]) -> (Vec<usize>, ScheduleWork) {
    if nodes.is_empty() {
        return (Vec::new(), ScheduleWork::default());
    }

    let mut scheduled = Vec::with_capacity(nodes.len());
    let mut remaining_deps = nodes.iter().map(|node| node.deps.len()).collect::<Vec<_>>();
    let mut ready: BinaryHeap<ReadyInst> = BinaryHeap::new();
    let mut work = ScheduleWork::default();

    for (idx, node) in nodes.iter().enumerate() {
        if remaining_deps[idx] == 0 {
            ready.push(ReadyInst {
                priority: node.priority,
                idx,
            });
            work.node_enqueued();
        }
    }

    while let Some(ReadyInst { idx, .. }) = ready.pop() {
        work.ready_heap_popped();
        scheduled.push(idx);

        for &user in &nodes[idx].users {
            remaining_deps[user] -= 1;
            work.dependency_edge_completed();
            if remaining_deps[user] == 0 {
                ready.push(ReadyInst {
                    priority: nodes[user].priority,
                    idx: user,
                });
                work.node_enqueued();
            }
        }
    }

    (scheduled, work)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_fan_in_readiness_work_is_edge_linear_and_ties_are_stable() {
        const FAN_IN: usize = 4_096;
        let sink = FAN_IN;
        let mut nodes = (0..=sink).map(|_| SchedNode::new(1)).collect::<Vec<_>>();
        let mut edges = HashSet::new();
        for predecessor in 0..FAN_IN {
            add_edge(&mut nodes, &mut edges, predecessor, sink);
            add_edge(&mut nodes, &mut edges, predecessor, sink);
        }
        calculate_priorities(&mut nodes);

        let (order, work) = schedule_block_with_work(&nodes);

        assert_eq!(edges.len(), FAN_IN, "duplicate edges are discarded once");
        assert_eq!(nodes[sink].deps.len(), FAN_IN);
        assert_eq!(order.len(), FAN_IN + 1);
        assert_eq!(order[..FAN_IN], (0..FAN_IN).collect::<Vec<_>>());
        assert_eq!(order[FAN_IN], sink);
        assert_eq!(work.nodes_enqueued, FAN_IN + 1);
        assert_eq!(work.ready_heap_pops, FAN_IN + 1);
        assert_eq!(work.dependency_edges_completed, FAN_IN);
        assert_eq!(
            work.nodes_enqueued + work.dependency_edges_completed,
            nodes.len() + edges.len(),
            "readiness work is one enqueue per vertex and one decrement per edge"
        );
    }
}
