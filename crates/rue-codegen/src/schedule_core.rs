//! Shared instruction scheduling core.
//!
//! Backends supply target-specific instruction facts. This module owns the
//! dependency DAG, priority calculation, list scheduling, and basic-block
//! traversal used by both machine backends.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::hash::Hash;

use crate::reg_class::RegClass;

/// Backend-specific facts required by the shared scheduler.
pub trait SchedulerAdapter {
    /// Backend MIR instruction type.
    type Inst: Clone;
    /// Backend physical register type.
    type Reg: Copy + Eq + Hash;

    /// The register class `reg` belongs to.
    ///
    /// The dependency graph keeps its per-register bookkeeping separately per
    /// class (see [`RegTracker`]), so a backend that numbers its
    /// floating-point registers independently of its integer ones cannot have
    /// the two collide. Both backends answer [`RegClass::Gp`] for every
    /// register today (RUE-1067).
    fn reg_class(&self, reg: Self::Reg) -> RegClass;

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

/// The dependency graph's per-physical-register bookkeeping, partitioned by
/// [`RegClass`].
///
/// "Which instruction last wrote this register" and "which instructions have
/// read it since" are what every RAW, WAW, WAR, and clobber edge is built
/// from. The maps are held one pair per register class rather than one pair
/// overall because registers of different classes are different registers:
/// nothing a floating-point register does can order an integer register's
/// readers, and a backend that numbers its floating-point registers
/// independently of its integer ones must not have the two share a map key.
///
/// Every register both backends name is [`RegClass::Gp`] today, so the `Fp`
/// partition stays empty and the resulting graph is exactly the one a single
/// pair of maps produced (RUE-1067).
struct RegTracker<Reg> {
    last_writer: [HashMap<Reg, usize>; RegClass::COUNT],
    last_readers: [HashMap<Reg, Vec<usize>>; RegClass::COUNT],
}

impl<Reg: Copy + Eq + Hash> RegTracker<Reg> {
    fn new() -> Self {
        Self {
            last_writer: std::array::from_fn(|_| HashMap::new()),
            last_readers: std::array::from_fn(|_| HashMap::new()),
        }
    }

    /// The instruction that last wrote or clobbered `reg`, if any.
    fn last_writer(&self, class: RegClass, reg: &Reg) -> Option<usize> {
        self.last_writer[class.index()].get(reg).copied()
    }

    /// The instructions that have read `reg` since it was last written.
    fn last_readers(&self, class: RegClass, reg: &Reg) -> Option<&Vec<usize>> {
        self.last_readers[class.index()].get(reg)
    }

    /// Record that instruction `idx` wrote or clobbered `reg`.
    ///
    /// Readers recorded before the write belong to the previous value, so they
    /// are dropped: a later instruction cannot have a WAR dependency on them.
    fn record_write(&mut self, class: RegClass, reg: Reg, idx: usize) {
        self.last_writer[class.index()].insert(reg, idx);
        self.last_readers[class.index()].remove(&reg);
    }

    /// Record that instruction `idx` read `reg`.
    fn record_read(&mut self, class: RegClass, reg: Reg, idx: usize) {
        self.last_readers[class.index()]
            .entry(reg)
            .or_default()
            .push(idx);
    }
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
    // Every edge discovered while visiting instruction `to` comes from an
    // earlier dense instruction index. Remember the most recent target seen
    // for each predecessor: equal targets are duplicate edges, while a later
    // target starts a new edge without clearing the whole table.
    let mut last_edge_target = vec![usize::MAX; block_len];

    // Track the last writer and the readers since that write, per register
    // class (see `RegTracker`).
    let mut regs: RegTracker<A::Reg> = RegTracker::new();
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
            if let Some(writer) = regs.last_writer(adapter.reg_class(*reg), reg) {
                add_edge(&mut nodes, &mut last_edge_target, writer, i);
            }
        }

        // WAW (Write After Write): this instruction writes what another wrote.
        for reg in &writes {
            if let Some(prev_writer) = regs.last_writer(adapter.reg_class(*reg), reg) {
                add_edge(&mut nodes, &mut last_edge_target, prev_writer, i);
            }
        }

        // WAR (Write After Read): this instruction writes what another read.
        for reg in &writes {
            if let Some(readers) = regs.last_readers(adapter.reg_class(*reg), reg) {
                for &reader in readers {
                    if reader != i {
                        add_edge(&mut nodes, &mut last_edge_target, reader, i);
                    }
                }
            }
        }

        // FLAGS dependencies.
        if adapter.reads_flags(inst)
            && let Some(writer) = last_flags_writer
        {
            add_edge(&mut nodes, &mut last_edge_target, writer, i);
        }

        if adapter.writes_flags(inst)
            && let Some(prev_writer) = last_flags_writer
        {
            add_edge(&mut nodes, &mut last_edge_target, prev_writer, i);
        }

