//! Type-created dependency nodes shared by the typed family stores.
//!
//! The graph knows only opaque node identities and observed stamps. Family
//! keys and artifacts never enter this structure and no downcast is possible.

use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet, VecDeque},
    marker::PhantomData,
};

use crate::typed_query_store::TerminalStamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct QueryNodeId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DependencyStamp {
    Leaf(u64),
    Terminal(TerminalStamp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ObservedDependency {
    pub(crate) node: QueryNodeId,
    pub(crate) stamp: DependencyStamp,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InvalidationCause {
    StampChanged {
        dependency: QueryNodeId,
        previous: DependencyStamp,
        current: DependencyStamp,
    },
    StampDisappeared {
        dependency: QueryNodeId,
        previous: DependencyStamp,
    },
}

#[derive(Debug, Clone)]
struct Node {
    owner: TypeId,
    stamp: Option<DependencyStamp>,
    direct: BTreeSet<ObservedDependency>,
    reverse: BTreeSet<QueryNodeId>,
    invalidated_by: BTreeSet<InvalidationCause>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct QueryGraph {
    nodes: Vec<Node>,
    free: Vec<QueryNodeId>,
    /// Disappeared nodes whose final store-owned tombstone was retired while
    /// reverse dependency edges still need the old node identity.
    ///
    /// This is explicit graph ownership, not a second validation cache. Each
    /// entry is pinned by at least one reverse edge and is reclaimed when the
    /// last such edge is removed.
    retained_disappeared: BTreeSet<QueryNodeId>,
    computing: Vec<TypeId>,
    aborted_computations: BTreeSet<TypeId>,
    invalidations: BTreeMap<TypeId, usize>,
}

impl QueryGraph {
    pub(crate) fn allocate<Q: 'static>(&mut self) -> QueryNodeId {
        if let Some(id) = self.free.pop() {
            self.nodes[id.0 as usize] = Node {
                owner: TypeId::of::<Q>(),
                stamp: None,
                direct: BTreeSet::new(),
                reverse: BTreeSet::new(),
                invalidated_by: BTreeSet::new(),
            };
            return id;
        }
        let id = QueryNodeId(self.nodes.len() as u64);
        self.nodes.push(Node {
            owner: TypeId::of::<Q>(),
            stamp: None,
            direct: BTreeSet::new(),
            reverse: BTreeSet::new(),
            invalidated_by: BTreeSet::new(),
        });
        id
    }

    pub(crate) fn begin_computation<Q: 'static>(&mut self) {
        self.aborted_computations.remove(&TypeId::of::<Q>());
        self.computing.push(TypeId::of::<Q>());
    }

    pub(crate) fn finish_computation<Q: 'static>(&mut self) {
        let owner = self
            .computing
            .pop()
            .expect("finishing a query computation requires an active query");
        assert_eq!(owner, TypeId::of::<Q>());
    }

    pub(crate) fn is_current_computation<Q: 'static>(&self) -> bool {
        self.computing.last() == Some(&TypeId::of::<Q>())
    }

    pub(crate) fn abort_cycle<Q: 'static>(&mut self) {
        let owner = TypeId::of::<Q>();
        let start = self
            .computing
            .iter()
            .position(|active| *active == owner)
            .expect("a cycle re-enters an active query family");
        for aborted in self.computing.drain(start..) {
            self.aborted_computations.insert(aborted);
        }
    }

    pub(crate) fn take_aborted<Q: 'static>(&mut self) -> bool {
        self.aborted_computations.remove(&TypeId::of::<Q>())
    }

    pub(crate) fn invalidation_count<Q: 'static>(&self) -> usize {
        self.invalidations
            .get(&TypeId::of::<Q>())
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn publish<Q: 'static>(
        &mut self,
        node: QueryNodeId,
        stamp: DependencyStamp,
        direct: impl IntoIterator<Item = ObservedDependency>,
    ) {
        let index = node.0 as usize;
        assert_eq!(self.nodes[index].owner, TypeId::of::<Q>());
        let old_direct = std::mem::take(&mut self.nodes[index].direct);
        let direct = direct.into_iter().collect::<BTreeSet<_>>();
        for dependency in old_direct {
            let dependency_index = dependency.node.0 as usize;
            self.nodes[dependency_index].reverse.remove(&node);
            let remains_observed = direct.iter().any(|current| current.node == dependency.node);
            if !remains_observed
                && self.nodes[dependency_index].stamp.is_none()
                && self.nodes[dependency_index].reverse.is_empty()
                && self.retained_disappeared.contains(&dependency.node)
            {
                self.retire_untyped(dependency.node);
            }
        }
        for dependency in &direct {
            self.nodes[dependency.node.0 as usize].reverse.insert(node);
        }
        self.nodes[index].direct = direct;
        let previous = self.nodes[index].stamp.replace(stamp);
        self.nodes[index].invalidated_by.clear();
        if let Some(previous) = previous
            && previous != stamp
        {
            self.propagate(node, previous, Some(stamp));
        }
        self.restore_reverse(node);
    }

    pub(crate) fn disappear<Q: 'static>(&mut self, node: QueryNodeId) {
        let index = node.0 as usize;
        assert_eq!(self.nodes[index].owner, TypeId::of::<Q>());
        if let Some(previous) = self.nodes[index].stamp.take() {
            self.propagate(node, previous, None);
        }
    }

    /// Permanently retire an unreferenced retained node and recycle its slot.
    /// Propagation happens before edges are detached so every dependent keeps
    /// the precise disappeared-stamp cause.
    pub(crate) fn retire<Q: 'static>(&mut self, node: QueryNodeId) {
        let index = node.0 as usize;
        assert_eq!(self.nodes[index].owner, TypeId::of::<Q>());
        self.disappear::<Q>(node);
        // A disappeared stamp remains a bounded validation tombstone while a
        // retained dependent still observes it. Reusing its slot would make
        // that old typed edge appear valid for an unrelated publication.
        if !self.nodes[index].reverse.is_empty() {
            self.retained_disappeared.insert(node);
            return;
        }
        self.retire_untyped(node);
    }

    fn retire_untyped(&mut self, node: QueryNodeId) {
        let index = node.0 as usize;
        debug_assert!(self.nodes[index].stamp.is_none());
        debug_assert!(self.nodes[index].reverse.is_empty());
        self.retained_disappeared.remove(&node);
        let direct = std::mem::take(&mut self.nodes[index].direct);
        for dependency in direct {
            let dependency_index = dependency.node.0 as usize;
            self.nodes[dependency_index].reverse.remove(&node);
            // A disappeared dependency may have been pinned solely by this
            // tombstone. Reclaim it transitively as soon as its final reverse
            // edge goes away, so churn cannot strand unbounded graph nodes.
            if self.nodes[dependency_index].stamp.is_none()
                && self.nodes[dependency_index].reverse.is_empty()
                && self.retained_disappeared.contains(&dependency.node)
            {
                self.retire_untyped(dependency.node);
            }
        }
        self.nodes[index].invalidated_by.clear();
        self.free.push(node);
    }

    /// Nodes explicitly retained by the graph after their owning store or
    /// leaf cache retired them. Every such node has disappeared and remains
    /// pinned solely to preserve identities observed by reverse dependents.
    pub(crate) fn retained_disappeared_count(&self) -> usize {
        debug_assert!(self.retained_disappeared.iter().all(|node| {
            let entry = &self.nodes[node.0 as usize];
            entry.stamp.is_none() && !entry.reverse.is_empty()
        }));
        self.retained_disappeared.len()
    }

    pub(crate) fn observe<Q: 'static>(&self, node: QueryNodeId) -> Option<ObservedDependency> {
        let entry = &self.nodes[node.0 as usize];
        assert_eq!(entry.owner, TypeId::of::<Q>());
        entry.stamp.map(|stamp| ObservedDependency { node, stamp })
    }

    pub(crate) fn is_current<Q: 'static>(&self, node: QueryNodeId) -> bool {
        let entry = &self.nodes[node.0 as usize];
        assert_eq!(entry.owner, TypeId::of::<Q>());
        entry.stamp.is_some() && entry.invalidated_by.is_empty()
    }

    pub(crate) fn valid<Q: 'static>(&mut self, node: QueryNodeId) -> bool {
        assert_eq!(self.nodes[node.0 as usize].owner, TypeId::of::<Q>());
        self.validate(node, &mut BTreeSet::new())
    }

    pub(crate) fn direct_dependencies<Q: 'static>(
        &self,
        node: QueryNodeId,
    ) -> Vec<ObservedDependency> {
        let entry = &self.nodes[node.0 as usize];
        assert_eq!(entry.owner, TypeId::of::<Q>());
        entry.direct.iter().copied().collect()
    }

    pub(crate) fn reverse_dependency_count<Q: 'static>(&self, node: QueryNodeId) -> usize {
        let entry = &self.nodes[node.0 as usize];
        assert_eq!(entry.owner, TypeId::of::<Q>());
        entry.reverse.len()
    }

    pub(crate) fn invalidation_causes<Q: 'static>(
        &self,
        node: QueryNodeId,
    ) -> &BTreeSet<InvalidationCause> {
        assert_eq!(self.nodes[node.0 as usize].owner, TypeId::of::<Q>());
        &self.nodes[node.0 as usize].invalidated_by
    }

    fn validate(&mut self, node: QueryNodeId, active: &mut BTreeSet<QueryNodeId>) -> bool {
        if !active.insert(node) {
            return false;
        }
        let direct = self.nodes[node.0 as usize].direct.clone();
        let mut causes = BTreeSet::new();
        for observed in direct {
            let current = self.nodes[observed.node.0 as usize].stamp;
            match current {
                Some(current)
                    if current == observed.stamp && self.validate(observed.node, active) => {}
                Some(current) => {
                    causes.insert(InvalidationCause::StampChanged {
                        dependency: observed.node,
                        previous: observed.stamp,
                        current,
                    });
                }
                None => {
                    causes.insert(InvalidationCause::StampDisappeared {
                        dependency: observed.node,
                        previous: observed.stamp,
                    });
                }
            }
        }
        active.remove(&node);
        self.nodes[node.0 as usize].invalidated_by = causes;
        self.nodes[node.0 as usize].stamp.is_some()
            && self.nodes[node.0 as usize].invalidated_by.is_empty()
    }

    fn propagate(
        &mut self,
        dependency: QueryNodeId,
        previous: DependencyStamp,
        current: Option<DependencyStamp>,
    ) {
        let mut queue = VecDeque::from([(dependency, previous, current)]);
        while let Some((dependency, previous, current)) = queue.pop_front() {
            let reverse = self.nodes[dependency.0 as usize].reverse.clone();
            for dependent in reverse {
                let cause = current.map_or(
                    InvalidationCause::StampDisappeared {
                        dependency,
                        previous,
                    },
                    |current| InvalidationCause::StampChanged {
                        dependency,
                        previous,
                        current,
                    },
                );
                let entry = &mut self.nodes[dependent.0 as usize];
                let was_valid = entry.invalidated_by.is_empty();
                if entry.invalidated_by.insert(cause) {
                    if was_valid {
                        *self.invalidations.entry(entry.owner).or_default() += 1;
                    }
                    if let Some(stamp) = entry.stamp {
                        queue.push_back((dependent, stamp, None));
                    }
                }
            }
        }
    }

    fn restore_reverse(&mut self, dependency: QueryNodeId) {
        let reverse = self.nodes[dependency.0 as usize].reverse.clone();
        for dependent in reverse {
            if self.validate(dependent, &mut BTreeSet::new()) {
                self.restore_reverse(dependent);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct TypedNode<Q> {
    id: QueryNodeId,
    marker: PhantomData<fn() -> Q>,
}

impl<Q> Copy for TypedNode<Q> {}

impl<Q> Clone for TypedNode<Q> {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Debug, Clone)]
struct RetainedLeaf<I> {
    value: I,
    node: TypedNode<I>,
    stamp: u64,
}

/// Bounded typed leaf publications. Equal values reselect their existing
/// publication; eviction makes the old stamp disappear before removal.
#[derive(Debug, Clone)]
pub(crate) struct TypedLeafStore<I> {
    retained: VecDeque<RetainedLeaf<I>>,
    next_publication: u64,
    limit: usize,
    selected: Option<QueryNodeId>,
}

impl<I> TypedLeafStore<I>
where
    I: Clone + Eq + 'static,
{
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            retained: VecDeque::new(),
            next_publication: 0,
            limit,
            selected: None,
        }
    }

    pub(crate) fn publish(&mut self, graph: &mut QueryGraph, value: I) -> ObservedDependency {
        if let Some(index) = self.retained.iter().position(|entry| entry.value == value) {
            let retained = self.retained.remove(index).unwrap();
            if self.selected != Some(retained.node.id()) {
                if let Some(selected) = self.selected {
                    graph.disappear::<I>(selected);
                }
                graph.publish::<I>(
                    retained.node.id(),
                    DependencyStamp::Leaf(retained.stamp),
                    [],
                );
                self.selected = Some(retained.node.id());
            }
            let observed = graph.observe::<I>(retained.node.id()).unwrap();
            self.retained.push_back(retained);
            return observed;
        }
        let node = TypedNode::<I>::allocate(graph);
        let stamp = self.next_publication;
        self.next_publication = self.next_publication.wrapping_add(1);
        if let Some(selected) = self.selected {
            graph.disappear::<I>(selected);
        }
        graph.publish::<I>(node.id(), DependencyStamp::Leaf(stamp), []);
        self.selected = Some(node.id());
        let node_id = node.id();
        self.retained.push_back(RetainedLeaf { value, node, stamp });
        while self.retained.len() > self.limit {
            let evicted = self.retained.pop_front().unwrap();
            graph.retire::<I>(evicted.node.id());
        }
        ObservedDependency {
            node: node_id,
            stamp: DependencyStamp::Leaf(stamp),
        }
    }

    /// Publish a request-value leaf without deselecting other retained values.
    /// Complete option values coexist, allowing an older query variant to be
    /// reused when a caller switches back to it.
    pub(crate) fn publish_retained(
        &mut self,
        graph: &mut QueryGraph,
        value: I,
    ) -> ObservedDependency {
        if let Some(index) = self.retained.iter().position(|entry| entry.value == value) {
            let retained = self.retained.remove(index).unwrap();
            let observed = graph.observe::<I>(retained.node.id()).unwrap_or_else(|| {
                graph.publish::<I>(
                    retained.node.id(),
                    DependencyStamp::Leaf(retained.stamp),
                    [],
                );
                graph.observe::<I>(retained.node.id()).unwrap()
            });
            self.selected = Some(retained.node.id());
            self.retained.push_back(retained);
            return observed;
        }
        let node = TypedNode::<I>::allocate(graph);
        let stamp = self.next_publication;
        self.next_publication = self.next_publication.wrapping_add(1);
        graph.publish::<I>(node.id(), DependencyStamp::Leaf(stamp), []);
        self.selected = Some(node.id());
        let node_id = node.id();
        self.retained.push_back(RetainedLeaf { value, node, stamp });
        while self.retained.len() > self.limit {
            let evicted = self.retained.pop_front().unwrap();
            graph.retire::<I>(evicted.node.id());
        }
        ObservedDependency {
            node: node_id,
            stamp: DependencyStamp::Leaf(stamp),
        }
    }

    pub(crate) fn selected(&self, graph: &QueryGraph) -> Option<ObservedDependency> {
        self.selected.and_then(|node| graph.observe::<I>(node))
    }
}

