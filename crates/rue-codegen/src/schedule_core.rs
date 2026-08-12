//! Shared instruction scheduling core.
//!
//! Backends supply target-specific instruction facts. This module owns the
//! dependency DAG, priority calculation, list scheduling, and basic-block
//! traversal used by both machine backends.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::hash::Hash;
use std::ops::Deref;

use smallvec::{IntoIter, SmallVec};

use crate::reg_class::RegClass;

/// Dependency edges attached to one scheduled instruction.
///
/// Lattice's maintained AArch64 compiler workload keeps 93.6% of incoming and
/// 88.6% of outgoing node degrees at two or fewer edges. Keep that common case
/// in the node allocation while allowing uncommon fan-in and fan-out to spill.
pub(crate) type EdgeList = SmallVec<[usize; 2]>;

/// Instructions that have read one physical register or the condition flags
/// since the corresponding value's last write.
///
/// These transient dependency facts contain one or two instruction indices in
/// the common case. Keep those indices inside the map value while allowing an
/// unusually long run of readers to spill without changing scheduler policy.
type DependencyReaderList = SmallVec<[usize; 2]>;

/// Physical-register facts attached to one scheduled instruction.
///
/// Both MIRs name at most three read or written registers on one instruction.
/// The hard bound prevents this transient per-instruction value from silently
/// falling back to heap allocation if a future MIR form grows that contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegList<R>(SmallVec<[R; 3]>);

impl<R> RegList<R> {
    /// Create an empty inline register list.
    #[must_use]
    pub fn new() -> Self {
        Self(SmallVec::new())
    }

    /// Append one register within the audited MIR bound.
    ///
    /// # Panics
    ///
    /// Panics when one instruction exposes more than three register facts.
    pub fn push(&mut self, reg: R) {
        assert!(
            self.0.len() < self.0.inline_size(),
            "inline scheduler register-fact capacity exceeded"
        );
        self.0.push(reg);
    }
}