        if adapter.writes_flags(inst) {
            for &reader in &last_flags_readers {
                if reader != i {
                    add_edge(&mut nodes, &mut last_edge_target, reader, i);
                }
            }
        }

        // Memory dependencies (conservative: order all memory accesses).
        if adapter.accesses_memory(inst) {
            if let Some(prev) = last_memory_access {
                add_edge(&mut nodes, &mut last_edge_target, prev, i);
            }
            last_memory_access = Some(i);
        }

        let clobbers = adapter.clobbers(inst);
        // Clobber dependencies.
        for &clobbered in &clobbers {
            let class = adapter.reg_class(clobbered);
            // This instruction clobbers the register, so it must come after
            // any readers.
            if let Some(readers) = regs.last_readers(class, &clobbered) {
                for &reader in readers {
                    if reader != i {
                        add_edge(&mut nodes, &mut last_edge_target, reader, i);
                    }
                }
            }
            // And after the last writer.
            if let Some(writer) = regs.last_writer(class, &clobbered) {
                add_edge(&mut nodes, &mut last_edge_target, writer, i);
            }
        }

        // Update tracking. Clobbers count as writes here: a later instruction
        // that writes (WAW) or reads (RAW) a clobbered register must not be
        // scheduled above the clobberer, or the clobber destroys its value.
        for clobbered in clobbers {
            regs.record_write(adapter.reg_class(clobbered), clobbered, i);
        }
        for reg in writes {
            regs.record_write(adapter.reg_class(reg), reg, i);
        }
        for reg in reads {
            regs.record_read(adapter.reg_class(reg), reg, i);
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

fn add_edge(nodes: &mut [SchedNode], last_edge_target: &mut [usize], from: usize, to: usize) {
    #[cfg(test)]
    assert!(
        from < to,
        "dependency edges must follow dense instruction order"
    );

    if last_edge_target[from] != to {
        last_edge_target[from] = to;
        nodes[to].deps.push(from);
        nodes[from].users.push(to);
    }
}

/// Calculate priority for each node (critical path length to exit).
///
/// `priority[idx] = latency[idx] + max(priority[u] for u in users[idx])`, i.e.
/// the longest path from `idx` to any exit node in the dependency DAG. Every
/// dependency edge points from an earlier dense instruction index to a
/// later one, so reverse instruction order is already a topological order.
pub(crate) fn calculate_priorities(nodes: &mut [SchedNode]) {
    for idx in (0..nodes.len()).rev() {
        let max_user = nodes[idx]
            .users
            .iter()
            .map(|&user| {
                #[cfg(test)]
                assert!(
                    user > idx,
                    "priority edges must follow dense instruction order"
                );
                nodes[user].priority
            })
            .max()
            .unwrap_or(0);
        nodes[idx].priority = nodes[idx].latency + max_user;
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

    /// A register named by class and number, so the same number can appear in
    /// two classes — the aliasing case the partitioned [`RegTracker`] rules
    /// out. No backend has such a register type yet.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct ClassedReg(RegClass, u32);

    /// A minimal instruction: what it reads, what it writes, nothing else.
    #[derive(Debug, Clone)]
    struct TestInst {
        reads: Vec<ClassedReg>,
        writes: Vec<ClassedReg>,
    }

    struct TestAdapter;

    impl SchedulerAdapter for TestAdapter {
        type Inst = TestInst;
        type Reg = ClassedReg;

        fn reg_class(&self, reg: Self::Reg) -> RegClass {
            reg.0
        }

        fn latency(&self, _inst: &Self::Inst) -> u32 {
            1
        }

        fn is_barrier(&self, _inst: &Self::Inst) -> bool {
            false
        }

        fn accesses_memory(&self, _inst: &Self::Inst) -> bool {
            false
        }

        fn regs_read(&self, inst: &Self::Inst) -> Vec<Self::Reg> {
            inst.reads.clone()
        }

        fn regs_written(&self, inst: &Self::Inst) -> Vec<Self::Reg> {
            inst.writes.clone()
        }

        fn clobbers(&self, _inst: &Self::Inst) -> Vec<Self::Reg> {
            Vec::new()
        }

        fn writes_flags(&self, _inst: &Self::Inst) -> bool {
            false
        }

        fn reads_flags(&self, _inst: &Self::Inst) -> bool {
            false
        }
    }

    fn inst(reads: &[ClassedReg], writes: &[ClassedReg]) -> TestInst {
        TestInst {
            reads: reads.to_vec(),
            writes: writes.to_vec(),
        }
    }

    #[test]
    fn same_class_registers_still_carry_a_read_after_write_dependency() {
        let gp0 = ClassedReg(RegClass::Gp, 0);
        let instructions = vec![inst(&[], &[gp0]), inst(&[gp0], &[])];

        let nodes = build_dep_graph::<TestAdapter>(&instructions, 0, 2, &TestAdapter);

        assert_eq!(nodes[1].deps, vec![0], "the reader depends on the writer");
        assert_eq!(nodes[0].users, vec![1]);
    }

    #[test]
    fn a_write_in_one_class_does_not_order_a_read_in_another() {
        // Same register *number*, different classes: two distinct machine
        // registers, so the second instruction reads something the first never
        // wrote and the two may be scheduled in either order.
        let gp0 = ClassedReg(RegClass::Gp, 0);
        let fp0 = ClassedReg(RegClass::Fp, 0);
        let instructions = vec![inst(&[], &[gp0]), inst(&[fp0], &[])];

        let nodes = build_dep_graph::<TestAdapter>(&instructions, 0, 2, &TestAdapter);

        assert!(
            nodes[1].deps.is_empty(),
            "a general-purpose write cannot order a floating-point read"
        );
        assert!(nodes[0].users.is_empty());
    }

    #[test]
    fn a_write_in_one_class_does_not_order_a_write_in_another() {
        let gp3 = ClassedReg(RegClass::Gp, 3);
        let fp3 = ClassedReg(RegClass::Fp, 3);
        let instructions = vec![inst(&[], &[gp3]), inst(&[], &[fp3])];

        let nodes = build_dep_graph::<TestAdapter>(&instructions, 0, 2, &TestAdapter);

        assert!(
            nodes[1].deps.is_empty(),
            "write-after-write is a per-register fact, and these are two registers"
        );
    }

    #[test]
    fn a_write_in_one_class_does_not_order_an_earlier_read_in_another() {
        // WAR: instruction 1 reads gp5, instruction 2 writes fp5. Different
        // registers, so nothing forces instruction 2 to stay after it.
        let gp5 = ClassedReg(RegClass::Gp, 5);
        let fp5 = ClassedReg(RegClass::Fp, 5);
        let instructions = vec![inst(&[], &[gp5]), inst(&[gp5], &[]), inst(&[], &[fp5])];

        let nodes = build_dep_graph::<TestAdapter>(&instructions, 0, 3, &TestAdapter);

        assert_eq!(nodes[1].deps, vec![0]);
        assert!(
            nodes[2].deps.is_empty(),
            "write-after-read is a per-register fact, and these are two registers"
        );
    }

    #[test]
    fn dense_graph_bookkeeping_preserves_edges_and_critical_path_priorities() {
        let mut nodes = [2, 4, 3, 1]
            .into_iter()
            .map(SchedNode::new)
            .collect::<Vec<_>>();
        let mut last_edge_target = vec![usize::MAX; nodes.len()];

        add_edge(&mut nodes, &mut last_edge_target, 0, 1);
        add_edge(&mut nodes, &mut last_edge_target, 0, 2);
        add_edge(&mut nodes, &mut last_edge_target, 1, 3);
        add_edge(&mut nodes, &mut last_edge_target, 2, 3);
        add_edge(&mut nodes, &mut last_edge_target, 1, 3);
        calculate_priorities(&mut nodes);

        assert_eq!(nodes[0].users, vec![1, 2]);
        assert_eq!(nodes[3].deps, vec![1, 2]);
        assert_eq!(
            nodes.iter().map(|node| node.priority).collect::<Vec<_>>(),
            vec![7, 5, 4, 1]
        );
    }

    #[test]
    fn high_fan_in_readiness_work_is_edge_linear_and_ties_are_stable() {
        const FAN_IN: usize = 4_096;
        let sink = FAN_IN;
        let mut nodes = (0..=sink).map(|_| SchedNode::new(1)).collect::<Vec<_>>();
        let mut last_edge_target = vec![usize::MAX; nodes.len()];
        for predecessor in 0..FAN_IN {
            add_edge(&mut nodes, &mut last_edge_target, predecessor, sink);
            add_edge(&mut nodes, &mut last_edge_target, predecessor, sink);
        }
        calculate_priorities(&mut nodes);

        let (order, work) = schedule_block_with_work(&nodes);
        let edge_count = nodes.iter().map(|node| node.users.len()).sum::<usize>();

        assert_eq!(edge_count, FAN_IN, "duplicate edges are discarded once");
        assert_eq!(nodes[sink].deps.len(), FAN_IN);
        assert_eq!(order.len(), FAN_IN + 1);
        assert_eq!(order[..FAN_IN], (0..FAN_IN).collect::<Vec<_>>());
        assert_eq!(order[FAN_IN], sink);
        assert_eq!(work.nodes_enqueued, FAN_IN + 1);
        assert_eq!(work.ready_heap_pops, FAN_IN + 1);
        assert_eq!(work.dependency_edges_completed, FAN_IN);
        assert_eq!(
            work.nodes_enqueued + work.dependency_edges_completed,
            nodes.len() + edge_count,
            "readiness work is one enqueue per vertex and one decrement per edge"
        );
    }
}
