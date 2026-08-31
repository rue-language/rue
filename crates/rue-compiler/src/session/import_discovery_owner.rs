//! Import-request staging, discovery closure, and capability-state ownership.

#[cfg(test)]
use super::ParseQueryRecord;
use super::{
    CanonicalImportGraphOutput, ClosedDiscoveryContinuation, CompileError, CompileErrors,
    CompilerSession, ContinuationState, ErrorKind, FrontendDiagnosticIdentity,
    FrontendDiagnosticSnapshot, ImportDiagnosticInputDescriptor, ImportDiagnosticQuery,
    ImportDiscoveryRevisionArtifact, ImportDiscoveryRevisionStatus, ImportGraphInputDescriptor,
    ImportRequestCheckpoint, IncrementalImportStage, ParsedModulesWork, ParsedProgram,
    QueryAttemptExecution, QueryComputationGuard, QueryStructuralWork, SourceSnapshot,
    SuccessorState, TrustedSuccessorDelta, no_published_program, validate_canonical_import_graph,
};
use ahash::AHashSet;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Opaque mutable authority for the import-discovery lifecycle.
///
/// The fields are private to this module, so sibling artifact owners can only
/// affect discovery through the narrow `CompilerSession` operations below.
#[derive(Debug, Default)]
pub(super) struct ImportDiscoveryOwner {
    open_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    validated_accepted_reads: Option<(SourceSnapshot, crate::AcceptedReadManifest)>,
    continuation: Option<ContinuationState>,
    successor_delta_nonce: Option<u64>,
    import_request_checkpoint: Option<ImportRequestCheckpoint>,
    next_continuation_nonce: u64,
    import_plan_groups_constructed: u64,
    import_close_records_reduced: u64,
    discovery_attempt: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    last_good_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    prior_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    oracle_import_fault: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    direct_import_diagnostic: Option<Arc<FrontendDiagnosticSnapshot>>,
}

impl ImportDiscoveryOwner {
    fn record_discovery_attempt(&mut self, artifact: Arc<ImportDiscoveryRevisionArtifact>) {
        if let Some(previous) = self.discovery_attempt.replace(artifact.clone())
            && previous.source_revision() != artifact.source_revision()
        {
            self.prior_discovery = Some(previous);
        }
        if artifact.status == ImportDiscoveryRevisionStatus::ClosedValid {
            self.last_good_discovery = Some(artifact);
        }
    }
}

