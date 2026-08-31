//! Query instrumentation and session work accounting.

use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendQueryWork {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
}

/// Aggregate lifecycle work for one collection of per-function backend
/// queries. The aggregate deliberately omits query keys and artifacts so an
/// external metrics consumer cannot become a second query-graph owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BackendQueryWork {
    pub(crate) computed: usize,
    pub(crate) reused: usize,
    pub(crate) joined: usize,
    pub(crate) canceled: usize,
}

impl BackendQueryWork {
    pub(super) fn observe(&mut self, execution: rue_query::RequestExecution) {
        match execution {
            rue_query::RequestExecution::Computed => self.computed += 1,
            rue_query::RequestExecution::Reused => self.reused += 1,
            rue_query::RequestExecution::Joined => self.joined += 1,
            rue_query::RequestExecution::Aborted => self.canceled += 1,
        }
    }
}

/// Session-owned indexed historical artifacts retained after the latest query.
///
/// These are gauges, not cumulative work counters. Caller-owned
/// [`Arc<FrontendDiagnosticSnapshot>`] values are deliberately excluded: once
/// returned, their lifetime is controlled by the caller rather than the
/// session's eviction policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendRetentionMetrics {
    /// Retained terminal artifacts across all typed query families.
    pub retained_query_records: usize,
    /// Current and peak deterministic terminal/artifact charge.
    pub retained_bytes: usize,
    pub peak_retained_bytes: usize,
    /// Runtime-wide soft artifact-charge budget.
    pub retained_byte_budget: usize,
    /// Retained dependency and input observations. Validation is pull-based;
    /// this is not a reverse-edge count.
    pub dependency_pins: usize,
    pub peak_dependency_pins: usize,
    pub dependency_pin_budget: usize,
    pub aggregate_retention_probes: usize,
    pub retained_byte_probe_quantum: usize,
    pub dependency_pin_probe_quantum: usize,
    pub retained_byte_probe_overshoot_bound: usize,
    pub dependency_pin_probe_overshoot_bound: usize,
    /// Protection gauges already maintained by their owning scopes.
    pub active_task_leases: usize,
    pub peak_task_leases: usize,
    pub active_retained_pins: usize,
    pub peak_retained_pins: usize,
    pub retained_revisions: usize,
    /// Exact retained views and value stamps for the compiler-owned input
    /// families. These expose per-family bounds independently of runtime
    /// aggregate retention.
    pub retained_module_input_views: usize,
    pub retained_module_source_stamps: usize,
    pub retained_import_input_views: usize,
    pub retained_import_context_stamps: usize,
    pub retained_import_topology_stamps: usize,
    pub retained_import_provenance_stamps: usize,
    pub retained_import_observation_stamps: usize,
    /// Protected soft-budget overflow and pressure evidence.
    pub retained_byte_pressure_events: usize,
    pub dependency_pin_pressure_events: usize,
    pub retained_byte_overflow_events: usize,
    pub dependency_pin_overflow_events: usize,
    pub peak_retained_byte_overage: usize,
    pub peak_dependency_pin_overage: usize,
    /// Lifetime artifact evictions across all typed query families.
    pub query_evictions: usize,
    pub retained_byte_evictions: usize,
    pub dependency_pin_evictions: usize,
    /// Diagnostic attempts indexed by the bounded diagnostic store.
    ///
    /// Producer caches may also retain the same origin `Arc`; those bounded or
    /// producer-lifetime references are deliberately excluded from this index
    /// gauge rather than counted twice.
    pub diagnostic_entries: usize,
    /// Distinct diagnostic source attempts indexed by the diagnostic store.
    pub diagnostic_source_attempts: usize,
    /// Source bytes across those distinct attempts (shared stages count once).
    pub diagnostic_source_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FrontendRuntimeMetrics {
    /// One canonical projection of cumulative query-runtime work. Live gauges
    /// and configured budgets remain in `FrontendRetentionMetrics`; keeping
    /// them out of this snapshot preserves deterministic work equality.
    pub(crate) query: crate::unstable::QueryRuntimeMetrics,
    pub(crate) semantic_reachability: crate::unstable::SemanticReachabilityMetrics,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerSessionWork {
    pub updates: usize,
    pub last_parse: ParsedModulesWork,
    pub warning_references: crate::unstable::WarningReferenceMetrics,
    pub last_invalidation: ParseInvalidationSummary,
    pub imports: FrontendQueryWork,
    pub import_diagnostics: FrontendQueryWork,
    pub merge: FrontendQueryWork,
    pub rir: FrontendQueryWork,
    pub downstream_invalidations: usize,
    pub last_merge: CanonicalMergeWork,
    pub last_rir: CanonicalRirWork,
    pub diagnostic_publications: usize,
    pub diagnostic_reuses: usize,
    pub diagnostic_invalidations: usize,
    /// Current bounded-retention gauges for long-lived service integrations.
    pub retention: FrontendRetentionMetrics,
    pub(crate) runtime: FrontendRuntimeMetrics,
}

pub(super) trait SessionQueryMetricsFamily {
    const NAME: &'static str;
    fn projection(work: &mut CompilerSessionWork) -> &mut FrontendQueryWork;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AttemptId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueryStructuralWork {
    None,
    Parse(ParsedModulesWork),
    Merge(CanonicalMergeWork),
    Rir(CanonicalRirWork),
}

#[derive(Debug, Clone)]
pub(super) struct IndexedAttempt {
    pub(super) family: &'static str,
    pub(super) attempt: Arc<dyn AttemptView>,
}

pub(super) const QUERY_ATTEMPT_RETENTION_LIMIT: usize = 256;

#[derive(Debug, Default)]
pub(super) struct QueryAttemptIndex {
    pub(super) next_id: u64,
    pub(super) retained: VecDeque<IndexedAttempt>,
    pub(super) pinned_origins: BTreeSet<AttemptId>,
    pub(super) evicted_projection: BTreeMap<&'static str, FrontendQueryWork>,
    pub(super) projections:
        BTreeMap<&'static str, fn(&mut CompilerSessionWork) -> &mut FrontendQueryWork>,
}

impl QueryAttemptIndex {
    pub(super) fn allocate(&mut self) -> AttemptId {
        let id = AttemptId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub(super) fn index(&mut self, family: &'static str, attempt: Arc<dyn AttemptView>) {
        if self
            .retained
            .iter()
            .any(|indexed| indexed.attempt.id() == attempt.id())
        {
            return;
        }
        self.retained.push_back(IndexedAttempt { family, attempt });
        if self.retained.len() > QUERY_ATTEMPT_RETENTION_LIMIT {
            // Keep every origin named by a retained reuse. The oldest
            // unreferenced request is evicted instead, preserving immutable
            // records and non-dangling provenance within the bounded ledger.
            let mut referenced = self
                .retained
                .iter()
                .map(|indexed| indexed.attempt.origin_id())
                .collect::<BTreeSet<_>>();
            referenced.extend(self.pinned_origins.iter().copied());
            let index = self
                .retained
                .iter()
                .position(|indexed| !referenced.contains(&indexed.attempt.id()))
                .unwrap_or(0);
            if let Some(evicted) = self.retained.remove(index) {
                project_lifecycle(
                    self.evicted_projection.entry(evicted.family).or_default(),
                    evicted.attempt.as_ref(),
                );
            }
        }
    }
}

pub(super) fn project_lifecycle(work: &mut FrontendQueryWork, attempt: &dyn AttemptView) {
    let _ = (
        attempt.outcome(),
        attempt.runtime_observations(),
        attempt.runtime_work(),
        attempt.work(),
    );
    work.calls += 1;
    match attempt.execution() {
        QueryAttemptExecution::Computed => work.executions += 1,
        QueryAttemptExecution::Reused => work.reuses += 1,
        QueryAttemptExecution::Rejected => {}
    }
}

/// A production query boundary. It owns work independently of the session
/// borrow and publishes a canceled record if computation unwinds or returns
/// before an explicit terminal is frozen.
pub(super) struct QueryComputationGuard {
    pub(super) sink: Arc<Mutex<QueryAttemptIndex>>,
    pub(super) id: AttemptId,
    pub(super) family: &'static str,
    pub(super) attempt: Option<Arc<dyn AttemptView>>,
    pub(super) diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    pub(super) structural: QueryStructuralWork,
    pub(super) cancel_requested: bool,
}

/// Lifecycle-only attempt retained for canonical phases that publish directly
/// into the revisioned runtime rather than a compatibility typed-query store.
#[derive(Debug)]
pub(super) struct InstrumentedQueryAttempt {
    pub(super) id: AttemptId,
    pub(super) execution: QueryAttemptExecution,
    pub(super) outcome: AttemptOutcomeKind,
    pub(super) diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    pub(super) work: QueryStructuralWork,
}

impl AttemptView for InstrumentedQueryAttempt {
    fn id(&self) -> AttemptId {
        self.id
    }

    fn execution(&self) -> QueryAttemptExecution {
        self.execution
    }

    fn outcome(&self) -> AttemptOutcomeKind {
        self.outcome
    }

    fn origin_id(&self) -> AttemptId {
        self.id
    }

    fn work(&self) -> &QueryStructuralWork {
        &self.work
    }

    fn diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostics.as_ref()
    }
}

impl QueryComputationGuard {
    pub(super) fn started(&mut self) {}

    pub(super) fn accrue(&mut self, structural: QueryStructuralWork) {
        self.structural = structural;
    }

    pub(super) fn bind(&mut self, attempt: Arc<dyn AttemptView>) {
        self.attempt = Some(attempt);
    }

    pub(super) fn attach_diagnostics(&mut self, diagnostics: Arc<FrontendDiagnosticSnapshot>) {
        self.diagnostics = Some(diagnostics);
    }

    pub(super) fn finish<T, E>(
        self,
        execution: QueryAttemptExecution,
        _reuse_origin: Option<AttemptId>,
        result: &Result<T, E>,
        structural: QueryStructuralWork,
    ) -> AttemptId {
        if let Some(attempt) = self.attempt {
            self.sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .index(self.family, attempt);
        } else {
            let outcome = if result.is_ok() {
                AttemptOutcomeKind::Success
            } else if self.cancel_requested {
                AttemptOutcomeKind::Aborted(AbortedQueryReason::Canceled)
            } else {
                AttemptOutcomeKind::Failure
            };
            self.sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .index(
                    self.family,
                    Arc::new(InstrumentedQueryAttempt {
                        id: self.id,
                        execution,
                        outcome,
                        diagnostics: self.diagnostics,
                        work: structural,
                    }),
                );
        }
        self.id
    }
}

/// Sole aggregate for query lifecycle, structural work, and retention gauges.
///
/// Query execution publishes immutable attempt values here. Compiler phases do
/// not reach into unrelated session counters, and replacing instrumentation
/// with a no-op cannot affect query results or control flow.
#[derive(Debug, Default, Clone)]
pub(super) struct CompilerSessionMetrics {
    pub(super) attempts: Arc<Mutex<QueryAttemptIndex>>,
    pub(super) projected_attempts: BTreeSet<AttemptId>,
    pub(super) aggregate: CompilerSessionWork,
}

impl CompilerSessionMetrics {
    pub(super) fn work(&self) -> &CompilerSessionWork {
        &self.aggregate
    }

    pub(super) fn begin<Q: SessionQueryMetricsFamily>(&self) -> QueryComputationGuard {
        let mut ledger = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.projections.insert(Q::NAME, Q::projection);
        let id = ledger.allocate();
        drop(ledger);
        QueryComputationGuard {
            sink: self.attempts.clone(),
            id,
            family: Q::NAME,
            attempt: None,
            diagnostics: None,
            structural: QueryStructuralWork::None,
            cancel_requested: false,
        }
    }

    pub(super) fn begin_unprojected(&self, family: &'static str) -> QueryComputationGuard {
        let mut ledger = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = ledger.allocate();
        drop(ledger);
        QueryComputationGuard {
            sink: self.attempts.clone(),
            id,
            family,
            attempt: None,
            diagnostics: None,
            structural: QueryStructuralWork::None,
            cancel_requested: false,
        }
    }

    pub(super) fn set_pinned_origins(&self, origins: BTreeSet<AttemptId>) {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pinned_origins = origins;
    }

    pub(super) fn synchronize(&mut self) {
        let ledger = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let new_attempts = ledger
            .retained
            .iter()
            .filter(|indexed| !self.projected_attempts.contains(&indexed.attempt.id()))
            .cloned()
            .collect::<Vec<_>>();
        let mut projected = ledger.evicted_projection.clone();
        for attempt in &ledger.retained {
            project_lifecycle(
                projected.entry(attempt.family).or_default(),
                attempt.attempt.as_ref(),
            );
        }
        for (family, projection) in &ledger.projections {
            *projection(&mut self.aggregate) = projected.remove(family).unwrap_or_default();
        }
        self.projected_attempts.retain(|id| {
            ledger
                .retained
                .iter()
                .any(|indexed| indexed.attempt.id() == *id)
        });
        drop(ledger);
        for attempt in new_attempts {
            self.project_structural_attempt(attempt.attempt.as_ref());
            self.projected_attempts.insert(attempt.attempt.id());
        }
    }

    pub(super) fn project_structural_attempt(&mut self, attempt: &dyn AttemptView) {
        match attempt.work() {
            QueryStructuralWork::None | QueryStructuralWork::Parse(_) => {}
            QueryStructuralWork::Merge(work)
                if attempt.outcome() == crate::typed_query_store::AttemptOutcomeKind::Success =>
            {
                self.aggregate.last_merge = *work;
            }
            QueryStructuralWork::Rir(work)
                if attempt.outcome() == crate::typed_query_store::AttemptOutcomeKind::Success =>
            {
                self.aggregate.last_rir = *work;
            }
            QueryStructuralWork::Merge(_) | QueryStructuralWork::Rir(_) => {}
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn attempts(&self) -> Vec<IndexedAttempt> {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retained
            .iter()
            .cloned()
            .collect()
    }

    pub(super) fn update(
        &mut self,
        parse: ParsedModulesWork,
        invalidation: ParseInvalidationSummary,
    ) {
        self.aggregate.updates += 1;
        self.aggregate.last_parse = parse;
        self.aggregate.last_invalidation = invalidation;
    }

    pub(super) fn set_warning_references(
        &mut self,
        work: crate::unstable::WarningReferenceMetrics,
    ) {
        self.aggregate.warning_references = work;
    }

    pub(super) fn project_dependency_invalidations(&mut self, changed_existing_revision: bool) {
        if changed_existing_revision {
            self.aggregate.downstream_invalidations += 1;
        }
        self.aggregate.last_merge = CanonicalMergeWork::default();
        self.aggregate.last_rir = CanonicalRirWork::default();
    }

    pub(super) fn diagnostic_publication(&mut self, invalidated_previous: bool) {
        if invalidated_previous {
            self.aggregate.diagnostic_invalidations += 1;
        }
        self.aggregate.diagnostic_publications += 1;
    }

    pub(super) fn diagnostic_reuse(&mut self) {
        self.aggregate.diagnostic_reuses += 1;
    }

    pub(super) fn set_retention(&mut self, retention: FrontendRetentionMetrics) {
        self.aggregate.retention = retention;
    }

    pub(super) fn set_runtime(&mut self, runtime: FrontendRuntimeMetrics) {
        self.aggregate.runtime = runtime;
    }
}