impl<Q: 'static> TypedNode<Q> {
    pub(crate) fn allocate(graph: &mut QueryGraph) -> Self {
        Self {
            id: graph.allocate::<Q>(),
            marker: PhantomData,
        }
    }
}

impl<Q> TypedNode<Q> {
    pub(crate) fn id(&self) -> QueryNodeId {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum SourceLeaf {}
    enum Dependent {}

    #[test]
    fn reverse_edges_dirty_restore_and_disappear_without_scanning() {
        let mut graph = QueryGraph::default();
        let mut leaves = TypedLeafStore::new(2);
        let one = leaves.publish(&mut graph, 1_u32);
        let dependent = TypedNode::<Dependent>::allocate(&mut graph);
        let terminal = TerminalStamp {
            outcome: crate::typed_query_store::OutcomePublication::Success(1),
            diagnostics: 2,
        };
        graph.publish::<Dependent>(dependent.id(), DependencyStamp::Terminal(terminal), [one]);
        assert!(graph.valid::<Dependent>(dependent.id()));

        leaves.publish(&mut graph, 2_u32);
        assert!(!graph.valid::<Dependent>(dependent.id()));
        let one_again = leaves.publish(&mut graph, 1_u32);
        assert_eq!(one, one_again);
        assert!(graph.valid::<Dependent>(dependent.id()));

        leaves.publish(&mut graph, 3_u32);
        leaves.publish(&mut graph, 4_u32);
        assert!(!graph.valid::<Dependent>(dependent.id()));
        let _ = std::any::TypeId::of::<SourceLeaf>();
    }

    #[test]
    fn retiring_last_dependent_reclaims_pinned_disappeared_nodes_transitively() {
        let mut graph = QueryGraph::default();
        let mut leaves = TypedLeafStore::new(1);
        let leaf = leaves.publish(&mut graph, 1_u32);
        let dependent = TypedNode::<Dependent>::allocate(&mut graph);
        let terminal = TerminalStamp {
            outcome: crate::typed_query_store::OutcomePublication::Success(1),
            diagnostics: 2,
        };
        graph.publish::<Dependent>(dependent.id(), DependencyStamp::Terminal(terminal), [leaf]);

        leaves.publish(&mut graph, 2_u32);
        assert_eq!(graph.free.len(), 0, "the dependent pins the old leaf");
        graph.retire::<Dependent>(dependent.id());
        assert_eq!(graph.free.len(), 2);

        let node_count = graph.nodes.len();
        let _first = TypedNode::<Dependent>::allocate(&mut graph);
        let _second = TypedNode::<SourceLeaf>::allocate(&mut graph);
        assert_eq!(graph.nodes.len(), node_count, "retired slots are recycled");
    }

    #[test]
    fn replacing_edges_reclaims_a_disappeared_dependency_after_its_last_pin() {
        let mut graph = QueryGraph::default();
        let mut leaves = TypedLeafStore::new(1);
        let leaf = leaves.publish(&mut graph, 1_u32);
        let dependent = TypedNode::<Dependent>::allocate(&mut graph);
        let terminal = TerminalStamp {
            outcome: crate::typed_query_store::OutcomePublication::Success(1),
            diagnostics: 2,
        };
        graph.publish::<Dependent>(dependent.id(), DependencyStamp::Terminal(terminal), [leaf]);
        leaves.publish(&mut graph, 2_u32);
        assert!(graph.free.is_empty());

        graph.publish::<Dependent>(dependent.id(), DependencyStamp::Terminal(terminal), []);
        assert_eq!(graph.free.len(), 1);
    }

    #[test]
    fn removing_a_reverse_edge_does_not_retire_a_store_owned_disappeared_leaf() {
        let mut graph = QueryGraph::default();
        let mut leaves = TypedLeafStore::new(2);
        let first = leaves.publish(&mut graph, 1_u32);
        let dependent = TypedNode::<Dependent>::allocate(&mut graph);
        let terminal = TerminalStamp {
            outcome: crate::typed_query_store::OutcomePublication::Success(1),
            diagnostics: 2,
        };
        graph.publish::<Dependent>(dependent.id(), DependencyStamp::Terminal(terminal), [first]);

        leaves.publish(&mut graph, 2_u32);
        graph.publish::<Dependent>(dependent.id(), DependencyStamp::Terminal(terminal), []);
        assert!(graph.free.is_empty());
        assert_eq!(graph.retained_disappeared_count(), 0);

        assert_eq!(leaves.publish(&mut graph, 1_u32), first);
    }

    #[test]
    fn graph_explicitly_owns_disappeared_nodes_until_all_reverse_pins_retire() {
        let mut graph = QueryGraph::default();
        let count = crate::typed_query_store::QUERY_TERMINAL_RETENTION_LIMIT * 3;
        let terminal = TerminalStamp {
            outcome: crate::typed_query_store::OutcomePublication::Success(1),
            diagnostics: 2,
        };
        let mut dependents = Vec::new();

        for publication in 0..count {
            let source = TypedNode::<SourceLeaf>::allocate(&mut graph);
            graph.publish::<SourceLeaf>(source.id(), DependencyStamp::Leaf(publication as u64), []);
            let observed = graph.observe::<SourceLeaf>(source.id()).unwrap();
            let dependent = TypedNode::<Dependent>::allocate(&mut graph);
            graph.publish::<Dependent>(
                dependent.id(),
                DependencyStamp::Terminal(terminal),
                [observed],
            );

            graph.retire::<SourceLeaf>(source.id());
            dependents.push(dependent);
        }

        assert_eq!(graph.retained_disappeared_count(), count);
        assert!(graph.free.is_empty());

        for dependent in dependents.drain(..count / 2) {
            graph.retire::<Dependent>(dependent.id());
        }
        assert_eq!(graph.retained_disappeared_count(), count - count / 2);
        assert_eq!(graph.free.len(), count);

        for dependent in dependents {
            graph.retire::<Dependent>(dependent.id());
        }
        assert_eq!(graph.retained_disappeared_count(), 0);
        assert_eq!(graph.free.len(), count * 2);
    }
}