impl CompilerSession {
    #[cfg(test)]
    pub(crate) fn import_discovery_plan(
        &self,
        context: crate::ImportDiscoveryContext,
    ) -> crate::CompileResult<crate::ImportDiscoveryPlan> {
        let program = self.published.as_ref().ok_or_else(|| {
            CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "import discovery requires a successfully parsed staging revision".into(),
            ))
        })?;
        crate::ImportDiscoveryPlan::new(program, context)
    }

    pub(crate) fn published_owner(&self) -> Option<&Arc<ParsedProgram>> {
        self.published.as_ref()
    }

    fn capture_import_request_checkpoint(&self) -> ImportRequestCheckpoint {
        ImportRequestCheckpoint {
            validated_accepted_reads: self.imports.validated_accepted_reads.clone(),
            continuation: self.imports.continuation.clone(),
            discovery_attempt: self.imports.discovery_attempt.clone(),
            prior_discovery: self.imports.prior_discovery.clone(),
            batch_diagnostic_order: self.batch_diagnostic_order.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    /// Begins one fresh rooted external-input request with granular immutable
    /// module source, accepted-read provenance, and observation leaves.
    /// Successor carry is available only through batch publication.
    pub(crate) fn begin_import_input_request(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: crate::AcceptedReadManifest,
    ) -> crate::CompileResult<crate::ImportInputRevision> {
        // Preserve the first uncommitted request boundary. A fresh observation
        // may deliberately supersede a provisional trusted successor after its
        // selectors have moved, but that successor is still part of the same
        // transaction: replacing this checkpoint would make abort restore the
        // provisional selectors and consumed continuation instead of the exact
        // committed predecessor (RUE-1862).
        if self.imports.import_request_checkpoint.is_none() {
            self.imports.import_request_checkpoint = Some(self.capture_import_request_checkpoint());
        }
        // A fresh observation generation invalidates any outstanding
        // trusted-toolchain continuation and successor-delta authority (RUE-1112).
        self.imports.continuation = None;
        self.imports.successor_delta_nonce = None;
        let begun = self
            .queries
            .revisioned
            .begin_import_inputs(snapshot, context, accepted_reads);
        if begun.is_err()
            && let Err(rollback) = self.abort_import_input_request()
        {
            return Err(rollback);
        }
        begun
    }

    pub(crate) fn import_demand_frontier_for_roots(
        &mut self,
        revision: crate::ImportInputRevision,
        plan: &crate::ImportDiscoveryPlan,
        mode: crate::ImportDemandMode,
        roots: &crate::ImportDemandRoots,
    ) -> crate::CompileResult<crate::ImportDemandFrontier> {
        self.queries
            .revisioned
            .import_frontier(revision, plan, mode, roots)
    }

    /// Publishes exactly one compiler-produced rooted host batch as one
    /// successor immutable revision.
    pub(crate) fn publish_import_observation_batch(
        &mut self,
        frontier: &crate::ImportDemandFrontier,
        snapshot: &SourceSnapshot,
        accepted_reads: crate::AcceptedReadManifest,
        observations: Vec<crate::ImportObservation>,
    ) -> crate::CompileResult<crate::ImportInputRevision> {
        self.queries.revisioned.publish_import_batch(
            frontier,
            snapshot,
            accepted_reads,
            observations,
        )
    }

    /// Open a discovery wave over the round's starting frontier (ADR-0075).
    ///
    /// The wave resolves the transitive import closure reachable from that
    /// frontier hop by hop against its own running ledger, then publishes once
    /// through [`Self::publish_import_wave`].
    pub(crate) fn begin_import_wave(
        &mut self,
        revision: crate::ImportInputRevision,
        plan: &crate::ImportDiscoveryPlan,
        frontier: &crate::ImportDemandFrontier,
    ) -> crate::CompileResult<crate::ImportDiscoveryWave> {
        self.begin_import_wave_with_accepted_reads(revision, plan, frontier, None)
    }

    pub(crate) fn begin_import_wave_with_accepted_reads(
        &mut self,
        revision: crate::ImportInputRevision,
        plan: &crate::ImportDiscoveryPlan,
        frontier: &crate::ImportDemandFrontier,
        accepted_reads: Option<&crate::AcceptedReadManifest>,
    ) -> crate::CompileResult<crate::ImportDiscoveryWave> {
        if frontier.revision() != revision {
            return Err(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "discovery wave frontier belongs to a stale immutable revision".into(),
            )));
        }
        let ledger = self.queries.revisioned.import_ledger(revision)?;
        crate::ImportDiscoveryWave::begin_with_accepted_reads(
            plan,
            frontier,
            ledger,
            accepted_reads,
        )
    }

    /// Record one wave hop's answers and derive the next hop's operations.
    pub(crate) fn extend_import_wave(
        &mut self,
        wave: &mut crate::ImportDiscoveryWave,
        observations: Vec<crate::ImportObservation>,
    ) -> crate::CompileResult<()> {
        wave.extend(observations)
    }

    /// Publish one whole wave as one successor immutable revision. The batch is
    /// every hop's operations and answers in hop order, so the published ledger
    /// records exactly the reads the hop-granular rounds would have recorded, in
    /// the same order.
    pub(crate) fn publish_import_wave(
        &mut self,
        mut wave: crate::ImportDiscoveryWave,
        snapshot: &SourceSnapshot,
        accepted_reads: crate::AcceptedReadManifest,
    ) -> crate::CompileResult<(crate::ImportInputRevision, crate::ImportDemandFrontier)> {
        // A wave may have parsed a source whose physical identity was already
        // assembled under another logical module (for example an import hard
        // link to the root). The assembler retains one representative, so do
        // not stage a tree for a module absent from the published snapshot.
        let staged_parses = wave
            .take_staged_parses()
            .into_iter()
            .filter(|staged| {
                snapshot
                    .files()
                    .any(|source| snapshot.module_id(source.file_id) == Some(staged.module()))
            })
            .collect();
        let (frontier, observations) = wave.into_batch();
        let revision = self.queries.revisioned.publish_import_batch(
            &frontier,
            snapshot,
            accepted_reads,
            observations,
        )?;
        // Stage only after the revision carrying these sources exists, so the
        // parse query can never observe a stage for unpublished content.
        self.queries.revisioned.stage_module_parses(staged_parses);
        Ok((revision, frontier))
    }

    /// Returns the immutable canonical ledger carried by one input revision.
    pub(crate) fn import_observation_ledger(
        &self,
        revision: crate::ImportInputRevision,
    ) -> crate::CompileResult<crate::ImportObservationLedger> {
        self.queries.revisioned.import_ledger(revision)
    }

    /// RUE-1576: how many declaration publications could not retain their
    /// projection cone this session. Expected zero; the pipeline gate pins it.
    pub(crate) fn publication_cone_retention_failures(&self) -> u64 {
        self.queries
            .revisioned
            .publication_cone_retention_failures()
    }

    /// Stages the current compiler-published import-input revision.
    ///
    /// Snapshot, context, accepted reads, and carried observations are read as
    /// one immutable compiler-owned view. A host can only advance this state by
    /// publishing a frontier batch, so it cannot substitute a peer plan or
    /// closure record.
    pub(crate) fn stage_import_input_request(
        &mut self,
        revision: crate::ImportInputRevision,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
        let Some((current, snapshot, context, accepted_reads, ledger, transition)) =
            self.queries.revisioned.current_import_view_state()
        else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "import staging requires a current compiler-published input revision".into(),
                ),
            )));
        };
        if current != revision {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "import staging requested a non-current compiler-published input revision"
                        .into(),
                ),
            )));
        }
        let incremental = match transition {
            crate::revisioned_query_database::ImportInputTransition::Fresh => None,
            crate::revisioned_query_database::ImportInputTransition::HostBatch {
                parent,
                added,
            } => {
                let Some(predecessor) = self.imports.open_discovery.as_deref().filter(|artifact| {
                    artifact.status == ImportDiscoveryRevisionStatus::Open
                        && artifact.input_revision == Some(parent)
                }) else {
                    return Err(CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(
                            "import staging cannot extend a host batch whose exact predecessor is not open"
                                .into(),
                        ),
                    )));
                };
                let Some(predecessor_plan) = predecessor.plan.clone() else {
                    return Err(CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(
                            "import staging predecessor carries no import plan".into(),
                        ),
                    )));
                };
                let Some(predecessor_parse) = predecessor.staging_parse_terminal.clone() else {
                    return Err(CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(
                            "import staging predecessor carries no exact parse terminal".into(),
                        ),
                    )));
                };
                Some(IncrementalImportStage {
                    revision: current,
                    plan_delta: added.clone(),
                    predecessor_plan,
                    parse_delta: added,
                    predecessor_parse,
                    inherited_parse_work: predecessor.parse_work,
                })
            }
            crate::revisioned_query_database::ImportInputTransition::TrustedSuccessor {
                parent,
                added,
            } => {
                return Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(format!(
                        "ordinary import staging cannot consume trusted-toolchain successor from {parent:?} with {} additions",
                        added.len()
                    )),
                )));
            }
        };
        self.stage_import_discovery_inner(
            &snapshot,
            context,
            accepted_reads,
            ledger,
            Some(current),
            incremental,
        )
    }

    /// Cumulative import occurrences the demand frontier has rooted (RUE-1112).
    /// One `ResolveImport` projection is dispatched per rooted occurrence, so the
    /// delta across a trusted-toolchain re-close counts only the newly appended
    /// leaves and modules newly discovered from them — never a predecessor
    /// occurrence. The host driver reads this to prove the re-close does not
    /// re-root the predecessor import topology.
    pub(crate) fn import_frontier_roots_requested(&self) -> u64 {
        self.queries.revisioned.import_frontier_roots_requested()
    }

    /// Cumulative import-plan request groups constructed during staging
    /// (RUE-1112). See the field docs on `import_plan_groups_constructed`.
    pub(crate) fn import_plan_groups_constructed(&self) -> u64 {
        self.imports.import_plan_groups_constructed
    }

    /// See the field docs on `parse_sources_materialized`.
    pub(crate) fn parse_sources_materialized(&self) -> u64 {
        self.parse_sources_materialized
    }

    /// Cumulative red/green validation certificate misses in the canonical
    /// query runtime. ADR-0073 makes fresh-build discovery preserve
    /// certificates across its append-only frontier rounds, so this count
    /// stays linear in module count on a deep import chain; a return toward
    /// rounds-times-graph growth is a structural regression.
    pub(crate) fn validation_certificate_misses(&self) -> u64 {
        self.queries
            .revisioned
            .runtime_retention_metrics()
            .validation
            .certificate_misses
    }

    /// See the field docs on `parse_key_entries_compared`.
    pub(crate) fn parse_key_entries_compared(&self) -> u64 {
        self.parse_key_entries_compared
    }

    /// See the field docs on `parse_modules_dispatched`.
    pub(crate) fn parse_modules_dispatched(&self) -> u64 {
        self.parse_modules_dispatched
    }

    /// See the field docs on `parse_invalidation_entries_compared`.
    pub(crate) fn parse_invalidation_entries_compared(&self) -> u64 {
        self.parse_invalidation_entries_compared
    }

    /// Module-identity resolutions asked of the published source snapshots, and
    /// the snapshot positions examined to answer them. See
    /// [`crate::source_snapshot::IdentityResolutionMeter`].
    pub(crate) fn snapshot_module_resolutions(&self) -> u64 {
        self.queries
            .revisioned
            .identity_resolution()
            .module_resolutions()
    }

    /// See [`Self::snapshot_module_resolutions`].
    pub(crate) fn snapshot_module_resolution_visits(&self) -> u64 {
        self.queries
            .revisioned
            .identity_resolution()
            .module_resolution_visits()
    }

    /// Physical-identity lookups asked of the accepted-read manifests, and the
    /// manifest entries examined to answer them. See
    /// [`crate::source_snapshot::IdentityResolutionMeter`].
    pub(crate) fn accepted_read_identity_lookups(&self) -> u64 {
        self.queries
            .revisioned
            .identity_resolution()
            .physical_identity_lookups()
    }

    /// See [`Self::accepted_read_identity_lookups`].
    pub(crate) fn accepted_read_identity_visits(&self) -> u64 {
        self.queries
            .revisioned
            .identity_resolution()
            .physical_identity_visits()
    }

    /// Attempt-handoff lifecycles offered to a task's observation scope, and
    /// the scope positions examined answering them. See
    /// [`rue_query::RuntimeMetrics::handoff_observations`].
    pub(crate) fn handoff_observations(&self) -> u64 {
        self.queries
            .revisioned
            .runtime_retention_metrics()
            .handoff_observations
    }

    /// See [`Self::handoff_observations`].
    pub(crate) fn handoff_observation_visits(&self) -> u64 {
        self.queries
            .revisioned
            .runtime_retention_metrics()
            .handoff_observation_visits
    }

    /// A snapshot of the production provider-op observation counters
    /// (ADR-0066 §4).
    pub(crate) fn provider_observation_metrics(
        &self,
    ) -> crate::unstable::ProviderObservationMetrics {
        self.queries.revisioned.provider_observation_metrics()
    }

    /// A snapshot of the lookup-family pressure metrics (RUE-1091, ADR-0066 §4).
    /// Production body publications retain their exact observed lookup
    /// terminals in the session's `PublishedRootLookupLease`.
    pub(crate) fn lookup_pressure_metrics(&self) -> crate::unstable::LookupPressureMetrics {
        self.queries.revisioned.lookup_pressure_metrics()
    }

    /// The currently selected parse terminal, for identity assertions.
    #[cfg(test)]
    pub(crate) fn selected_parse_terminal(
        &self,
    ) -> Option<Arc<rue_query::QueryTerminal<ParseQueryRecord>>> {
        self.queries.revisioned.selected_parse_terminal()
    }

    /// Cumulative canonical-frontier replacement events. A strictly-additive
    /// successor adoption preserves the predecessor revision and contributes
    /// zero; ordinary source replacement advances this counter once.
    pub(crate) fn frontend_query_invalidations(&self) -> u64 {
        self.metrics.work().downstream_invalidations as u64
    }

    /// Cumulative close-time `ResolveImport` projections dispatched (RUE-1112).
    pub(crate) fn exact_import_groups_dispatched(&self) -> u64 {
        self.queries.revisioned.exact_import_groups_dispatched()
    }

    /// Cumulative canonical import records reduced and validated during close
    /// (RUE-1112). See the field docs on `import_close_records_reduced`.
    pub(crate) fn import_close_records_reduced(&self) -> u64 {
        self.imports.import_close_records_reduced
    }

    /// Cumulative leaves published through the complete publication path
    /// (fresh generations); scales with the program (RUE-1112).
    pub(crate) fn import_view_full_leaves_published(&self) -> u64 {
        self.queries.revisioned.import_view_full_leaves_published()
    }

    /// Cumulative delta leaves published through the sparse successor overlay
    /// path; predecessor leaves are structurally inherited and never counted
    /// (RUE-1112).
    pub(crate) fn import_view_overlay_leaves_published(&self) -> u64 {
        self.queries
            .revisioned
            .import_view_overlay_leaves_published()
    }

    /// Cumulative predecessor ledger observations deep-cloned into successor
    /// view ledgers (visible remaining cost; RUE-1112).
    pub(crate) fn import_view_ledger_entries_cloned(&self) -> u64 {
        self.queries.revisioned.import_view_ledger_entries_cloned()
    }

    /// Predecessor source entries compared by the overlay publication's fallback
    /// diff; zero whenever the structural-authority path ran (RUE-1112).
    pub(crate) fn import_view_source_entries_compared(&self) -> u64 {
        self.queries
            .revisioned
            .import_view_source_entries_compared()
    }

    /// Predecessor accepted-read entries compared by the overlay publication's
    /// provenance diff (RUE-1112).
    pub(crate) fn import_view_read_entries_compared(&self) -> u64 {
        self.queries.revisioned.import_view_read_entries_compared()
    }

    /// Structural-sharing witness for the committed import discovery's three
    /// additively shared artifacts (RUE-1112): for each of the canonical graph
    /// records, the plan's request groups, and the module-resolution table, the
    /// identity address of its shared predecessor segment and its delta length. A
    /// trusted-toolchain successor carries each predecessor segment `Arc` by
    /// reference, so every address equals the predecessor close's — proving no
    /// predecessor entry was copied, re-sorted, or reallocated.
    pub(crate) fn committed_successor_sharing(&self) -> Option<[(usize, usize); 3]> {
        let artifact = self.committed_import_discovery_artifact()?;
        let graph = artifact.graph.as_ref()?;
        let plan = artifact.plan.as_ref()?;
        let record_segments = graph.graph().record_segments();
        let group_segments = plan.group_segments();
        let module_segments = graph.input().resolution.module_segments();
        let witness =
            |predecessor_ptr: *const (), delta_len: usize| (predecessor_ptr as usize, delta_len);
        Some([
            witness(
                Arc::as_ptr(record_segments.predecessor_segment()) as *const (),
                record_segments.delta_segment().len(),
            ),
            witness(
                Arc::as_ptr(group_segments.predecessor_segment()) as *const (),
                group_segments.delta_segment().len(),
            ),
            witness(
                Arc::as_ptr(module_segments.predecessor_segment()) as *const (),
                module_segments.delta_segment().len(),
            ),
        ])
    }
    #[cfg(test)]
    pub(crate) fn discovery_attempt(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.discovery_attempt_artifact()
    }

    pub(crate) fn discovery_attempt_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.imports
            .oracle_import_fault
            .as_ref()
            .or(self.imports.open_discovery.as_ref())
            .or(self.imports.discovery_attempt.as_ref())
    }
    #[cfg(test)]
    pub(crate) fn last_good_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.last_good_discovery_artifact()
    }

    pub(crate) fn last_good_discovery_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.imports.last_good_discovery.as_ref()
    }

    pub(crate) fn committed_import_discovery_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        let source = self.published.as_ref()?.source_revision();
        self.imports.discovery_attempt.as_ref().filter(|artifact| {
            artifact.status == ImportDiscoveryRevisionStatus::ClosedValid
                && artifact.source_revision() == source
        })
    }

    /// Return the canonical graph and captured resolution context adopted for
    /// the current compiler revision.
    pub fn committed_import_graph(&self) -> Result<Arc<CanonicalImportGraphOutput>, CompileErrors> {
        let committed = self.committed_import_discovery_artifact().ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "no closed-valid import discovery revision is committed".into(),
            )))
        })?;
        Ok(committed
            .graph()
            .expect("closed-valid discovery revisions retain their canonical graph")
            .clone())
    }

    /// Return the sole compiler-owned diagnostic batch for the current import
    /// attempt. Batch, emit, and incremental consumers all receive this exact
    /// memoized `Arc`. Direct no-I/O sessions use the same query for parser
    /// shape preflight; ordinary open discovery work has no publishable batch.
    pub fn import_diagnostics(&mut self) -> Result<Arc<FrontendDiagnosticSnapshot>, CompileErrors> {
        let mut guard = self.metrics.begin::<ImportDiagnosticQuery>();
        let mut reused = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.import_diagnostics_attempt(&mut guard, &mut reused)
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        guard.finish(
            if reused {
                QueryAttemptExecution::Reused
            } else {
                QueryAttemptExecution::Computed
            },
            None,
            &result,
            QueryStructuralWork::None,
        );
        self.metrics.synchronize();
        result
    }

    fn import_diagnostics_attempt(
        &mut self,
        guard: &mut QueryComputationGuard,
        reused: &mut bool,
    ) -> Result<Arc<FrontendDiagnosticSnapshot>, CompileErrors> {
        let diagnostics = if let Some(attempt) = self.discovery_attempt_artifact() {
            let diagnostics = attempt.diagnostic_snapshot.as_ref().ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "open import discovery work has no canonical diagnostic batch".into(),
                )))
            })?;
            *reused = true;
            diagnostics.clone()
        } else {
            let source = self
                .published_snapshot
                .clone()
                .ok_or_else(no_published_program)?;
            let input = ImportDiagnosticInputDescriptor {
                source: source.source_revision().clone(),
                context: None,
                plan: None,
                ledger: crate::ImportObservationLedger::default(),
                accepted_reads: crate::AcceptedReadManifest::from_entries(Vec::new()),
            };
            if let Some(diagnostics) =
                self.imports
                    .direct_import_diagnostic
                    .as_ref()
                    .filter(|diagnostics| {
                        diagnostics.source_revision() == source.source_revision()
                            && diagnostics.identity()
                                == &FrontendDiagnosticIdentity::Import(input.clone())
                    })
            {
                *reused = true;
                diagnostics.clone()
            } else {
                let program = self.published.as_ref().ok_or_else(no_published_program)?;
                let errors = crate::ImportDiscoveryPlan::shape_diagnostics(program);
                guard.started();
                let diagnostics = self.publish_diagnostics(
                    &source,
                    FrontendDiagnosticIdentity::Import(input),
                    Some(&errors),
                    &[],
                );
                self.imports.direct_import_diagnostic = Some(diagnostics.clone());
                diagnostics
            }
        };
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        Ok(diagnostics)
    }

    pub(super) fn require_successful_import_diagnostics(&mut self) -> Result<(), CompileErrors> {
        let diagnostics = self.import_diagnostics()?;
        if diagnostics.errors().is_empty() {
            Ok(())
        } else {
            Err(CompileErrors::from(diagnostics.errors().to_vec()))
        }
    }

    #[cfg(test)]
    pub(crate) fn stage_import_discovery(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
        carried_ledger: crate::ImportObservationLedger,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
        self.stage_import_discovery_inner(
            snapshot,
            context,
            crate::AcceptedReadManifest::from_shared(accepted_reads),
            carried_ledger,
            None,
            None,
        )
    }

    /// Verify a [`TrustedSuccessorDelta`] against this session and the CURRENT
    /// compiler-published import-input view, returning that view's exact state
    /// together with the derived module delta (RUE-1112). The successor stage and
    /// close consume ONLY this published state — snapshot, context, provenance,
    /// and ledger — so a caller cannot substitute any replacement; the view
    /// itself is only extendable through justified overlay publications, so the
    /// derived delta always equals the accumulated compiler-authorized additions.
    fn derive_successor_state(
        &self,
        delta: &TrustedSuccessorDelta,
    ) -> Result<SuccessorState, CompileErrors> {
        let reject = |message: &str| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("trusted-toolchain successor delta rejected: {message}"),
            )))
        };
        if !Arc::ptr_eq(&delta.session, &self.identity) {
            return Err(reject("the successor delta belongs to a different session"));
        }
        let Some(outstanding) = self.imports.successor_delta_nonce else {
            return Err(reject(
                "no outstanding successor-delta authority; it was already consumed or invalidated",
            ));
        };
        if outstanding != delta.nonce {
            return Err(reject(
                "the successor delta is stale (superseded by a newer publish, request, or close)",
            ));
        }
        let Some((current, snapshot, context, accepted_reads, ledger, transition)) =
            self.queries.revisioned.current_import_view_state()
        else {
            return Err(reject(
                "no current import-input view backs the successor delta",
            ));
        };
        if current.request_generation != delta.revision.request_generation {
            return Err(reject(
                "the successor delta belongs to a different request generation than the current view",
            ));
        }
        // The module delta is the session-owned recorded-additions lineage: the
        // exact additions each overlay publication recorded since the committed
        // close. Predecessor byte-identity is enforced where state changes — at
        // every overlay publication — so nothing is re-derived by scanning
        // complete views here; this read is O(delta).
        let mut new_modules: Vec<crate::ModuleRevision> =
            self.queries.revisioned.lineage_additions().to_vec();
        new_modules.sort_by(|a, b| a.module.cmp(&b.module));
        new_modules.dedup();
        // Every authorized appended module must be present, so the successor can
        // never omit a demanded trusted module.
        let new_set: std::collections::BTreeSet<&crate::ModuleId> = new_modules
            .iter()
            .map(|revision| &revision.module)
            .collect();
        for module in delta.appended.iter() {
            if !new_set.contains(module) {
                return Err(reject(
                    "the successor omits an authorized appended module; it must contain every demanded trusted module",
                ));
            }
        }
        Ok(SuccessorState {
            snapshot,
            context,
            accepted_reads,
            ledger,
            revision: current,
            transition,
            delta: new_modules.into(),
        })
    }

    /// Stage a strictly-additive trusted-toolchain successor (RUE-1112). The
    /// staged snapshot, context, provenance, and carried ledger are the CURRENT
    /// compiler-published view's own state, and the module delta is derived and
    /// verified from the opaque `delta` capability — the caller supplies nothing
    /// but the capability. The plan reuses the committed predecessor plan's
    /// request groups and constructs groups only for the delta's import
    /// occurrences, so predecessor occurrences are never re-staged.
    pub(crate) fn stage_import_discovery_successor(
        &mut self,
        delta: &TrustedSuccessorDelta,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
        let state = self.derive_successor_state(delta)?;
        let revision = state.revision;
        let reject = |message: &str| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("trusted-toolchain successor staging rejected: {message}"),
            )))
        };

        // The successor plan's delta means every module added since the
        // committed close. Keep that plan anchored to the committed artifact so
        // close-time graph reduction sees the complete cumulative delta even
        // when the final staging round appends nothing.
        let committed = self
            .last_good_discovery_artifact()
            .filter(|artifact| {
                artifact.input_revision.is_some_and(|predecessor| {
                    predecessor.request_generation == revision.request_generation
                })
            })
            .ok_or_else(|| {
                reject("the successor has no committed predecessor in its generation")
            })?;
        if let crate::revisioned_query_database::ImportInputTransition::TrustedSuccessor {
            parent,
            ..
        } = &state.transition
            && committed.input_revision != Some(*parent)
        {
            return Err(reject(
                "the first trusted-successor step does not extend the committed predecessor",
            ));
        }
        let predecessor_plan = committed
            .plan
            .clone()
            .ok_or_else(|| reject("the committed predecessor has no import plan"))?;
        let plan_delta = state.delta.clone();

        // Parse staging has a deliberately narrower predecessor: the exact
        // request-local candidate produced by the previous round. A request may
        // also stage the same immutable input view again after its frontier
        // becomes empty to prove the closing witness. Chain that zero-delta
        // stage to the exact open artifact for this revision. Neither path
        // promotes the provisional terminal across the commit boundary.
        let same_revision = self.imports.open_discovery.as_ref().filter(|artifact| {
            artifact.status == ImportDiscoveryRevisionStatus::Open
                && artifact.input_revision == Some(revision)
                && continues_discovery_lifecycle(
                    artifact,
                    &state.snapshot,
                    &state.context,
                    &state.accepted_reads,
                    &state.ledger,
                )
        });
        let (parse_delta, predecessor_parse, inherited_parse_work) = if let Some(predecessor) =
            same_revision
        {
            let parse = predecessor.staging_parse_terminal.clone().ok_or_else(|| {
                reject("the open same-revision predecessor has no exact parse terminal")
            })?;
            (
                Arc::<[crate::ModuleRevision]>::from([]),
                parse,
                predecessor.parse_work,
            )
        } else {
            match &state.transition {
                crate::revisioned_query_database::ImportInputTransition::HostBatch {
                    parent,
                    added,
                } => {
                    let predecessor = self
                        .imports
                        .open_discovery
                        .as_ref()
                        .filter(|artifact| {
                            artifact.status == ImportDiscoveryRevisionStatus::Open
                                && artifact.input_revision == Some(*parent)
                        })
                        .ok_or_else(|| {
                            reject("the host-batch parent has no exact open predecessor artifact")
                        })?;
                    let parse = predecessor.staging_parse_terminal.clone().ok_or_else(|| {
                        reject("the open host-batch predecessor has no exact parse terminal")
                    })?;
                    (added.clone(), parse, predecessor.parse_work)
                }
                crate::revisioned_query_database::ImportInputTransition::TrustedSuccessor {
                    parent,
                    added,
                } => {
                    let predecessor = self
                        .last_good_discovery_artifact()
                        .filter(|artifact| artifact.input_revision == Some(*parent))
                        .ok_or_else(|| {
                            reject("the trusted successor has no exact committed predecessor")
                        })?;
                    let parse = self
                        .queries
                        .revisioned
                        .last_good_parse_terminal()
                        .cloned()
                        .ok_or_else(|| {
                            reject("the committed predecessor has no exact parse terminal")
                        })?;
                    (added.clone(), parse, predecessor.parse_work)
                }
                crate::revisioned_query_database::ImportInputTransition::Fresh => {
                    return Err(reject(
                        "a fresh import view cannot consume trusted-successor authority",
                    ));
                }
            }
        };
        self.stage_import_discovery_inner(
            &state.snapshot,
            state.context,
            state.accepted_reads,
            state.ledger,
            Some(revision),
            Some(IncrementalImportStage {
                revision,
                plan_delta,
                predecessor_plan,
                parse_delta,
                predecessor_parse,
                inherited_parse_work,
            }),
        )
    }

    /// Prove the staged snapshot and its accepted-read provenance manifest agree,
    /// fail-closed, before anything is staged from them.
    ///
    /// A stage whose snapshot AND manifest are both direct structural extensions
    /// of the exact pair this session last proved re-checks only the appended
    /// entries. The prefix is not assumed unchanged: both values are immutable
    /// and the extension proof is pointer lineage, so the retained prefix IS the
    /// pair already checked. Any other stage — a fresh lineage, a rebuilt
    /// snapshot, a manifest that compacted away its lineage — re-checks the whole
    /// pair, which is also what a mid-discovery source change lands on, because a
    /// changed source cannot appear as an extension of an already proved pair.
    fn validate_staged_accepted_reads(
        &mut self,
        snapshot: &SourceSnapshot,
        accepted_reads: &crate::AcceptedReadManifest,
    ) -> Result<(), CompileErrors> {
        let extension = self.imports.validated_accepted_reads.as_ref().and_then(
            |(previous_snapshot, previous_reads)| {
                let files = snapshot.direct_appended_file_ids_from(previous_snapshot)?;
                let entries = accepted_reads
                    .segments()
                    .direct_delta_from(previous_reads.segments())?;
                Some((files, entries, previous_reads))
            },
        );
        match extension {
            Some((files, entries, previous_reads)) => {
                validate_appended_accepted_reads(
                    snapshot,
                    accepted_reads,
                    previous_reads,
                    &files,
                    entries,
                )?;
            }
            None => validate_accepted_read_manifest(snapshot, accepted_reads)?,
        }
        self.imports.validated_accepted_reads = Some((snapshot.clone(), accepted_reads.clone()));
        Ok(())
    }

    fn stage_import_discovery_inner(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: crate::AcceptedReadManifest,
        carried_ledger: crate::ImportObservationLedger,
        input_revision: Option<crate::ImportInputRevision>,
        incremental: Option<IncrementalImportStage>,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
        let new_module_ids: Option<Vec<crate::ModuleId>> = incremental.as_ref().map(|stage| {
            let delta = &stage.plan_delta;
            delta
                .iter()
                .map(|revision| revision.module.clone())
                .collect()
        });
        let new_modules: Option<&[crate::ModuleId]> = new_module_ids.as_deref();
        let continuation = incremental
            .is_none()
            .then(|| {
                self.imports.open_discovery.as_deref().filter(|attempt| {
                    continues_discovery_lifecycle(
                        attempt,
                        snapshot,
                        &context,
                        &accepted_reads,
                        &carried_ledger,
                    )
                })
            })
            .flatten();
        let mut parse_work = incremental.as_ref().map_or_else(
            || continuation.map_or_else(ParsedModulesWork::default, |attempt| attempt.parse_work),
            |stage| stage.inherited_parse_work,
        );
        // Reinstall protocol context only if staging reaches Open. Closed
        // attempts are retained as projections of the canonical frontier.
        self.imports.open_discovery = None;
        let source_revision = snapshot.source_revision().clone();
        if let Err(errors) = self.validate_staged_accepted_reads(snapshot, &accepted_reads) {
            let diagnostic_snapshot = self.publish_import_diagnostics(
                snapshot,
                Some(context.clone()),
                None,
                carried_ledger.clone(),
                accepted_reads.clone(),
                &errors,
            );
            let attempted_artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                status: ImportDiscoveryRevisionStatus::ClosedAttempted,
                input_revision,
                source_revision: source_revision.clone(),
                context: context.clone(),
                snapshot: snapshot.clone(),
                program: None,
                parse_work,
                plan: None,
                ledger: carried_ledger.clone(),
                accepted_reads: accepted_reads.clone(),
                graph: None,
                diagnostics: errors.clone(),
                diagnostic_snapshot: Some(diagnostic_snapshot),
                staging_parse_terminal: None,
                successor_parse: None,
            });
            self.imports.record_discovery_attempt(attempted_artifact);
            return Err(errors);
        }
        // Staging splits into the canonical parse of everything read so far and
        // the import-plan construction over the resulting program. Both were
        // previously folded into the driver's unattributed region (RUE-786).
        let parse_staging_span = tracing::info_span!("import_parse_staging").entered();
        let (parse_result, staged_work, staged_successor_parse, staging_parse_terminal) = self
            .parse_staging_snapshot(
                snapshot,
                incremental.as_ref().map(|stage| {
                    (
                        stage.revision,
                        &stage.parse_delta,
                        stage.predecessor_parse.clone(),
                    )
                }),
            );
        drop(parse_staging_span);
        parse_work.accumulate(staged_work);
        let program = match parse_result {
            Ok(program) => program,
            Err(errors) => {
                let diagnostic_snapshot = self.publish_import_diagnostics(
                    snapshot,
                    Some(context.clone()),
                    None,
                    carried_ledger.clone(),
                    accepted_reads.clone(),
                    &errors,
                );
                let attempted_artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                    status: ImportDiscoveryRevisionStatus::ClosedAttempted,
                    input_revision,
                    source_revision: source_revision.clone(),
                    context: context.clone(),
                    snapshot: snapshot.clone(),
                    program: None,
                    parse_work,
                    plan: None,
                    ledger: carried_ledger.clone(),
                    accepted_reads: accepted_reads.clone(),
                    graph: None,
                    diagnostics: errors.clone(),
                    diagnostic_snapshot: Some(diagnostic_snapshot),
                    staging_parse_terminal: None,
                    successor_parse: None,
                });
                self.imports.record_discovery_attempt(attempted_artifact);
                return Err(errors);
            }
        };
        // A compiler-proven additive stage reuses the predecessor plan's request
        // groups and constructs groups only for the newly appended modules'
        // occurrences; predecessor occurrences are never re-staged. Ordinary
        // host batches and capability-authorized successors reach this one path
        // through private, exact parent/delta transitions.
        let plan_build_span = tracing::info_span!("import_plan_build").entered();
        let plan_build = match (new_modules, incremental.as_ref()) {
            (Some(new_modules), Some(stage)) => crate::ImportDiscoveryPlan::extend_successor(
                &stage.predecessor_plan,
                &program,
                context.clone(),
                new_modules,
            )
            .map(|(plan, constructed)| {
                self.imports.import_plan_groups_constructed = self
                    .imports
                    .import_plan_groups_constructed
                    .saturating_add(constructed);
                plan
            }),
            _ => crate::ImportDiscoveryPlan::new(&program, context.clone()).inspect(|plan| {
                self.imports.import_plan_groups_constructed = self
                    .imports
                    .import_plan_groups_constructed
                    .saturating_add(plan.groups().len() as u64);
            }),
        };
        let plan = match plan_build {
            Ok(plan) => plan,
            Err(error) => {
                let errors = CompileErrors::from(error);
                let diagnostic_snapshot = self.publish_import_diagnostics(
                    snapshot,
                    Some(context.clone()),
                    None,
                    carried_ledger.clone(),
                    accepted_reads.clone(),
                    &errors,
                );
                let attempted_artifact = Arc::new(ImportDiscoveryRevisionArtifact {
                    status: ImportDiscoveryRevisionStatus::ClosedAttempted,
                    input_revision,
                    source_revision: source_revision.clone(),
                    context: context.clone(),
                    snapshot: snapshot.clone(),
                    program: Some(program),
                    parse_work,
                    plan: None,
                    ledger: carried_ledger.clone(),
                    accepted_reads: accepted_reads.clone(),
                    graph: None,
                    diagnostics: errors.clone(),
                    diagnostic_snapshot: Some(diagnostic_snapshot),
                    staging_parse_terminal: None,
                    successor_parse: None,
                });
                self.imports.record_discovery_attempt(attempted_artifact);
                return Err(errors);
            }
        };
        drop(plan_build_span);
        let _plan_publish_span = tracing::info_span!("import_plan_publish").entered();
        let shape_diagnostics = crate::ImportDiscoveryPlan::shape_diagnostics(&program);
        self.publish_import_diagnostics(
            snapshot,
            Some(context.clone()),
            Some(plan.clone()),
            carried_ledger.clone(),
            accepted_reads.clone(),
            &shape_diagnostics,
        );
        self.imports.open_discovery = Some(Arc::new(ImportDiscoveryRevisionArtifact {
            status: ImportDiscoveryRevisionStatus::Open,
            input_revision,
            source_revision: program.source_revision().clone(),
            context,
            snapshot: snapshot.clone(),
            program: Some(program),
            parse_work,
            plan: Some(plan.clone()),
            ledger: carried_ledger,
            accepted_reads,
            graph: None,
            diagnostics: CompileErrors::new(),
            diagnostic_snapshot: None,
            staging_parse_terminal,
            successor_parse: staged_successor_parse,
        }));
        Ok(plan)
    }

    #[cfg(test)]
    pub(crate) fn close_import_discovery(
        &mut self,
        ledger: crate::ImportObservationLedger,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
        self.close_import_discovery_artifact(ledger, None)
    }

    /// Closes the current compiler-published import-input revision.
    ///
    /// The closing ledger comes from the same immutable revision that supplied
    /// the staged plan. This keeps root discovery authority in the compiler.
    pub(crate) fn close_import_input_request(
        &mut self,
        revision: crate::ImportInputRevision,
    ) -> Result<Arc<crate::ImportDiscoveryView>, CompileErrors> {
        let Some((current, _, _, _, ledger, _)) =
            self.queries.revisioned.current_import_view_state()
        else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "import closure requires a current compiler-published input revision".into(),
                ),
            )));
        };
        if current != revision {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "import closure requested a non-current compiler-published input revision"
                        .into(),
                ),
            )));
        }
        self.close_import_discovery_artifact(ledger, None)
            .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact)))
    }

    /// Close a strictly-additive trusted-toolchain successor (RUE-1112). The
    /// closing ledger is the CURRENT compiler-published view's own carried
    /// ledger and the module delta is derived from the opaque capability — the
    /// caller supplies nothing but the capability, so no replacement ledger or
    /// module set can be substituted. The close projects and reduces only the
    /// delta occurrences and merges them into the committed predecessor's closed
    /// graph, never re-projecting or re-reducing predecessor occurrences.
    pub(crate) fn close_import_discovery_successor(
        &mut self,
        delta: &TrustedSuccessorDelta,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
        let state = self.derive_successor_state(delta)?;
        let closed = self
            .close_import_discovery_artifact(state.ledger, Some((state.revision, state.delta)))?;
        // Consume the single-use delta authority only on a successful close.
        self.imports.successor_delta_nonce = None;
        Ok(closed)
    }

    fn close_import_discovery_artifact(
        &mut self,
        ledger: crate::ImportObservationLedger,
        successor: Option<(crate::ImportInputRevision, Arc<[crate::ModuleRevision]>)>,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
        let new_module_ids: Option<Vec<crate::ModuleId>> = successor.as_ref().map(|(_, delta)| {
            delta
                .iter()
                .map(|revision| revision.module.clone())
                .collect()
        });
        let new_modules: Option<&[crate::ModuleId]> = new_module_ids.as_deref();
        let open = self
            .imports
            .open_discovery
            .as_deref()
            .filter(|artifact| artifact.status == ImportDiscoveryRevisionStatus::Open)
            .ok_or_else(|| CompileErrors::from(no_published_program()))?
            .clone();
        let plan = open
            .plan
            .as_ref()
            .expect("open discovery attempt retains its plan")
            .clone();
        let program = open
            .program
            .as_ref()
            .expect("open discovery attempt retains its program")
            .clone();
        // A trusted-toolchain successor close carries the committed predecessor's
        // closed graph and projects/reduces only the newly appended modules'
        // occurrences. When no predecessor graph is retained it falls back to a
        // full close so the committed graph is always complete.
        let new_module_set: Option<std::collections::BTreeSet<crate::ModuleId>> =
            new_modules.map(|modules| modules.iter().cloned().collect());
        let predecessor_graph = new_modules.and_then(|_| {
            self.last_good_discovery_artifact()
                .and_then(|artifact| artifact.graph.clone())
        });
        let narrow = match (new_module_set, predecessor_graph) {
            (Some(set), Some(graph)) => Some((set, graph)),
            _ => None,
        };
        // A successor projects only the delta occurrences, derived directly from
        // the plan's delta segment — never by filtering the merged plan.
        let roots = match &narrow {
            Some(_) => plan.delta_roots(),
            None => crate::ImportDemandRoots::whole_plan(&plan),
        };
        let exact_groups = match self.queries.revisioned.current_import_revision() {
            Some(revision) => match self
                .queries
                .revisioned
                .exact_import_groups(revision, &roots)
            {
                Ok(groups) => groups,
                Err(error) => {
                    let errors = CompileErrors::from(error);
                    self.publish_failed_import_attempt(
                        open,
                        plan,
                        ledger,
                        successor.as_ref(),
                        ImportDiscoveryRevisionStatus::ClosedAttempted,
                        None,
                        &errors,
                    );
                    return Err(errors);
                }
            },
            None => {
                #[cfg(test)]
                {
                    plan.groups().to_vec()
                }
                #[cfg(not(test))]
                {
                    return Err(CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(
                            "import closure requires a current compiler-published input revision"
                                .into(),
                        ),
                    )));
                }
            }
        };
        // The predecessor ledger portion was validated at the predecessor close;
        // a successor close validates and reduces only the newly appended
        // occurrences' observations. Those observations are gathered directly from
        // the plan's delta groups (O(delta)), never by scanning the full carried
        // ledger. The full `ledger` is still what the committed artifact carries.
        let narrow_ledger = match &narrow {
            Some(_) => {
                let new_observations = plan
                    .delta_groups()
                    .iter()
                    .flat_map(|group| group.iter())
                    .filter_map(|request| ledger.get(request).cloned())
                    .collect::<Vec<_>>();
                let mut filtered = crate::ImportObservationLedger::default();
                let mut record_error = None;
                for observation in new_observations {
                    if let Err(error) = filtered.record(observation) {
                        record_error = Some(error);
                        break;
                    }
                }
                if let Some(error) = record_error {
                    let errors = CompileErrors::from(error);
                    self.publish_failed_import_attempt(
                        open,
                        plan,
                        ledger,
                        successor.as_ref(),
                        ImportDiscoveryRevisionStatus::ClosedAttempted,
                        None,
                        &errors,
                    );
                    return Err(errors);
                }
                Some(filtered)
            }
            None => None,
        };
        let check_ledger = narrow_ledger.as_ref().unwrap_or(&ledger);
        if let Err(error) =
            crate::import_discovery::validate_exact_import_ledger(&exact_groups, check_ledger)
        {
            let errors = CompileErrors::from(error);
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                successor.as_ref(),
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        if !crate::import_discovery::exact_import_pending_requests(&exact_groups, check_ledger)
            .is_empty()
        {
            let errors =
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "import discovery ledger is incomplete; the attempted revision cannot close"
                        .into(),
                )));
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                successor.as_ref(),
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        let diagnostics = crate::import_discovery::exact_import_diagnostics(
            &program,
            plan.context(),
            &exact_groups,
            check_ledger,
            &open.accepted_reads,
        );
        if crate::import_discovery::exact_import_has_failures(&exact_groups, check_ledger) {
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                successor.as_ref(),
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &diagnostics,
            );
            return Err(diagnostics);
        }

        // A successor shares the committed predecessor's module-resolution table
        // by reference and appends only the delta modules (looked up by identity),
        // so the complete table is never reconstructed or re-sorted. A full close
        // builds the whole table.
        let resolution_build = match &narrow {
            Some((set, predecessor)) => {
                let delta: Vec<crate::ModuleResolutionInput> = set
                    .iter()
                    .filter_map(|module_id| program.module(module_id))
                    .map(|module| crate::ModuleResolutionInput {
                        module: module.module_id().clone(),
                        physical_path: Arc::from(module.physical_path()),
                    })
                    .collect();
                crate::ModuleResolutionInputs::extend_successor(
                    &predecessor.input().resolution,
                    delta,
                )
            }
            None => crate::ModuleResolutionInputs::new(
                program.root().clone(),
                program
                    .modules()
                    .iter()
                    .map(|module| crate::ModuleResolutionInput {
                        module: module.module_id().clone(),
                        physical_path: Arc::from(module.physical_path()),
                    })
                    .collect(),
            ),
        };
        let resolution = match resolution_build {
            Ok(resolution) => resolution,
            Err(error) => {
                let errors = CompileErrors::from(error);
                self.publish_failed_import_attempt(
                    open,
                    plan,
                    ledger,
                    successor.as_ref(),
                    ImportDiscoveryRevisionStatus::ClosedAttempted,
                    None,
                    &errors,
                );
                return Err(errors);
            }
        };
        let input = ImportGraphInputDescriptor {
            sources: program.source_revision().clone(),
            resolution,
            std_dir: open.context.std_root().map(Arc::from),
        };
        // Reduce only the projected occurrences: the whole plan for a full close,
        // or exactly the newly appended modules' occurrences for a trusted-toolchain
        // successor. `reduced` therefore holds the new records in successor mode.
        let reduced = match crate::import_discovery::reduce_exact_import_graph(
            program.root().clone(),
            &exact_groups,
            check_ledger,
            &open.accepted_reads,
        ) {
            Ok(graph) => graph,
            Err(error) => {
                let errors = CompileErrors::from(error);
                self.publish_failed_import_attempt(
                    open,
                    plan,
                    ledger,
                    successor.as_ref(),
                    ImportDiscoveryRevisionStatus::ClosedAttempted,
                    None,
                    &errors,
                );
                return Err(errors);
            }
        };
        self.imports.import_close_records_reduced = self
            .imports
            .import_close_records_reduced
            .saturating_add(reduced.records().len() as u64);
        // In successor mode, merge the new records into the committed predecessor's
        // closed graph and validate incrementally (the predecessor topology is
        // carried, never re-walked). A full close reduces and validates the whole
        // graph directly.
        let (reduced, validation) = match &narrow {
            Some((set, predecessor)) => {
                // The reduction produced only the delta records; build the
                // successor graph from the exact delta. Untouched size tiers stay
                // shared; any tail compaction preserves canonical order. Validate
                // only the delta against the carried predecessor result.
                let new_records = reduced.records().to_vec();
                let merged = crate::CanonicalImportGraph::from_additive_successor(
                    program.root().clone(),
                    predecessor.graph(),
                    new_records.clone(),
                );
                let validation = crate::validate_additive_successor(
                    predecessor.validation(),
                    &new_records,
                    &input.resolution,
                    set,
                );
                (merged, validation)
            }
            None => {
                let validation = validate_canonical_import_graph(&reduced, &input.resolution);
                (reduced, validation)
            }
        };
        let graph = Arc::new(CanonicalImportGraphOutput {
            input: input.clone(),
            graph: reduced,
            validation,
        });
        let resolution_only = graph.validation().problems().iter().all(|problem| {
            matches!(
                problem,
                crate::CanonicalImportGraphProblem::MissingResolution { .. }
            )
        });
        if !resolution_only {
            let mut errors = diagnostics;
            errors.push(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "import discovery produced a structurally invalid canonical graph".into(),
            )));
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                successor.as_ref(),
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        if !graph.validation().is_valid() || !diagnostics.is_empty() {
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                successor.as_ref(),
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                Some(graph),
                &diagnostics,
            );
            return Err(diagnostics);
        }

        let adoption = self.select_parse_for_presentation(
            &open.snapshot,
            successor.is_some(),
            open.successor_parse.clone(),
        );
        if let Err(errors) = adoption.into_result() {
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                successor.as_ref(),
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        let diagnostic_snapshot = self.publish_import_diagnostics(
            &open.snapshot,
            Some(open.context.clone()),
            Some(plan),
            ledger.clone(),
            open.accepted_reads.clone(),
            &diagnostics,
        );
        let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
            status: ImportDiscoveryRevisionStatus::ClosedValid,
            ledger,
            graph: Some(graph.clone()),
            diagnostics,
            diagnostic_snapshot: Some(diagnostic_snapshot),
            ..open
        });
        self.imports.record_discovery_attempt(artifact.clone());
        self.imports.open_discovery = None;
        // A committed close is a lineage boundary: additions recorded before it
        // belong to the closed graph, so the recorded-additions lineage resets.
        self.queries.revisioned.clear_lineage_additions();
        // Record the closed state for a possible trusted-toolchain continuation
        // (RUE-1112), but only when the canonical begin/frontier/publish
        // protocol produced a current import-input revision — legacy embedders
        // that bypass it get no continuation. The state retains everything the
        // successor verification needs (predecessor snapshot, context, accepted
        // reads, carried ledger) so the check is record-only, never filesystem.
        //
        // The closed state is deliberately NON-AUTHORIZING here (`attached_demands`
        // is `None`): a close by itself mints no token and authorizes no successor.
        // Only a subsequent rooted body-closure park attaches its exact missing-demand
        // set to this same state, so a later close whose attempt never parks can
        // never inherit an earlier park's authority.
        self.imports.continuation =
            self.queries
                .revisioned
                .current_import_revision()
                .map(|revision| {
                    self.imports.next_continuation_nonce += 1;
                    ContinuationState {
                        nonce: self.imports.next_continuation_nonce,
                        revision,
                        snapshot: artifact.snapshot().clone(),
                        accepted_reads: artifact.accepted_read_manifest().clone(),
                        ledger: artifact.ledger().clone(),
                        attached_demands: None,
                    }
                });
        self.queries.revisioned.commit_import_request();
        self.imports.import_request_checkpoint = None;
        Ok(artifact)
    }

    /// Mint the trusted-toolchain continuation token for the current successful
    /// import-discovery close, if one is outstanding AND authorizing (RUE-1112).
    /// A closed state becomes authorizing only once a rooted body-closure park
    /// has attached its exact missing-demand set; a close whose attempt is ready
    /// (or never parked) mints no token. The token is opaque and single-use; the
    /// host hands it back to [`Self::publish_trusted_toolchain_successor`].
    pub(crate) fn closed_discovery_continuation(&self) -> Option<ClosedDiscoveryContinuation> {
        self.imports
            .continuation
            .as_ref()
            .filter(|state| state.attached_demands.is_some())
            .map(|state| ClosedDiscoveryContinuation {
                session: self.identity.clone(),
                nonce: state.nonce,
                revision: state.revision,
            })
    }

    /// Publish exactly one strictly-additive trusted-toolchain successor on the
    /// continuation's closed revision (RUE-1112).
    ///
    /// The host has already done all filesystem work through the B4-hardened
    /// path — read each demanded module, checked containment/manifest/stable-read
    /// provenance, assembled the successor snapshot and accepted-read records.
    /// This verifies that work purely from records (no filesystem access) by
    /// diffing the successor against the continuation's predecessor, then
    /// publishes in the SAME request generation carrying the predecessor ledger
    /// unchanged. The added leaves carry no observation in the predecessor
    /// ledger yet; discovery of the `@import` edges they introduce (a trusted
    /// leaf such as `strbuf.rue` imports `option.rue`/`arraybuf.rue`/`rawbuf.rue`)
    /// is the driver's subsequent re-close, which roots its frontier only in
    /// these new leaves.
    ///
    /// Returns the successor revision together with the exact set of module IDs
    /// it appended (the verified `added == demanded` set). The re-close uses that
    /// set as the sole discovery frontier roots, so the predecessor import
    /// topology is never re-rooted or re-resolved.
    pub(crate) fn publish_trusted_toolchain_successor(
        &mut self,
        token: ClosedDiscoveryContinuation,
        issued_frontier: &crate::ImportDemandFrontier,
        successor: &SourceSnapshot,
        accepted_reads: crate::AcceptedReadManifest,
    ) -> Result<TrustedSuccessorDelta, CompileErrors> {
        let reject = |message: &str| {
            CompileErrors::from(crate::CompileError::without_span(
                rue_error::ErrorKind::InvalidCompilerInput(format!(
                    "trusted-toolchain successor rejected: {message}"
                )),
            ))
        };

        // Same session.
        if !Arc::ptr_eq(&token.session, &self.identity) {
            return Err(reject("continuation token belongs to a different session"));
        }
        // Token current + unused. Peek without consuming so a rejected batch
        // leaves the token valid for a corrected retry; only a successful publish
        // consumes it (a reused token then finds no outstanding state).
        let state = match self.imports.continuation.as_ref() {
            Some(state) if token.nonce == state.nonce && token.revision == state.revision => {
                state.clone()
            }
            Some(_) => {
                return Err(reject(
                    "continuation token is stale (superseded by a newer close or request)",
                ));
            }
            None => {
                return Err(reject(
                    "no outstanding closed-discovery continuation; the token was already used or invalidated",
                ));
            }
        };

        // The closure witness: the empty rooted frontier of the token's closed
        // revision. Only a genuinely-closed predecessor may continue.
        if issued_frontier.mode() != crate::ImportDemandMode::Rooted {
            return Err(reject("the closure witness frontier must be rooted"));
        }
        if issued_frontier.revision() != state.revision {
            return Err(reject(
                "the closure witness frontier does not belong to the continuation's revision",
            ));
        }
        if !issued_frontier.requests().is_empty() {
            return Err(reject(
                "the closure witness frontier is not empty; the predecessor did not close",
            ));
        }

        // Same compilation root (the context/read policy is carried unchanged
        // into the successor below).
        if successor.source_revision().root() != state.snapshot.source_revision().root() {
            return Err(reject("the successor changed the compilation root"));
        }

        // Strict additive source evolution: every predecessor module revision
        // must appear byte-identical in the successor; the additions are exactly
        // the new leaves.
        let old_modules: std::collections::BTreeSet<&crate::ModuleRevision> =
            state.snapshot.source_revision().modules().iter().collect();
        let new_modules: std::collections::BTreeSet<&crate::ModuleRevision> =
            successor.source_revision().modules().iter().collect();
        if !old_modules.is_subset(&new_modules) {
            return Err(reject(
                "a predecessor module revision was mutated or removed (source evolution must be strictly additive)",
            ));
        }
        let additions: Vec<&crate::ModuleRevision> =
            new_modules.difference(&old_modules).copied().collect();
        if additions.is_empty() {
            return Err(reject(
                "a trusted-toolchain successor must add at least one leaf",
            ));
        }

        // Every predecessor accepted-read entry must appear byte-identical in the
        // successor manifest (altered old provenance rejected).
        let new_reads: AHashSet<&crate::AcceptedReadManifestEntry> =
            accepted_reads.iter().collect();
        for old in state.accepted_reads.iter() {
            if !new_reads.contains(old) {
                return Err(reject(
                    "a predecessor accepted-read provenance entry was altered or removed",
                ));
            }
        }

        // Demand authority lives only in the attached park set. A close whose
        // rooted attempt never parked is non-authorizing: it may not consume the
        // token or admit any leaf, so a later ready close can never reuse an
        // earlier park's demands.
        let Some(attached_demands) = state.attached_demands.as_ref() else {
            return Err(reject(
                "the closed continuation is not authorizing; no rooted body-closure park has attached a demanded-module set",
            ));
        };

        // Every addition is a trusted standard-library leaf with well-formed
        // accepted-read provenance in the successor manifest.
        for addition in &additions {
            if !addition.module.is_trusted_standard_library() {
                return Err(reject(
                    "an added leaf is not a trusted standard-library module",
                ));
            }
            if !accepted_reads
                .iter()
                .any(|entry| entry.module() == &addition.module)
            {
                return Err(reject(
                    "an added trusted leaf has no accepted-read provenance",
                ));
            }
        }

        // The successor's added module-ID set must EQUAL the park's demanded
        // missing set — set equality, not per-member membership. This enforces
        // the one-park/one-batched-successor contract in both directions: an
        // arbitrary or uninvited module (added ⊄ demanded) is rejected, and a
        // partial batch that omits a demanded member (demanded ⊄ added) is
        // rejected WITHOUT consuming the single-use token (the peek above only
        // consumes on the successful publish below).
        let demanded: std::collections::BTreeSet<crate::ModuleId> = attached_demands
            .iter()
            .map(|demand| demand.trusted_module_id())
            .collect::<Result<_, _>>()
            .map_err(CompileErrors::from)?;
        let added: std::collections::BTreeSet<crate::ModuleId> = additions
            .iter()
            .map(|addition| addition.module.clone())
            .collect();
        if added != demanded {
            return Err(reject(
                "the successor's added trusted modules must equal the rooted park's demanded missing set exactly (one park, one batched successor)",
            ));
        }

        // Publish the strictly-additive successor in the SAME request generation
        // as a sparse overlay over the predecessor view: the carried ledger and
        // topology are inherited unchanged, only the verified added leaves'
        // source/provenance leaves are published, and the overlay re-derives the
        // additions from the published parent view (they must equal `added`).
        let checkpoint = self.capture_import_request_checkpoint();
        assert!(
            self.imports.import_request_checkpoint.is_none(),
            "a committed predecessor must clear its import-request checkpoint before successor publication"
        );
        let published = self
            .queries
            .revisioned
            .publish_trusted_successor_view(
                state.revision,
                successor,
                accepted_reads,
                state.ledger.clone(),
                &added,
                state.revision.frontier_round + 1,
            )
            .map_err(CompileErrors::from)?;
        // Successor publication mutates the selected import view before the
        // host's trusted re-close can commit. Preserve the exact committed
        // predecessor selectors so any failed or superseded re-close can roll
        // the overlay back without letting a later request checkpoint it as
        // committed state (RUE-1862/RUE-1863).
        self.imports.import_request_checkpoint = Some(checkpoint);
        // Consume the single-use continuation only on success.
        self.imports.continuation = None;
        // Mint the opaque successor-delta authority from the VERIFIED `added`
        // set (equal to the park's demanded missing set). `BTreeSet` iteration is
        // sorted, so the appended roots are deterministic. The host receives only
        // this opaque value; it cannot inspect or edit the module identities.
        let appended: Arc<[crate::ModuleId]> = added.into_iter().collect::<Vec<_>>().into();
        self.imports.next_continuation_nonce += 1;
        let nonce = self.imports.next_continuation_nonce;
        self.imports.successor_delta_nonce = Some(nonce);
        Ok(TrustedSuccessorDelta {
            session: self.identity.clone(),
            nonce,
            revision: published,
            appended,
        })
    }

    fn publish_failed_import_attempt(
        &mut self,
        open: ImportDiscoveryRevisionArtifact,
        plan: crate::ImportDiscoveryPlan,
        ledger: crate::ImportObservationLedger,
        _successor: Option<&(crate::ImportInputRevision, Arc<[crate::ModuleRevision]>)>,
        status: ImportDiscoveryRevisionStatus,
        graph: Option<Arc<CanonicalImportGraphOutput>>,
        errors: &CompileErrors,
    ) -> Arc<ImportDiscoveryRevisionArtifact> {
        debug_assert_ne!(status, ImportDiscoveryRevisionStatus::ClosedValid);
        let diagnostic_snapshot = self.publish_import_diagnostics(
            &open.snapshot,
            Some(open.context.clone()),
            Some(plan),
            ledger.clone(),
            open.accepted_reads.clone(),
            errors,
        );
        let artifact = Arc::new(ImportDiscoveryRevisionArtifact {
            status,
            ledger,
            graph,
            diagnostics: errors.clone(),
            diagnostic_snapshot: Some(diagnostic_snapshot),
            ..open
        });
        self.imports.record_discovery_attempt(artifact.clone());
        self.imports.open_discovery = None;
        artifact
    }

    pub(super) fn require_closed_discovery(&self) -> Result<(), CompileErrors> {
        if self
            .discovery_attempt_artifact()
            .is_some_and(|attempt| attempt.status != ImportDiscoveryRevisionStatus::ClosedValid)
        {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "semantic and dependency queries require a closed valid discovery revision"
                        .into(),
                ),
            )));
        }
        Ok(())
    }

    /// Revoke protocol capabilities that were issued for a source view which
    /// an ordinary parse update is about to replace. Keeping this mutation in
    /// the discovery owner prevents artifact projection code from reaching
    /// into the import lifecycle's authority state.
    pub(super) fn invalidate_import_successor_authority(&mut self) {
        self.imports.continuation = None;
        self.imports.successor_delta_nonce = None;
    }

    pub(super) fn inject_stale_import_query_for_oracle(&mut self) -> bool {
        let Some(stale) = self.imports.prior_discovery.clone() else {
            return false;
        };
        let Some(current) = self.imports.discovery_attempt.as_ref() else {
            return false;
        };
        if stale.source_revision() == current.source_revision() {
            return false;
        }
        self.imports.oracle_import_fault = Some(stale);
        true
    }

    /// Drop an open discovery attempt when parsing republishes any source view
    /// other than the exact snapshot that attempt owns.
    pub(super) fn retain_open_discovery_for_exact_snapshot(&mut self, snapshot: &SourceSnapshot) {
        if self
            .imports
            .open_discovery
            .as_deref()
            .is_some_and(|artifact| !artifact.snapshot.is_same_exact_snapshot(snapshot))
        {
            self.imports.open_discovery = None;
        }
    }

    /// Discard protocol-only state from a superseded filesystem observation.
    /// Immutable query terminals may remain retained, but no open attempt is
    /// allowed to shadow the last closed-valid discovery selected for public
    /// semantic queries.
    pub(crate) fn abort_import_input_request(&mut self) -> crate::CompileResult<()> {
        let committed = self.last_good_discovery_artifact().map(|artifact| {
            (
                artifact.input_revision(),
                artifact.snapshot().clone(),
                artifact.accepted_read_manifest().clone(),
                artifact.ledger().clone(),
            )
        });
        self.imports.open_discovery = None;
        self.imports.successor_delta_nonce = None;
        self.queries
            .revisioned
            .restore_import_revision_after_abort(
                committed.as_ref().and_then(|(revision, _, _, _)| *revision),
            )?;
        if let Some(checkpoint) = self.imports.import_request_checkpoint.take() {
            self.imports.validated_accepted_reads = checkpoint.validated_accepted_reads;
            self.imports.continuation = checkpoint.continuation;
            self.imports.discovery_attempt = checkpoint.discovery_attempt;
            self.imports.prior_discovery = checkpoint.prior_discovery;
            self.batch_diagnostic_order = checkpoint.batch_diagnostic_order;
            self.diagnostics = checkpoint.diagnostics;
            // A fresh rooted import request invalidates a provisional trusted
            // successor delta permanently. Restoring only its nonce after the
            // revisioned database has reselected the committed predecessor
            // would manufacture a live-looking capability whose exact overlay
            // revision and lineage no longer exist.
            self.imports.successor_delta_nonce = None;
            self.refresh_retention_metrics();
            return Ok(());
        }
        self.imports.validated_accepted_reads = committed
            .as_ref()
            .map(|(_, snapshot, accepted_reads, _)| (snapshot.clone(), accepted_reads.clone()));
        self.imports.continuation =
            committed.and_then(|(revision, snapshot, accepted_reads, ledger)| {
                revision.map(|revision| {
                    self.imports.next_continuation_nonce += 1;
                    ContinuationState {
                        nonce: self.imports.next_continuation_nonce,
                        revision,
                        snapshot,
                        accepted_reads,
                        ledger,
                        attached_demands: None,
                    }
                })
            });
        Ok(())
    }

    pub(super) fn attach_toolchain_park(&mut self, park: &crate::ParkedToolchainModules) {
        // Atomically attach this rooted park's exact sorted missing-demand set
        // to the outstanding closed continuation, making it authorizing
        // (RUE-1112).
        if let Some(state) = self.imports.continuation.as_mut() {
            let mut demands = park.demands().to_vec();
            demands.sort();
            demands.dedup();
            state.attached_demands = Some(Arc::from(demands));
        }
    }
}

