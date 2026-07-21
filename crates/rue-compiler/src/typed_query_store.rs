//! Shared typed storage for immutable query terminal attempts.
//!
//! The store owns lookup, insertion validation, and deterministic retention.
//! `CompilerSession` remains responsible for deciding when and how a query is
//! computed; this component never calls compiler phases.

use std::collections::{BTreeSet, VecDeque};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::query_graph::{
    DependencyStamp, InvalidationCause, ObservedDependency, QueryGraph, TypedNode,
};
use crate::session::{AttemptId, QueryStructuralWork};

/// Deterministic FIFO retention for typed query families.
///
/// Records survive source publication changes. Their declared dependency
/// edges decide whether they remain reusable; the bound prevents old source
/// and option variants from creating unbounded session-owned history.
///
/// `LIMIT + 1` must not exceed the number of distinct
/// `(preview_features, target)` query keys the eviction tests can enumerate with
/// two preview features and three targets (`2^2 * 3 = 12`). Stabilizing a
/// preview feature shrinks that key space, so the retention bound tracks it
/// (RUE-987 took it from `16` to `10` when `aggregate_layout` retired); see
/// `session::tests::retention_variants`.
pub(crate) const QUERY_TERMINAL_RETENTION_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    Success,
    Failure,
}

/// Session-local publication identity for either kind of terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OutcomePublication {
    Success(u64),
    Failure(u64),
}

/// Identity observed by dependents. Attempt/work identity is deliberately
/// absent: only the typed outcome and its immutable diagnostic batch matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TerminalStamp {
    pub(crate) outcome: OutcomePublication,
    pub(crate) diagnostics: u64,
}

pub(crate) trait TypedQueryFamily: std::fmt::Debug + Send + Sync {
    type Key: Eq + std::hash::Hash + Clone + std::fmt::Debug + Send + Sync;
    type Record: std::fmt::Debug + Clone + Send + Sync;
    const MAX_TERMINALS: usize;
    const MAX_TOMBSTONES: usize = Self::MAX_TERMINALS;

    fn key(record: &Self::Record) -> &Self::Key;
    fn terminal_kind(record: &Self::Record) -> TerminalKind;
    /// Exhaustive family-owned equality for the success value or failure
    /// payload. Called only for records with the same terminal variant.
    fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool;
    /// Exhaustive family-owned equality for the attached diagnostic batch,
    /// including the empty batch.
    fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool;
    fn diagnostics(record: &Self::Record) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        let _ = record;
        None
    }
    fn record_is_consistent(record: &Self::Record) -> bool;
}

/// A family-owned secondary index predicate with a statically complete key.
///
/// This supports deliberate cross-variant reuse without exposing records or an
/// arbitrary predicate to `CompilerSession`.
pub(crate) trait TypedSecondaryLookupFamily: TypedQueryFamily {
    type SecondaryKey: Eq;

    fn matches_secondary(record: &Self::Record, key: &Self::SecondaryKey) -> bool;
}

