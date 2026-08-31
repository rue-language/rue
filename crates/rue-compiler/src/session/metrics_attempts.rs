//! Session metrics, diagnostics, and attempt-publication coordination.

use super::{
    CompileErrors, CompileWarning, CompilerSession, CompilerSessionWork,
    DiagnosticAttemptProvenance, FrontendDiagnosticIdentity, FrontendDiagnosticSnapshot,
    FrontendRetentionMetrics, FrontendRuntimeMetrics, ImportDiagnosticInputDescriptor,
    SourceSnapshot,
};
use std::collections::BTreeSet;
use std::sync::Arc;

impl CompilerSession {
    pub(crate) fn work(&self) -> &CompilerSessionWork {
        self.metrics.work()
    }

    pub(crate) fn endpoint_capability_owner(&self) -> Arc<()> {
        self.identity.clone()
    }

    pub(crate) fn endpoint_capability_generation(&self) -> usize {
        self.metrics.work().updates
    }
    /// Return an owned snapshot of explicitly unstable compiler metrics.
    ///
    /// The snapshot cannot be installed back into this or another session and
    /// therefore grants no access to query ownership or invalidation state.
    pub fn unstable_metrics(&self) -> crate::unstable::MetricsSnapshot {
        let mut work = self.metrics.work().clone();
        // Query tasks publish counters independently of the session's phase
        // projections. Read the runtime at this observation boundary so a
        // baseline-to-successor delta cannot inherit late predecessor work.
        let runtime = self.queries.revisioned.runtime_retention_metrics();
        work.runtime = FrontendRuntimeMetrics {
            query: runtime.into(),
            semantic_reachability: self.queries.revisioned.body_reachability_metrics(),
        };
        crate::unstable::MetricsSnapshot::new(work)
    }
    #[cfg(test)]
    pub(crate) fn set_module_input_retention_for_test(&self, retention_limit: usize) {
        self.queries
            .revisioned
            .set_module_input_retention_for_test(retention_limit);
    }
    #[cfg(test)]
    pub(crate) fn module_source_stamp_for_test(
        &self,
        source: &crate::ModuleRevision,
    ) -> Option<u64> {
        self.queries.revisioned.module_source_stamp_for_test(source)
    }
    /// Diagnostic snapshot from the most recently attempted query, whether it
    /// succeeded or failed. In-tree warm/fresh parity oracles compare this
    /// retained selection; it is not part of the stable facade.
    pub(crate) fn latest_diagnostics_for_test(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostics.latest()
    }