fn validate_accepted_read_manifest(
    snapshot: &SourceSnapshot,
    accepted_reads: &crate::AcceptedReadManifest,
) -> Result<(), CompileErrors> {
    if accepted_reads.len() != snapshot.len() {
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::InvalidCompilerInput(
                "accepted read manifest does not cover the staging source snapshot".into(),
            ),
        )));
    }
    let entries = accepted_reads
        .iter()
        .map(|entry| (entry.module(), entry))
        .collect::<BTreeMap<_, _>>();
    if entries.len() != accepted_reads.len() {
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::InvalidCompilerInput(
                "accepted read manifest contains duplicate logical modules".into(),
            ),
        )));
    }
    for source in snapshot.files() {
        let module = snapshot
            .module_id(source.file_id)
            .expect("snapshot files have logical module IDs");
        let Some(entry) = entries.get(module) else {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(format!(
                    "accepted read manifest is missing logical module {module}"
                )),
            )));
        };
        if entry.content_fingerprint() != crate::import_discovery::source_fingerprint(source.source)
        {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(format!(
                    "accepted read manifest content does not match logical module {module}"
                )),
            )));
        }
    }
    Ok(())
}

/// The appended half of [`validate_accepted_read_manifest`], for a snapshot and
/// manifest that both directly extend an already validated pair. It proves the
/// same properties over the appended entries: the manifest covers the snapshot
/// exactly, names no module twice, and carries each source's exact content
/// fingerprint. Failure messages match the whole-pair check, because a caller
/// cannot observe which half proved the disagreement.
fn validate_appended_accepted_reads(
    snapshot: &SourceSnapshot,
    accepted_reads: &crate::AcceptedReadManifest,
    previous_reads: &crate::AcceptedReadManifest,
    appended_files: &[crate::FileId],
    appended_entries: &[crate::AcceptedReadManifestEntry],
) -> Result<(), CompileErrors> {
    let reject = |message: String| {
        Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::InvalidCompilerInput(message),
        )))
    };
    if accepted_reads.len() != snapshot.len() {
        return reject("accepted read manifest does not cover the staging source snapshot".into());
    }
    if appended_entries.len() != appended_files.len() {
        // Equal totals over equal prefixes make this unreachable; a pair that
        // reaches it is not the shape this delta check reasons about, so it is
        // proved by the whole-pair check rather than diagnosed from here.
        return validate_accepted_read_manifest(snapshot, accepted_reads);
    }
    for entry in appended_entries {
        if previous_reads.find_module(entry.module()).is_some() {
            return reject("accepted read manifest contains duplicate logical modules".into());
        }
    }
    for file_id in appended_files {
        let module = snapshot
            .module_id(*file_id)
            .expect("snapshot files have logical module IDs");
        let Ok(index) = appended_entries.binary_search_by(|entry| entry.module().cmp(module))
        else {
            return reject(format!(
                "accepted read manifest is missing logical module {module}"
            ));
        };
        let source = snapshot
            .source(*file_id)
            .expect("snapshot files retain their source text");
        if appended_entries[index].content_fingerprint()
            != crate::import_discovery::source_fingerprint(source.source)
        {
            return reject(format!(
                "accepted read manifest content does not match logical module {module}"
            ));
        }
    }
    Ok(())
}