pub(crate) trait TypedEquivalentLookupFamily: TypedSecondaryLookupFamily {
    fn rekey_equivalent(record: &Self::Record, key: Self::Key) -> Option<Self::Record>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptExecution {
    Computed,
    Reused,
    Adopted,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptOutcomeKind {
    Success,
    Failure,
    Aborted(AbortedQueryReason),
}

#[derive(Debug)]
enum AttemptPayload<F: TypedQueryFamily> {
    Terminal {
        stamp: TerminalStamp,
        node: TypedNode<F>,
        record: F::Record,
    },
    Reused {
        origin: Arc<Attempt<F>>,
    },
    Aborted {
        reason: AbortedQueryReason,
        diagnostics: Option<Arc<crate::FrontendDiagnosticSnapshot>>,
    },
}

pub(crate) trait AttemptView: std::fmt::Debug + Send + Sync {
    fn id(&self) -> AttemptId;
    fn execution(&self) -> AttemptExecution;
    fn outcome(&self) -> AttemptOutcomeKind;
    fn origin_id(&self) -> AttemptId;
    fn dependencies(&self) -> &[ObservedDependency];
    /// Runtime-native observations which cannot be forged into legacy graph
    /// node identities during family migration.
    fn runtime_observations(&self) -> &[RuntimeObservation] {
        &[]
    }
    /// Runtime-owned reduced work for revisioned-runtime attempts. This is
    /// separate from family-typed structural work during migration.
    fn runtime_work(&self) -> &[(Arc<str>, u64)] {
        &[]
    }
    fn work(&self) -> &QueryStructuralWork;
    fn diagnostics(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>>;
}

/// Explicit compatibility projection for revisioned-runtime provenance.
///
/// These values remain runtime-owned; adapters must not allocate peer
/// `QueryGraph` nodes merely to satisfy the legacy `dependencies()` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeObservation {
    Dependency(rue_query::Observation),
    Input(rue_query::InputObservation),
}

/// The sole immutable record of one request to a typed query family.
///
/// Computed attempts own their terminal value, diagnostic batch (nested in the
/// family record), dependency edges, and structural work. Reuse attempts own
/// no peer copies: they point at the computed terminal origin and carry zero
/// work. Aborts have no terminal stamp or reusable value, but retain the exact
/// dependency prefix, diagnostic batch (when one was frozen), and work prefix.
#[derive(Debug)]
pub(crate) struct Attempt<F: TypedQueryFamily> {
    pub(crate) id: AttemptId,
    pub(crate) key: F::Key,
    pub(crate) execution: AttemptExecution,
    payload: AttemptPayload<F>,
    pub(crate) dependencies: Arc<[ObservedDependency]>,
    pub(crate) work: QueryStructuralWork,
}

impl<F: TypedQueryFamily> Attempt<F> {
    fn terminal_origin(&self) -> Option<&Attempt<F>> {
        match &self.payload {
            AttemptPayload::Terminal { .. } => Some(self),
            AttemptPayload::Reused { origin } => origin.terminal_origin(),
            AttemptPayload::Aborted { .. } => None,
        }
    }

    fn record(&self) -> Option<&F::Record> {
        match &self.terminal_origin()?.payload {
            AttemptPayload::Terminal { record, stamp, .. } => {
                debug_assert_eq!(
                    matches!(stamp.outcome, OutcomePublication::Success(_)),
                    matches!(F::terminal_kind(record), TerminalKind::Success),
                    "terminal stamp kind must match its immutable record"
                );
                Some(record)
            }
            AttemptPayload::Reused { .. } | AttemptPayload::Aborted { .. } => unreachable!(),
        }
    }

    fn stamp(&self) -> Option<TerminalStamp> {
        match &self.terminal_origin()?.payload {
            AttemptPayload::Terminal { stamp, .. } => Some(*stamp),
            AttemptPayload::Reused { .. } | AttemptPayload::Aborted { .. } => unreachable!(),
        }
    }

    fn node(&self) -> Option<TypedNode<F>> {
        match &self.terminal_origin()?.payload {
            AttemptPayload::Terminal { node, .. } => Some(node.clone()),
            AttemptPayload::Reused { .. } | AttemptPayload::Aborted { .. } => unreachable!(),
        }
    }

    pub(crate) fn origin_id(&self) -> AttemptId {
        self.terminal_origin().map_or(self.id, |origin| origin.id)
    }

    #[cfg(test)]
    pub(crate) fn aborted_reason(&self) -> Option<AbortedQueryReason> {
        match self.payload {
            AttemptPayload::Aborted { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

impl<F: TypedQueryFamily + 'static> AttemptView for Attempt<F> {
    fn id(&self) -> AttemptId {
        self.id
    }

    fn execution(&self) -> AttemptExecution {
        self.execution
    }

    fn outcome(&self) -> AttemptOutcomeKind {
        match &self.payload {
            AttemptPayload::Terminal { record, .. } => match F::terminal_kind(record) {
                TerminalKind::Success => AttemptOutcomeKind::Success,
                TerminalKind::Failure => AttemptOutcomeKind::Failure,
            },
            AttemptPayload::Reused { origin } => origin.outcome(),
            AttemptPayload::Aborted { reason, .. } => AttemptOutcomeKind::Aborted(*reason),
        }
    }

    fn origin_id(&self) -> AttemptId {
        Attempt::origin_id(self)
    }

    fn dependencies(&self) -> &[ObservedDependency] {
        &self.dependencies
    }

    fn work(&self) -> &QueryStructuralWork {
        &self.work
    }

    fn diagnostics(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        match &self.payload {
            AttemptPayload::Aborted { diagnostics, .. } => diagnostics.as_ref(),
            AttemptPayload::Terminal { .. } | AttemptPayload::Reused { .. } => {
                self.record().and_then(F::diagnostics)
            }
        }
    }
}

#[derive(Debug)]
struct ValidationTombstone<F: TypedQueryFamily> {
    key: F::Key,
    stamp: TerminalStamp,
    node: TypedNode<F>,
    dependencies: Vec<ObservedDependency>,
}

impl<F: TypedQueryFamily> Clone for ValidationTombstone<F> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            stamp: self.stamp,
            node: self.node,
            dependencies: self.dependencies.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbortedQueryReason {
    Canceled,
    DuplicateInFlight,
    DependencyCycle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct QueryStoreRetention {
    pub(crate) retained: usize,
    pub(crate) protected: usize,
    pub(crate) pinned: usize,
    pub(crate) tombstones: usize,
    pub(crate) evictions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BeginSelectedError {
    DuplicateInFlight,
    DependencyCycle,
    KeyNotSelected,
    AlreadyTerminal,
}

#[derive(Debug)]
pub(crate) enum SelectedQueryState<F: TypedQueryFamily> {
    Unpublished {
        key: F::Key,
    },
    Dirty {
        key: F::Key,
        retained: Option<TerminalHandle<F>>,
        invalidated_by: BTreeSet<InvalidationCause>,
    },
    Computing {
        key: F::Key,
        attempt_id: u64,
        last_good: Option<TerminalHandle<F>>,
    },
    Success {
        key: F::Key,
        current: TerminalHandle<F>,
        last_good: TerminalHandle<F>,
    },
    Failure {
        key: F::Key,
        current: TerminalHandle<F>,
        last_good: Option<TerminalHandle<F>>,
    },
}

impl<F: TypedQueryFamily> Clone for SelectedQueryState<F> {
    fn clone(&self) -> Self {
        match self {
            Self::Unpublished { key } => Self::Unpublished { key: key.clone() },
            Self::Dirty {
                key,
                retained,
                invalidated_by,
            } => Self::Dirty {
                key: key.clone(),
                retained: retained.clone(),
                invalidated_by: invalidated_by.clone(),
            },
            Self::Computing {
                key,
                attempt_id,
                last_good,
            } => Self::Computing {
                key: key.clone(),
                attempt_id: *attempt_id,
                last_good: last_good.clone(),
            },
            Self::Success {
                key,
                current,
                last_good,
            } => Self::Success {
                key: key.clone(),
                current: current.clone(),
                last_good: last_good.clone(),
            },
            Self::Failure {
                key,
                current,
                last_good,
            } => Self::Failure {
                key: key.clone(),
                current: current.clone(),
                last_good: last_good.clone(),
            },
        }
    }
}

impl<F: TypedQueryFamily> SelectedQueryState<F> {
    pub(crate) fn key(&self) -> &F::Key {
        match self {
            Self::Unpublished { key }
            | Self::Dirty { key, .. }
            | Self::Computing { key, .. }
            | Self::Success { key, .. }
            | Self::Failure { key, .. } => key,
        }
    }
}

/// Bounded memo table for one statically typed query family.
#[derive(Debug)]
pub(crate) struct TypedQueryStore<F: TypedQueryFamily> {
    terminals: VecDeque<Arc<Attempt<F>>>,
    history: VecDeque<Arc<Attempt<F>>>,
    tombstones: VecDeque<ValidationTombstone<F>>,
    next_publication: u64,
    evictions: usize,
    selected: Option<SelectedQueryState<F>>,
    last_good: Option<TerminalHandle<F>>,
    family: PhantomData<fn() -> F>,
}

#[derive(Debug)]
pub(crate) struct TerminalHandle<F: TypedQueryFamily> {
    attempt: Arc<Attempt<F>>,
}

impl<F: TypedQueryFamily> Clone for TerminalHandle<F> {
    fn clone(&self) -> Self {
        Self {
            attempt: self.attempt.clone(),
        }
    }
}

impl<F: TypedQueryFamily + 'static> TerminalHandle<F> {
    fn node_id(&self) -> crate::query_graph::QueryNodeId {
        self.attempt.node().unwrap().id()
    }

    pub(crate) fn observed(&self) -> ObservedDependency {
        ObservedDependency {
            node: self.attempt.node().unwrap().id(),
            stamp: DependencyStamp::Terminal(self.attempt.stamp().unwrap()),
        }
    }

    pub(crate) fn is_valid(&self, graph: &mut QueryGraph) -> bool {
        graph.valid::<F>(self.attempt.node().unwrap().id())
    }

    #[cfg(test)]
    pub(crate) fn invalidation_cause_count(&self, graph: &QueryGraph) -> usize {
        graph
            .invalidation_causes::<F>(self.attempt.node().unwrap().id())
            .len()
    }
}

impl<F: TypedQueryFamily + 'static> TerminalHandle<F> {
    #[cfg(test)]
    pub(crate) fn stamp(&self) -> TerminalStamp {
        self.attempt.stamp().unwrap()
    }

    pub(crate) fn origin_attempt_id(&self) -> AttemptId {
        self.attempt.origin_id()
    }

    pub(crate) fn as_view(&self) -> Arc<dyn AttemptView>
    where
        F: 'static,
    {
        self.attempt.clone()
    }

    #[cfg(test)]
    pub(crate) fn attempt(&self) -> &Arc<Attempt<F>> {
        &self.attempt
    }
}

impl<F: TypedQueryFamily> Clone for TypedQueryStore<F> {
    fn clone(&self) -> Self {
        Self {
            terminals: self.terminals.clone(),
            history: self.history.clone(),
            tombstones: self.tombstones.clone(),
            next_publication: self.next_publication,
            evictions: self.evictions,
            selected: self.selected.clone(),
            last_good: self.last_good.clone(),
            family: PhantomData,
        }
    }
}

impl<F: TypedQueryFamily> Default for TypedQueryStore<F> {
    fn default() -> Self {
        Self {
            terminals: VecDeque::new(),
            history: VecDeque::new(),
            tombstones: VecDeque::new(),
            next_publication: 0,
            evictions: 0,
            selected: None,
            last_good: None,
            family: PhantomData,
        }
    }
}

impl<F: TypedQueryFamily> TypedQueryStore<F> {
    /// Select and validate one exact key, returning its immutable terminal or
    /// atomically entering Computing. Every session query uses this exhaustive
    /// boundary instead of independently scanning memo records.
    pub(crate) fn request_selected(
        &mut self,
        graph: &mut QueryGraph,
        key: F::Key,
        attempt_id: u64,
    ) -> Result<Option<(F::Record, TerminalHandle<F>)>, BeginSelectedError>
    where
        F: 'static,
    {
        self.recover_aborted_cycle(graph);
        if let Some(SelectedQueryState::Computing {
            key: active,
            attempt_id: active_attempt,
            ..
        }) = self.selected.as_ref()
            && active != &key
        {
            let _ = active_attempt;
            self.push_abort(
                attempt_id,
                key,
                AbortedQueryReason::DuplicateInFlight,
                QueryStructuralWork::None,
                AttemptExecution::Rejected,
                [],
                None,
            );
            return Err(BeginSelectedError::DuplicateInFlight);
        }
        self.select(graph, key.clone());
        match self.selected.as_ref() {
            Some(SelectedQueryState::Success { current, .. })
            | Some(SelectedQueryState::Failure { current, .. }) => {
                let origin = current.attempt.clone();
                let record = self
                    .record_for_handle(current)
                    .expect("selected terminal retains its artifact")
                    .clone();
                if F::terminal_kind(&record) == TerminalKind::Success {
                    self.last_good = Some(current.clone());
                }
                let request = Arc::new(Attempt {
                    id: AttemptId(attempt_id),
                    key,
                    execution: AttemptExecution::Reused,
                    payload: AttemptPayload::Reused { origin },
                    dependencies: Arc::from([]),
                    work: QueryStructuralWork::None,
                });
                self.push_history(request.clone());
                Ok(Some((record, TerminalHandle { attempt: request })))
            }
            Some(SelectedQueryState::Unpublished { .. } | SelectedQueryState::Dirty { .. }) => {
                self.begin_selected(graph, &key, attempt_id)?;
                Ok(None)
            }
            Some(SelectedQueryState::Computing { .. }) => {
                self.begin_selected(graph, &key, attempt_id)?;
                unreachable!("a computing request is rejected by begin_selected")
            }
            None => Err(BeginSelectedError::KeyNotSelected),
        }
    }

    pub(crate) fn select(&mut self, graph: &mut QueryGraph, key: F::Key)
    where
        F: 'static,
    {
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.key() == &key)
            && matches!(self.selected, Some(SelectedQueryState::Computing { .. }))
        {
            return;
        }
        let retained = self.handle(&key);
        self.selected = match retained {
            Some(handle) if handle.is_valid(graph) => match self
                .get(&key)
                .map(F::terminal_kind)
                .expect("retained handle belongs to a retained record")
            {
                TerminalKind::Success => Some(SelectedQueryState::Success {
                    key,
                    current: handle.clone(),
                    last_good: self.last_good.clone().unwrap_or(handle),
                }),
                TerminalKind::Failure => Some(SelectedQueryState::Failure {
                    key,
                    current: handle,
                    last_good: self.last_good.clone(),
                }),
            },
            retained @ Some(_) => {
                let invalidated_by = retained
                    .as_ref()
                    .map(|handle| graph.invalidation_causes::<F>(handle.node_id()).clone())
                    .unwrap_or_default();
                Some(SelectedQueryState::Dirty {
                    key,
                    retained,
                    invalidated_by,
                })
            }
            None => Some(SelectedQueryState::Unpublished { key }),
        };
    }

    #[cfg(test)]
    fn synchronize_selected(&mut self, graph: &QueryGraph)
    where
        F: 'static,
    {
        let invalidated = match self.selected.as_ref() {
            Some(
                SelectedQueryState::Success { key, current, .. }
                | SelectedQueryState::Failure { key, current, .. },
            ) if !graph.is_current::<F>(current.node_id()) => Some((key.clone(), current.clone())),
            _ => None,
        };
        if let Some((key, retained)) = invalidated {
            let invalidated_by = graph.invalidation_causes::<F>(retained.node_id()).clone();
            self.selected = Some(SelectedQueryState::Dirty {
                key,
                retained: Some(retained),
                invalidated_by,
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn selected_state(&mut self, graph: &QueryGraph) -> Option<&SelectedQueryState<F>>
    where
        F: 'static,
    {
        self.synchronize_selected(graph);
        self.selected.as_ref()
    }

    pub(crate) fn computing_key(&self) -> Option<F::Key> {
        match self.selected.as_ref()? {
            SelectedQueryState::Computing { key, .. } => Some(key.clone()),
            _ => None,
        }
    }

    pub(crate) fn selected_record(&self, graph: &QueryGraph) -> Option<&F::Record>
    where
        F: 'static,
    {
        let handle = match self.selected.as_ref()? {
            SelectedQueryState::Success { current, .. }
            | SelectedQueryState::Failure { current, .. } => current,
            _ => return None,
        };
        if !graph.is_current::<F>(handle.node_id()) {
            return None;
        }
        self.record_for_handle(handle)
    }

    pub(crate) fn selected_handle(&self, graph: &QueryGraph) -> Option<TerminalHandle<F>>
    where
        F: 'static,
    {
        let handle = match self.selected.as_ref()? {
            SelectedQueryState::Success { current, .. }
            | SelectedQueryState::Failure { current, .. } => current,
            _ => return None,
        };
        graph
            .is_current::<F>(handle.node_id())
            .then(|| handle.clone())
    }

    pub(crate) fn last_good_record(&self) -> Option<&F::Record> {
        self.last_good
            .as_ref()
            .and_then(|handle| self.record_for_handle(handle))
    }

    pub(crate) fn begin_selected(
        &mut self,
        graph: &mut QueryGraph,
        key: &F::Key,
        attempt_id: u64,
    ) -> Result<(), BeginSelectedError>
    where
        F: 'static,
    {
        self.recover_aborted_cycle(graph);
        let selected = self
            .selected
            .as_ref()
            .ok_or(BeginSelectedError::KeyNotSelected)?;
        if selected.key() != key {
            return Err(BeginSelectedError::KeyNotSelected);
        }
        match selected {
            SelectedQueryState::Computing { .. } => {
                let reason = if graph.is_current_computation::<F>() {
                    AbortedQueryReason::DuplicateInFlight
                } else {
                    AbortedQueryReason::DependencyCycle
                };
                if reason == AbortedQueryReason::DependencyCycle {
                    graph.abort_cycle::<F>();
                    self.recover_aborted_cycle(graph);
                }
                self.push_abort(
                    attempt_id,
                    key.clone(),
                    reason,
                    QueryStructuralWork::None,
                    AttemptExecution::Rejected,
                    [],
                    None,
                );
                return Err(match reason {
                    AbortedQueryReason::DependencyCycle => BeginSelectedError::DependencyCycle,
                    AbortedQueryReason::DuplicateInFlight => BeginSelectedError::DuplicateInFlight,
                    AbortedQueryReason::Canceled => unreachable!(),
                });
            }
            SelectedQueryState::Success { .. } | SelectedQueryState::Failure { .. } => {
                return Err(BeginSelectedError::AlreadyTerminal);
            }
            SelectedQueryState::Unpublished { .. } | SelectedQueryState::Dirty { .. } => {}
        }
        self.selected = Some(SelectedQueryState::Computing {
            key: key.clone(),
            attempt_id,
            last_good: self.last_good.clone(),
        });
        graph.begin_computation::<F>();
        Ok(())
    }

    /// Project a graph-level cycle abort back into this family's authoritative
    /// selected state. The graph marks every guard in the cycle; each typed
    /// store consumes its mark at its next boundary, without publishing a
    /// terminal or requiring callers to cancel partially unwound work.
    pub(crate) fn recover_aborted_cycle(&mut self, graph: &mut QueryGraph) -> bool
    where
        F: 'static,
    {
        if !graph.take_aborted::<F>() {
            return false;
        }
        let Some(SelectedQueryState::Computing {
            key, attempt_id, ..
        }) = self.selected.take()
        else {
            return true;
        };
        let retained = self.handle(&key);
        let invalidated_by = retained
            .as_ref()
            .map(|handle| graph.invalidation_causes::<F>(handle.node_id()).clone())
            .unwrap_or_default();
        self.push_abort(
            attempt_id,
            key.clone(),
            AbortedQueryReason::DependencyCycle,
            QueryStructuralWork::None,
            AttemptExecution::Rejected,
            [],
            None,
        );
        self.selected = Some(SelectedQueryState::Dirty {
            key,
            retained,
            invalidated_by,
        });
        true
    }

    pub(crate) fn publish_selected(
        &mut self,
        graph: &mut QueryGraph,
        record: F::Record,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
    ) -> Result<TerminalHandle<F>, BeginSelectedError>
    where
        F: 'static,
    {
        self.publish_selected_with_work(graph, record, dependencies, QueryStructuralWork::None)
    }

    pub(crate) fn publish_selected_with_work(
        &mut self,
        graph: &mut QueryGraph,
        record: F::Record,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
        work: QueryStructuralWork,
    ) -> Result<TerminalHandle<F>, BeginSelectedError>
    where
        F: 'static,
    {
        self.publish_selected_attempt(
            graph,
            record,
            dependencies,
            work,
            AttemptExecution::Computed,
        )
    }

    pub(crate) fn publish_selected_attempt(
        &mut self,
        graph: &mut QueryGraph,
        record: F::Record,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
        work: QueryStructuralWork,
        execution: AttemptExecution,
    ) -> Result<TerminalHandle<F>, BeginSelectedError>
    where
        F: 'static,
    {
        if self.recover_aborted_cycle(graph) {
            return Err(BeginSelectedError::DependencyCycle);
        }
        let key = F::key(&record).clone();
        let attempt_id = match self.selected.as_ref() {
            Some(SelectedQueryState::Computing {
                key: selected,
                attempt_id,
                ..
            }) if selected == &key => *attempt_id,
            _ => panic!("terminal publication requires its selected computation"),
        };
        let kind = F::terminal_kind(&record);
        let handle = self.insert_with_dependencies_and_work(
            graph,
            AttemptId(attempt_id),
            record,
            dependencies,
            work,
            execution,
        );
        graph.finish_computation::<F>();
        self.selected = Some(match kind {
            TerminalKind::Success => {
                self.last_good = Some(handle.clone());
                SelectedQueryState::Success {
                    key,
                    current: handle.clone(),
                    last_good: handle.clone(),
                }
            }
            TerminalKind::Failure => SelectedQueryState::Failure {
                key,
                current: handle.clone(),
                last_good: self.last_good.clone(),
            },
        });
        Ok(handle)
    }

    #[cfg(test)]
    pub(crate) fn cancel_selected(&mut self, graph: &mut QueryGraph, attempt_id: u64)
    where
        F: 'static,
    {
        let Some(SelectedQueryState::Computing { key, .. }) = self.selected.take() else {
            return;
        };
        let retained = self.handle(&key);
        if let Some(handle) = &retained {
            graph.disappear::<F>(handle.node_id());
        }
        graph.finish_computation::<F>();
        self.push_abort(
            attempt_id,
            key.clone(),
            AbortedQueryReason::Canceled,
            QueryStructuralWork::None,
            AttemptExecution::Computed,
            [],
            None,
        );
        self.selected = Some(SelectedQueryState::Dirty {
            key,
            retained,
            invalidated_by: BTreeSet::new(),
        });
    }

    fn push_abort(
        &mut self,
        attempt_id: u64,
        key: F::Key,
        reason: AbortedQueryReason,
        work: QueryStructuralWork,
        execution: AttemptExecution,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
        diagnostics: Option<Arc<crate::FrontendDiagnosticSnapshot>>,
    ) -> Arc<Attempt<F>> {
        let attempt = Arc::new(Attempt {
            id: AttemptId(attempt_id),
            key,
            execution,
            payload: AttemptPayload::Aborted {
                reason,
                diagnostics,
            },
            dependencies: dependencies.into_iter().collect(),
            work,
        });
        self.push_history(attempt.clone());
        attempt
    }

    fn push_history(&mut self, attempt: Arc<Attempt<F>>) {
        let _ = (
            &attempt.key,
            attempt.execution,
            &attempt.dependencies,
            &attempt.work,
        );
        let _ = AttemptExecution::Adopted;
        self.history.push_back(attempt);
        while self.history.len() > F::MAX_TERMINALS {
            self.history.pop_front();
        }
    }

    /// Freeze an aborted request without rolling back completed dependencies.
    /// Terminal publication is the only operation that mutates this family's
    /// graph edges, so abandoning a provisional computation only has to close
    /// its graph guard and restore the retained terminal as Dirty.
    pub(crate) fn record_aborted_attempt(
        &mut self,
        graph: &mut QueryGraph,
        key: F::Key,
        attempt_id: u64,
        reason: AbortedQueryReason,
        work: QueryStructuralWork,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
        diagnostics: Option<Arc<crate::FrontendDiagnosticSnapshot>>,
    ) -> Arc<dyn AttemptView>
    where
        F: 'static,
    {
        if matches!(
            self.selected.as_ref(),
            Some(SelectedQueryState::Computing { key: selected, .. }) if selected == &key
        ) {
            graph.finish_computation::<F>();
        }
        let retained = self.handle(&key);
        let invalidated_by = retained
            .as_ref()
            .map(|handle| graph.invalidation_causes::<F>(handle.node_id()).clone())
            .unwrap_or_default();
        let attempt = self.push_abort(
            attempt_id,
            key.clone(),
            reason,
            work,
            AttemptExecution::Computed,
            dependencies,
            diagnostics,
        );
        self.selected = Some(SelectedQueryState::Dirty {
            key,
            retained,
            invalidated_by,
        });
        attempt
    }

    fn record_for_handle<'a>(&self, handle: &'a TerminalHandle<F>) -> Option<&'a F::Record> {
        handle.attempt.record()
    }

    pub(crate) fn get(&self, key: &F::Key) -> Option<&F::Record> {
        self.terminals
            .iter()
            .find_map(|attempt| attempt.record().filter(|record| F::key(record) == key))
    }

    pub(crate) fn get_with_handle(&self, key: &F::Key) -> Option<(&F::Record, TerminalHandle<F>)> {
        self.terminals
            .iter()
            .find(|attempt| attempt.record().is_some_and(|record| F::key(record) == key))
            .map(|attempt| {
                (
                    attempt.record().unwrap(),
                    TerminalHandle {
                        attempt: attempt.clone(),
                    },
                )
            })
    }

    pub(crate) fn insert_with_dependencies(
        &mut self,
        graph: &mut QueryGraph,
        record: F::Record,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
    ) -> TerminalHandle<F>
    where
        F: 'static,
    {
        self.insert_with_dependencies_and_work(
            graph,
            AttemptId(0),
            record,
            dependencies,
            QueryStructuralWork::None,
            AttemptExecution::Computed,
        )
    }

    pub(crate) fn insert_with_dependencies_and_work(
        &mut self,
        graph: &mut QueryGraph,
        attempt_id: AttemptId,
        record: F::Record,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
        work: QueryStructuralWork,
        execution: AttemptExecution,
    ) -> TerminalHandle<F>
    where
        F: 'static,
    {
        assert!(
            F::record_is_consistent(&record),
            "typed query record key does not match its terminal artifact revision"
        );
        // A complete intrinsic key can legitimately be selected again after a
        // direct dependency stamp changes (for example request-local FileIds
        // after relocation). Supersede that key's dirty terminal atomically;
        // ordinary valid requests never reach insertion because they reuse it.
        let dependencies = dependencies.into_iter().collect::<Vec<_>>();
        if let Some(index) = self
            .tombstones
            .iter()
            .position(|tombstone| &tombstone.key == F::key(&record))
        {
            // The artifact needed for typed equality was evicted. Its next
            // publication must be fresh even when the recomputed value happens
            // to be equal, and retained dependents must observe disappearance.
            let tombstone = self.tombstones.remove(index).unwrap();
            graph.retire::<F>(tombstone.node.id());
        }
        let previous = self
            .terminals
            .iter()
            .position(|attempt| {
                attempt
                    .record()
                    .is_some_and(|previous| F::key(previous) == F::key(&record))
            })
            .map(|index| self.terminals.remove(index).unwrap());
        let mut allocate = || {
            let publication = self.next_publication;
            self.next_publication = self.next_publication.wrapping_add(1);
            publication
        };
        let outcome = previous
            .as_ref()
            .filter(|attempt| {
                F::terminal_kind(attempt.record().unwrap()) == F::terminal_kind(&record)
                    && F::outcome_equal(attempt.record().unwrap(), &record)
            })
            .map_or_else(
                || match F::terminal_kind(&record) {
                    TerminalKind::Success => OutcomePublication::Success(allocate()),
                    TerminalKind::Failure => OutcomePublication::Failure(allocate()),
                },
                |attempt| attempt.stamp().unwrap().outcome,
            );
        let diagnostics = previous
            .as_ref()
            .filter(|attempt| F::diagnostics_equal(attempt.record().unwrap(), &record))
            .map_or_else(&mut allocate, |attempt| {
                attempt.stamp().unwrap().diagnostics
            });
        let stamp = TerminalStamp {
            outcome,
            diagnostics,
        };
        let node = previous
            .as_ref()
            .and_then(|attempt| attempt.node())
            .unwrap_or_else(|| TypedNode::<F>::allocate(graph));
        graph.publish::<F>(
            node.id(),
            DependencyStamp::Terminal(stamp),
            dependencies.iter().copied(),
        );
        let attempt = Arc::new(Attempt {
            id: attempt_id,
            key: F::key(&record).clone(),
            execution,
            payload: AttemptPayload::Terminal {
                stamp,
                node: node.clone(),
                record,
            },
            dependencies: dependencies.clone().into(),
            work,
        });
        self.terminals.push_back(attempt.clone());
        self.push_history(attempt.clone());
        while self.terminals.len() > F::MAX_TERMINALS {
            let protected = |attempt: &Arc<Attempt<F>>| {
                let node = attempt.node().unwrap().id();
                self.last_good
                    .as_ref()
                    .is_some_and(|handle| handle.node_id() == node)
                    || self
                        .selected
                        .as_ref()
                        .is_some_and(|selected| match selected {
                            SelectedQueryState::Success { current, .. }
                            | SelectedQueryState::Failure { current, .. } => {
                                current.node_id() == node
                            }
                            SelectedQueryState::Dirty {
                                retained: Some(retained),
                                ..
                            } => retained.node_id() == node,
                            _ => false,
                        })
            };
            let index = self
                .terminals
                .iter()
                .position(|attempt| !protected(attempt))
                .expect("terminal bound exceeds the constant-size protected selected set");
            let evicted = self.terminals.remove(index).unwrap();
            self.tombstones.push_back(ValidationTombstone {
                key: F::key(evicted.record().unwrap()).clone(),
                stamp: evicted.stamp().unwrap(),
                node: evicted.node().unwrap(),
                dependencies: graph.direct_dependencies::<F>(evicted.node().unwrap().id()),
            });
            self.evictions += 1;
        }
        while self.tombstones.len() > F::MAX_TOMBSTONES {
            let tombstone = self
                .tombstones
                .pop_front()
                .expect("retention overflow always has an evicted tombstone");
            graph.retire::<F>(tombstone.node.id());
        }
        TerminalHandle { attempt }
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, record: F::Record) -> TerminalStamp
    where
        F: 'static,
    {
        let mut graph = QueryGraph::default();
        self.insert_with_dependencies(&mut graph, record, [])
            .stamp()
    }

    pub(crate) fn len(&self) -> usize {
        self.terminals.len()
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = &F::Record> {
        self.terminals.iter().filter_map(|attempt| attempt.record())
    }

    pub(crate) fn handle(&self, key: &F::Key) -> Option<TerminalHandle<F>> {
        self.terminals
            .iter()
            .find(|attempt| attempt.record().is_some_and(|record| F::key(record) == key))
            .map(|attempt| TerminalHandle {
                attempt: attempt.clone(),
            })
    }

    pub(crate) fn evictions(&self) -> usize {
        self.evictions
    }

    fn protected_count(&self) -> usize
    where
        F: 'static,
    {
        let mut protected = Vec::new();
        if let Some(last_good) = &self.last_good {
            protected.push(last_good.node_id());
        }
        if let Some(selected) = &self.selected {
            let current = match selected {
                SelectedQueryState::Success { current, .. }
                | SelectedQueryState::Failure { current, .. } => Some(current.node_id()),
                SelectedQueryState::Dirty {
                    retained: Some(retained),
                    ..
                } => Some(retained.node_id()),
                _ => None,
            };
            if let Some(current) = current
                && !protected.contains(&current)
            {
                protected.push(current);
            }
        }
        protected.len()
    }

    pub(crate) fn retention(&self, graph: &QueryGraph) -> QueryStoreRetention
    where
        F: 'static,
    {
        QueryStoreRetention {
            retained: self.terminals.len(),
            protected: self.protected_count(),
            pinned: self
                .terminals
                .iter()
                .filter(|attempt| {
                    graph.reverse_dependency_count::<F>(attempt.node().unwrap().id()) != 0
                })
                .count()
                + self
                    .tombstones
                    .iter()
                    .filter(|tombstone| {
                        graph.reverse_dependency_count::<F>(tombstone.node.id()) != 0
                    })
                    .count(),
            tombstones: self.tombstones.len(),
            evictions: self.evictions,
        }
    }

    pub(crate) fn aborted_len(&self) -> usize {
        self.history
            .iter()
            .filter(|attempt| matches!(attempt.payload, AttemptPayload::Aborted { .. }))
            .count()
    }

    pub(crate) fn origin_attempt_ids(&self) -> impl Iterator<Item = AttemptId> + '_ {
        self.terminals.iter().map(|attempt| attempt.id)
    }

    #[cfg(test)]
    pub(crate) fn attempt_history(&self) -> impl Iterator<Item = &Arc<Attempt<F>>> {
        self.history.iter()
    }
}

impl<F: TypedSecondaryLookupFamily> TypedQueryStore<F> {
    pub(crate) fn get_secondary_with_handle(
        &self,
        key: &F::SecondaryKey,
    ) -> Option<(&F::Record, TerminalHandle<F>)> {
        self.terminals
            .iter()
            .find(|attempt| {
                attempt
                    .record()
                    .is_some_and(|record| F::matches_secondary(record, key))
            })
            .map(|attempt| {
                (
                    attempt.record().unwrap(),
                    TerminalHandle {
                        attempt: attempt.clone(),
                    },
                )
            })
    }
}

impl<F: TypedEquivalentLookupFamily> TypedQueryStore<F> {
    pub(crate) fn publish_selected_equivalent(
        &mut self,
        graph: &mut QueryGraph,
        key: F::Key,
        secondary: &F::SecondaryKey,
        dependencies: impl IntoIterator<Item = ObservedDependency>,
    ) -> Option<F::Record>
    where
        F: 'static,
    {
        let (record, origin) = self.get_secondary_with_handle(secondary)?;
        if !origin.is_valid(graph) {
            return None;
        }
        let record = F::rekey_equivalent(record, key)?;
        let dependencies = dependencies
            .into_iter()
            .chain(std::iter::once(origin.observed()));
        self.publish_selected_attempt(
            graph,
            record.clone(),
            dependencies,
            QueryStructuralWork::None,
            AttemptExecution::Adopted,
        )
        .expect("equivalent publication owns an active computation");
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct TestRecord {
        key: u32,
        artifact_revision: u32,
        terminal: TerminalKind,
    }

    #[derive(Debug)]
    struct TestFamily;

    impl TypedQueryFamily for TestFamily {
        type Key = u32;
        type Record = TestRecord;
        const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

        fn key(record: &Self::Record) -> &Self::Key {
            &record.key
        }

        fn terminal_kind(record: &Self::Record) -> TerminalKind {
            record.terminal
        }

        fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.artifact_revision == right.artifact_revision
        }

        fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.key % 2 == right.key % 2
        }

        fn record_is_consistent(record: &Self::Record) -> bool {
            record.key == record.artifact_revision
        }
    }

    #[derive(Debug)]
    struct SingleEntryFamily;

    impl TypedQueryFamily for SingleEntryFamily {
        type Key = u32;
        type Record = TestRecord;
        const MAX_TERMINALS: usize = 1;

        fn key(record: &Self::Record) -> &Self::Key {
            &record.key
        }

        fn terminal_kind(record: &Self::Record) -> TerminalKind {
            record.terminal
        }

        fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.artifact_revision == right.artifact_revision
        }

        fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.key % 2 == right.key % 2
        }

        fn record_is_consistent(record: &Self::Record) -> bool {
            record.key == record.artifact_revision
        }
    }

    fn record(key: u32, terminal: TerminalKind) -> TestRecord {
        TestRecord {
            key,
            artifact_revision: key,
            terminal,
        }
    }

    #[test]
    fn success_and_failure_receive_typed_terminal_stamps() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        let mut graph = QueryGraph::default();
        assert_eq!(
            store
                .insert_with_dependencies(&mut graph, record(1, TerminalKind::Success), [])
                .stamp(),
            TerminalStamp {
                outcome: OutcomePublication::Success(0),
                diagnostics: 1,
            }
        );
        assert_eq!(
            store
                .insert_with_dependencies(&mut graph, record(2, TerminalKind::Failure), [])
                .stamp(),
            TerminalStamp {
                outcome: OutcomePublication::Failure(2),
                diagnostics: 3,
            }
        );
        assert!(matches!(
            store.handle(&1).unwrap().stamp().outcome,
            OutcomePublication::Success(_)
        ));
        assert!(Arc::ptr_eq(
            store.handle(&1).unwrap().attempt(),
            store.attempt_history().next().unwrap()
        ));
        assert!(matches!(
            store.handle(&2).unwrap().stamp().outcome,
            OutcomePublication::Failure(_)
        ));
    }

    #[test]
    fn equal_success_and_failure_preserve_outcome_and_diagnostic_identity() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        let mut graph = QueryGraph::default();
        let success =
            store.insert_with_dependencies(&mut graph, record(4, TerminalKind::Success), []);
        assert_eq!(
            store
                .insert_with_dependencies(&mut graph, record(4, TerminalKind::Success), [])
                .stamp(),
            success.stamp()
        );
        let failure =
            store.insert_with_dependencies(&mut graph, record(5, TerminalKind::Failure), []);
        assert_eq!(
            store
                .insert_with_dependencies(&mut graph, record(5, TerminalKind::Failure), [])
                .stamp(),
            failure.stamp()
        );

        let changed_variant =
            store.insert_with_dependencies(&mut graph, record(5, TerminalKind::Success), []);
        assert_ne!(changed_variant.stamp().outcome, failure.stamp().outcome);
        assert_eq!(
            changed_variant.stamp().diagnostics,
            failure.stamp().diagnostics
        );
    }

    #[test]
    fn retention_evicts_oldest_terminal_attempt() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        let mut graph = QueryGraph::default();
        for key in 0..=QUERY_TERMINAL_RETENTION_LIMIT as u32 {
            store.insert_with_dependencies(&mut graph, record(key, TerminalKind::Success), []);
        }
        assert!(store.get(&0).is_none());
        assert!(store.get(&1).is_some());
        assert_eq!(store.len(), QUERY_TERMINAL_RETENTION_LIMIT);
        assert_eq!(store.evictions(), 1);
    }