impl<R> Default for RegList<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> Deref for RegList<R> {
    type Target = [R];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<R> IntoIterator for RegList<R> {
    type Item = R;
    type IntoIter = IntoIter<[R; 3]>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<R> From<RegList<R>> for Vec<R> {
    fn from(regs: RegList<R>) -> Self {
        regs.0.into_vec()
    }
}

impl<'a, R> IntoIterator for &'a RegList<R> {
    type Item = &'a R;
    type IntoIter = std::slice::Iter<'a, R>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Backend-specific facts required by the shared scheduler.
pub trait SchedulerAdapter {
    /// Backend MIR instruction type.
    type Inst;
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
    fn regs_read(&self, inst: &Self::Inst) -> RegList<Self::Reg>;

    /// Physical registers written by this instruction.
    fn regs_written(&self, inst: &Self::Inst) -> RegList<Self::Reg>;

    /// Physical registers clobbered by this instruction.
    fn clobbers(&self, inst: &Self::Inst) -> &[Self::Reg];

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
    last_readers: [HashMap<Reg, DependencyReaderList>; RegClass::COUNT],
}

impl<Reg: Copy + Eq + Hash> RegTracker<Reg> {
    fn new() -> Self {
        Self {
            last_writer: std::array::from_fn(|_| HashMap::new()),
            last_readers: std::array::from_fn(|_| HashMap::new()),
        }
    }

    /// Forget one block's facts while retaining the maps' backing storage.
    fn clear(&mut self) {
        for writers in &mut self.last_writer {
            writers.clear();
        }
        for readers in &mut self.last_readers {
            readers.clear();
        }
    }

    /// The instruction that last wrote or clobbered `reg`, if any.
    fn last_writer(&self, class: RegClass, reg: &Reg) -> Option<usize> {
        self.last_writer[class.index()].get(reg).copied()
    }

    /// The instructions that have read `reg` since it was last written.
    fn last_readers(&self, class: RegClass, reg: &Reg) -> Option<&DependencyReaderList> {
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
    pub(crate) deps: EdgeList,
    /// Instructions that depend on this (must execute after this).
    pub(crate) users: EdgeList,
    /// Scheduling priority (higher = schedule earlier).
    pub(crate) priority: u32,
    /// Latency in cycles until result is ready.
    latency: u32,
}

impl SchedNode {
    fn new(latency: u32) -> Self {
        Self {
            deps: EdgeList::new(),
            users: EdgeList::new(),
            priority: 0,
            latency,
        }
    }

    /// Prepare one retained node slot for another basic block.
    fn reset(&mut self, latency: u32) {
        self.deps.clear();
        self.users.clear();
        self.priority = 0;
        self.latency = latency;
    }
}

/// Dense dependency-graph storage reused across blocks in one function.
#[derive(Default)]
struct GraphScratch {
    nodes: Vec<SchedNode>,
    last_edge_target: Vec<usize>,
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

    // `destination[old] = new`: build the complete order while every block can
    // still inspect the original instruction slice, then realize that
    // permutation in place. Keeping only dense indices avoids cloning every
    // MIR instruction into a second full-width vector.
    let mut destination = (0..instructions.len()).collect::<Vec<_>>();
    let mut block_start = 0;
    let mut regs = RegTracker::new();
    let mut graph = GraphScratch::default();

    for block_end in 0..instructions.len() {
        if !adapter.is_barrier(&instructions[block_end]) {
            continue;
        }

        record_block_permutation(
            instructions,
            adapter,
            block_start,
            block_end,
            &mut destination,
            &mut regs,
            &mut graph,
        );
        block_start = block_end + 1;
    }

    if block_start < instructions.len() {
        record_block_permutation(
            instructions,
            adapter,
            block_start,
            instructions.len(),
            &mut destination,
            &mut regs,
            &mut graph,
        );
    }

    apply_permutation(instructions, &mut destination);
}

/// Record the schedule for the non-barrier instructions in one basic block.
fn record_block_permutation<A>(
    instructions: &[A::Inst],
    adapter: &A,
    start: usize,
    sched_end: usize,
    destination: &mut [usize],
    regs: &mut RegTracker<A::Reg>,
    graph: &mut GraphScratch,
) where
    A: SchedulerAdapter,
{
    if sched_end - start <= 2 {
        return;
    }

    let nodes = build_dep_graph_reusing(instructions, start, sched_end, adapter, regs, graph);
    calculate_priorities(nodes);
    let order = schedule_block(nodes);

    for (new_offset, &old_offset) in order.iter().enumerate() {
        destination[start + old_offset] = start + new_offset;
    }
}

/// Apply an `old index -> new index` permutation in place.
///
/// Swapping the destinations with their elements keeps each destination
/// attached to the original element it describes. Once slot `current` maps to
/// itself, the correct original element occupies that slot and cannot move
/// again.
fn apply_permutation<T>(items: &mut [T], destination: &mut [usize]) {
    #[cfg(test)]
    {
        assert_eq!(items.len(), destination.len());
        let mut complete = destination.to_vec();
        complete.sort_unstable();
        assert_eq!(complete, (0..destination.len()).collect::<Vec<_>>());
    }

    for current in 0..destination.len() {
        while destination[current] != current {
            let target = destination[current];
            items.swap(current, target);
            destination.swap(current, target);
        }
    }
}

/// Build the dependency graph for a basic block of instructions.
#[cfg(test)]
pub(crate) fn build_dep_graph<A>(
    instructions: &[A::Inst],
    start: usize,
    end: usize,
    adapter: &A,
) -> Vec<SchedNode>
where
    A: SchedulerAdapter,
{
    let mut graph = GraphScratch::default();
    build_dep_graph_reusing(
        instructions,
        start,
        end,
        adapter,
        &mut RegTracker::new(),
        &mut graph,
    );
    graph.nodes.truncate(end - start);
    graph.nodes
}

fn build_dep_graph_reusing<'a, A>(
    instructions: &[A::Inst],
    start: usize,
    end: usize,
    adapter: &A,
    regs: &mut RegTracker<A::Reg>,
    graph: &'a mut GraphScratch,
) -> &'a mut [SchedNode]
where
    A: SchedulerAdapter,
{
    let block_len = end - start;
    graph.nodes.resize_with(block_len, || SchedNode::new(0));
    for (node, inst) in graph.nodes[..block_len]
        .iter_mut()
        .zip(&instructions[start..end])
    {
        node.reset(adapter.latency(inst));
    }
    // Every edge discovered while visiting instruction `to` comes from an
    // earlier dense instruction index. Remember the most recent target seen
    // for each predecessor: equal targets are duplicate edges, while a later
    // target starts a new edge without clearing the whole table.
    graph.last_edge_target.clear();
    graph.last_edge_target.resize(block_len, usize::MAX);
    let nodes = &mut graph.nodes[..block_len];
    let last_edge_target = &mut graph.last_edge_target;

    // Track the last writer and the readers since that write, per register
    // class (see `RegTracker`). Retain map capacity across blocks in one MIR.
    regs.clear();
    // Track last memory access (conservative).
    let mut last_memory_access: Option<usize> = None;
    // Track last FLAGS writer and readers.
    let mut last_flags_writer: Option<usize> = None;
    let mut last_flags_readers = DependencyReaderList::new();

    for i in 0..block_len {
        let inst = &instructions[start + i];
        let reads = adapter.regs_read(inst);
        let writes = adapter.regs_written(inst);

        // RAW (Read After Write): this instruction reads what another wrote.
        for reg in &reads {
            if let Some(writer) = regs.last_writer(adapter.reg_class(*reg), reg) {
                add_edge(nodes, last_edge_target, writer, i);
            }
        }

        // WAW (Write After Write): this instruction writes what another wrote.
        for reg in &writes {
            if let Some(prev_writer) = regs.last_writer(adapter.reg_class(*reg), reg) {
                add_edge(nodes, last_edge_target, prev_writer, i);
            }
        }

        // WAR (Write After Read): this instruction writes what another read.
        for reg in &writes {
            if let Some(readers) = regs.last_readers(adapter.reg_class(*reg), reg) {
                for &reader in readers {
                    if reader != i {
                        add_edge(nodes, last_edge_target, reader, i);
                    }
                }
            }
        }

        // FLAGS dependencies.
        if adapter.reads_flags(inst)
            && let Some(writer) = last_flags_writer
        {
            add_edge(nodes, last_edge_target, writer, i);
        }

        if adapter.writes_flags(inst)
            && let Some(prev_writer) = last_flags_writer
        {
            add_edge(nodes, last_edge_target, prev_writer, i);
        }

        if adapter.writes_flags(inst) {
            for &reader in &last_flags_readers {
                if reader != i {
                    add_edge(nodes, last_edge_target, reader, i);
                }
            }
        }

        // Memory dependencies (conservative: order all memory accesses).
        if adapter.accesses_memory(inst) {
            if let Some(prev) = last_memory_access {
                add_edge(nodes, last_edge_target, prev, i);
            }
            last_memory_access = Some(i);
        }

        let clobbers = adapter.clobbers(inst);
        // Clobber dependencies.
        for &clobbered in clobbers {
            let class = adapter.reg_class(clobbered);
            // This instruction clobbers the register, so it must come after
            // any readers.
            if let Some(readers) = regs.last_readers(class, &clobbered) {
                for &reader in readers {
                    if reader != i {
                        add_edge(nodes, last_edge_target, reader, i);
                    }
                }
            }
            // And after the last writer.
            if let Some(writer) = regs.last_writer(class, &clobbered) {
                add_edge(nodes, last_edge_target, writer, i);
            }
        }

        // Update tracking. Clobbers count as writes here: a later instruction
        // that writes (WAW) or reads (RAW) a clobbered register must not be
        // scheduled above the clobberer, or the clobber destroys its value.
        for &clobbered in clobbers {
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

    #[test]
    fn in_place_permutation_moves_a_cycle_and_keeps_fixed_positions() {
        let mut items = ['a', 'b', 'c', 'd'];
        let mut destination = [2, 0, 1, 3];

        apply_permutation(&mut items, &mut destination);

        assert_eq!(items, ['b', 'c', 'a', 'd']);
        assert_eq!(destination, [0, 1, 2, 3]);
    }

    #[test]
    fn scheduler_register_facts_stay_inline_through_the_mir_maximum() {
        let mut regs = RegList::new();
        regs.push(0u8);
        regs.push(1);
        regs.push(2);

        assert_eq!(&*regs, &[0, 1, 2]);
        assert!(!regs.0.spilled());
    }

    #[test]
    #[should_panic(expected = "inline scheduler register-fact capacity exceeded")]
    fn scheduler_register_facts_fail_loudly_when_the_mir_bound_changes() {
        let mut regs = RegList::new();
        for reg in 0u8..4 {
            regs.push(reg);
        }
    }

    /// A register named by class and number, so the same number can appear in
    /// two classes — the aliasing case the partitioned [`RegTracker`] rules
    /// out. No backend has such a register type yet.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct ClassedReg(RegClass, u32);

    #[test]
    fn common_dependency_reader_lists_stay_inline() {
        let reg = ClassedReg(RegClass::Gp, 0);
        let mut tracker = RegTracker::new();

        tracker.record_read(RegClass::Gp, reg, 3);
        tracker.record_read(RegClass::Gp, reg, 7);

        let readers = tracker.last_readers(RegClass::Gp, &reg).unwrap();
        assert_eq!(readers.as_slice(), [3, 7]);
        assert!(!readers.spilled());
    }

    /// A minimal instruction: what it reads, what it writes, nothing else.
    #[derive(Debug, Clone)]
    struct TestInst {
        id: u32,
        reads: Vec<ClassedReg>,
        writes: Vec<ClassedReg>,
        latency: u32,
        barrier: bool,
    }

    struct TestAdapter;

    impl SchedulerAdapter for TestAdapter {
        type Inst = TestInst;
        type Reg = ClassedReg;

        fn reg_class(&self, reg: Self::Reg) -> RegClass {
            reg.0
        }

        fn latency(&self, inst: &Self::Inst) -> u32 {
            inst.latency
        }

        fn is_barrier(&self, inst: &Self::Inst) -> bool {
            inst.barrier
        }

        fn accesses_memory(&self, _inst: &Self::Inst) -> bool {
            false
        }

        fn regs_read(&self, inst: &Self::Inst) -> RegList<Self::Reg> {
            let mut regs = RegList::new();
            for &reg in &inst.reads {
                regs.push(reg);
            }
            regs
        }

        fn regs_written(&self, inst: &Self::Inst) -> RegList<Self::Reg> {
            let mut regs = RegList::new();
            for &reg in &inst.writes {
                regs.push(reg);
            }
            regs
        }

        fn clobbers(&self, _inst: &Self::Inst) -> &[Self::Reg] {
            &[]
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
            id: 0,
            reads: reads.to_vec(),
            writes: writes.to_vec(),
            latency: 1,
            barrier: false,
        }
    }

    fn independent_inst(id: u32, latency: u32, barrier: bool) -> TestInst {
        TestInst {
            id,
            reads: Vec::new(),
            writes: Vec::new(),
            latency,
            barrier,
        }
    }

    #[test]
    fn in_place_scheduling_preserves_barriers_and_block_boundaries() {
        let mut instructions = vec![
            independent_inst(0, 1, false),
            independent_inst(1, 2, false),
            independent_inst(2, 3, false),
            independent_inst(99, 100, true),
            independent_inst(3, 1, false),
            independent_inst(4, 2, false),
            independent_inst(5, 3, false),
        ];

        schedule_instructions(&mut instructions, &TestAdapter);

        assert_eq!(
            instructions.iter().map(|inst| inst.id).collect::<Vec<_>>(),
            vec![2, 1, 0, 99, 5, 4, 3]
        );
    }

    #[test]
    fn direct_block_walk_preserves_leading_adjacent_and_trailing_barriers() {
        let mut instructions = vec![
            independent_inst(90, 100, true),
            independent_inst(91, 100, true),
            independent_inst(0, 1, false),
            independent_inst(1, 2, false),
            independent_inst(2, 3, false),
            independent_inst(99, 100, true),
        ];

        schedule_instructions(&mut instructions, &TestAdapter);

        assert_eq!(
            instructions.iter().map(|inst| inst.id).collect::<Vec<_>>(),
            vec![90, 91, 2, 1, 0, 99]
        );
    }

    #[test]
    fn reused_register_tracker_does_not_carry_dependencies_between_blocks() {
        let gp0 = ClassedReg(RegClass::Gp, 0);
        let instructions = vec![inst(&[], &[gp0]), inst(&[gp0], &[])];
        let mut tracker = RegTracker::new();
        let mut graph = GraphScratch::default();

        let first =
            build_dep_graph_reusing(&instructions, 0, 1, &TestAdapter, &mut tracker, &mut graph);
        assert!(first[0].deps.is_empty());
        let second =
            build_dep_graph_reusing(&instructions, 1, 2, &TestAdapter, &mut tracker, &mut graph);

        assert!(second[0].deps.is_empty());
    }

    #[test]
    fn reused_graph_scratch_resets_nodes_and_edge_deduplication() {
        let gp0 = ClassedReg(RegClass::Gp, 0);
        let instructions = vec![
            inst(&[], &[gp0]),
            inst(&[gp0], &[]),
            independent_inst(2, 5, false),
            independent_inst(3, 6, false),
            inst(&[], &[gp0]),
            inst(&[gp0], &[]),
        ];
        let mut tracker = RegTracker::new();
        let mut graph = GraphScratch::default();

        let first =
            build_dep_graph_reusing(&instructions, 0, 2, &TestAdapter, &mut tracker, &mut graph);
        calculate_priorities(first);
        assert_eq!(first[0].users.as_slice(), [1]);
        assert_eq!(first[1].deps.as_slice(), [0]);
        assert!(first.iter().any(|node| node.priority != 0));

        let independent =
            build_dep_graph_reusing(&instructions, 2, 4, &TestAdapter, &mut tracker, &mut graph);
        assert!(independent.iter().all(|node| node.deps.is_empty()));
        assert!(independent.iter().all(|node| node.users.is_empty()));
        assert!(independent.iter().all(|node| node.priority == 0));

        let repeated =
            build_dep_graph_reusing(&instructions, 4, 6, &TestAdapter, &mut tracker, &mut graph);
        assert_eq!(repeated[0].users.as_slice(), [1]);
        assert_eq!(repeated[1].deps.as_slice(), [0]);
    }

    #[test]
    fn same_class_registers_still_carry_a_read_after_write_dependency() {
        let gp0 = ClassedReg(RegClass::Gp, 0);
        let instructions = vec![inst(&[], &[gp0]), inst(&[gp0], &[])];

        let nodes = build_dep_graph::<TestAdapter>(&instructions, 0, 2, &TestAdapter);

        assert_eq!(
            nodes[1].deps.as_slice(),
            [0],
            "the reader depends on the writer"
        );
        assert_eq!(nodes[0].users.as_slice(), [1]);
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

        assert_eq!(nodes[1].deps.as_slice(), [0]);
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

        assert_eq!(nodes[0].users.as_slice(), [1, 2]);
        assert_eq!(nodes[3].deps.as_slice(), [1, 2]);
        assert!(!nodes[0].users.spilled());
        assert!(!nodes[3].deps.spilled());
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
        assert!(nodes[sink].deps.spilled());
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