/// Successive fixed-point snapshots belong to one bounded discovery parse run
/// only when they preserve all prior source, manifest, context, and ledger
/// provenance. Any failed/closed/superseding attempt starts fresh accounting.
fn continues_discovery_lifecycle(
    previous: &ImportDiscoveryRevisionArtifact,
    snapshot: &SourceSnapshot,
    context: &crate::ImportDiscoveryContext,
    accepted_reads: &crate::AcceptedReadManifest,
    carried_ledger: &crate::ImportObservationLedger,
) -> bool {
    if previous.status != ImportDiscoveryRevisionStatus::Open || previous.context != *context {
        return false;
    }
    let Some(program) = previous.program.as_deref() else {
        return false;
    };
    if program.root() != snapshot.source_revision().root()
        || !program.modules_iter().all(|module| {
            let file_id = module.file_id();
            snapshot.module_id(file_id) == Some(module.module_id())
                && snapshot.source_id(file_id) == Some(module.source_id())
                && snapshot.metadata().physical_path(file_id) == Some(module.physical_path())
        })
    {
        return false;
    }
    if !previous
        .accepted_reads
        .iter()
        .all(|entry| accepted_reads.contains_entry(entry))
    {
        return false;
    }
    previous
        .ledger
        .iter()
        .all(|observation| carried_ledger.get(observation.request()) == Some(observation))
}

#[cfg(test)]
mod tests;