    /// Look up the currently selected, or otherwise most recently indexed,
    /// diagnostic batch matching a source-attempt and public query stage.
    ///
    /// Canonical and presentation-ordered producer attempts can share the same
    /// public stage. When the current selection matches, this returns that
    /// exact batch; otherwise it returns the most recently indexed match.
    /// Clone the `Arc` when the artifact must outlive index eviction.
    #[cfg(test)]
    pub(crate) fn most_recent_diagnostics_for(
        &self,
        source: &SourceSnapshot,
        stage: &FrontendDiagnosticIdentity,
    ) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostics.find(source, stage)
    }

    pub(super) fn publish_diagnostics(
        &mut self,
        source: &SourceSnapshot,
        stage: FrontendDiagnosticIdentity,
        errors: Option<&CompileErrors>,
        warnings: &[CompileWarning],
    ) -> Arc<FrontendDiagnosticSnapshot> {
        let provenance = match &stage {
            FrontendDiagnosticIdentity::Syntax => self
                .batch_diagnostic_order
                .as_ref()
                .map_or(DiagnosticAttemptProvenance::Canonical, |order| {
                    DiagnosticAttemptProvenance::Presentation(order.clone())
                }),
            FrontendDiagnosticIdentity::Merge if errors.is_some() => self
                .batch_diagnostic_order
                .as_ref()
                .map_or(DiagnosticAttemptProvenance::Canonical, |order| {
                    DiagnosticAttemptProvenance::Presentation(order.clone())
                }),
            FrontendDiagnosticIdentity::Merge => DiagnosticAttemptProvenance::Canonical,
            FrontendDiagnosticIdentity::Import(_)
            | FrontendDiagnosticIdentity::Rir(_)
            | FrontendDiagnosticIdentity::Semantic(_) => DiagnosticAttemptProvenance::Canonical,
        };
        if let Some(existing) = self
            .diagnostics
            .find_exact(source, &stage, &provenance)
            .cloned()
        {
            self.metrics.diagnostic_reuse();
            self.diagnostics.select_snapshot(&existing);
            self.refresh_retention_metrics();
            return existing;
        }
        let invalidated_previous = self.diagnostics.latest().is_some();
        let snapshot = Arc::new(FrontendDiagnosticSnapshot {
            source: source.clone(),
            stage,
            provenance,
            errors: errors
                .map(|errors| errors.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .into(),
            warnings: warnings.to_vec().into(),
        });
        self.metrics.diagnostic_publication(invalidated_previous);
        self.refresh_retention_metrics();
        snapshot
    }

    pub(super) fn reuse_diagnostics(&mut self, snapshot: Arc<FrontendDiagnosticSnapshot>) {
        self.metrics.diagnostic_reuse();
        self.diagnostics.select_snapshot(&snapshot);
        self.refresh_retention_metrics();
    }

    pub(super) fn publish_import_diagnostics(
        &mut self,
        source: &SourceSnapshot,
        context: Option<crate::ImportDiscoveryContext>,
        plan: Option<crate::ImportDiscoveryPlan>,
        ledger: crate::ImportObservationLedger,
        accepted_reads: crate::AcceptedReadManifest,
        errors: &CompileErrors,
    ) -> Arc<FrontendDiagnosticSnapshot> {
        let input = ImportDiagnosticInputDescriptor {
            source: source.source_revision().clone(),
            context,
            plan,
            ledger,
            accepted_reads,
        };
        self.publish_diagnostics(
            source,
            FrontendDiagnosticIdentity::Import(input),
            Some(errors),
            &[],
        )
    }

    pub(super) fn refresh_retention_metrics(&mut self) {
        let diagnostics = self.diagnostics.retention_metrics();
        let runtime = self.queries.revisioned.runtime_retention_metrics();
        let input_stamps = self.queries.revisioned.input_stamp_retention_metrics();

        let mut pinned_attempts = BTreeSet::new();
        pinned_attempts.extend(self.queries.revisioned.parse_origin_attempt_ids());
        self.metrics.set_pinned_origins(pinned_attempts);

        self.metrics.set_retention(FrontendRetentionMetrics {
            retained_query_records: runtime.retained_terminals as usize,
            retained_bytes: runtime.retained_bytes as usize,
            peak_retained_bytes: runtime.peak_retained_bytes as usize,
            retained_byte_budget: runtime.retained_byte_budget as usize,
            dependency_pins: runtime.retained_dependency_pins as usize,
            peak_dependency_pins: runtime.peak_retained_dependency_pins as usize,
            dependency_pin_budget: runtime.dependency_pin_budget as usize,
            aggregate_retention_probes: runtime.aggregate_retention_probes as usize,
            retained_byte_probe_quantum: runtime.retained_byte_probe_quantum as usize,
            dependency_pin_probe_quantum: runtime.dependency_pin_probe_quantum as usize,
            retained_byte_probe_overshoot_bound: runtime.retained_byte_probe_overshoot_bound
                as usize,
            dependency_pin_probe_overshoot_bound: runtime.dependency_pin_probe_overshoot_bound
                as usize,
            active_task_leases: runtime.active_task_leases as usize,
            peak_task_leases: runtime.peak_task_leases as usize,
            active_retained_pins: runtime.active_retained_pins as usize,
            peak_retained_pins: runtime.peak_retained_pins as usize,
            retained_revisions: runtime.retained_revisions as usize,
            retained_module_input_views: input_stamps.module_views,
            retained_module_source_stamps: input_stamps.module_source_stamps,
            retained_import_input_views: input_stamps.import_views,
            retained_import_context_stamps: input_stamps.import_context_stamps,
            retained_import_topology_stamps: input_stamps.accepted_topology_stamps,
            retained_import_provenance_stamps: input_stamps.accepted_read_provenance_stamps,
            retained_import_observation_stamps: input_stamps.import_observation_stamps,
            retained_byte_pressure_events: runtime.retained_byte_pressure_events as usize,
            dependency_pin_pressure_events: runtime.dependency_pin_pressure_events as usize,
            retained_byte_overflow_events: runtime.retained_byte_overflow_events as usize,
            dependency_pin_overflow_events: runtime.dependency_pin_overflow_events as usize,
            peak_retained_byte_overage: runtime.peak_retained_byte_overage as usize,
            peak_dependency_pin_overage: runtime.peak_dependency_pin_overage as usize,
            query_evictions: runtime.evictions as usize,
            retained_byte_evictions: runtime.retained_byte_evictions as usize,
            dependency_pin_evictions: runtime.dependency_pin_evictions as usize,
            diagnostic_entries: diagnostics.entries,
            diagnostic_source_attempts: diagnostics.source_attempts,
            diagnostic_source_bytes: diagnostics.source_bytes,
        });
        self.metrics.set_runtime(FrontendRuntimeMetrics {
            query: runtime.into(),
            semantic_reachability: self.queries.revisioned.body_reachability_metrics(),
        });
    }
}