    #[test]
    fn selected_state_separates_current_last_good_and_cancellation() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        let mut graph = QueryGraph::default();
        store.select(&mut graph, 7);
        assert!(matches!(
            store.selected_state(&graph),
            Some(SelectedQueryState::Unpublished { key: 7 })
        ));
        store.begin_selected(&mut graph, &7, 41).unwrap();
        store
            .publish_selected(&mut graph, record(7, TerminalKind::Success), [])
            .unwrap();
        assert!(store.selected_record(&graph).is_some());
        assert!(store.last_good_record().is_some());

        store.select(&mut graph, 8);
        store.begin_selected(&mut graph, &8, 42).unwrap();
        store
            .publish_selected(&mut graph, record(8, TerminalKind::Failure), [])
            .unwrap();
        assert!(matches!(
            store.selected_state(&graph),
            Some(SelectedQueryState::Failure { .. })
        ));
        assert_eq!(store.last_good_record().unwrap().key, 7);

        store.select(&mut graph, 9);
        store.begin_selected(&mut graph, &9, 43).unwrap();
        assert_eq!(
            store.begin_selected(&mut graph, &9, 44),
            Err(BeginSelectedError::DuplicateInFlight)
        );
        store.cancel_selected(&mut graph, 43);
        assert!(matches!(
            store.selected_state(&graph),
            Some(SelectedQueryState::Dirty { key: 9, .. })
        ));
        assert_eq!(store.last_good_record().unwrap().key, 7);
        let aborted = store
            .attempt_history()
            .filter_map(|attempt| attempt.aborted_reason())
            .collect::<Vec<_>>();
        assert_eq!(aborted.len(), 2);
        assert_eq!(aborted[0], AbortedQueryReason::DuplicateInFlight);
        assert_eq!(aborted[1], AbortedQueryReason::Canceled);

        assert!(store.request_selected(&mut graph, 9, 45).unwrap().is_none());
        store
            .publish_selected(&mut graph, record(9, TerminalKind::Success), [])
            .unwrap();
        assert!(matches!(
            store.selected_state(&graph),
            Some(SelectedQueryState::Success { key: 9, .. })
        ));
    }

    #[test]
    fn nested_reentry_records_cycle_without_publishing_a_terminal_and_recovers() {
        #[derive(Debug)]
        struct NestedFamily;

        impl TypedQueryFamily for NestedFamily {
            type Key = u32;
            type Record = TestRecord;
            const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

            fn key(record: &Self::Record) -> &Self::Key {
                &record.key
            }

            fn terminal_kind(record: &Self::Record) -> TerminalKind {
                record.terminal
            }

            fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
                left.artifact_revision == right.artifact_revision
            }

            fn diagnostics_equal(_left: &Self::Record, _right: &Self::Record) -> bool {
                true
            }

            fn record_is_consistent(record: &Self::Record) -> bool {
                record.key == record.artifact_revision
            }
        }

        let mut outer = TypedQueryStore::<TestFamily>::default();
        let mut nested = TypedQueryStore::<NestedFamily>::default();
        let mut graph = QueryGraph::default();
        assert!(outer.request_selected(&mut graph, 1, 1).unwrap().is_none());
        assert!(nested.request_selected(&mut graph, 2, 2).unwrap().is_none());
        assert!(matches!(
            outer.request_selected(&mut graph, 1, 3),
            Err(BeginSelectedError::DependencyCycle)
        ));
        assert!(outer.get(&1).is_none());
        assert_eq!(
            outer
                .attempt_history()
                .find_map(|attempt| attempt.aborted_reason()),
            Some(AbortedQueryReason::DependencyCycle)
        );

        let aborted = nested.publish_selected(&mut graph, record(2, TerminalKind::Success), []);
        assert!(matches!(aborted, Err(BeginSelectedError::DependencyCycle)));
        assert!(matches!(
            outer.selected_state(&graph),
            Some(SelectedQueryState::Dirty { .. })
        ));
        assert!(matches!(
            nested.selected_state(&graph),
            Some(SelectedQueryState::Dirty { .. })
        ));
        assert_eq!(
            nested
                .attempt_history()
                .find_map(|attempt| attempt.aborted_reason()),
            Some(AbortedQueryReason::DependencyCycle)
        );
        assert!(outer.request_selected(&mut graph, 1, 4).unwrap().is_none());
        outer
            .publish_selected(&mut graph, record(1, TerminalKind::Success), [])
            .unwrap();
        assert!(outer.get(&1).is_some());
    }

    #[test]
    fn evicted_artifacts_leave_bounded_validation_tombstones_and_republish_fresh() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        let mut graph = QueryGraph::default();
        let first =
            store.insert_with_dependencies(&mut graph, record(0, TerminalKind::Success), []);
        for key in 1..=QUERY_TERMINAL_RETENTION_LIMIT as u32 {
            store.insert_with_dependencies(&mut graph, record(key, TerminalKind::Success), []);
        }
        let retention = store.retention(&graph);
        assert_eq!(retention.retained, QUERY_TERMINAL_RETENTION_LIMIT);
        assert_eq!(retention.tombstones, 1);
        assert_eq!(retention.evictions, 1);

        assert!(store.request_selected(&mut graph, 0, 1).unwrap().is_none());
        let republished = store
            .publish_selected(&mut graph, record(0, TerminalKind::Success), [])
            .unwrap();
        assert_ne!(first.stamp(), republished.stamp());
        assert!(republished.is_valid(&mut graph));
        assert_eq!(store.retention(&graph).tombstones, 1);
    }

    #[test]
    fn popped_tombstone_transfers_ownership_to_graph_until_reverse_pin_retires() {
        #[derive(Debug)]
        struct DependentFamily;

        let mut store = TypedQueryStore::<SingleEntryFamily>::default();
        let mut graph = QueryGraph::default();
        let first =
            store.insert_with_dependencies(&mut graph, record(0, TerminalKind::Success), []);
        let dependent = TypedNode::<DependentFamily>::allocate(&mut graph);
        graph.publish::<DependentFamily>(
            dependent.id(),
            DependencyStamp::Terminal(TerminalStamp {
                outcome: OutcomePublication::Success(0),
                diagnostics: 0,
            }),
            [first.observed()],
        );

        store.insert_with_dependencies(&mut graph, record(1, TerminalKind::Success), []);
        assert_eq!(store.retention(&graph).tombstones, 1);
        assert_eq!(graph.retained_disappeared_count(), 0);

        store.insert_with_dependencies(&mut graph, record(2, TerminalKind::Success), []);
        assert_eq!(store.retention(&graph).tombstones, 1);
        assert_eq!(graph.retained_disappeared_count(), 1);

        graph.retire::<DependentFamily>(dependent.id());
        assert_eq!(graph.retained_disappeared_count(), 0);
    }

    #[test]
    #[should_panic(expected = "typed query record key does not match")]
    fn insertion_rejects_artifact_from_another_revision() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        let mut graph = QueryGraph::default();
        store.insert_with_dependencies(
            &mut graph,
            TestRecord {
                key: 1,
                artifact_revision: 2,
                terminal: TerminalKind::Success,
            },
            [],
        );
    }
}
