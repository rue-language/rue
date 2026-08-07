//! In-process canonical parse, merge, and RIR query orchestration.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use rue_air::{DeclarationBindingWork, SemanticBindingManifestWork};

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalImportGraph, CanonicalImportGraphValidation,
    CanonicalMergeWork, CanonicalMergedProgram, CanonicalRirOutput, CanonicalRirWork,
    CanonicalSemanticOutput, CanonicalSemanticWork, CodegenInputDescriptor, CompileError,
    CompileErrors, CompileOptions, CompileWarning, ErrorKind, ModuleResolutionInputs,
    ParseInvalidationSummary, ParsedModulesWork, SemanticInputDescriptor, SourceRevision,
    SourceSnapshot, StablePreviewFeatures,
    canonical_lower::project_module_rirs_with_work,
    canonical_merge::merge_parsed_modules_reusing_indexes,
    canonical_semantic::{
        CanonicalSemanticFailure, analyze_prepared_canonical_program_reusing_declarations,
        prepare_query_declaration_shells,
    },
    parsed_modules::{ParsedProgram, classify_invalidation},
    validate_canonical_import_graph,
};

pub(crate) use crate::diagnostic_attempt_store::FRONTEND_DIAGNOSTIC_RETENTION_LIMIT;
use crate::diagnostic_attempt_store::{
    DiagnosticAttemptProvenance, DiagnosticAttemptStore, FrontendDiagnosticIdentity,
    FrontendDiagnosticSnapshot, ImportDiagnosticInputDescriptor,
};
use crate::retained_charge::RetainedCharge;
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as QueryAttemptExecution, AttemptOutcomeKind, AttemptView,
    QUERY_TERMINAL_RETENTION_LIMIT, TerminalKind, TypedQueryFamily,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrontendQueryWork {
    pub calls: usize,
    pub executions: usize,
    pub reuses: usize,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerSessionWork {
    pub updates: usize,
    pub last_parse: ParsedModulesWork,
    pub last_invalidation: ParseInvalidationSummary,
    pub imports: FrontendQueryWork,
    pub import_diagnostics: FrontendQueryWork,
    pub merge: FrontendQueryWork,
    pub rir: FrontendQueryWork,
    pub downstream_invalidations: usize,
    pub last_merge: CanonicalMergeWork,
    pub last_rir: CanonicalRirWork,
    pub semantic: FrontendQueryWork,
    pub semantic_entries: usize,
    pub semantic_entries_invalidated: usize,
    pub semantic_records: Vec<SemanticQueryRecord>,
    pub definitions: FrontendQueryWork,
    pub definition_entries: usize,
    pub definition_entries_invalidated: usize,
    pub definition_records: Vec<DefinitionQueryRecord>,
    pub diagnostic_publications: usize,
    pub diagnostic_reuses: usize,
    pub diagnostic_invalidations: usize,
    pub declaration_reuse_plans: usize,
    pub durable_records_compared: usize,
    pub durable_records_reused: usize,
    pub ordinary_declaration_resolutions_skipped: usize,
    pub durable_installs: usize,
    pub declaration_reuse_fallbacks: usize,
    /// Current bounded-retention gauges for long-lived service integrations.
    pub retention: FrontendRetentionMetrics,
}

trait SessionQueryMetricsFamily {
    const NAME: &'static str;
    fn projection(work: &mut CompilerSessionWork) -> &mut FrontendQueryWork;
}

#[derive(Debug)]
struct ImportsMetricsQuery;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AttemptId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueryStructuralWork {
    None,
    Parse(ParsedModulesWork),
    Merge(CanonicalMergeWork),
    Rir(CanonicalRirWork),
    Semantic(Box<SemanticQueryRecord>),
    Definition(Box<DefinitionQueryRecord>),
}

#[derive(Debug, Clone)]
struct IndexedAttempt {
    family: &'static str,
    attempt: Arc<dyn AttemptView>,
}

const QUERY_ATTEMPT_RETENTION_LIMIT: usize = 256;

#[derive(Debug, Default)]
struct QueryAttemptIndex {
    next_id: u64,
    retained: VecDeque<IndexedAttempt>,
    pinned_origins: BTreeSet<AttemptId>,
    evicted_projection: BTreeMap<&'static str, FrontendQueryWork>,
    projections: BTreeMap<&'static str, fn(&mut CompilerSessionWork) -> &mut FrontendQueryWork>,
}

impl QueryAttemptIndex {
    fn allocate(&mut self) -> AttemptId {
        let id = AttemptId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    fn index(&mut self, family: &'static str, attempt: Arc<dyn AttemptView>) {
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

fn project_lifecycle(work: &mut FrontendQueryWork, attempt: &dyn AttemptView) {
    let _ = (
        attempt.outcome(),
        attempt.runtime_observations(),
        attempt.runtime_work(),
        attempt.work(),
    );
    work.calls += 1;
    match attempt.execution() {
        QueryAttemptExecution::Computed => work.executions += 1,
        QueryAttemptExecution::Reused | QueryAttemptExecution::Adopted => work.reuses += 1,
        QueryAttemptExecution::Rejected => {}
    }
}

/// A production query boundary. It owns work independently of the session
/// borrow and publishes a canceled record if computation unwinds or returns
/// before an explicit terminal is frozen.
struct QueryComputationGuard {
    sink: Arc<Mutex<QueryAttemptIndex>>,
    id: AttemptId,
    family: &'static str,
    attempt: Option<Arc<dyn AttemptView>>,
    diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    structural: QueryStructuralWork,
    cancel_requested: bool,
}

/// Lifecycle-only attempt retained for canonical phases that publish directly
/// into the revisioned runtime rather than a compatibility typed-query store.
#[derive(Debug)]
struct InstrumentedQueryAttempt {
    id: AttemptId,
    execution: QueryAttemptExecution,
    outcome: AttemptOutcomeKind,
    diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    work: QueryStructuralWork,
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
    fn started(&mut self) {}

    fn accrue(&mut self, structural: QueryStructuralWork) {
        self.structural = structural;
    }

    fn bind(&mut self, attempt: Arc<dyn AttemptView>) {
        self.attempt = Some(attempt);
    }

    fn attach_diagnostics(&mut self, diagnostics: Arc<FrontendDiagnosticSnapshot>) {
        self.diagnostics = Some(diagnostics);
    }

    fn request_cancel(&mut self) {
        self.cancel_requested = true;
    }

    fn finish<T, E>(
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
struct CompilerSessionMetrics {
    attempts: Arc<Mutex<QueryAttemptIndex>>,
    projected_attempts: BTreeSet<AttemptId>,
    aggregate: CompilerSessionWork,
    projected_semantic_invalidations: usize,
    projected_definition_invalidations: usize,
}

impl CompilerSessionMetrics {
    fn work(&self) -> &CompilerSessionWork {
        &self.aggregate
    }

    fn begin<Q: SessionQueryMetricsFamily>(&self) -> QueryComputationGuard {
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

    fn begin_unprojected(&self, family: &'static str) -> QueryComputationGuard {
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

    fn set_pinned_origins(&self, origins: BTreeSet<AttemptId>) {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pinned_origins = origins;
    }

    fn synchronize(&mut self) {
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

    fn project_structural_attempt(&mut self, attempt: &dyn AttemptView) {
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
            QueryStructuralWork::Semantic(record) => {
                let reuse = record.work.declaration_reuse;
                self.aggregate.declaration_reuse_plans += reuse.plan_executions;
                self.aggregate.durable_records_compared += reuse.durable_records_compared;
                if !record.failed {
                    self.aggregate.durable_records_reused += reuse.durable_records_reused;
                    self.aggregate.ordinary_declaration_resolutions_skipped +=
                        reuse.ordinary_declaration_resolutions_skipped;
                    self.aggregate.durable_installs += reuse.install_invocations;
                    self.aggregate.declaration_reuse_fallbacks += reuse.fallbacks;
                }
                self.aggregate.semantic_records.push((**record).clone());
            }
            QueryStructuralWork::Definition(record) => {
                self.aggregate.definition_records.push((**record).clone());
            }
            QueryStructuralWork::Merge(_) | QueryStructuralWork::Rir(_) => {}
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn attempts(&self) -> Vec<IndexedAttempt> {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retained
            .iter()
            .cloned()
            .collect()
    }

    fn update(&mut self, parse: ParsedModulesWork, invalidation: ParseInvalidationSummary) {
        self.aggregate.updates += 1;
        self.aggregate.last_parse = parse;
        self.aggregate.last_invalidation = invalidation;
    }

    fn project_dependency_invalidations(&mut self, changed_existing_revision: bool) {
        if changed_existing_revision {
            self.aggregate.downstream_invalidations += 1;
        }
        self.projected_semantic_invalidations = 0;
        self.projected_definition_invalidations = 0;
        self.aggregate.last_merge = CanonicalMergeWork::default();
        self.aggregate.last_rir = CanonicalRirWork::default();
        self.aggregate.semantic_entries = 0;
        self.aggregate.semantic_records.clear();
        self.aggregate.definition_entries = 0;
        self.aggregate.definition_records.clear();
    }

    fn publish_semantic(&mut self, retained_entries: usize) {
        self.aggregate.semantic_entries = retained_entries;
    }

    fn publish_definition(&mut self, retained_entries: usize) {
        self.aggregate.definition_entries = retained_entries;
    }

    fn diagnostic_publication(&mut self, invalidated_previous: bool) {
        if invalidated_previous {
            self.aggregate.diagnostic_invalidations += 1;
        }
        self.aggregate.diagnostic_publications += 1;
    }

    fn diagnostic_reuse(&mut self) {
        self.aggregate.diagnostic_reuses += 1;
    }

    fn set_retention(&mut self, retention: FrontendRetentionMetrics) {
        self.aggregate.retention = retention;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ImportGraphInputDescriptor {
    pub(crate) sources: SourceRevision,
    pub(crate) resolution: ModuleResolutionInputs,
    pub(crate) std_dir: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct CanonicalImportGraphOutput {
    input: ImportGraphInputDescriptor,
    graph: CanonicalImportGraph,
    validation: CanonicalImportGraphValidation,
}

impl CanonicalImportGraphOutput {
    pub(crate) fn input(&self) -> &ImportGraphInputDescriptor {
        &self.input
    }
    pub fn graph(&self) -> &CanonicalImportGraph {
        &self.graph
    }
    pub fn validation(&self) -> &CanonicalImportGraphValidation {
        &self.validation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticQueryRecord {
    pub input: CodegenInputDescriptor,
    pub work: CanonicalSemanticWork,
    pub failure: Option<crate::CanonicalSemanticFailureWork>,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionQueryRecord {
    pub input: SemanticInputDescriptor,
    pub binding: DeclarationBindingWork,
    pub manifest: SemanticBindingManifestWork,
    pub issuance: BoundDefinitionWork,
    pub failed: bool,
}

pub struct CompilerSessionUpdate {
    result: Result<Arc<ParsedProgram>, CompileErrors>,
    work: ParsedModulesWork,
    #[cfg(test)]
    invalidation: ParseInvalidationSummary,
    downstream_invalidated: bool,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

impl std::fmt::Debug for CompilerSessionUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompilerSessionUpdate")
            .field("successful", &self.result.is_ok())
            .field("downstream_invalidated", &self.downstream_invalidated)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportDiscoveryRevisionStatus {
    Open,
    ClosedAttempted,
    ClosedValid,
}

#[derive(Debug, Clone)]
pub(crate) struct ImportDiscoveryRevisionArtifact {
    status: ImportDiscoveryRevisionStatus,
    source_revision: SourceRevision,
    context: crate::ImportDiscoveryContext,
    snapshot: SourceSnapshot,
    program: Option<Arc<ParsedProgram>>,
    parse_work: ParsedModulesWork,
    plan: Option<crate::ImportDiscoveryPlan>,
    ledger: crate::ImportObservationLedger,
    accepted_reads: crate::AcceptedReadManifest,
    graph: Option<Arc<CanonicalImportGraphOutput>>,
    diagnostics: CompileErrors,
    diagnostic_snapshot: Option<Arc<FrontendDiagnosticSnapshot>>,
    /// The exact successor parse record this stage computed (RUE-1112). The
    /// successor close adopts by re-selecting THIS terminal — same key, same
    /// revision — never by re-deriving an extension against the now-selected
    /// successor state.
    successor_parse: Option<ParseQueryRecord>,
}

impl ImportDiscoveryRevisionArtifact {
    pub(crate) fn status(&self) -> ImportDiscoveryRevisionStatus {
        self.status
    }
    pub(crate) fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }

    #[cfg(test)]
    pub(crate) fn parse_work(&self) -> ParsedModulesWork {
        self.parse_work
    }
    pub(crate) fn context(&self) -> &crate::ImportDiscoveryContext {
        &self.context
    }
    pub(crate) fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }
    #[cfg(test)]
    pub(crate) fn program(&self) -> Option<&Arc<ParsedProgram>> {
        self.program.as_ref()
    }
    /// Exact parse work performed by this bounded discovery lifecycle.
    #[cfg(test)]
    pub(crate) fn plan(&self) -> Option<&crate::ImportDiscoveryPlan> {
        self.plan.as_ref()
    }
    pub(crate) fn ledger(&self) -> &crate::ImportObservationLedger {
        &self.ledger
    }
    pub(crate) fn accepted_read_manifest(&self) -> &crate::AcceptedReadManifest {
        &self.accepted_reads
    }
    pub(crate) fn graph(&self) -> Option<&Arc<CanonicalImportGraphOutput>> {
        self.graph.as_ref()
    }
    pub(crate) fn diagnostics(&self) -> &CompileErrors {
        &self.diagnostics
    }
    /// Revision-labeled canonical diagnostic batch for this attempted import
    /// revision. An ordinary open iteration has none; an I/O/policy failure
    /// publishes its batch without pretending that a graph closed.
    pub(crate) fn diagnostic_snapshot(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostic_snapshot.as_ref()
    }
}

impl CompilerSessionUpdate {
    pub fn result(&self) -> Result<crate::SyntaxView, &CompileErrors> {
        self.result
            .as_ref()
            .map(|owner| crate::SyntaxView::new(owner.clone()))
    }
    pub fn into_result(self) -> Result<crate::SyntaxView, CompileErrors> {
        self.result.map(crate::SyntaxView::new)
    }
    #[cfg(test)]
    pub(crate) fn result_owner(&self) -> Result<&Arc<ParsedProgram>, &CompileErrors> {
        self.result.as_ref()
    }
    #[cfg(test)]
    pub(crate) fn into_owner_result(self) -> Result<Arc<ParsedProgram>, CompileErrors> {
        self.result
    }
    #[cfg(test)]
    pub(crate) fn work(&self) -> ParsedModulesWork {
        self.work
    }
    /// Return the explicitly unstable parse-work metrics for this update.
    pub fn unstable_metrics(&self) -> crate::unstable::ParseMetrics {
        crate::unstable::ParseMetrics::from_work(self.work)
    }
    #[cfg(test)]
    pub(crate) fn invalidation(&self) -> &ParseInvalidationSummary {
        &self.invalidation
    }
    pub fn downstream_invalidated(&self) -> bool {
        self.downstream_invalidated
    }
    pub fn diagnostics(&self) -> &Arc<FrontendDiagnosticSnapshot> {
        &self.diagnostics
    }
}

#[derive(Debug, Default)]
pub struct CompilerSession {
    identity: Arc<()>,
    /// Test-only differential-oracle perturbation requested through the
    /// unstable test bridge. It corrupts a canonical projection at the next
    /// observation point without reviving a retired selected-result store.
    oracle_fault: Option<crate::unstable::DifferentialOracleFault>,
    /// Protocol context only while the typed import-closure query is open.
    /// Closed attempts live exclusively in their plan or closure terminal.
    open_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    /// Trusted-toolchain continuation state (RUE-1112). Set only at a
    /// successful import-discovery close and single-use: consumed by a
    /// successful `publish_trusted_toolchain_successor`, and cleared on any new
    /// import-input request or source update so a stale token cannot continue.
    continuation: Option<ContinuationState>,
    /// Nonce of the outstanding trusted-toolchain successor delta authority
    /// (RUE-1112), set by a successful `publish_trusted_toolchain_successor` and
    /// cleared by the successor close it authorizes (or by any new import-input
    /// request/source update, so a stale delta can neither stage nor close).
    successor_delta_nonce: Option<u64>,
    /// Monotonic nonce source for continuation tokens; a token is valid only
    /// while its nonce matches the outstanding state.
    next_continuation_nonce: u64,
    /// Cumulative request groups constructed during import-plan staging
    /// (RUE-1112). A full plan build constructs one per program occurrence; a
    /// trusted-toolchain successor stage reuses the predecessor plan's groups and
    /// constructs only the newly appended occurrences', so a predecessor
    /// occurrence contributes here exactly once — at the initial close.
    import_plan_groups_constructed: u64,
    /// Cumulative canonical import records reduced and validated during
    /// close (RUE-1112). A full close reduces/validates one per program
    /// occurrence; a trusted-toolchain successor close carries the predecessor's
    /// closed graph and reduces/validates only the newly appended occurrences', so
    /// a predecessor occurrence contributes here exactly once — at the initial
    /// close.
    import_close_records_reduced: u64,
    /// Cumulative source entries materialized into whole-program parse
    /// projections (RUE-1112): the presentation order, demanded module set, and
    /// merged program construction a FULL parse build enumerates. A
    /// trusted-toolchain successor stage extends the retained predecessor
    /// artifact instead, so a predecessor entry contributes here exactly once —
    /// at the initial close.
    parse_sources_materialized: u64,
    /// Cumulative source entries embedded in parse query keys (RUE-1112): an
    /// ordinary key carries every file's exact content identity; a successor
    /// key carries only the published lineage identity plus its appended
    /// segment, so key hashing and equality never touch a predecessor entry.
    parse_key_entries_compared: u64,
    /// Cumulative module parse queries dispatched by the parse projection
    /// (RUE-1112). A full build dispatches one per module; a successor stage
    /// dispatches only the appended modules'.
    parse_modules_dispatched: u64,
    /// Cumulative entries examined by parse invalidation classification
    /// (RUE-1112). A full classification examines every current module; a
    /// successor classifies only its appended delta.
    parse_invalidation_entries_compared: u64,
    queries: FrontendQueryDatabase,
    #[cfg(test)]
    rooted_cfg_executions: Vec<(crate::FunctionInstanceKey, rue_query::RequestExecution)>,
    #[cfg(test)]
    warning_reference_executions: Vec<(crate::StableDefinitionKey, rue_query::RequestExecution)>,
    #[cfg(test)]
    codegen_executions: Vec<(crate::FunctionInstanceKey, rue_query::RequestExecution)>,
    #[cfg(test)]
    codegen_attempt_work: Vec<(crate::FunctionInstanceKey, Vec<(std::sync::Arc<str>, u64)>)>,
    #[cfg(test)]
    codegen_collections: usize,
    /// One-shot cancellation injections, each consumed with `mem::take` at a
    /// fixed point inside an attempt.
    ///
    /// Test-only because production cancellation arrives asynchronously through
    /// a `CancellationToken`, and a test cannot deterministically land it
    /// between two chosen steps. They gate no selection logic: the branch each
    /// one triggers is the same cancellation path a real token drives, so both
    /// configurations still compile one implementation of that path (RUE-1143).
    #[cfg(test)]
    cancel_merge_before_commit: bool,
    #[cfg(test)]
    cancel_semantic_after_dependency: bool,
    #[cfg(test)]
    cancel_semantic_before_publication: bool,
    published: Option<Arc<ParsedProgram>>,
    published_snapshot: Option<SourceSnapshot>,
    batch_diagnostic_order: Option<crate::shared_segments::SharedList<crate::ModuleId>>,
    definition_shard_baseline: Option<crate::DefinitionSnapshot>,
    metrics: CompilerSessionMetrics,
    diagnostics: DiagnosticAttemptStore,
}

#[cfg(test)]
fn stable_type_definition_root(
    value: &crate::TypeInstanceKey,
) -> Option<&crate::StableDefinitionKey> {
    use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
    match value {
        T::Nominal(N::Named(value)) => Some(value),
        T::Nominal(N::Anonymous(value)) => stable_producer_definition_root(&value.producer),
        T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
            stable_type_definition_root(element)
        }
        _ => None,
    }
}

#[cfg(test)]
fn stable_function_definition_root(
    value: &crate::FunctionInstanceKey,
) -> Option<&crate::StableDefinitionKey> {
    use crate::FunctionInstanceKey as F;
    match value {
        F::Definition(value) => Some(value),
        F::Specialization { base, .. } => stable_function_definition_root(base),
        F::AnonymousMember { owner, .. } | F::DropGlue(owner) => stable_type_definition_root(owner),
    }
}

#[cfg(test)]
fn stable_producer_definition_root(
    producer: &crate::StableProducerId,
) -> Option<&crate::StableDefinitionKey> {
    match producer {
        crate::StableProducerId::Definition(definition) => Some(definition),
        crate::StableProducerId::Function(function) => stable_function_definition_root(function),
    }
}

#[derive(Debug)]
enum SemanticRequestControl {
    Compile(CompileErrors),
    Abort(rue_query::QueryAbort),
    /// A reached body demanded a trusted toolchain module absent from the current
    /// revision (RUE-1112). The rooted attempt recorded the park before
    /// entering the body transaction; the host driver acquires the demanded
    /// modules and retries on a successor, while stable no-filesystem entries
    /// convert the park to an error/absence result at their outer boundary.
    Parked(Box<crate::ParkedToolchainModules>),
}

/// The outcome of the rooted, park-aware semantic entry
/// [`CompilerSession::semantic_or_toolchain_park`] (RUE-1112), consumed by the
/// host source-loading driver through the unstable facade.
pub enum SemanticParkOutcome {
    /// Analysis completed against a revision that satisfied every reached body's
    /// trusted-toolchain-module demand.
    Ready(Arc<crate::SemanticView>),
    /// Analysis produced deterministic program diagnostics.
    Errors(CompileErrors),
    /// A reached body demanded a trusted toolchain module absent from the current
    /// revision. The host driver must acquire the demanded modules, publish a
    /// successor, and retry.
    Parked(Box<crate::ParkedToolchainModules>),
}

/// Park-aware result for the production body-closure root. Unlike
/// [`SemanticParkOutcome`], success carries no recomposed program semantic
/// value: normal compilation only needs the query-owned reached terminals.
pub enum RootedParkOutcome {
    Ready,
    Errors(CompileErrors),
    Parked(Box<crate::ParkedToolchainModules>),
}

#[derive(Debug, Clone)]
pub(crate) struct RootedCfgUnit {
    pub(crate) function: crate::FunctionInstanceKey,
    pub(crate) optimized_cfg_key: crate::cfg_query::OptimizedCfgQueryKey,
    pub(crate) record: Arc<crate::cfg_query::CfgRecord>,
    #[allow(dead_code)]
    pub(crate) body_span: rue_span::Span,
}

#[derive(Debug)]
pub(crate) struct RootedCfgOutput {
    graph: RootedBodyGraph,
    pub(crate) cfgs: Vec<RootedCfgUnit>,
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) work: crate::CanonicalSemanticWork,
    backend_root: crate::revisioned_query_database::BackendRootCandidate,
}

pub(crate) struct RootedCodegenOutput {
    pub(crate) units: Vec<crate::codegen_query::CollectedCodegenUnit>,
    #[allow(dead_code)]
    pub(crate) cfgs: Vec<RootedCfgUnit>,
    pub(crate) exports: Vec<crate::program_image_plan::RootedExportThunk>,
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) work: crate::CanonicalSemanticWork,
}

#[derive(Debug, Clone)]
struct RootedBodyGraph {
    revision: rue_query::Revision,
    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    declarations: Arc<[crate::DurableDeclarationSemantic]>,
    anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    declaration_dependencies: Arc<[crate::semantic_query_nucleus::SemanticDeclarationDependency]>,
    c_export_roots: Arc<[crate::StableDefinitionKey]>,
    modules: Arc<[Arc<crate::parsed_modules::ParsedModule>]>,
    main: crate::StableDefinitionKey,
    closure: crate::body_query::BodyClosureOutput,
    work: crate::CanonicalSemanticWork,
}

/// An opaque, single-use continuation issued ONLY from a successful close of
/// import discovery (RUE-1112).
///
/// It authorizes exactly one strictly-additive trusted-toolchain successor on
/// the closed revision, in the same request generation. It is bound to the
/// issuing session (`session`) and to the outstanding close (`nonce` +
/// `revision`); a token from a different session, a stale token (superseded by a
/// newer close or a new request), or a reused token (after a successful publish)
/// is rejected. The fields are private: the host holds the token and hands it
/// back, never inspecting or constructing it.
#[derive(Debug, Clone)]
pub struct ClosedDiscoveryContinuation {
    session: Arc<()>,
    nonce: u64,
    revision: crate::ImportInputRevision,
}

/// Opaque, compiler-derived authority for the modules a trusted-toolchain
/// successor may stage, project, reduce, and commit (RUE-1112).
///
/// It is minted ONLY by [`CompilerSession::publish_trusted_toolchain_successor`]
/// from the verified `added == demanded` set — never from host input — and is
/// bound to the issuing session and successor revision. Its fields are private,
/// so the host cannot construct, inspect, or edit the module set: it carries the
/// value opaquely between the successor stage and close. The successor stage and
/// close derive the exact module delta from the committed predecessor and the
/// current snapshot and verify the carried `appended` roots are present, so a
/// caller can neither omit an authorized module (committing a graph that lacks
/// imports for modules actually in the snapshot) nor admit an unauthorized one.
#[derive(Debug, Clone)]
pub struct TrustedSuccessorDelta {
    session: Arc<()>,
    nonce: u64,
    revision: crate::ImportInputRevision,
    appended: Arc<[crate::ModuleId]>,
}

impl TrustedSuccessorDelta {
    /// The successor input revision this delta was minted on. Exposing the
    /// revision does not expose the authorized module set; the host needs it only
    /// to continue discovery in the same request generation.
    pub fn revision(&self) -> crate::ImportInputRevision {
        self.revision
    }
}

/// Session-held authority backing an outstanding [`ClosedDiscoveryContinuation`].
/// Retains the predecessor snapshot, context, accepted-read provenance, and the
/// carried ledger so `publish_trusted_toolchain_successor` can verify a strictly
/// additive successor entirely from records, without any filesystem access.
///
/// A close alone leaves the state NON-AUTHORIZING (`attached_demands` is `None`):
/// no token can be minted and no successor authorized. Authority is granted
/// only when a rooted semantic park atomically attaches that park's exact sorted
/// missing-demand set to this same state. Demand authority therefore lives here,
/// bound to one closed revision and one park — never in an ambient session field
/// a later, non-parking close could inherit.
/// The CURRENT compiler-published view state a verified successor stage/close
/// consumes, with the derived module delta (RUE-1112). Everything here comes
/// from the published lineage; none of it is host-suppliable.
struct SuccessorState {
    snapshot: SourceSnapshot,
    context: crate::ImportDiscoveryContext,
    accepted_reads: crate::AcceptedReadManifest,
    ledger: crate::ImportObservationLedger,
    /// The published lineage identity this state was read from.
    revision: crate::ImportInputRevision,
    /// The appended module revisions (view sources minus the committed
    /// predecessor), in canonical module order.
    delta: Arc<[crate::ModuleRevision]>,
}

#[derive(Debug, Clone)]
struct ContinuationState {
    nonce: u64,
    revision: crate::ImportInputRevision,
    snapshot: SourceSnapshot,
    accepted_reads: crate::AcceptedReadManifest,
    ledger: crate::ImportObservationLedger,
    /// The exact sorted missing-demand set the rooted park attached, or `None`
    /// while the closed state is non-authorizing (no park has arrived for it).
    attached_demands: Option<Arc<[crate::TrustedToolchainModuleDemand]>>,
}

/// Convert an unsatisfied trusted-toolchain park to the error a stable
/// no-filesystem semantic entry returns at its outer boundary (RUE-1112).
///
/// This is a deterministic contract failure, never an ICE: the source is
/// otherwise valid, but a guaranteed toolchain input the reached bodies demand
/// was not supplied. The park-aware host driver acquires and retries; a stable
/// embedder that omits the input gets this distinguishable classification.
fn unresolved_toolchain_park_errors(park: &crate::ParkedToolchainModules) -> CompileErrors {
    let modules = park
        .demands()
        .iter()
        .map(|demand| demand.logical_path().to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    CompileErrors::from(crate::CompileError::without_span(
        rue_error::ErrorKind::UnsatisfiedTrustedToolchainInput(format!(
            "reached bodies demand trusted standard-library module(s) [{modules}] that are not present in this compilation; supply them (a std root the host can acquire from) before semantic analysis"
        )),
    ))
}

impl From<CompileErrors> for SemanticRequestControl {
    fn from(errors: CompileErrors) -> Self {
        Self::Compile(errors)
    }
}

/// Canonical frontend runtime state owned by `CompilerSession`.
///
/// Import staging and closure artifacts are thin projections of the revisioned
/// source/import frontier. They retain presentation-facing immutable results,
/// never a second selected-query authority.
#[derive(Debug)]
struct FrontendQueryDatabase {
    revisioned: crate::revisioned_query_database::RevisionedQueryDatabase,
    discovery_attempt: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    last_good_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    prior_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    oracle_import_fault: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    direct_import_diagnostic: Option<Arc<FrontendDiagnosticSnapshot>>,
}

impl Default for FrontendQueryDatabase {
    fn default() -> Self {
        Self {
            revisioned: crate::revisioned_query_database::RevisionedQueryDatabase::default(),
            discovery_attempt: None,
            last_good_discovery: None,
            prior_discovery: None,
            oracle_import_fault: None,
            direct_import_diagnostic: None,
        }
    }
}

impl FrontendQueryDatabase {
    fn record_discovery_attempt(&mut self, artifact: Arc<ImportDiscoveryRevisionArtifact>) {
        if let Some(previous) = self.discovery_attempt.replace(artifact.clone()) {
            if previous.source_revision() != artifact.source_revision() {
                self.prior_discovery = Some(previous);
            }
        }
        if artifact.status == ImportDiscoveryRevisionStatus::ClosedValid {
            self.last_good_discovery = Some(artifact);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ParseQueryKey {
    /// Ordinary content-addressed parsing: keyed on the exact source content,
    /// table order, and presentation, so any equal re-request reuses the
    /// terminal.
    Ordinary(Box<OrdinaryParseKey>),
    /// A trusted-toolchain successor parse projection (RUE-1112): keyed on the
    /// published predecessor lineage identity plus the exact appended source
    /// segment. Content is pinned by the published revision, so key hashing and
    /// equality never touch a predecessor entry.
    Successor {
        revision: crate::ImportInputRevision,
        segment: Arc<[crate::ModuleRevision]>,
        /// The exact retained predecessor parse terminal this successor
        /// extends, verified by structural source ancestry at preparation.
        predecessor: rue_query::Revision,
    },
}

impl ParseQueryKey {
    /// The exact source identity an Ordinary key pins (and whose stamp it
    /// retains); a Successor key pins its sources through the published
    /// lineage identity instead and allocates no stamp.
    pub(crate) fn pinned_source(&self) -> Option<&ExactSourceInput> {
        match self {
            Self::Ordinary(key) => Some(&key.source),
            Self::Successor { .. } => None,
        }
    }
}

/// The reconciled inputs of one successor parse extension (RUE-1112), prepared
/// without side effects so both the staging and adoption paths verify the
/// predecessor binding before starting a metrics attempt.
struct PreparedSuccessorParse {
    predecessor_program: Arc<ParsedProgram>,
    predecessor_order: crate::shared_segments::SharedList<crate::ModuleId>,
    /// The retained predecessor parse terminal's exact runtime identity; the
    /// successor key embeds it, so the successor terminal is bound to THIS
    /// predecessor artifact, never an ambient "latest".
    predecessor_revision: rue_query::Revision,
    /// The predecessor parse terminal ITSELF, minted into the exact-terminal
    /// adoption capability by the parse family's content-addressed
    /// registration. The successor computation records it as a runtime
    /// dependency, so the graph carries a real successor-after-predecessor
    /// edge with the captured terminal's exact node, incarnation, and stamp.
    predecessor_terminal: rue_query::AdoptableTerminal<ParseQueryRecord>,
    appended: Vec<(crate::ModuleId, crate::FileId)>,
    /// The exact source segment appended by this parse stage. The opaque
    /// successor capability carries the cumulative additions since the
    /// committed close, but parse extends the retained predecessor by only this
    /// suffix, so its key carries only these module revisions.
    segment: Arc<[crate::ModuleRevision]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OrdinaryParseKey {
    source: ExactSourceInput,
    /// Caller-owned source table order. This is presentation state rather than
    /// module identity, but a selected parse record retains the exact snapshot
    /// and diagnostic source table, so order changes must reproject the outer
    /// record while granular module terminals remain reusable.
    file_order: Arc<[crate::FileId]>,
    presentation: DiagnosticAttemptProvenance,
}

#[derive(Debug, Clone)]
pub(crate) struct ParseQueryRecord {
    key: ParseQueryKey,
    runtime_revision: rue_query::Revision,
    snapshot: SourceSnapshot,
    result: Result<Arc<ParsedProgram>, CompileErrors>,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
    work: ParsedModulesWork,
    invalidation: ParseInvalidationSummary,
}

impl RetainedCharge for ExactSourceInput {
    fn retained_charge(&self) -> u64 {
        self.revision
            .retained_charge()
            .saturating_add(self.metadata.retained_charge())
    }
}

impl RetainedCharge for OrdinaryParseKey {
    fn retained_charge(&self) -> u64 {
        self.source
            .retained_charge()
            .saturating_add(self.file_order.retained_charge())
            .saturating_add(self.presentation.retained_charge())
    }
}

impl RetainedCharge for ParseQueryKey {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Ordinary(key) => key.retained_charge(),
            Self::Successor { segment, .. } => segment.retained_charge(),
        }
    }
}

impl RetainedCharge for ParseQueryRecord {
    fn retained_charge(&self) -> u64 {
        self.key
            .retained_charge()
            .saturating_add(self.snapshot.retained_charge())
            .saturating_add(self.result.retained_charge())
            .saturating_add(self.diagnostics.retained_charge())
            .saturating_add(self.invalidation.retained_charge())
    }
}

impl ParseQueryRecord {
    pub(crate) fn runtime_revision(&self) -> rue_query::Revision {
        self.runtime_revision
    }
}

#[derive(Debug)]
pub(crate) struct ParseQuery;

impl TypedQueryFamily for ParseQuery {
    type Key = ParseQueryKey;
    type Record = ParseQueryRecord;
    const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

    fn key(record: &Self::Record) -> &Self::Key {
        &record.key
    }

    fn terminal_kind(record: &Self::Record) -> TerminalKind {
        if record.result.is_ok() {
            TerminalKind::Success
        } else {
            TerminalKind::Failure
        }
    }

    fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
        match (&left.result, &right.result) {
            // The complete key contains exact source bytes, metadata, and
            // presentation provenance. Parsing is deterministic, so equal
            // keys prove equal typed syntax even across distinct allocations.
            (Ok(left), Ok(right)) => left.source_revision() == right.source_revision(),
            (Err(left), Err(right)) => compile_errors_equal(left, right),
            _ => false,
        }
    }

    fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
        diagnostic_batches_equal(&left.diagnostics, &right.diagnostics)
    }

    fn diagnostics(record: &Self::Record) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        Some(&record.diagnostics)
    }

    fn record_is_consistent(record: &Self::Record) -> bool {
        match &record.key {
            ParseQueryKey::Ordinary(key) => {
                record.snapshot.source_revision() == &key.source.revision
                    && record.snapshot.metadata() == &key.source.metadata
                    && record
                        .snapshot
                        .files()
                        .map(|source| source.file_id)
                        .eq(key.file_order.iter().copied())
                    && match &record.result {
                        Ok(program) => program.source_revision() == &key.source.revision,
                        Err(_) => true,
                    }
                    && record.diagnostics.source_revision() == &key.source.revision
                    && record.diagnostics.identity() == &FrontendDiagnosticIdentity::Syntax
                    && record.diagnostics.provenance == key.presentation
            }
            ParseQueryKey::Successor { segment, .. } => {
                // Content identity is pinned by the published lineage in the
                // key; consistency stays O(segment) — a predecessor entry is
                // never re-enumerated here.
                record.snapshot.len() >= segment.len()
                    && match &record.result {
                        Ok(program) => program.modules_len() == record.snapshot.len(),
                        Err(_) => true,
                    }
                    && record.diagnostics.identity() == &FrontendDiagnosticIdentity::Syntax
            }
        }
    }
}

#[derive(Debug)]
struct DefinitionComputation {
    result: Result<Arc<BoundDefinitionSet>, CompileErrors>,
    binding: DeclarationBindingWork,
    manifest: SemanticBindingManifestWork,
    issuance: BoundDefinitionWork,
}

/// Metric-family markers for canonical projections; they do not own terminals.
#[derive(Debug)]
struct ImportDiagnosticQuery;
#[derive(Debug)]
struct MergeQuery;
#[derive(Debug)]
struct RirQuery;
#[derive(Debug)]
struct SemanticQuery;
#[derive(Debug)]
struct DefinitionQuery;

macro_rules! session_query_metrics_family {
    ($query:ty, $name:literal, $field:ident) => {
        impl SessionQueryMetricsFamily for $query {
            const NAME: &'static str = $name;

            fn projection(work: &mut CompilerSessionWork) -> &mut FrontendQueryWork {
                &mut work.$field
            }
        }
    };
}

session_query_metrics_family!(ImportsMetricsQuery, "imports", imports);
session_query_metrics_family!(
    ImportDiagnosticQuery,
    "import-diagnostics",
    import_diagnostics
);
session_query_metrics_family!(MergeQuery, "merge", merge);
session_query_metrics_family!(RirQuery, "rir", rir);
session_query_metrics_family!(SemanticQuery, "semantic", semantic);
session_query_metrics_family!(DefinitionQuery, "definitions", definitions);

/// Explicit compiler inputs read by a terminal attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExactSourceInput {
    revision: SourceRevision,
    metadata: crate::SourceMetadata,
}

fn compile_errors_equal(left: &CompileErrors, right: &CompileErrors) -> bool {
    left.iter().eq(right.iter())
}

fn diagnostic_batches_equal(
    left: &FrontendDiagnosticSnapshot,
    right: &FrontendDiagnosticSnapshot,
) -> bool {
    left.stage == right.stage
        && left.provenance == right.provenance
        && left.errors == right.errors
        && left.warnings == right.warnings
}

impl ExactSourceInput {
    pub(crate) fn new(snapshot: &SourceSnapshot) -> Self {
        Self {
            revision: snapshot.source_revision().clone(),
            metadata: snapshot.metadata().clone(),
        }
    }
}

fn compute_stable_definitions(
    merged: &CanonicalMergedProgram,
    options: &CompileOptions,
    imports: &CanonicalImportGraph,
    semantic: &CanonicalSemanticOutput,
) -> DefinitionComputation {
    let _ = (options, imports);
    let definitions = semantic
        .body_owner_issuer()
        .projected_for_source_revision(merged.ast().source_revision());
    let work = semantic.work();
    let binding = work.binding;
    let manifest = work.manifest;
    let issuance = definitions.work();
    let result = Ok(Arc::new(definitions));
    DefinitionComputation {
        result,
        binding,
        manifest,
        issuance,
    }
}

impl CompilerSession {
    #[cfg(test)]
    pub(crate) fn with_query_concurrency(workers: usize) -> Self {
        let mut session = Self::default();
        session.queries.revisioned =
            crate::revisioned_query_database::RevisionedQueryDatabase::with_query_concurrency(
                workers,
            );
        session
    }

    /// Force one exact production CodegenUnit request through an owner/joiner
    /// schedule. The normal rooted CFG query supplies the key; the registered
    /// CodegenUnit evaluator supplies the value. This controls only when the
    /// owner may finish and never constructs a peer artifact path.
    #[cfg(test)]
    pub(crate) fn exercise_codegen_schedule_for_test(
        &mut self,
        options: &CompileOptions,
        cancel_joiner: bool,
    ) -> (rue_query::RequestExecution, rue_query::RequestExecution) {
        let rooted = self
            .rooted_cfg(options)
            .expect("the schedule fixture reaches a valid CFG");
        let [cfg] = rooted.cfgs.as_slice() else {
            panic!("the schedule fixture must reach exactly one CodegenUnit");
        };
        let revision = rooted.graph.revision;
        let key = cfg.optimized_cfg_key.clone();
        let database = &self.queries.revisioned;
        let gate = database.arm_codegen_evaluator_gate_for_test();
        let baseline = database.runtime_metrics_for_test();
        let joiner_cancellation = rue_query::CancellationToken::new();

        let (owner_execution, joiner_execution) = std::thread::scope(|scope| {
            let owner_key = key.clone();
            let owner = scope.spawn(|| {
                database
                    .codegen_unit(
                        revision,
                        owner_key,
                        options.target,
                        rue_codegen::BackendArtifactRequest::default(),
                        options.opt_level,
                        rue_query::CancellationToken::new(),
                    )
                    .expect("the owner CodegenUnit request is registered")
            });
            gate.wait_until_entered();

            let joiner_key = key.clone();
            let joiner_token = joiner_cancellation.clone();
            let joiner = scope.spawn(|| {
                database
                    .codegen_unit(
                        revision,
                        joiner_key,
                        options.target,
                        rue_codegen::BackendArtifactRequest::default(),
                        options.opt_level,
                        joiner_token,
                    )
                    .expect("the joining CodegenUnit request is registered")
            });

            let wait_for = |predicate: &dyn Fn(rue_query::RuntimeMetrics) -> bool| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !predicate(database.runtime_metrics_for_test())
                    && std::time::Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                predicate(database.runtime_metrics_for_test())
            };
            let joined = wait_for(&|metrics| metrics.joins > baseline.joins);
            let canceled = if cancel_joiner && joined {
                joiner_cancellation.cancel();
                wait_for(&|metrics| metrics.cancellations > baseline.cancellations)
            } else {
                true
            };

            // Always release the owner before asserting the schedule so a
            // failed observation cannot strand a scoped worker.
            gate.release();
            let owner = owner.join().expect("CodegenUnit owner did not panic");
            let joiner = joiner.join().expect("CodegenUnit joiner did not panic");
            assert!(
                joined,
                "the exact-key request did not join within 5 seconds"
            );
            assert!(
                canceled,
                "the joined waiter did not cancel within 5 seconds"
            );
            assert!(
                owner.terminal().is_some(),
                "the live owner must publish the canonical CodegenUnit"
            );
            if cancel_joiner {
                assert!(matches!(
                    joiner.abort(),
                    Some(rue_query::QueryAbort::Canceled)
                ));
                assert!(joiner.terminal().is_none());
            } else {
                assert!(joiner.terminal().is_some());
            }
            (owner.execution(), joiner.execution())
        });
        (owner_execution, joiner_execution)
    }

    /// Gate the first `gated_children` production CodegenUnit evaluators in one
    /// rooted batch and return their peak simultaneous occupancy.
    #[cfg(test)]
    pub(crate) fn exercise_codegen_batch_overlap_for_test(
        &mut self,
        options: &CompileOptions,
        gated_children: usize,
        rendezvous: bool,
    ) -> (usize, usize) {
        let gate = self
            .queries
            .revisioned
            .arm_codegen_batch_evaluator_gate_for_test(gated_children, rendezvous);
        if rendezvous {
            std::thread::scope(|scope| {
                let compilation = scope.spawn(|| {
                    self.rooted_codegen(options, rue_codegen::BackendArtifactRequest::default())
                });
                let all_entered = gate.wait_until_all_entered_and_release();
                compilation
                    .join()
                    .expect("CodegenUnit batch compilation did not panic")
                    .expect("CodegenUnit batch fixture compiles successfully");
                assert!(
                    all_entered,
                    "CodegenUnit evaluators did not reach the requested concurrent occupancy"
                );
            });
        } else {
            self.rooted_codegen(options, rue_codegen::BackendArtifactRequest::default())
                .expect("CodegenUnit batch fixture compiles successfully");
        }
        (gate.peak(), gate.entered())
    }
    /// Perturb one canonical observation for the in-tree differential oracle.
    #[doc(hidden)]
    pub(crate) fn inject_stale_query_for_oracle(
        &mut self,
        fault: crate::unstable::DifferentialOracleFault,
    ) -> bool {
        match fault {
            crate::unstable::DifferentialOracleFault::Semantic => {
                self.oracle_fault = Some(fault);
                true
            }
            crate::unstable::DifferentialOracleFault::Diagnostic => {
                let Some(source) = self.published_snapshot.clone() else {
                    return false;
                };
                let errors = CompileErrors::from(CompileError::without_span(
                    ErrorKind::InternalError("differential diagnostic fault".into()),
                ));
                // This intentional oracle-only corruption must be selected as a
                // distinct canonical attempt. `publish_diagnostics` correctly
                // reuses an equal RIR key, which would hide this oracle fault.
                let snapshot = Arc::new(FrontendDiagnosticSnapshot {
                    source: source.clone(),
                    stage: FrontendDiagnosticIdentity::Rir(source.source_revision().clone()),
                    provenance: DiagnosticAttemptProvenance::Canonical,
                    errors: errors.iter().cloned().collect::<Vec<_>>().into(),
                    warnings: Arc::from([]),
                });
                self.diagnostics.select_snapshot(&snapshot);
                self.refresh_retention_metrics();
                true
            }
            crate::unstable::DifferentialOracleFault::Import => {
                let Some(stale) = self.queries.prior_discovery.clone() else {
                    return false;
                };
                let Some(current) = self.queries.discovery_attempt.as_ref() else {
                    return false;
                };
                if stale.source_revision() == current.source_revision() {
                    return false;
                }
                self.queries.oracle_import_fault = Some(stale);
                true
            }
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    fn resume_canceled_query(
        &mut self,
        guard: &mut QueryComputationGuard,
        payload: Box<dyn std::any::Any + Send>,
    ) -> ! {
        match guard.family {
            "import-diagnostics" | "merge" | "rir" | "semantic" | "definitions" | "imports"
            | "parse" => {}
            family => unreachable!("unknown query guard family {family}"),
        }
        self.metrics.synchronize();
        std::panic::resume_unwind(payload)
    }

    fn cancel_merge_at_commit_boundary(&mut self) -> bool {
        #[cfg(test)]
        return std::mem::take(&mut self.cancel_merge_before_commit);
        #[cfg(not(test))]
        false
    }

    /// Select the accepted import topology for semantic construction.
    ///
    /// Import-bearing revisions must come from the atomically adopted
    /// discovery artifact. A direct session remains usable for an import-free
    /// snapshot by supplying the uniquely valid empty graph; it may never
    /// reconstruct resolved imports from paths or environment state.
    fn accepted_semantic_import_graph(&self) -> Result<CanonicalImportGraph, CompileErrors> {
        let program = self.published.as_ref().ok_or_else(no_published_program)?;
        let graph = if !program.import_directives().is_empty() {
            let committed = self.committed_import_graph()?;
            if &committed.input().sources != program.source_revision() {
                return Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(
                        "committed import graph belongs to a foreign source revision".into(),
                    ),
                )));
            }
            committed.graph().clone()
        } else {
            crate::import_graph::import_free_canonical_graph(program.as_ref())?
        };
        Ok(graph)
    }
    pub fn published(&self) -> Option<crate::SyntaxView> {
        self.published.as_ref().cloned().map(crate::SyntaxView::new)
    }

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

    /// Begins one fresh rooted external-input request with granular immutable
    /// module source, accepted-read provenance, and observation leaves.
    /// Successor carry is available only through batch publication.
    pub(crate) fn begin_import_input_request(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: crate::AcceptedReadManifest,
    ) -> crate::CompileResult<crate::ImportInputRevision> {
        // A fresh observation generation invalidates any outstanding
        // trusted-toolchain continuation and successor-delta authority (RUE-1112).
        self.continuation = None;
        self.successor_delta_nonce = None;
        self.queries
            .revisioned
            .begin_import_inputs(snapshot, context, accepted_reads)
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

    /// Returns the immutable canonical ledger carried by one input revision.
    pub(crate) fn import_observation_ledger(
        &self,
        revision: crate::ImportInputRevision,
    ) -> crate::CompileResult<crate::ImportObservationLedger> {
        self.queries.revisioned.import_ledger(revision)
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
        let Some((current, snapshot, context, accepted_reads, ledger)) =
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
        self.stage_import_discovery_inner(&snapshot, context, accepted_reads, ledger, None)
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
        self.import_plan_groups_constructed
    }

    /// See the field docs on `parse_sources_materialized`.
    pub(crate) fn parse_sources_materialized(&self) -> u64 {
        self.parse_sources_materialized
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
        self.import_close_records_reduced
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
        self.queries
            .oracle_import_fault
            .as_ref()
            .or(self.open_discovery.as_ref())
            .or(self.queries.discovery_attempt.as_ref())
    }
    #[cfg(test)]
    pub(crate) fn last_good_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.last_good_discovery_artifact()
    }

    pub(crate) fn last_good_discovery_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.queries.last_good_discovery.as_ref()
    }

    pub(crate) fn committed_import_discovery_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        let source = self.published.as_ref()?.source_revision();
        self.queries.discovery_attempt.as_ref().filter(|artifact| {
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
                self.queries
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
                self.queries.direct_import_diagnostic = Some(diagnostics.clone());
                diagnostics
            }
        };
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        Ok(diagnostics)
    }

    fn require_successful_import_diagnostics(&mut self) -> Result<(), CompileErrors> {
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
        let Some(outstanding) = self.successor_delta_nonce else {
            return Err(reject(
                "no outstanding successor-delta authority; it was already consumed or invalidated",
            ));
        };
        if outstanding != delta.nonce {
            return Err(reject(
                "the successor delta is stale (superseded by a newer publish, request, or close)",
            ));
        }
        let Some((current, snapshot, context, accepted_reads, ledger)) =
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
        self.stage_import_discovery_inner(
            &state.snapshot,
            state.context,
            state.accepted_reads,
            state.ledger,
            Some((state.revision, state.delta)),
        )
    }

    fn stage_import_discovery_inner(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: crate::AcceptedReadManifest,
        carried_ledger: crate::ImportObservationLedger,
        successor: Option<(crate::ImportInputRevision, Arc<[crate::ModuleRevision]>)>,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
        let new_module_ids: Option<Vec<crate::ModuleId>> = successor.as_ref().map(|(_, delta)| {
            delta
                .iter()
                .map(|revision| revision.module.clone())
                .collect()
        });
        let new_modules: Option<&[crate::ModuleId]> = new_module_ids.as_deref();
        let continuation = self.open_discovery.as_deref().filter(|attempt| {
            continues_discovery_lifecycle(
                attempt,
                snapshot,
                &context,
                &accepted_reads,
                &carried_ledger,
            )
        });
        let mut parse_work =
            continuation.map_or_else(ParsedModulesWork::default, |attempt| attempt.parse_work);
        // Reinstall protocol context only if staging reaches Open. Closed
        // attempts are retained as projections of the canonical frontier.
        self.open_discovery = None;
        let source_revision = snapshot.source_revision().clone();
        if let Err(errors) = validate_accepted_read_manifest(snapshot, &accepted_reads) {
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
                successor_parse: None,
            });
            self.queries.record_discovery_attempt(attempted_artifact);
            return Err(errors);
        }
        // Staging splits into the canonical parse of everything read so far and
        // the import-plan construction over the resulting program. Both were
        // previously folded into the driver's unattributed region (RUE-786).
        let parse_staging_span = tracing::info_span!("import_parse_staging").entered();
        let (parse_result, staged_work, staged_successor_parse) = self.parse_staging_snapshot(
            snapshot,
            successor
                .as_ref()
                .map(|(revision, delta)| (*revision, delta)),
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
                    successor_parse: None,
                });
                self.queries.record_discovery_attempt(attempted_artifact);
                return Err(errors);
            }
        };
        // A trusted-toolchain successor stage reuses the committed predecessor
        // plan's request groups and constructs groups only for the newly appended
        // modules' occurrences; predecessor occurrences are never re-staged. When
        // no predecessor plan is retained (an unexpected legacy state) it falls
        // back to a full build so the plan is always complete.
        let predecessor_plan = new_modules.and_then(|_| {
            self.last_good_discovery_artifact()
                .and_then(|artifact| artifact.plan.clone())
        });
        let plan_build_span = tracing::info_span!("import_plan_build").entered();
        let plan_build = match (new_modules, predecessor_plan) {
            (Some(new_modules), Some(predecessor)) => {
                crate::ImportDiscoveryPlan::extend_trusted_successor(
                    &predecessor,
                    &program,
                    context.clone(),
                    new_modules,
                )
                .map(|(plan, constructed)| {
                    self.import_plan_groups_constructed = self
                        .import_plan_groups_constructed
                        .saturating_add(constructed);
                    plan
                })
            }
            _ => crate::ImportDiscoveryPlan::new(&program, context.clone()).inspect(|plan| {
                self.import_plan_groups_constructed = self
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
                    successor_parse: None,
                });
                self.queries.record_discovery_attempt(attempted_artifact);
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
        self.open_discovery = Some(Arc::new(ImportDiscoveryRevisionArtifact {
            status: ImportDiscoveryRevisionStatus::Open,
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
        let Some((current, _, _, _, ledger)) = self.queries.revisioned.current_import_view_state()
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
        self.successor_delta_nonce = None;
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
            &exact_groups,
            check_ledger,
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
        self.import_close_records_reduced = self
            .import_close_records_reduced
            .saturating_add(reduced.records().len() as u64);
        // In successor mode, merge the new records into the committed predecessor's
        // closed graph and validate incrementally (the predecessor topology is
        // carried, never re-walked). A full close reduces and validates the whole
        // graph directly.
        let (reduced, validation) = match &narrow {
            Some((set, predecessor)) => {
                // The reduction produced only the delta records; build the
                // successor graph by structurally sharing the predecessor's record
                // segment (no predecessor record is copied or re-sorted) and
                // validate only the delta against the carried predecessor result.
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
                    | crate::CanonicalImportGraphProblem::AmbiguousResolution { .. }
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

        let adoption = self.adopt_discovery_program_for_presentation(
            &open.snapshot,
            program.clone(),
            open.parse_work,
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
        self.queries.record_discovery_attempt(artifact.clone());
        self.open_discovery = None;
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
        // Only a subsequent rooted semantic park attaches its exact missing-demand
        // set to this same state, so a later close whose attempt never parks can
        // never inherit an earlier park's authority.
        self.continuation = self
            .queries
            .revisioned
            .current_import_revision()
            .map(|revision| {
                self.next_continuation_nonce += 1;
                ContinuationState {
                    nonce: self.next_continuation_nonce,
                    revision,
                    snapshot: artifact.snapshot().clone(),
                    accepted_reads: artifact.accepted_read_manifest().clone(),
                    ledger: artifact.ledger().clone(),
                    attached_demands: None,
                }
            });
        Ok(artifact)
    }

    /// Mint the trusted-toolchain continuation token for the current successful
    /// import-discovery close, if one is outstanding AND authorizing (RUE-1112).
    /// A closed state becomes authorizing only once a rooted semantic park
    /// has attached its exact missing-demand set; a close whose attempt is ready
    /// (or never parked) mints no token. The token is opaque and single-use; the
    /// host hands it back to [`Self::publish_trusted_toolchain_successor`].
    pub(crate) fn closed_discovery_continuation(&self) -> Option<ClosedDiscoveryContinuation> {
        self.continuation
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
        let state = match self.continuation.as_ref() {
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
        let new_reads: std::collections::HashSet<&crate::AcceptedReadManifestEntry> =
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
                "the closed continuation is not authorizing; no rooted semantic park has attached a demanded-module set",
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
        // Consume the single-use continuation only on success.
        self.continuation = None;
        // Mint the opaque successor-delta authority from the VERIFIED `added`
        // set (equal to the park's demanded missing set). `BTreeSet` iteration is
        // sorted, so the appended roots are deterministic. The host receives only
        // this opaque value; it cannot inspect or edit the module identities.
        let appended: Arc<[crate::ModuleId]> = added.into_iter().collect::<Vec<_>>().into();
        self.next_continuation_nonce += 1;
        let nonce = self.next_continuation_nonce;
        self.successor_delta_nonce = Some(nonce);
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
        self.queries.record_discovery_attempt(artifact.clone());
        self.open_discovery = None;
        artifact
    }

    fn require_closed_discovery(&self) -> Result<(), CompileErrors> {
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
    pub(crate) fn work(&self) -> &CompilerSessionWork {
        self.metrics.work()
    }
    /// Return an owned snapshot of explicitly unstable compiler metrics.
    ///
    /// The snapshot cannot be installed back into this or another session and
    /// therefore grants no access to query ownership or invalidation state.
    pub fn unstable_metrics(&self) -> crate::unstable::MetricsSnapshot {
        crate::unstable::MetricsSnapshot::new(self.metrics.work().clone())
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
    /// succeeded or failed.
    pub fn latest_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostics.latest()
    }
    /// Most recently queried diagnostic snapshot with no errors.
    pub fn latest_successful_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostics.latest_successful()
    }
    /// Most recent successful semantic diagnostic snapshot.
    ///
    /// Syntax or semantic failures never replace this last-good semantic
    /// baseline. A caller may clone the returned `Arc` to pin it independently
    /// of later session eviction.
    pub fn last_good_semantic_diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.diagnostics.last_good_semantic()
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

    /// Compatibility name for [`Self::most_recent_diagnostics_for`].
    ///
    /// This is not an exact lookup when canonical and presentation provenance
    /// share a public stage; it follows the selection contract documented by
    /// `most_recent_diagnostics_for`.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn diagnostics_for(
        &self,
        source: &SourceSnapshot,
        stage: &FrontendDiagnosticIdentity,
    ) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.most_recent_diagnostics_for(source, stage)
    }

    fn publish_diagnostics(
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

    fn reuse_diagnostics(&mut self, snapshot: Arc<FrontendDiagnosticSnapshot>) {
        self.metrics.diagnostic_reuse();
        self.diagnostics.select_snapshot(&snapshot);
        self.refresh_retention_metrics();
    }

    fn publish_import_diagnostics(
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

    fn refresh_retention_metrics(&mut self) {
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
    }

    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CompilerSessionUpdate {
        // A source update supersedes the predecessor any outstanding
        // trusted-toolchain continuation or successor-delta authority was
        // issued against (RUE-1112): a stale capability can neither stage nor
        // close over an artifact the update replaced.
        self.continuation = None;
        self.successor_delta_nonce = None;
        self.select_diagnostic_presentation(None);
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    /// Publish a snapshot while retaining its caller-selected presentation order.
    ///
    /// Query artifacts still use stable module identity. Only syntax and merge
    /// diagnostic ordering follows [`SourceSnapshot::files`], which is useful
    /// for command-line and other presentation-oriented consumers.
    pub(crate) fn update_for_presentation(
        &mut self,
        snapshot: &SourceSnapshot,
    ) -> CompilerSessionUpdate {
        // A presentation update replaces the retained parse artifact exactly
        // like a source update, so it likewise supersedes any outstanding
        // trusted-toolchain continuation or successor-delta authority
        // (RUE-1112).
        self.continuation = None;
        self.successor_delta_nonce = None;
        self.select_diagnostic_presentation(Some(crate::shared_segments::SharedList::flat(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        )));
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    fn adopt_discovery_program_for_presentation(
        &mut self,
        snapshot: &SourceSnapshot,
        _program: Arc<ParsedProgram>,
        _work: ParsedModulesWork,
        successor: bool,
        retained_successor_parse: Option<ParseQueryRecord>,
    ) -> CompilerSessionUpdate {
        // A trusted-successor close adopts by RE-SELECTING the exact successor
        // parse terminal its stage computed and retained on the open artifact
        // — same key, same revision — never by re-deriving an extension
        // against the now-selected successor state (which would mint a second
        // empty-extension terminal). A missing retained terminal rejects the
        // close.
        if successor {
            return match retained_successor_parse {
                Some(record) => self.run_parse_update_successor(snapshot, record),
                None => {
                    let errors = CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(
                            "trusted-toolchain successor close rejected: the staged successor parse terminal is not retained".into(),
                        ),
                    ));
                    let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
                        source: snapshot.clone(),
                        stage: FrontendDiagnosticIdentity::Syntax,
                        provenance: DiagnosticAttemptProvenance::Canonical,
                        errors: errors.as_slice().to_vec().into(),
                        warnings: Arc::from([]),
                    });
                    CompilerSessionUpdate {
                        result: Err(errors),
                        work: ParsedModulesWork::default(),
                        #[cfg(test)]
                        invalidation: ParseInvalidationSummary::default(),
                        downstream_invalidated: false,
                        diagnostics,
                    }
                }
            };
        }
        self.select_diagnostic_presentation(Some(crate::shared_segments::SharedList::flat(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        )));
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    fn parse_baseline(&self) -> Option<Arc<ParsedProgram>> {
        self.queries
            .revisioned
            .last_good_parse_record()
            .and_then(|record| record.result.as_ref().ok())
            .cloned()
    }

    fn parse_invalidation(&self, snapshot: &SourceSnapshot) -> ParseInvalidationSummary {
        let baseline = self.parse_baseline();
        classify_invalidation(snapshot, baseline.as_deref())
    }

    fn syntax_diagnostic_provenance(&self) -> DiagnosticAttemptProvenance {
        self.batch_diagnostic_order
            .as_ref()
            .map_or(DiagnosticAttemptProvenance::Canonical, |order| {
                DiagnosticAttemptProvenance::Presentation(order.clone())
            })
    }

    fn select_diagnostic_presentation(
        &mut self,
        order: Option<crate::shared_segments::SharedList<crate::ModuleId>>,
    ) {
        self.batch_diagnostic_order = order;
    }

    fn execute_parse_query(
        &mut self,
        snapshot: &SourceSnapshot,
        presentation: DiagnosticAttemptProvenance,
        attempt_id: AttemptId,
    ) -> (
        ParseQueryRecord,
        Arc<dyn AttemptView>,
        QueryAttemptExecution,
        ParsedModulesWork,
        ParseInvalidationSummary,
    ) {
        // Keying, module parsing, and terminal publication are separate costs
        // inside the parse query. Timing them apart keeps the staging residual
        // from hiding whole-snapshot content hashing behind `parse_file`
        // (RUE-786).
        let key_span = tracing::info_span!("parse_query_key").entered();
        let source = ExactSourceInput::new(snapshot);
        // An ordinary key carries every file's exact content identity, so the
        // typed store hashes and compares each of them.
        self.parse_key_entries_compared = self
            .parse_key_entries_compared
            .saturating_add(snapshot.len() as u64);
        let key = ParseQueryKey::Ordinary(Box::new(OrdinaryParseKey {
            source: source.clone(),
            file_order: snapshot
                .files()
                .map(|source| source.file_id)
                .collect::<Vec<_>>()
                .into(),
            presentation: presentation.clone(),
        }));
        let revision = self.queries.revisioned.source_revision(&source, snapshot);
        let demanded_modules = match &presentation {
            DiagnosticAttemptProvenance::Canonical => snapshot
                .source_revision()
                .modules()
                .iter()
                .map(|source| source.module.clone())
                .collect::<Vec<_>>(),
            DiagnosticAttemptProvenance::Presentation(order) => order.iter().cloned().collect(),
        };
        self.parse_sources_materialized = self
            .parse_sources_materialized
            .saturating_add(demanded_modules.len() as u64);
        self.parse_modules_dispatched = self
            .parse_modules_dispatched
            .saturating_add(demanded_modules.len() as u64);
        drop(key_span);
        let (modular_result, modular_work) = {
            let _span = tracing::info_span!("parse_program").entered();
            self.queries.revisioned.parse_program(
                revision,
                snapshot.source_revision().root(),
                demanded_modules,
            )
        };
        let _commit_span = tracing::info_span!("parse_query_commit").entered();
        self.parse_invalidation_entries_compared = self
            .parse_invalidation_entries_compared
            .saturating_add(snapshot.len() as u64);
        let baseline = self.parse_baseline();
        let attempt =
            self.queries
                .revisioned
                .request_parse(revision, attempt_id, key.clone(), |context| {
                    context.input(rue_query::InputIdentity::new(
                        crate::revisioned_query_database::RevisionedQueryDatabase::SOURCE_INPUT,
                        "current",
                    ))?;
                    let work = modular_work;
                    let invalidation = classify_invalidation(snapshot, baseline.as_deref());
                    let result = modular_result;
                    // Freeze diagnostics privately with the query output. Session
                    // selection and metrics happen only after atomic publication.
                    let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
                        source: snapshot.clone(),
                        stage: FrontendDiagnosticIdentity::Syntax,
                        provenance: presentation.clone(),
                        errors: result.as_ref().err().map_or_else(
                            || Arc::from([]),
                            |errors| errors.as_slice().to_vec().into(),
                        ),
                        warnings: Arc::from([]),
                    });
                    Ok(ParseQueryRecord {
                        key,
                        runtime_revision: revision,
                        snapshot: snapshot.clone(),
                        result,
                        diagnostics,
                        work,
                        invalidation,
                    })
                });
        self.queries.revisioned.select_parse(&attempt);
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()));
        let record = match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => record.clone(),
            rue_query::QueryOutcome::Failure(_) => unreachable!("parse retains typed records"),
        };
        let execution = match attempt.execution() {
            rue_query::RequestExecution::Computed => {
                self.metrics
                    .diagnostic_publication(self.diagnostics.latest().is_some());
                QueryAttemptExecution::Computed
            }
            rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                self.reuse_diagnostics(record.diagnostics.clone());
                QueryAttemptExecution::Reused
            }
            rue_query::RequestExecution::Aborted => unreachable!(),
        };
        let work = if execution == QueryAttemptExecution::Computed {
            record.work
        } else {
            ParsedModulesWork::default()
        };
        let invalidation = if execution == QueryAttemptExecution::Computed {
            record.invalidation.clone()
        } else {
            self.parse_invalidation(snapshot)
        };
        let view = self.queries.revisioned.parse_attempt_view(
            attempt_id,
            attempt,
            QueryStructuralWork::Parse(work),
        );
        self.diagnostics.select(view.clone());
        (record, view, execution, work, invalidation)
    }

    /// Reconcile one successor parse extension without side effects: the
    /// retained parse artifact this stage extends (within one trusted re-close,
    /// the committed predecessor for the first stage and the prior successor
    /// stage after a frontier batch), its presentation order, and the appended
    /// (module, file) pairs. The retained artifact must PROVE it is the
    /// successor snapshot's structural ancestor — every one of its source
    /// segments carried by `Arc` identity, same root, and its exact
    /// presentation order — so a parse record from any other snapshot (an
    /// intervening source or presentation update) can never be extended; the
    /// capability is rejected instead. Everything here is O(appended); content
    /// identity is pinned by the published revision, never re-hashed or
    /// re-compared.
    fn prepare_successor_parse(
        &self,
        snapshot: &SourceSnapshot,
        delta: &Arc<[crate::ModuleRevision]>,
    ) -> Result<PreparedSuccessorParse, CompileErrors> {
        let reject = |message: &str| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("trusted-toolchain successor parse rejected: {message}"),
            )))
        };
        let Some(terminal) = self.queries.revisioned.last_good_parse_terminal() else {
            return Err(reject("no predecessor parse artifact is retained"));
        };
        let Ok(predecessor_terminal) = self
            .queries
            .revisioned
            .parse_family()
            .adoptable_terminal(terminal)
        else {
            return Err(reject(
                "the retained predecessor parse terminal is not adoptable",
            ));
        };
        let rue_query::QueryOutcome::Success(record) = predecessor_terminal.terminal().outcome()
        else {
            return Err(reject("the retained predecessor parse artifact failed"));
        };
        let Ok(predecessor_program) = record.result.as_ref().cloned() else {
            return Err(reject("the retained predecessor parse artifact failed"));
        };
        let DiagnosticAttemptProvenance::Presentation(predecessor_order) =
            &record.diagnostics.provenance
        else {
            return Err(reject(
                "the retained parse artifact carries no staging presentation order",
            ));
        };
        let predecessor_order = predecessor_order.clone();
        let predecessor_revision = record.runtime_revision;
        // STRUCTURAL ANCESTRY: the successor snapshot must carry every source
        // segment of the retained artifact's snapshot by `Arc` identity, with
        // the same root. An artifact retained by an intervening update over a
        // different or reordered snapshot cannot share this lineage and is
        // rejected here rather than silently extended.
        let predecessor_snapshot = record.snapshot.clone();
        {
            let successor_segments = snapshot.source_revision().module_segments().segments();
            let predecessor_segments = predecessor_snapshot
                .source_revision()
                .module_segments()
                .segments();
            let shared_prefix = successor_segments.len() >= predecessor_segments.len()
                && successor_segments
                    .iter()
                    .zip(predecessor_segments.iter())
                    .all(|(a, b)| Arc::ptr_eq(a, b));
            if !shared_prefix
                || snapshot.source_revision().root()
                    != predecessor_snapshot.source_revision().root()
            {
                return Err(reject(
                    "the retained parse artifact is not the successor snapshot's structural ancestor",
                ));
            }
        }
        let predecessor_len = predecessor_program.modules_len();
        if predecessor_len != predecessor_snapshot.len()
            || predecessor_order.len() != predecessor_len
        {
            return Err(reject(
                "the retained parse artifact does not cover its own snapshot",
            ));
        }
        // A re-stage whose snapshot appended nothing since the retained parse
        // (a frontier round that only grew observations) extends with an empty
        // delta and reuses every retained module.
        if predecessor_len > snapshot.len() || snapshot.len() - predecessor_len > delta.len() {
            return Err(reject(
                "the successor snapshot does not extend the retained parse artifact by the authorized delta",
            ));
        }
        // The appended sources extend the predecessor's dense file table, so
        // the appended (module, file) pairs are exactly the tail file IDs.
        let mut appended = Vec::with_capacity(snapshot.len() - predecessor_len);
        for index in predecessor_len as u32 + 1..=snapshot.len() as u32 {
            let file_id = crate::FileId::new(index);
            let Some(module) = snapshot.module_id(file_id) else {
                return Err(reject("an appended source has no logical module"));
            };
            appended.push((module.clone(), file_id));
        }
        // Every appended module revision must be one of the
        // capability-verified additions. The capability delta is cumulative
        // since the committed close; the parse key below keeps only this
        // stage's exact suffix.
        let mut segment = Vec::with_capacity(appended.len());
        for (module, file_id) in &appended {
            let source = snapshot
                .source_id(*file_id)
                .expect("the appended source has a stable content identity")
                .clone();
            let Ok(index) = delta.binary_search_by(|revision| revision.module.cmp(module)) else {
                return Err(reject(
                    "an appended module is outside the capability-verified delta",
                ));
            };
            if delta[index].source != source {
                return Err(reject(
                    "an appended module's source differs from the capability-verified delta",
                ));
            }
            segment.push(crate::ModuleRevision {
                module: module.clone(),
                source,
            });
        }
        segment.sort_by(|left, right| left.module.cmp(&right.module));
        Ok(PreparedSuccessorParse {
            predecessor_program,
            predecessor_order,
            predecessor_revision,
            predecessor_terminal,
            appended,
            segment: segment.into(),
        })
    }

    /// The successor parse projection (RUE-1112): keyed on the published
    /// lineage identity plus the exact appended segment, parsing ONLY the
    /// appended modules and structurally extending the retained predecessor
    /// parsed program and presentation order.
    #[allow(clippy::type_complexity)]
    fn execute_parse_query_successor(
        &mut self,
        snapshot: &SourceSnapshot,
        revision: crate::ImportInputRevision,
        prepared: PreparedSuccessorParse,
        attempt_id: AttemptId,
    ) -> (
        ParseQueryRecord,
        Arc<dyn AttemptView>,
        QueryAttemptExecution,
        ParsedModulesWork,
        ParseInvalidationSummary,
    ) {
        let PreparedSuccessorParse {
            predecessor_program,
            predecessor_order,
            predecessor_revision,
            predecessor_terminal,
            appended,
            segment,
        } = prepared;
        let successor_order = crate::shared_segments::SharedList::extend(
            &predecessor_order,
            appended.iter().map(|(module, _)| module.clone()).collect(),
        );
        self.select_diagnostic_presentation(Some(successor_order.clone()));
        let presentation = DiagnosticAttemptProvenance::Presentation(successor_order);

        // A successor key embeds only the published lineage identity and its
        // appended segment.
        self.parse_key_entries_compared = self
            .parse_key_entries_compared
            .saturating_add(segment.len() as u64);
        let key = ParseQueryKey::Successor {
            revision,
            segment,
            predecessor: predecessor_revision,
        };
        let runtime_revision =
            // The runtime revision's compatibility slot is the observation
            // regime, not the per-request counter (RUE-1137). This must match
            // how import publication built the revision, or the module-input
            // and parse projections cannot find their published views.
            rue_query::Revision::new(revision.revision_id, revision.compatibility_token);
        self.parse_modules_dispatched = self
            .parse_modules_dispatched
            .saturating_add(appended.len() as u64);
        let (modular_result, modular_work) = self.queries.revisioned.parse_program_extension(
            runtime_revision,
            &predecessor_program,
            &appended,
        );
        self.parse_invalidation_entries_compared = self
            .parse_invalidation_entries_compared
            .saturating_add(appended.len() as u64);
        let appended_modules: Vec<crate::ModuleId> =
            appended.iter().map(|(module, _)| module.clone()).collect();
        let parse_family = self.queries.revisioned.parse_family();
        let attempt = self.queries.revisioned.request_parse(
            runtime_revision,
            attempt_id,
            key.clone(),
            |context| {
                // The record adopts the CAPTURED predecessor parse terminal as a
                // runtime dependency — the exact terminal held by preparation,
                // observed by node, incarnation, and stamp with no key hash or
                // content comparison — so successor-after-predecessor is a real
                // query edge: red/green validation and leases flow through it,
                // and the node's endorsement at this revision carries the exact
                // stamp to every compatible descendant. Adoption is sound here
                // because parse keys are content-addressed: the key alone pins
                // the terminal's value. A stale or evicted terminal aborts the
                // attempt rather than being silently re-derived.
                if parse_family
                    .observe_adopted_terminal(context, &predecessor_terminal)
                    .is_err()
                {
                    return Err(rue_query::QueryAbort::Canceled);
                }
                // Plus exactly the appended modules' input leaves; the remaining
                // predecessor content is pinned by the dependency above and the
                // published lineage identity in the key.
                for (module, _) in &appended {
                    context.input(
                    crate::revisioned_query_database::RevisionedQueryDatabase::module_source_input(
                        module,
                    ),
                )?;
                }
                let work = modular_work;
                let invalidation =
                    crate::parsed_modules::classify_successor_invalidation(&appended_modules);
                let result = modular_result;
                // Freeze diagnostics privately with the query output. Session
                // selection and metrics happen only after atomic publication.
                let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
                    source: snapshot.clone(),
                    stage: FrontendDiagnosticIdentity::Syntax,
                    provenance: presentation.clone(),
                    errors: result
                        .as_ref()
                        .err()
                        .map_or_else(|| Arc::from([]), |errors| errors.as_slice().to_vec().into()),
                    warnings: Arc::from([]),
                });
                Ok(ParseQueryRecord {
                    key,
                    runtime_revision,
                    snapshot: snapshot.clone(),
                    result,
                    diagnostics,
                    work,
                    invalidation,
                })
            },
        );
        self.queries.revisioned.select_parse(&attempt);
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()));
        let record = match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => record.clone(),
            rue_query::QueryOutcome::Failure(_) => unreachable!("parse retains typed records"),
        };
        let execution = match attempt.execution() {
            rue_query::RequestExecution::Computed => {
                self.metrics
                    .diagnostic_publication(self.diagnostics.latest().is_some());
                QueryAttemptExecution::Computed
            }
            rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                self.reuse_diagnostics(record.diagnostics.clone());
                QueryAttemptExecution::Reused
            }
            rue_query::RequestExecution::Aborted => unreachable!(),
        };
        let work = if execution == QueryAttemptExecution::Computed {
            record.work
        } else {
            ParsedModulesWork::default()
        };
        // A successor record's classification is relative to the retained
        // predecessor its key pins, so the reused branch reuses it verbatim.
        let invalidation = record.invalidation.clone();
        let view = self.queries.revisioned.parse_attempt_view(
            attempt_id,
            attempt,
            QueryStructuralWork::Parse(work),
        );
        self.diagnostics.select(view.clone());
        (record, view, execution, work, invalidation)
    }

    fn parse_staging_snapshot(
        &mut self,
        snapshot: &SourceSnapshot,
        successor: Option<(crate::ImportInputRevision, &Arc<[crate::ModuleRevision]>)>,
    ) -> (
        Result<Arc<ParsedProgram>, CompileErrors>,
        ParsedModulesWork,
        Option<ParseQueryRecord>,
    ) {
        // A successor stage MUST extend its verified predecessor: a failed
        // predecessor binding rejects the stage rather than silently falling
        // back to a full content-keyed build under successor authority.
        let prepared_successor = match successor {
            Some((revision, delta)) => match self.prepare_successor_parse(snapshot, delta) {
                Ok(prepared) => Some((revision, prepared)),
                Err(errors) => return (Err(errors), ParsedModulesWork::default(), None),
            },
            None => None,
        };
        let staged_successor = prepared_successor.is_some();
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        let (record, view, execution, work, _invalidation) = match prepared_successor {
            Some((revision, prepared)) => {
                self.execute_parse_query_successor(snapshot, revision, prepared, attempt_id)
            }
            None => {
                let order = snapshot
                    .files()
                    .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                    .collect::<Vec<_>>();
                self.parse_sources_materialized = self
                    .parse_sources_materialized
                    .saturating_add(order.len() as u64);
                let order = crate::shared_segments::SharedList::flat(order.into());
                self.select_diagnostic_presentation(Some(order.clone()));
                let presentation = DiagnosticAttemptProvenance::Presentation(order);
                self.execute_parse_query(snapshot, presentation, attempt_id)
            }
        };
        guard.started();
        let result = record.result.clone();
        guard.attach_diagnostics(record.diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        let retained = staged_successor.then(|| record.clone());
        (result, work, retained)
    }

    fn run_parse_update(
        &mut self,
        snapshot: &SourceSnapshot,
        presentation: DiagnosticAttemptProvenance,
    ) -> CompilerSessionUpdate {
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        let (record, view, execution, parse_work, invalidation) =
            self.execute_parse_query(snapshot, presentation, attempt_id);
        guard.started();
        self.metrics.update(parse_work, invalidation.clone());
        let result = record.result.clone();
        let diagnostics = record.diagnostics.clone();
        guard.attach_diagnostics(diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        self.refresh_retention_metrics();
        match result {
            Ok(candidate) => {
                if self.open_discovery.as_deref().is_some_and(|artifact| {
                    artifact.source_revision != *candidate.source_revision()
                }) {
                    self.open_discovery = None;
                }
                let exact = self.published.as_deref().is_some_and(|published| {
                    programs_are_pointer_equivalent(published, &candidate)
                });
                let downstream_invalidated = self.published.is_some() && !exact;
                if exact {
                    self.published_snapshot = Some(snapshot.clone());
                    CompilerSessionUpdate {
                        result: Ok(self.published.as_ref().unwrap().clone()),
                        work: parse_work,
                        #[cfg(test)]
                        invalidation,
                        downstream_invalidated: false,
                        diagnostics,
                    }
                } else {
                    self.metrics
                        .project_dependency_invalidations(downstream_invalidated);
                    self.published = Some(candidate.clone());
                    self.published_snapshot = Some(snapshot.clone());
                    CompilerSessionUpdate {
                        result: Ok(candidate),
                        work: parse_work,
                        #[cfg(test)]
                        invalidation,
                        downstream_invalidated,
                        diagnostics,
                    }
                }
            }
            Err(errors) => CompilerSessionUpdate {
                result: Err(errors),
                work: parse_work,
                #[cfg(test)]
                invalidation,
                downstream_invalidated: false,
                diagnostics,
            },
        }
    }

    /// The successor-close counterpart of [`Self::run_parse_update`]: adopts
    /// the successor parse terminal for semantic queries with the same
    /// publication bookkeeping, without re-running the whole-program
    /// content-keyed projection (RUE-1112). The candidate extends the retained
    /// predecessor by construction, so downstream invalidation follows from an
    /// existing publication rather than a module-table comparison.
    fn run_parse_update_successor(
        &mut self,
        snapshot: &SourceSnapshot,
        retained: ParseQueryRecord,
    ) -> CompilerSessionUpdate {
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        // Re-request the exact staged terminal: same key, same revision. The
        // stage's selection protects that terminal, so this reuses it without
        // publishing anything new; the recompute body republishes the retained
        // record verbatim only if the terminal were ever evicted.
        if let ParseQueryKey::Successor { segment, .. } = &retained.key {
            self.parse_key_entries_compared = self
                .parse_key_entries_compared
                .saturating_add(segment.len() as u64);
        }
        let key = retained.key.clone();
        let runtime_revision = retained.runtime_revision;
        let recompute = retained.clone();
        let attempt =
            self.queries
                .revisioned
                .request_parse(runtime_revision, attempt_id, key, |context| {
                    for module in &recompute.invalidation.added {
                        context.input(
                    crate::revisioned_query_database::RevisionedQueryDatabase::module_source_input(
                        module,
                    ),
                )?;
                    }
                    Ok(recompute.clone())
                });
        self.queries.revisioned.select_parse(&attempt);
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()));
        let record = match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => record.clone(),
            rue_query::QueryOutcome::Failure(_) => unreachable!("parse retains typed records"),
        };
        let execution = match attempt.execution() {
            rue_query::RequestExecution::Computed => QueryAttemptExecution::Computed,
            rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                self.reuse_diagnostics(record.diagnostics.clone());
                QueryAttemptExecution::Reused
            }
            rue_query::RequestExecution::Aborted => unreachable!(),
        };
        // The stage already accounted this terminal's parse work; re-selecting
        // it at close performs none.
        let parse_work = ParsedModulesWork::default();
        let invalidation = record.invalidation.clone();
        let view = self.queries.revisioned.parse_attempt_view(
            attempt_id,
            attempt,
            QueryStructuralWork::Parse(parse_work),
        );
        self.diagnostics.select(view.clone());
        guard.started();
        self.metrics.update(parse_work, invalidation.clone());
        let result = record.result.clone();
        let diagnostics = record.diagnostics.clone();
        guard.attach_diagnostics(diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        match result {
            Ok(candidate) => {
                if self.open_discovery.as_deref().is_some_and(|artifact| {
                    artifact.source_revision != *candidate.source_revision()
                }) {
                    self.open_discovery = None;
                }
                let downstream_invalidated = self.published.is_some();
                // The predecessor source leaf stays live: additive adoption
                // must not disappear it and transitively invalidate every
                // retained terminal that still correctly depends on it.
                self.metrics.project_dependency_invalidations(false);
                self.published = Some(candidate.clone());
                self.published_snapshot = Some(snapshot.clone());
                self.refresh_retention_metrics();
                CompilerSessionUpdate {
                    result: Ok(candidate),
                    work: parse_work,
                    #[cfg(test)]
                    invalidation,
                    downstream_invalidated,
                    diagnostics,
                }
            }
            Err(errors) => CompilerSessionUpdate {
                result: Err(errors),
                work: parse_work,
                #[cfg(test)]
                invalidation,
                downstream_invalidated: false,
                diagnostics,
            },
        }
    }

    /// Return the graph adopted by import discovery.
    ///
    /// Import-free direct sessions synthesize the uniquely valid empty graph;
    /// import-bearing sessions never reconstruct resolution from loaded paths.
    pub fn import_graph(
        &mut self,
        std_dir: Option<&str>,
    ) -> Result<Arc<CanonicalImportGraphOutput>, CompileErrors> {
        let mut guard = self.metrics.begin::<ImportsMetricsQuery>();
        let mut execution = QueryAttemptExecution::Rejected;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.import_graph_attempt(std_dir, &mut guard, &mut execution)
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        if execution == QueryAttemptExecution::Reused {
            execution = QueryAttemptExecution::Adopted;
        }
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        result
    }

    fn import_graph_attempt(
        &mut self,
        std_dir: Option<&str>,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
    ) -> Result<Arc<CanonicalImportGraphOutput>, CompileErrors> {
        self.require_closed_discovery()?;
        if let Some(committed) = self.committed_import_discovery_artifact() {
            let graph = committed
                .graph()
                .expect("closed-valid discovery revisions retain their canonical graph");
            if graph.input().std_dir.as_deref() != std_dir {
                return Err(CompileErrors::from(CompileError::without_span(
                    ErrorKind::InvalidCompilerInput(
                        "the requested standard-library context differs from the committed import discovery revision"
                            .into(),
                    ),
                )));
            }
            *execution = QueryAttemptExecution::Reused;
            return Ok(graph.clone());
        }
        let parsed = self.published.clone().ok_or_else(no_published_program)?;
        if !parsed.import_directives().is_empty() {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "import-bearing revisions require a committed discovery graph".into(),
                ),
            )));
        }
        let resolution = ModuleResolutionInputs::new(
            parsed.root().clone(),
            parsed
                .modules()
                .iter()
                .map(|module| crate::ModuleResolutionInput {
                    module: module.module_id().clone(),
                    physical_path: Arc::from(module.physical_path()),
                })
                .collect(),
        )
        .expect("published parsed modules have validated resolution inputs");
        let input = ImportGraphInputDescriptor {
            sources: parsed.source_revision().clone(),
            resolution,
            std_dir: std_dir.map(Arc::from),
        };
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let graph = crate::import_graph::import_free_canonical_graph(parsed.as_ref())?;
        let validation = validate_canonical_import_graph(&graph, &input.resolution);
        Ok(Arc::new(CanonicalImportGraphOutput {
            input,
            graph,
            validation,
        }))
    }

    pub(crate) fn merge(&mut self) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        let mut guard = self.metrics.begin::<MergeQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_work = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.merge_attempt(
                attempt_id,
                &mut guard,
                &mut execution,
                &mut origin,
                &mut attempt_work,
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        let structural = attempt_work
            .map(QueryStructuralWork::Merge)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.synchronize();
        result
    }

    fn merge_attempt(
        &mut self,
        _attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        _origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<CanonicalMergeWork>,
    ) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        self.require_closed_discovery()?;
        let parsed = self.published.clone().ok_or_else(no_published_program)?;
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let runtime_revision = self
            .queries
            .revisioned
            .last_good_parse_record()
            .expect("merge has a successful parse terminal")
            .runtime_revision;
        let projected_indexes = {
            let _span = tracing::info_span!("module_index_projection").entered();
            self.queries
                .revisioned
                .projected_module_indexes(runtime_revision, &parsed)
        };
        // Freeze the traversal work before the fallible duplicate/definition
        // checks so deterministic merge failures retain the work already done.
        *attempt_work = Some(CanonicalMergeWork {
            modules_visited: parsed.modules().len(),
            items_visited: parsed
                .modules()
                .iter()
                .map(|module| module.ast().items.len())
                .sum(),
            candidates_visited: projected_indexes.as_ref().map_or(0, |indexes| {
                indexes.iter().map(|index| index.definitions.len()).sum()
            }),
            ..CanonicalMergeWork::default()
        });
        guard.accrue(QueryStructuralWork::Merge(
            attempt_work.expect("merge prefix just installed"),
        ));
        let batch_order = self
            .batch_diagnostic_order
            .as_ref()
            .map(crate::shared_segments::SharedList::as_arc);
        let merged = {
            let _span = tracing::info_span!("canonical_merge").entered();
            projected_indexes
                .and_then(|indexes| {
                    merge_parsed_modules_reusing_indexes(
                        &parsed,
                        &indexes,
                        self.definition_shard_baseline.as_ref(),
                        batch_order.as_deref(),
                    )
                })
                .map(Arc::new)
        };
        if self.cancel_merge_at_commit_boundary() {
            guard.request_cancel();
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput("merge query canceled before commit".into()),
            )));
        }
        if let Ok(merged) = &merged {
            debug_assert_eq!(merged.ast().source_revision(), parsed.source_revision());
            *attempt_work = Some(merged.work());
            guard.accrue(QueryStructuralWork::Merge(merged.work()));
        }
        if let Ok(merged) = &merged {
            self.definition_shard_baseline = Some(merged.definitions().clone());
        }
        let source = self
            .published_snapshot
            .clone()
            .expect("published program retains source snapshot");
        let diagnostics = self.publish_diagnostics(
            &source,
            FrontendDiagnosticIdentity::Merge,
            merged.as_ref().err(),
            &[],
        );
        guard.attach_diagnostics(diagnostics.clone());
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        merged
    }

    /// Query the canonical RIR through an immutable, owner-retaining view.
    pub fn rir(&mut self) -> Result<Arc<crate::RirView>, CompileErrors> {
        self.canonical_rir().map(crate::RirView::new).map(Arc::new)
    }

    pub(crate) fn canonical_rir(&mut self) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        let mut guard = self.metrics.begin::<RirQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_work = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.rir_attempt(
                attempt_id,
                &mut guard,
                &mut execution,
                &mut origin,
                &mut attempt_work,
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        let structural = attempt_work
            .map(QueryStructuralWork::Rir)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.synchronize();
        result
    }

    fn rir_attempt(
        &mut self,
        _attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        _origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<CanonicalRirWork>,
    ) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        self.require_successful_import_diagnostics()?;
        let source = self
            .published
            .as_ref()
            .ok_or_else(no_published_program)?
            .source_revision()
            .clone();
        let merged = self.merge();
        let result = match &merged {
            Ok(merged) => {
                *execution = QueryAttemptExecution::Computed;
                guard.started();
                let revision = self
                    .queries
                    .revisioned
                    .last_good_parse_record()
                    .expect("published syntax has a successful parse terminal")
                    .runtime_revision;
                let module_ids = merged
                    .ast()
                    .modules()
                    .iter()
                    .map(|module| module.module_id().clone())
                    .collect::<Vec<_>>();
                let (module_rirs, query_work) = {
                    let _span = tracing::info_span!("module_rir_lowering").entered();
                    self.queries.revisioned.module_rirs(revision, module_ids)
                };
                match module_rirs {
                    Ok(modules) => {
                        let projected = {
                            let _span = tracing::info_span!("rir_projection").entered();
                            project_module_rirs_with_work(merged, &modules, query_work)
                        };
                        match projected {
                            Ok(rir) => {
                                let rir = Arc::new(rir);
                                *attempt_work = Some(rir.work());
                                guard.accrue(QueryStructuralWork::Rir(rir.work()));
                                Ok(rir)
                            }
                            Err((error, work)) => {
                                *attempt_work = Some(work);
                                guard.accrue(QueryStructuralWork::Rir(work));
                                Err(CompileErrors::from(error))
                            }
                        }
                    }
                    Err(errors) => {
                        *attempt_work = Some(query_work);
                        guard.accrue(QueryStructuralWork::Rir(query_work));
                        Err(errors)
                    }
                }
            }
            Err(errors) => Err(errors.clone()),
        };
        if let Ok(rir) = &result {
            debug_assert_eq!(rir.source_revision(), &source);
        }
        let source_snapshot = self
            .published_snapshot
            .clone()
            .expect("RIR query retains its exact source snapshot");
        let diagnostics = Arc::new(FrontendDiagnosticSnapshot {
            source: source_snapshot,
            stage: FrontendDiagnosticIdentity::Rir(source.clone()),
            provenance: DiagnosticAttemptProvenance::Canonical,
            errors: result
                .as_ref()
                .err()
                .map_or_else(|| Arc::from([]), |errors| errors.as_slice().to_vec().into()),
            warnings: Arc::from([]),
        });
        guard.attach_diagnostics(diagnostics.clone());
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        result
    }

    /// Analyze the current published revision without issuing stable definition IDs.
    /// Query semantic analysis and optimized CFGs through immutable views.
    pub fn semantic(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<crate::SemanticView>, CompileErrors> {
        if self.oracle_fault.take() == Some(crate::unstable::DifferentialOracleFault::Semantic) {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError("differential semantic fault".into()),
            )));
        }
        let owner = self.canonical_semantic(options)?;
        let rir = owner.rir_owner().clone();
        Ok(Arc::new(crate::SemanticView::new(owner, rir)))
    }

    pub(crate) fn canonical_semantic(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<CanonicalSemanticOutput>, CompileErrors> {
        match self
            .canonical_semantic_with_cancellation(options, rue_query::CancellationToken::new())
        {
            Ok(output) => Ok(output),
            Err(SemanticRequestControl::Compile(errors)) => Err(errors),
            // `canonical_semantic` is a stable no-filesystem entry: it cannot
            // acquire trusted toolchain modules, so it converts an unsatisfied
            // park to its own error result at this outer boundary (RUE-1112).
            // The park-aware host driver uses `semantic_or_toolchain_park`, which
            // surfaces the park distinctly and retries after acquisition.
            Err(SemanticRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                panic!("uncanceled semantic request aborted: {abort:?}")
            }
        }
    }

    fn rooted_body_graph_with_cancellation(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedBodyGraph, SemanticRequestControl> {
        self.require_successful_import_diagnostics()
            .map_err(SemanticRequestControl::Compile)?;
        let _imports = self
            .accepted_semantic_import_graph()
            .map_err(SemanticRequestControl::Compile)?;
        let program = self
            .published
            .clone()
            .ok_or_else(|| SemanticRequestControl::Compile(no_published_program()))?;
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .ok_or_else(|| SemanticRequestControl::Compile(no_published_program()))?;
        let modules = program
            .modules_iter()
            .map(|module| module.module_id().clone())
            .collect::<Vec<_>>();
        let projection = match self
            .queries
            .revisioned
            .projected_declaration_semantics_for_modules(
                revision,
                modules.iter().cloned(),
                options.target,
                &options.preview_features,
                cancellation.clone(),
            ) {
            Ok(projection) => projection,
            Err(crate::revisioned_query_database::SemanticNucleusBatchFailure::Query(abort)) => {
                return Err(SemanticRequestControl::Abort(abort));
            }
            Err(crate::revisioned_query_database::SemanticNucleusBatchFailure::Stable {
                declaration,
                failure,
            }) => {
                return Err(SemanticRequestControl::Compile(
                    semantic_nucleus_failure_diagnostics(
                        program.modules(),
                        declaration.as_ref(),
                        &failure,
                    ),
                ));
            }
        };
        let Some(main_declaration) = projection.declarations.iter().find(|declaration| {
            declaration.key.kind() == crate::StableDefinitionKind::Function
                && declaration.key.name() == "main"
                && declaration.key.module() == program.root()
        }) else {
            return Err(SemanticRequestControl::Compile(
                CompileError::without_span(ErrorKind::NoMainFunction).into(),
            ));
        };
        let crate::durable_semantics::DurableDeclarationPayload::Callable {
            parameters,
            result,
            ..
        } = &main_declaration.payload
        else {
            return Err(SemanticRequestControl::Compile(
                CompileError::without_span(ErrorKind::NoMainFunction).into(),
            ));
        };
        let invalid_main = if !parameters.is_empty() {
            Some("`main` must not declare parameters")
        } else if !matches!(
            result,
            crate::durable_semantics::DurableType::I32
                | crate::durable_semantics::DurableType::Unit
        ) {
            Some("`main` must return `i32` or `()`")
        } else {
            None
        };
        if let Some(reason) = invalid_main {
            let span = program.module(program.root()).and_then(|module| {
                module.ast().items.iter().find_map(|item| match item {
                    rue_parser::ast::Item::Function(function)
                        if module.resolve_raw_symbol(function.name.name) == "main" =>
                    {
                        Some(function.span)
                    }
                    _ => None,
                })
            });
            let kind = ErrorKind::InvalidMainSignature { reason };
            return Err(SemanticRequestControl::Compile(
                match span {
                    Some(span) => CompileError::new(kind, span),
                    None => CompileError::without_span(kind),
                }
                .into(),
            ));
        }

        let main = main_declaration.key.clone();
        let mut roots = BTreeSet::from([crate::FunctionInstanceKey::Definition(main.clone())]);
        roots.extend(
            projection
                .c_export_roots
                .iter()
                .cloned()
                .map(crate::FunctionInstanceKey::Definition),
        );
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: options.target,
            preview_features: StablePreviewFeatures::new(&options.preview_features),
        };
        // This compiler-owned consumer boundary includes retained-terminal
        // validation, query dispatch, deterministic terminal collection, and
        // the immediate work reduction. The timing layer records it
        // worker-locally, so the broad boundary does not serialize the query
        // runtime (RUE-1223).
        let _body_closure_collection_span =
            tracing::info_span!("body_closure_collection", phase = "semantic_analysis").entered();
        let request = self
            .queries
            .revisioned
            .body_closure(
                revision,
                crate::body_query::BodyClosureQueryKey {
                    modules: modules.into(),
                    roots: roots.into_iter().collect::<Vec<_>>().into(),
                    configuration: configuration.clone(),
                },
                cancellation.clone(),
            )
            .map_err(SemanticRequestControl::Abort)?;
        let closure_terminal = &request.terminal;
        let rue_query::QueryOutcome::Success(closure) = closure_terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        if let Some(park) = &closure.parked_toolchain {
            return Err(SemanticRequestControl::Parked(Box::new(park.clone())));
        }
        let mut work = crate::CanonicalSemanticWork::default();
        work.body_analysis.closure_bodies_visited = closure.bodies.len();
        for closure_body in closure.bodies.iter() {
            match request.execution_for(&closure_body.key) {
                rue_query::RequestExecution::Computed => {
                    work.body_analysis.body_analyses_computed += 1;
                    if request.was_retained(&closure_body.key) {
                        work.body_analysis.body_analyses_invalidated += 1;
                    }
                }
                rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                    work.body_analysis.body_analyses_reused += 1;
                }
                rue_query::RequestExecution::Aborted => unreachable!(
                    "a successful rooted body closure cannot retain an aborted body attempt"
                ),
            }
        }
        drop(_body_closure_collection_span);
        let mut errors = closure
            .scheduling_errors
            .iter()
            .flat_map(|(_, errors)| errors.iter().cloned())
            .collect::<Vec<_>>();
        if let Some(fatal) = &closure.fatal {
            let fatal_errors = match fatal {
                crate::body_query::BodyClosureFatal::DeclarationFailed {
                    declaration,
                    failure,
                } => semantic_nucleus_failure_diagnostics(
                    program.modules(),
                    declaration.as_ref(),
                    failure,
                ),
                crate::body_query::BodyClosureFatal::ProducerFailed { failure, .. } => {
                    semantic_nucleus_failure_diagnostics(program.modules(), None, failure)
                }
                crate::body_query::BodyClosureFatal::WellKnownOptionResolution {
                    failure, ..
                } => well_known_option_resolution_diagnostics(program.modules(), failure),
                other => CompileError::without_span(ErrorKind::InternalError(format!(
                    "rooted body closure failed: {other:?}"
                )))
                .into(),
            };
            errors.extend(fatal_errors.iter().cloned());
        }

        let mut anonymous = projection
            .anonymous_nominals
            .iter()
            .cloned()
            .map(|fact| (fact.identity.clone(), fact))
            .collect::<BTreeMap<_, _>>();
        for closure_body in closure.bodies.iter() {
            let rue_query::QueryOutcome::Success(bundle) = closure_body.bundle.outcome() else {
                unreachable!("BodyAnalysisBundle publishes typed values")
            };
            if matches!(
                bundle.transaction,
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            ) {
                let locator = self
                    .queries
                    .revisioned
                    .body_source_locator_projection(
                        revision,
                        closure_body.key.clone(),
                        cancellation.clone(),
                    )
                    .map_err(SemanticRequestControl::Abort)?;
                let rue_query::QueryOutcome::Success(locator) = locator.outcome() else {
                    unreachable!("BodySourceLocator publishes typed values")
                };
                let projected = crate::revisioned_query_database::project_transaction_diagnostics(
                    bundle.transaction.clone(),
                    locator.as_ref(),
                );
                if let crate::body_query::BodyTransaction::DeterministicFailure {
                    errors: body_errors,
                    ..
                } = projected
                {
                    errors.extend(body_errors.iter().cloned());
                }
            }
            if let crate::body_query::BodyTransaction::Success {
                produced_anonymous_nominals,
                consulted_anonymous_nominals,
                ..
            } = &bundle.transaction
            {
                for fact in produced_anonymous_nominals
                    .0
                    .iter()
                    .chain(consulted_anonymous_nominals.0.iter())
                {
                    anonymous.insert(fact.identity.clone(), fact.clone());
                }
            }
            if let Some(crate::body_query::ProducedAnonymous::Produced(produced)) =
                &bundle.produced_anonymous
            {
                for fact in produced.0.iter() {
                    anonymous.insert(fact.identity.clone(), fact.clone());
                }
            }
        }
        for nominal in anonymous.values() {
            let crate::durable_semantics::DurableAnonymousNominalShape::Struct { methods, .. } =
                &nominal.shape
            else {
                continue;
            };
            let mut names = BTreeSet::new();
            if let Some(duplicate) = methods
                .iter()
                .find(|method| !names.insert(method.name.clone()))
            {
                errors.push(CompileError::without_span(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "duplicate method `{}` in an anonymous struct",
                            duplicate.name
                        ),
                    },
                ));
            }
        }
        if !errors.is_empty() {
            return Err(SemanticRequestControl::Compile(errors.into()));
        }

        Ok(RootedBodyGraph {
            revision,
            configuration,
            declarations: projection.declarations,
            anonymous_nominals: anonymous.into_values().collect::<Vec<_>>().into(),
            declaration_dependencies: projection.dependencies,
            c_export_roots: projection.c_export_roots,
            modules: program.modules().to_vec().into(),
            main,
            closure: closure.clone(),
            work,
        })
    }

    fn rooted_warning_references(
        &mut self,
        graph: &RootedBodyGraph,
    ) -> Result<BTreeSet<crate::StableDefinitionKey>, CompileErrors> {
        let functions = graph
            .declarations
            .iter()
            .filter(|declaration| declaration.key.kind() == crate::StableDefinitionKind::Function)
            .map(|declaration| {
                (
                    (
                        declaration.key.module().clone(),
                        Arc::<str>::from(declaration.key.name()),
                    ),
                    declaration.key.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let module_bindings = graph
            .declarations
            .iter()
            .filter_map(|declaration| {
                let crate::durable_semantics::DurableDeclarationPayload::ModuleBinding { target } =
                    &declaration.payload
                else {
                    return None;
                };
                Some((
                    (
                        declaration.key.module().clone(),
                        Arc::<str>::from(declaration.key.name()),
                    ),
                    target.clone(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let callable_aliases = graph
            .declarations
            .iter()
            .filter_map(|declaration| {
                let crate::durable_semantics::DurableDeclarationPayload::Const {
                    value: crate::durable_semantics::DurableConstValue::Function(target),
                    ..
                } = &declaration.payload
                else {
                    return None;
                };
                Some((
                    (
                        declaration.key.module().clone(),
                        Arc::<str>::from(declaration.key.name()),
                    ),
                    target.clone(),
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let resolve_head =
            |caller: &crate::ModuleId,
             head: &crate::revisioned_query_database::WarningStaticCallHead| {
                let (name, qualifiers) = head.components.split_last()?;
                let mut module = head.module.clone().unwrap_or_else(|| caller.clone());
                for qualifier in qualifiers {
                    module = module_bindings.get(&(module, qualifier.clone()))?.clone();
                }
                callable_aliases
                    .get(&(module.clone(), name.clone()))
                    .cloned()
                    .or_else(|| functions.get(&(module, name.clone())).cloned())
            };

        #[cfg(test)]
        self.warning_reference_executions.clear();
        let mut referenced = BTreeSet::new();
        for declaration in graph
            .declarations
            .iter()
            .filter(|declaration| declaration.key.kind().owns_body())
        {
            let (execution, projected) = self
                .queries
                .revisioned
                .warning_body_references(
                    graph.revision,
                    crate::body_query::BodyQueryKey {
                        instance: crate::FunctionInstanceKey::Definition(declaration.key.clone()),
                        configuration: graph.configuration.clone(),
                    },
                    rue_query::CancellationToken::new(),
                )
                .map_err(|abort| {
                    CompileError::without_span(ErrorKind::InternalError(format!(
                        "warning body-reference query aborted: {abort:?}"
                    )))
                })?;
            #[cfg(not(test))]
            let _ = execution;
            #[cfg(test)]
            self.warning_reference_executions
                .push((declaration.key.clone(), execution));
            let heads = match projected {
                crate::revisioned_query_database::WarningBodyReferencesValue::Available(heads) => {
                    heads
                }
                crate::revisioned_query_database::WarningBodyReferencesValue::Failure(failure) => {
                    return Err(CompileError::without_span(ErrorKind::InternalError(format!(
                        "warning body-reference projection failed: {failure:?}"
                    )))
                    .into());
                }
            };
            referenced.extend(
                heads
                    .iter()
                    .filter_map(|head| resolve_head(declaration.key.module(), head)),
            );
        }
        Ok(referenced)
    }

    pub(crate) fn rooted_cfg(
        &mut self,
        options: &CompileOptions,
    ) -> Result<RootedCfgOutput, CompileErrors> {
        let graph = match self
            .rooted_body_graph_with_cancellation(options, rue_query::CancellationToken::new())
        {
            Ok(graph) => graph,
            Err(SemanticRequestControl::Compile(errors)) => return Err(errors),
            Err(SemanticRequestControl::Parked(park)) => {
                return Err(unresolved_toolchain_park_errors(&park));
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                panic!("uncanceled rooted CFG request aborted: {abort:?}")
            }
        };
        let mut work = graph.work;
        let mut identities = graph
            .closure
            .reached
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        identities.extend(
            graph
                .closure
                .demanded_drop_glue
                .iter()
                .cloned()
                .map(|owner| crate::FunctionInstanceKey::DropGlue(Box::new(owner))),
        );
        let main_identity = crate::FunctionInstanceKey::Definition(graph.main.clone());
        let callable_symbols = identities
            .iter()
            .cloned()
            .map(|identity| {
                let symbol = if identity == main_identity {
                    Arc::from("main")
                } else {
                    crate::local_semantic_materialization::rooted_callable_symbol(&identity)
                };
                (identity, symbol)
            })
            .collect::<BTreeMap<_, _>>();
        let mut cfg_inputs = Vec::with_capacity(identities.len());
        let warning_references = self.rooted_warning_references(&graph)?;
        let mut warnings = rooted_unused_function_warnings(&graph, &warning_references);
        for closure_body in graph.closure.bodies.iter() {
            let rue_query::QueryOutcome::Success(bundle) = closure_body.bundle.outcome() else {
                unreachable!("BodyAnalysisBundle publishes typed values")
            };
            let crate::body_query::BodyTransaction::Success { body, .. } = &bundle.transaction
            else {
                continue;
            };
            let locator = self
                .queries
                .revisioned
                .body_source_locator_projection(
                    graph.revision,
                    closure_body.key.clone(),
                    rue_query::CancellationToken::new(),
                )
                .map_err(|abort| {
                    CompileError::without_span(ErrorKind::InternalError(format!(
                        "body source locator query aborted: {abort:?}"
                    )))
                })?;
            let rue_query::QueryOutcome::Success(locator) = locator.outcome() else {
                unreachable!("BodySourceLocator publishes typed values")
            };
            let Some(locator) = locator.as_ref() else {
                return Err(CompileError::without_span(ErrorKind::InternalError(format!(
                    "reached body {:?} has no source locator",
                    closure_body.key.instance
                )))
                .into());
            };
            let body_span = match body.as_ref() {
                crate::body_query::CanonicalBody::Ordinary { .. } => {
                    rue_span::Span::with_file(locator.file_id, locator.body_start, locator.body_end)
                }
                crate::body_query::CanonicalBody::Anonymous { body_anchor, .. } => {
                    rue_span::Span::with_file(
                        locator.file_id,
                        locator.body_start + body_anchor.start,
                        locator.body_start + body_anchor.end,
                    )
                }
                crate::body_query::CanonicalBody::Specialization { .. } => {
                    rue_span::Span::with_file(
                        locator.file_id,
                        locator.declaration_start,
                        locator.declaration_end,
                    )
                }
            };
            let semantic_body = match body.as_ref() {
                crate::body_query::CanonicalBody::Ordinary { body, .. }
                | crate::body_query::CanonicalBody::Anonymous { body, .. }
                | crate::body_query::CanonicalBody::Specialization { body, .. } => body,
            };
            warnings.extend(import_semantic_body_warnings(semantic_body, body_span));
            // Comptime providers participate in body reachability because their
            // results can produce runtime declarations and anonymous nominals,
            // but they have no runtime CFG/codegen terminal of their own.
            if semantic_body.return_type == rue_air::SemanticImportType::ComptimeType {
                work.cfg.comptime_functions_filtered += 1;
                continue;
            }
            work.cfg.functions_considered += 1;
            let materialization =
                crate::local_semantic_materialization::select_materialization_facts(
                    &closure_body.key.instance,
                    semantic_body,
                    &graph.declarations,
                    &graph.anonymous_nominals,
                    &callable_symbols,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "CFG materialization fact selection failed: {error:?}"
                        )),
                        body_span,
                    )
                })?;
            cfg_inputs.push((
                closure_body.key.instance.clone(),
                crate::cfg_query::CfgSemanticInput::Body {
                    input: Arc::new(crate::cfg_query::CfgBodyInput {
                        function: closure_body.key.instance.clone(),
                        canonical: Arc::new((**body).clone()),
                        body_span,
                    }),
                    materialization: Arc::new(materialization),
                },
                body_span,
            ));
        }
        let fallback_span = cfg_inputs
            .iter()
            .find(|(identity, _, _)| identity == &main_identity)
            .map_or(rue_span::Span::default(), |(_, _, span)| *span);
        for (owner, facts) in graph.closure.demanded_drop_glue_plans.iter() {
            work.cfg.drop_glue_functions_synthesized += 1;
            let identity = crate::FunctionInstanceKey::DropGlue(Box::new(owner.clone()));
            let materialization =
                crate::local_semantic_materialization::select_drop_glue_materialization_facts(
                    owner,
                    facts,
                    &graph.declarations,
                    &graph.anonymous_nominals,
                    &callable_symbols,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "drop-glue materialization fact selection failed: {error:?}"
                        )),
                        fallback_span,
                    )
                })?;
            cfg_inputs.push((
                identity,
                crate::cfg_query::CfgSemanticInput::DropGlue {
                    owner: owner.clone(),
                    facts: Box::new(facts.clone()),
                    materialization: Arc::new(materialization),
                    body_span: fallback_span,
                },
                fallback_span,
            ));
        }
        cfg_inputs.sort_by(|left, right| left.0.cmp(&right.0));
        let mut raw_accessor_keys = std::collections::BTreeMap::new();
        for (function, semantic_input, _) in &cfg_inputs {
            raw_accessor_keys.insert(
                function.clone(),
                crate::cfg_query::CfgQueryKey::new(
                    function.clone(),
                    graph.configuration.clone(),
                    semantic_input.clone(),
                ),
            );
        }
        let accessor_subgraph = crate::cfg_query::accessor_cfg_subgraph(raw_accessor_keys)
            .map_err(|failure| {
                let (kind, span) = match failure {
                    crate::cfg_query::AccessorCfgSubgraphFailure::Missing(identity) => (
                        ErrorKind::InternalError(format!(
                            "accessor CFG dependency is missing: {identity:?}"
                        )),
                        fallback_span,
                    ),
                    crate::cfg_query::AccessorCfgSubgraphFailure::Cycle(identity) => {
                        let span = cfg_inputs
                            .iter()
                            .find(|(function, _, _)| function == &identity)
                            .map_or(fallback_span, |(_, _, body_span)| *body_span);
                        (
                            ErrorKind::AccessorRecursion {
                                method: crate::cfg_query::accessor_source_name(&identity),
                            },
                            span,
                        )
                    }
                };
                CompileError::new(kind, span)
            })?;
        let accessor_roots = accessor_subgraph.roots;
        let accessor_dependencies = accessor_subgraph.dependencies;
        let accessor_functions = accessor_subgraph.accessors;
        let cfg_requests = cfg_inputs
            .into_iter()
            .filter(|(function, _, _)| !accessor_functions.contains(function))
            .map(|(function, semantic_input, body_span)| {
                let cfg = crate::cfg_query::CfgQueryKey::new(
                    function.clone(),
                    graph.configuration.clone(),
                    accessor_roots
                        .get(&function)
                        .map(|key| key.semantic_input.clone())
                        .unwrap_or(semantic_input),
                );
                let optimized_cfg_key = crate::cfg_query::OptimizedCfgQueryKey::new(
                    cfg,
                    options.opt_level,
                    accessor_dependencies
                        .get(&function)
                        .cloned()
                        .unwrap_or_else(|| Arc::new([])),
                );
                (function, optimized_cfg_key, body_span)
            })
            .collect::<Vec<_>>();
        let optimized_keys = cfg_requests
            .iter()
            .map(|(_, key, _)| key.clone())
            .collect::<Vec<_>>()
            .into();
        let mut cfgs = Vec::with_capacity(cfg_requests.len());
        let mut backend_root = self.queries.revisioned.begin_backend_root();
        #[cfg(test)]
        self.rooted_cfg_executions.clear();
        let _cfg_collection_span =
            tracing::info_span!("optimized_cfg_collection", phase = "cfg_and_optimization")
                .entered();
        let (cfg_batch_key, attempt) = self.queries.revisioned.optimized_cfg_batch(
            graph.revision,
            optimized_keys,
            rue_query::CancellationToken::new(),
        );
        let batch_execution = attempt.execution();
        let executions = if batch_execution == rue_query::RequestExecution::Computed {
            let executions = attempt
                .nested_attempts()
                .iter()
                .filter(|attempt| attempt.node().family() == "compiler.optimized-cfg")
                .map(rue_query::NestedQueryAttempt::execution)
                .collect::<Vec<_>>();
            assert_eq!(
                executions.len(),
                cfg_requests.len(),
                "an evaluated optimized-CFG batch records one direct child per key"
            );
            executions
        } else {
            vec![batch_execution; cfg_requests.len()]
        };
        if let Some(terminal) = attempt.terminal() {
            self.queries.revisioned.retain_backend_optimized_cfg_batch(
                &mut backend_root,
                &cfg_batch_key,
                terminal,
            );
        }
        let batch = attempt.into_result().map_err(|abort| {
            CompileError::without_span(ErrorKind::InternalError(format!(
                "optimized CFG batch query aborted: {abort:?}"
            )))
        })?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("OptimizedCfgBatch publishes typed values")
        };
        assert_eq!(batch.values.len(), cfg_requests.len());
        for (((function, optimized_cfg_key, body_span), value), execution) in cfg_requests
            .into_iter()
            .zip(batch.values.iter())
            .zip(executions)
        {
            #[cfg(test)]
            self.rooted_cfg_executions
                .push((function.clone(), execution));
            match execution {
                rue_query::RequestExecution::Computed => {
                    work.cfg.cfg_builds_attempted += 1;
                    work.cfg.optimization_attempts += 1;
                }
                rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined => {
                    work.cfg.cfg_reuse_candidates += 1;
                    work.cfg.cfg_reuses += 1;
                }
                rue_query::RequestExecution::Aborted => {}
            }
            let record = match value {
                crate::cfg_query::CfgValue::Available(record) => {
                    if execution == rue_query::RequestExecution::Computed {
                        work.cfg.cfg_builds_succeeded += 1;
                        work.cfg.optimization_completions += 1;
                    }
                    record.clone()
                }
                crate::cfg_query::CfgValue::Failure {
                    errors,
                    body_span: old_span,
                } => {
                    return Err(crate::cfg_query::import_errors(
                        errors, *old_span, body_span,
                    ));
                }
            };
            warnings.extend(crate::cfg_query::import_warnings(
                &record.materialization_warnings,
                record.body_span,
                body_span,
            ));
            warnings.extend(crate::cfg_query::import_warnings(
                &record.warnings,
                record.body_span,
                body_span,
            ));
            cfgs.push(RootedCfgUnit {
                function,
                optimized_cfg_key,
                record,
                body_span,
            });
        }
        drop(_cfg_collection_span);

        // Preserve the canonical backend/presentation order independently of
        // the query scheduling order. Function-instance identity is the right
        // key for query work, while machine symbols are the established public
        // ordering for MIR, assembly, and object-image consumers.
        cfgs.sort_by(|left, right| {
            left.record
                .codegen
                .defined_symbol
                .cmp(&right.record.codegen.defined_symbol)
        });

        warnings.sort_by(|left, right| {
            let key = |warning: &CompileWarning| {
                let span = warning.span();
                let module = span
                    .and_then(|span| {
                        graph
                            .modules
                            .iter()
                            .find(|module| module.file_id() == span.file_id)
                    })
                    .map(|module| module.module_id().as_str())
                    .unwrap_or("");
                (
                    module,
                    span.map(|span| span.start).unwrap_or(0),
                    span.map(|span| span.end).unwrap_or(0),
                    warning.to_string(),
                    format!("{:?}", warning.diagnostic()),
                )
            };
            key(left).cmp(&key(right))
        });
        warnings.dedup();

        Ok(RootedCfgOutput {
            graph,
            cfgs,
            warnings,
            work,
            backend_root,
        })
    }

    pub(crate) fn rooted_codegen(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<RootedCodegenOutput, CompileErrors> {
        let RootedCfgOutput {
            graph,
            cfgs,
            warnings,
            work,
            mut backend_root,
        } = self.rooted_cfg(options)?;

        let codegen_keys = cfgs
            .iter()
            .map(|cfg| {
                crate::codegen_query::CodegenUnitQueryKey::new(
                    cfg.optimized_cfg_key.clone(),
                    options.target,
                    request,
                    options.opt_level,
                )
            })
            .collect::<Vec<_>>()
            .into();
        let mut units = Vec::with_capacity(cfgs.len());
        #[cfg(test)]
        {
            self.codegen_executions.clear();
            self.codegen_attempt_work.clear();
            self.codegen_collections = 0;
        }
        let _codegen_collection_span =
            tracing::info_span!("codegen_collection", phase = "backend").entered();
        let (codegen_batch_key, attempt) = self.queries.revisioned.codegen_unit_batch(
            graph.revision,
            codegen_keys,
            rue_query::CancellationToken::new(),
        );
        #[cfg(test)]
        let batch_execution = attempt.execution();
        #[cfg(test)]
        let child_attempts = if batch_execution == rue_query::RequestExecution::Computed {
            let attempts = attempt
                .nested_attempts()
                .iter()
                .filter(|attempt| attempt.node().family() == "compiler.codegen-unit")
                .map(|attempt| (attempt.execution(), attempt.work().to_vec()))
                .collect::<Vec<_>>();
            assert_eq!(
                attempts.len(),
                cfgs.len(),
                "an evaluated CodegenUnit batch records one direct child per key"
            );
            Some(attempts)
        } else {
            None
        };
        if let Some(terminal) = attempt.terminal() {
            self.queries.revisioned.retain_backend_codegen_batch(
                &mut backend_root,
                &codegen_batch_key,
                terminal,
            );
        }
        let batch = attempt.into_result().map_err(|abort| {
            CompileError::without_span(ErrorKind::InternalError(format!(
                "codegen batch query aborted: {abort:?}"
            )))
        })?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("CodegenUnitBatch publishes typed terminals")
        };
        assert_eq!(batch.values.len(), cfgs.len());
        for (index, (cfg, value)) in cfgs.iter().zip(batch.values.iter()).enumerate() {
            #[cfg(not(test))]
            let _ = index;
            #[cfg(test)]
            let execution = child_attempts
                .as_ref()
                .map_or(batch_execution, |attempts| attempts[index].0);
            #[cfg(test)]
            {
                self.codegen_executions
                    .push((cfg.function.clone(), execution));
                self.codegen_attempt_work.push((
                    cfg.function.clone(),
                    child_attempts
                        .as_ref()
                        .map_or_else(Vec::new, |attempts| attempts[index].1.clone()),
                ));
            }
            match value {
                crate::codegen_query::CodegenUnitValue::Available(unit) => {
                    units.push(crate::codegen_query::CollectedCodegenUnit {
                        function: cfg.function.clone(),
                        unit: unit.clone(),
                    });
                    #[cfg(test)]
                    {
                        self.codegen_collections += 1;
                    }
                }
                crate::codegen_query::CodegenUnitValue::Failure(errors) => {
                    return Err(errors.clone());
                }
            }
        }
        drop(_codegen_collection_span);
        let export_roots = graph
            .c_export_roots
            .iter()
            .cloned()
            .map(crate::FunctionInstanceKey::Definition)
            .collect::<BTreeSet<_>>();
        let exports = cfgs
            .iter()
            .filter(|cfg| export_roots.contains(&cfg.function))
            .map(|cfg| {
                let mut param_types =
                    vec![rue_air::Type::I64; cfg.record.cfg.num_params() as usize];
                for block in cfg.record.cfg.blocks() {
                    for &value in &block.insts {
                        let instruction = cfg.record.cfg.get_inst(value);
                        if let rue_cfg::CfgInstData::Param { index } = instruction.data
                            && let Some(slot) = param_types.get_mut(index as usize)
                        {
                            *slot = instruction.ty;
                        }
                    }
                }
                crate::program_image_plan::RootedExportThunk {
                    function: cfg.function.clone(),
                    exported_symbol: match &cfg.function {
                        crate::FunctionInstanceKey::Definition(key) => key.name().to_owned(),
                        _ => unreachable!("C export roots are source definitions"),
                    },
                    native_symbol: cfg.record.codegen.defined_symbol.to_string(),
                    param_types,
                }
            })
            .collect();
        self.queries
            .revisioned
            .publish_backend_root(graph.revision, backend_root, codegen_batch_key)
            .map_err(|abort| {
                CompileError::without_span(ErrorKind::InternalError(format!(
                    "backend root publication aborted: {abort:?}"
                )))
            })?;
        Ok(RootedCodegenOutput {
            units,
            cfgs,
            exports,
            warnings,
            work,
        })
    }

    /// Collect the canonical per-function backend terminals for one semantic
    /// result. This is deliberately a deterministic adapter: `CodegenUnit`
    /// owns lowering, allocation, scheduling, emission, and requested
    /// presentation projections; callers only order and project terminals.
    #[cfg(test)]
    pub(crate) fn codegen_products(
        &mut self,
        semantic: &crate::CanonicalSemanticOutput,
        options: &crate::CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<Vec<crate::backend::FunctionBackendProduct>, crate::CompileErrors> {
        Ok(self
            .codegen_units(semantic, options, request)?
            .into_iter()
            .map(|collected| collected.unit.backend_product())
            .collect())
    }

    /// Collect reached canonical codegen terminals without immediately
    /// collapsing them into the historical backend-product representation.
    /// Object and link consumers aggregate this exact result in a
    /// `ProgramImagePlan`; presentation consumers may still use the thin
    /// `codegen_products` projection above. RUE-1217 owns replacing this
    /// remaining semantic root enumeration with query-native image roots.
    #[cfg(test)]
    pub(crate) fn codegen_units(
        &mut self,
        semantic: &crate::CanonicalSemanticOutput,
        options: &crate::CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<Vec<crate::codegen_query::CollectedCodegenUnit>, crate::CompileErrors> {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .ok_or_else(|| {
                crate::CompileErrors::from(crate::CompileError::without_span(
                    crate::ErrorKind::InvalidCompilerInput(
                        "code generation requires a published semantic revision".into(),
                    ),
                ))
            })?;
        let mut units = Vec::with_capacity(semantic.functions().len());
        #[cfg(test)]
        {
            self.codegen_executions.clear();
            self.codegen_attempt_work.clear();
            self.codegen_collections = 0;
        }
        for function in semantic.functions() {
            let attempt = self
                .queries
                .revisioned
                .codegen_unit(
                    revision,
                    function.optimized_cfg_key.clone(),
                    options.target,
                    request,
                    options.opt_level,
                    rue_query::CancellationToken::new(),
                )
                .map_err(|abort| {
                    crate::CompileErrors::from(crate::CompileError::without_span(
                        crate::ErrorKind::InternalError(format!(
                            "codegen query aborted: {abort:?}"
                        )),
                    ))
                })?;
            #[cfg(test)]
            {
                self.codegen_executions
                    .push((function.semantic_identity.clone(), attempt.execution()));
                self.codegen_attempt_work
                    .push((function.semantic_identity.clone(), attempt.work().to_vec()));
            }
            let terminal = attempt.into_result().map_err(|abort| {
                crate::CompileErrors::from(crate::CompileError::without_span(
                    crate::ErrorKind::InternalError(format!("codegen query aborted: {abort:?}")),
                ))
            })?;
            let rue_query::QueryOutcome::Success(unit) = terminal.outcome() else {
                unreachable!("CodegenUnit publishes typed terminals")
            };
            match unit {
                crate::codegen_query::CodegenUnitValue::Available(unit) => {
                    units.push(crate::codegen_query::CollectedCodegenUnit {
                        function: function.semantic_identity.clone(),
                        unit: unit.clone(),
                    });
                    #[cfg(test)]
                    {
                        self.codegen_collections += 1;
                    }
                }
                crate::codegen_query::CodegenUnitValue::Failure(errors) => {
                    return Err(errors.clone());
                }
            }
        }
        Ok(units)
    }

    #[cfg(test)]
    pub(crate) fn codegen_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.codegen_executions
    }

    #[cfg(test)]
    pub(crate) fn rooted_cfg_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.rooted_cfg_executions
    }

    #[cfg(test)]
    pub(crate) fn warning_reference_executions(
        &self,
    ) -> &[(crate::StableDefinitionKey, rue_query::RequestExecution)] {
        &self.warning_reference_executions
    }

    #[cfg(test)]
    pub(crate) fn codegen_attempt_work(
        &self,
    ) -> &[(crate::FunctionInstanceKey, Vec<(std::sync::Arc<str>, u64)>)] {
        &self.codegen_attempt_work
    }

    #[cfg(test)]
    pub(crate) fn codegen_collections(&self) -> usize {
        self.codegen_collections
    }

    #[cfg(test)]
    pub(crate) fn backend_root_metrics(
        &self,
    ) -> crate::revisioned_query_database::PublishedBackendRootMetrics {
        self.queries.revisioned.backend_root_metrics_for_test()
    }

    #[cfg(test)]
    pub(crate) fn backend_cfg_key_is_retained(&self, key: &crate::cfg_query::CfgQueryKey) -> bool {
        self.queries
            .revisioned
            .backend_cfg_key_is_retained_for_test(key)
    }

    #[cfg(test)]
    pub(crate) fn query_evictions_for_test(&self) -> u64 {
        self.queries.revisioned.query_evictions_for_test()
    }

    /// Analyze the current revision, surfacing an unsatisfied trusted-toolchain
    /// park distinctly instead of converting it to an error (RUE-1112). This
    /// is the rooted, park-aware entry the host compile driver retries: on
    /// [`SemanticParkOutcome::Parked`] it acquires exactly the demanded modules,
    /// publishes a successor, and calls this again.
    pub(crate) fn semantic_or_toolchain_park(
        &mut self,
        options: &CompileOptions,
    ) -> SemanticParkOutcome {
        match self
            .canonical_semantic_with_cancellation(options, rue_query::CancellationToken::new())
        {
            Ok(owner) => {
                let rir = owner.rir_owner().clone();
                SemanticParkOutcome::Ready(Arc::new(crate::SemanticView::new(owner, rir)))
            }
            Err(SemanticRequestControl::Compile(errors)) => SemanticParkOutcome::Errors(errors),
            Err(SemanticRequestControl::Parked(park)) => {
                self.attach_toolchain_park(&park);
                SemanticParkOutcome::Parked(park)
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                panic!("uncanceled semantic request aborted: {abort:?}")
            }
        }
    }

    pub(crate) fn rooted_or_toolchain_park(
        &mut self,
        options: &CompileOptions,
    ) -> RootedParkOutcome {
        match self.rooted_body_graph_with_cancellation(options, rue_query::CancellationToken::new())
        {
            Ok(_) => RootedParkOutcome::Ready,
            Err(SemanticRequestControl::Compile(errors)) => RootedParkOutcome::Errors(errors),
            Err(SemanticRequestControl::Parked(park)) => {
                self.attach_toolchain_park(&park);
                RootedParkOutcome::Parked(park)
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                panic!("uncanceled rooted body-closure request aborted: {abort:?}")
            }
        }
    }

    fn attach_toolchain_park(&mut self, park: &crate::ParkedToolchainModules) {
        // Atomically attach this rooted park's exact sorted missing-demand set
        // to the outstanding closed continuation, making it authorizing
        // (RUE-1112).
        if let Some(state) = self.continuation.as_mut() {
            let mut demands = park.demands().to_vec();
            demands.sort();
            demands.dedup();
            state.attached_demands = Some(Arc::from(demands));
        }
    }

    fn canonical_semantic_with_cancellation(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<Arc<CanonicalSemanticOutput>, SemanticRequestControl> {
        let mut guard = self.metrics.begin::<SemanticQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_record = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.semantic_attempt(
                options,
                &cancellation,
                attempt_id,
                &mut guard,
                &mut execution,
                &mut origin,
                &mut attempt_record,
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        // A trusted-toolchain park exits the attempt before the body transaction
        // publishes a terminal, leaving the semantic query in-flight. Clear that
        // in-flight attempt exactly as an abort would, so the host driver's retry
        // (after acquiring the demanded module and re-closing on a new revision)
        // begins a fresh selection instead of colliding with a stale computing
        // key (RUE-1112).
        if matches!(
            result,
            Err(SemanticRequestControl::Abort(_)) | Err(SemanticRequestControl::Parked(_))
        ) {
            guard.request_cancel();
        }
        let structural = attempt_record
            .clone()
            .map(Box::new)
            .map(QueryStructuralWork::Semantic)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.publish_semantic(0);
        self.metrics.synchronize();
        result
    }

    fn semantic_attempt(
        &mut self,
        options: &CompileOptions,
        cancellation: &rue_query::CancellationToken,
        _attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        _origin: &mut Option<AttemptId>,
        attempt_record: &mut Option<SemanticQueryRecord>,
    ) -> Result<Arc<CanonicalSemanticOutput>, SemanticRequestControl> {
        if cancellation.is_canceled() {
            return Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        self.require_successful_import_diagnostics()?;
        let imports = self.accepted_semantic_import_graph()?;
        if cancellation.is_canceled() {
            return Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        let source = self
            .published_snapshot
            .clone()
            .expect("semantic query retains its exact source snapshot");
        let input = CodegenInputDescriptor {
            semantic: SemanticInputDescriptor::new(
                &source,
                options.target,
                &options.preview_features,
            ),
            opt_level: options.opt_level.into(),
        };
        let rir_result = self.canonical_rir();
        if cancellation.is_canceled() {
            return Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        let rir = match rir_result {
            Ok(rir) => rir,
            Err(errors) => {
                let record = SemanticQueryRecord {
                    input: input.clone(),
                    work: CanonicalSemanticWork::default(),
                    failure: None,
                    failed: true,
                };
                guard.accrue(QueryStructuralWork::Semantic(Box::new(record.clone())));
                *attempt_record = Some(record);
                let diagnostics = self.publish_diagnostics(
                    &source,
                    FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(
                        &input,
                        imports.clone(),
                    )),
                    Some(&errors),
                    &[],
                );
                guard.attach_diagnostics(diagnostics.clone());
                self.diagnostics.select_snapshot(&diagnostics);
                self.refresh_retention_metrics();
                return Err(SemanticRequestControl::Compile(errors));
            }
        };
        let merged = self.merge().map_err(SemanticRequestControl::Compile)?;
        #[cfg(test)]
        if std::mem::take(&mut self.cancel_semantic_after_dependency) {
            cancellation.cancel();
        }
        if cancellation.is_canceled() {
            return Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let runtime_revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("semantic preparation observes a published source/import revision");
        let declaration_shells_span =
            tracing::info_span!("declaration_shells", phase = "semantic_analysis").entered();
        let query_shells = {
            let _span = tracing::info_span!("declaration_shell_projection").entered();
            self.queries.revisioned.projected_declaration_shells(
                runtime_revision,
                merged.ast(),
                cancellation.clone(),
            )
        };
        let mut prepared = match query_shells {
            Ok(query_shells) => {
                let _span = tracing::info_span!("declaration_shell_prepare").entered();
                prepare_query_declaration_shells(&merged, &rir, options, &imports, &query_shells)
            }
            Err(crate::revisioned_query_database::DeclarationShellBatchFailure::Query(abort)) => {
                return Err(SemanticRequestControl::Abort(abort));
            }
            Err(crate::revisioned_query_database::DeclarationShellBatchFailure::Stable(
                failure,
            )) => Err(CanonicalSemanticFailure::declaration(
                declaration_shell_failure_diagnostics(merged.ast().modules(), &failure),
                CanonicalSemanticWork::default(),
            )),
        };
        drop(declaration_shells_span);
        let declaration_semantics = {
            let _span = tracing::info_span!(
                "declaration_semantics_projection",
                phase = "semantic_analysis"
            )
            .entered();
            self.queries.revisioned.projected_declaration_semantics(
                runtime_revision,
                merged.ast(),
                options.target,
                &options.preview_features,
                cancellation.clone(),
            )
        };
        let (
            query_declarations,
            query_anonymous_nominals,
            query_declaration_dependencies,
            query_c_export_roots,
        ) = match declaration_semantics {
            Ok(projection) => (
                Some(projection.declarations),
                projection.anonymous_nominals,
                projection.dependencies,
                projection.c_export_roots,
            ),
            Err(crate::revisioned_query_database::SemanticNucleusBatchFailure::Query(abort)) => {
                return Err(SemanticRequestControl::Abort(abort));
            }
            Err(crate::revisioned_query_database::SemanticNucleusBatchFailure::Stable {
                declaration,
                failure,
            }) => {
                let work = match &prepared {
                    Ok(prepared) => CanonicalSemanticWork {
                        declaration_index: prepared.declaration_index_work(),
                        ..CanonicalSemanticWork::default()
                    },
                    Err(preparation_failure) => preparation_failure.failure.work,
                };
                prepared = Err(CanonicalSemanticFailure::declaration(
                    semantic_nucleus_failure_diagnostics(
                        merged.ast().modules(),
                        declaration.as_ref(),
                        &failure,
                    ),
                    work,
                ));
                (None, Arc::from([]), Arc::from([]), Arc::from([]))
            }
        };
        if let (Some(declarations), Ok(prepared_definitions)) =
            (query_declarations.as_deref(), prepared.as_ref())
            && let Some(main) = prepared_definitions
                .definitions()
                .definitions()
                .iter()
                .find(|record| {
                    let key = record.stable_key();
                    key.kind() == crate::StableDefinitionKind::Function
                        && key.name() == "main"
                        && key.module() == merged.ast().root()
                })
            && let Some(declaration) = declarations
                .iter()
                .find(|declaration| declaration.key == *main.stable_key())
            && let crate::durable_semantics::DurableDeclarationPayload::Callable {
                parameters,
                result,
                ..
            } = &declaration.payload
        {
            let reason = if !parameters.is_empty() {
                Some("`main` must not declare parameters")
            } else if !matches!(
                result,
                crate::durable_semantics::DurableType::I32
                    | crate::durable_semantics::DurableType::Unit
            ) {
                Some("`main` must return `i32` or `()`")
            } else {
                None
            };
            if let Some(reason) = reason {
                let work = CanonicalSemanticWork {
                    declaration_index: prepared_definitions.declaration_index_work(),
                    ..CanonicalSemanticWork::default()
                };
                prepared = Err(CanonicalSemanticFailure::declaration(
                    CompileErrors::from(CompileError::new(
                        ErrorKind::InvalidMainSignature { reason },
                        main.declaration_span(),
                    )),
                    work,
                ));
            }
        }
        let durable_body_work = crate::DurableBodyWork::default();
        let mut durable_body_candidates = Vec::new();
        let mut durable_specialized_body_candidates = Vec::new();
        let mut durable_anonymous_body_candidates = Vec::new();
        let mut queried_body_work = rue_air::BodyAnalysisWork::default();
        let mut body_produced_anonymous = BTreeMap::new();
        let mut body_consulted_anonymous = BTreeMap::new();
        let mut body_query_errors = BTreeMap::new();
        let mut body_query_reference_cache = BTreeMap::new();
        let mut demanded_drop_glue: Arc<[crate::TypeInstanceKey]> = Arc::from([]);
        let mut demanded_drop_glue_plans: Arc<
            [(crate::TypeInstanceKey, crate::type_queries::DropGlueFacts)],
        > = Arc::from([]);
        // RUE-1027 production boundary: derive the reached callable frontier
        // serially from the references owned by each body transaction. Ordinary
        // call recursion is worklist state, never a query dependency cycle.
        // RUE-1028 replaces this deterministic coordinator with database-owned
        // reachability and parallel frontier scheduling.
        if let Ok(prepared_definitions) = prepared.as_ref() {
            let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: options.target,
                preview_features: StablePreviewFeatures::new(&options.preview_features),
            };
            let mut pending = std::collections::BTreeSet::new();
            for record in prepared_definitions.definitions().definitions() {
                let stable = record.stable_key();
                if stable.kind() == crate::StableDefinitionKind::Function
                    && stable.name() == "main"
                    && stable.module() == merged.ast().root()
                {
                    pending.insert(crate::FunctionInstanceKey::Definition(stable.clone()));
                }
            }
            pending.extend(
                query_c_export_roots
                    .iter()
                    .cloned()
                    .map(crate::FunctionInstanceKey::Definition),
            );
            let roots = pending;
            let closure_request = match self.queries.revisioned.body_closure(
                runtime_revision,
                crate::body_query::BodyClosureQueryKey {
                    modules: merged
                        .ast()
                        .modules()
                        .iter()
                        .map(|module| module.module_id().clone())
                        .collect::<Vec<_>>()
                        .into(),
                    roots: roots.iter().cloned().collect::<Vec<_>>().into(),
                    configuration: configuration.clone(),
                },
                cancellation.clone(),
            ) {
                Ok(request) => request,
                Err(abort) => return Err(SemanticRequestControl::Abort(abort)),
            };
            let closure_terminal = &closure_request.terminal;
            let rue_query::QueryOutcome::Success(closure_output) = closure_terminal.outcome()
            else {
                unreachable!("BodyClosure publishes typed values")
            };
            demanded_drop_glue = closure_output.demanded_drop_glue.clone();
            demanded_drop_glue_plans = closure_output.demanded_drop_glue_plans.clone();
            if let Some(park) = &closure_output.parked_toolchain {
                return Err(SemanticRequestControl::Parked(Box::new(park.clone())));
            }
            body_query_errors.extend(closure_output.scheduling_errors.iter().cloned());
            if let Some(fatal) = &closure_output.fatal {
                let (instance, errors) = match fatal {
                    crate::body_query::BodyClosureFatal::DeclarationFailed {
                        declaration,
                        failure,
                    } => (
                        roots.iter().next().cloned(),
                        semantic_nucleus_failure_diagnostics(
                            merged.ast().modules(),
                            declaration.as_ref(),
                            failure,
                        ),
                    ),
                    crate::body_query::BodyClosureFatal::BodyAvailability { instance, detail } => (
                        Some(instance.clone()),
                        crate::CompileErrors::from(crate::CompileError::without_span(
                            rue_error::ErrorKind::InternalError(format!(
                                "body availability was incomplete for {instance:?}: {detail}"
                            )),
                        )),
                    ),
                    crate::body_query::BodyClosureFatal::ProducerFailed { instance, failure } => (
                        Some(instance.clone()),
                        semantic_nucleus_failure_diagnostics(merged.ast().modules(), None, failure),
                    ),
                    crate::body_query::BodyClosureFatal::WellKnownOptionResolution {
                        instance,
                        failure,
                    } => (
                        Some(instance.clone()),
                        well_known_option_resolution_diagnostics(merged.ast().modules(), failure),
                    ),
                    crate::body_query::BodyClosureFatal::TypeQuery { ty, detail } => (
                        roots.iter().next().cloned(),
                        crate::CompileErrors::from(crate::CompileError::without_span(
                            rue_error::ErrorKind::InternalError(format!(
                                "canonical type query failed for {ty:?}: {detail}"
                            )),
                        )),
                    ),
                    crate::body_query::BodyClosureFatal::AnonymousDigestCollision {
                        digest,
                        first,
                        second,
                    } => (
                        roots.iter().next().cloned(),
                        crate::CompileErrors::from(crate::CompileError::without_span(
                            rue_error::ErrorKind::InternalError(format!(
                                "stable anonymous symbol digest {digest:032x} is shared by distinct \
                                 producer-nominal identities {first:?} and {second:?}"
                            )),
                        )),
                    ),
                };
                if let Some(instance) = instance {
                    body_query_errors.insert(instance, errors);
                } else {
                    return Err(SemanticRequestControl::Compile(errors));
                }
            }
            body_query_reference_cache.clear();
            for closure_body in closure_output.bodies.iter() {
                let instance = &closure_body.key.instance;
                let key = &closure_body.key;
                let rue_query::QueryOutcome::Success(analysis) = closure_body.bundle.outcome()
                else {
                    unreachable!("BodyAnalysisBundle publishes typed values")
                };
                let needs_source_locator = match &analysis.transaction {
                    crate::body_query::BodyTransaction::DeterministicFailure { .. } => true,
                    crate::body_query::BodyTransaction::Success { body, .. } => matches!(
                        body.as_ref(),
                        crate::body_query::CanonicalBody::Anonymous { .. }
                    ),
                    crate::body_query::BodyTransaction::Control(_) => false,
                };
                let current_source_locator = if needs_source_locator {
                    let locator = self
                        .queries
                        .revisioned
                        .body_source_locator_projection(
                            runtime_revision,
                            key.clone(),
                            cancellation.clone(),
                        )
                        .map_err(SemanticRequestControl::Abort)?;
                    let rue_query::QueryOutcome::Success(locator) = locator.outcome() else {
                        unreachable!("BodySourceLocator publishes typed values")
                    };
                    locator.clone()
                } else {
                    None
                };
                let projected_transaction;
                let transaction = if matches!(
                    &analysis.transaction,
                    crate::body_query::BodyTransaction::DeterministicFailure { .. }
                ) {
                    projected_transaction =
                        crate::revisioned_query_database::project_transaction_diagnostics(
                            analysis.transaction.clone(),
                            current_source_locator.as_ref(),
                        );
                    &projected_transaction
                } else {
                    &analysis.transaction
                };
                let computed = matches!(
                    closure_request.execution_for(key),
                    rue_query::RequestExecution::Computed
                );
                let had_retained_body = closure_request.was_retained(key);
                if computed {
                    queried_body_work.body_analyses_computed += 1;
                } else {
                    queried_body_work.body_analyses_reused += 1;
                }
                if computed {
                    let specialized =
                        matches!(instance, crate::FunctionInstanceKey::Specialization { .. });
                    queried_body_work.bodies_attempted += 1;
                    if had_retained_body {
                        queried_body_work.body_analyses_invalidated += 1;
                    }
                    if specialized {
                        queried_body_work.specialized_bodies_attempted += 1;
                    }
                    match transaction {
                        crate::body_query::BodyTransaction::Success { body, .. } => {
                            queried_body_work.bodies_succeeded += 1;
                            let body = match body.as_ref() {
                                crate::body_query::CanonicalBody::Ordinary { body, .. }
                                | crate::body_query::CanonicalBody::Anonymous { body, .. }
                                | crate::body_query::CanonicalBody::Specialization {
                                    body, ..
                                } => body,
                            };
                            queried_body_work.air_instructions_produced += body.instructions.len();
                            queried_body_work.local_strings_produced += body.strings.len();
                            if specialized {
                                queried_body_work.specialized_bodies_succeeded += 1;
                                queried_body_work.specialized_body_exports_attempted += 1;
                                queried_body_work.specialized_body_exports_succeeded += 1;
                                queried_body_work.specialized_body_export_instructions_emitted +=
                                    body.instructions.len();
                                queried_body_work.specialized_body_export_places_emitted +=
                                    body.places.len();
                                queried_body_work.specialized_body_export_strings_emitted +=
                                    body.strings.len();
                            } else {
                                queried_body_work.ordinary_body_exports_attempted += 1;
                                queried_body_work.ordinary_body_exports_succeeded += 1;
                                queried_body_work.ordinary_body_export_instructions_emitted +=
                                    body.instructions.len();
                                queried_body_work.ordinary_body_export_places_emitted +=
                                    body.places.len();
                                queried_body_work.ordinary_body_export_strings_emitted +=
                                    body.strings.len();
                            }
                        }
                        crate::body_query::BodyTransaction::DeterministicFailure { .. } => {
                            queried_body_work.bodies_failed += 1;
                            if specialized {
                                queried_body_work.specialized_bodies_failed += 1;
                            }
                        }
                        crate::body_query::BodyTransaction::Control(_) => {
                            unreachable!("body closure unwraps transaction control")
                        }
                    }
                }
                body_query_reference_cache
                    .insert(instance.clone(), transaction.references().clone());
                if let crate::body_query::BodyTransaction::DeterministicFailure { errors, .. } =
                    transaction
                {
                    body_query_errors.insert(instance.clone(), errors.clone());
                    continue;
                }
                let crate::body_query::BodyTransaction::Success {
                    body,
                    consulted_anonymous_nominals,
                    ..
                } = transaction
                else {
                    unreachable!("body closure contains no control transactions")
                };
                body_consulted_anonymous.extend(
                    consulted_anonymous_nominals
                        .0
                        .iter()
                        .cloned()
                        .map(|nominal| (nominal.identity.clone(), nominal)),
                );
                if let Some(crate::body_query::ProducedAnonymous::Produced(produced)) =
                    analysis.produced_anonymous.as_ref()
                {
                    body_produced_anonymous.extend(
                        produced
                            .0
                            .iter()
                            .cloned()
                            .map(|nominal| (nominal.identity.clone(), nominal)),
                    );
                }
                let canonical_body = body.clone();
                match body.as_ref() {
                    crate::body_query::CanonicalBody::Ordinary { owner, body } => {
                        let Some(record) =
                            prepared_definitions.definitions().definition_by_key(owner)
                        else {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} published an ordinary body whose owner has no issued definition record"
                                        )),
                                    ),
                                ),
                            );
                            continue;
                        };
                        let Some(body_span) = record.body_span() else {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} published an ordinary body whose owner definition record carries no body span"
                                        )),
                                    ),
                                ),
                            );
                            continue;
                        };
                        durable_body_candidates.push(
                            crate::canonical_semantic::PreparedDurableBodyCandidate {
                                owner: owner.clone(),
                                body_span,
                                body: body.clone(),
                                canonical: canonical_body.clone().into(),
                            },
                        );
                    }
                    crate::body_query::CanonicalBody::Anonymous {
                        identity,
                        body_anchor,
                        body,
                    } => {
                        let crate::FunctionInstanceKey::AnonymousMember { owner, member: _ } =
                            identity
                        else {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} published an anonymous body whose identity is not an anonymous member"
                                        )),
                                    ),
                                ),
                            );
                            continue;
                        };
                        let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(
                            _,
                        )) = owner.as_ref()
                        else {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} published an anonymous body whose owner is not an anonymous nominal type"
                                        )),
                                    ),
                                ),
                            );
                            continue;
                        };
                        let Some(source) = current_source_locator.as_ref() else {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} published an anonymous body without a current source locator"
                                        )),
                                    ),
                                ),
                            );
                            continue;
                        };
                        let body_span = rue_span::Span::with_file(
                            source.file_id,
                            source
                                .body_start
                                .saturating_add(body_anchor.start)
                                .min(source.body_end),
                            source
                                .body_start
                                .saturating_add(body_anchor.end)
                                .min(source.body_end),
                        );
                        durable_anonymous_body_candidates.push(
                            crate::canonical_semantic::PreparedDurableAnonymousBodyCandidate {
                                identity: identity.clone(),
                                body_span,
                                body: body.clone(),
                                canonical: canonical_body.clone().into(),
                            },
                        );
                    }
                    crate::body_query::CanonicalBody::Specialization { identity, body, .. } => {
                        let Some(record) = prepared_definitions
                            .definitions()
                            .definition_by_key(&identity.base)
                        else {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(
                                    crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} published a specialization body whose base has no issued definition record"
                                        )),
                                    ),
                                ),
                            );
                            continue;
                        };
                        durable_specialized_body_candidates.push(
                            crate::canonical_semantic::PreparedDurableSpecializedBodyCandidate {
                                instance: instance.clone(),
                                identity: identity.clone(),
                                body_span: record.declaration_span(),
                                body: body.clone(),
                                canonical: canonical_body.clone().into(),
                            },
                        );
                    }
                }
            }
            queried_body_work.closure_bodies_visited = closure_output.bodies.len();
            tracing::info!(
                bodies_attempted = queried_body_work.bodies_attempted,
                bodies_visited = queried_body_work.closure_bodies_visited,
                closure_restarts = queried_body_work.closure_restarts,
                deferred_producer_retries = queried_body_work.deferred_producer_retries,
                max_specialization_depth = queried_body_work.max_specialization_depth,
                "body closure complete"
            );
        }
        let query_anonymous_nominals = {
            let mut all = query_anonymous_nominals
                .iter()
                .cloned()
                .map(|nominal| (nominal.identity.clone(), nominal))
                .collect::<BTreeMap<_, _>>();
            all.extend(body_consulted_anonymous);
            all.extend(body_produced_anonymous);
            Arc::from(all.into_values().collect::<Vec<_>>())
        };
        // Declaration payloads have one production authority: the revisioned
        // semantic query nucleus. The old durable declaration cache remains a
        // test-oracle fixture only; it must never select or revive a second
        // declaration resolver in a real request.
        let analysis = prepared.and_then(|prepared| {
            if !body_query_errors.is_empty() {
                let errors = body_query_errors.into_values().fold(
                    CompileErrors::new(),
                    |mut all, errors| {
                        all.extend(errors);
                        all
                    },
                );
                let mut work = CanonicalSemanticWork {
                    declaration_index: prepared.declaration_index_work(),
                    declaration_reuse: prepared.declaration_reuse_work(),
                    ..CanonicalSemanticWork::default()
                };
                work.accrue_body_query_work(queried_body_work);
                return Err(CanonicalSemanticFailure::body(errors, work));
            }
            let durable = query_declarations
                .expect("successful semantic-nucleus projection publishes declaration payloads");
            let definitions = prepared.definitions().clone();
            let mut output = analyze_prepared_canonical_program_reusing_declarations(
                &merged,
                rir.clone(),
                options,
                &imports,
                prepared,
                &definitions,
                &durable,
                &query_anonymous_nominals,
                &query_declaration_dependencies,
                durable_body_candidates,
                durable_specialized_body_candidates,
                durable_anonymous_body_candidates,
                demanded_drop_glue,
                demanded_drop_glue_plans,
                durable_body_work,
                &self.queries.revisioned,
                runtime_revision,
                cancellation.clone(),
            )?;
            output.accrue_body_query_work(queried_body_work);
            Ok(output)
        });
        #[cfg(test)]
        if std::mem::take(&mut self.cancel_semantic_before_publication) {
            cancellation.cancel();
        }
        if cancellation.is_canceled() {
            return Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        let failure = analysis.as_ref().err().map(|failure| failure.failure);
        let semantic_work = analysis
            .as_ref()
            .map(|output| output.work())
            .unwrap_or_else(|failure| failure.failure.work);
        let result = analysis.map(Arc::new).map_err(|failure| failure.errors);
        let record = SemanticQueryRecord {
            input: input.clone(),
            work: semantic_work,
            failure,
            failed: result.is_err(),
        };
        guard.accrue(QueryStructuralWork::Semantic(Box::new(record.clone())));
        *attempt_record = Some(record);
        if let Ok(output) = &result {
            debug_assert_eq!(output.input(), &input);
            debug_assert_eq!(semantic_work.binding.bind_invocations, 1);
            debug_assert_eq!(semantic_work.manifest.build_invocations, 1);
        }
        let diagnostic_input = semantic_diagnostic_input(&input, imports.clone());
        let diagnostics = self.publish_diagnostics(
            &source,
            FrontendDiagnosticIdentity::Semantic(diagnostic_input),
            result.as_ref().err(),
            result
                .as_ref()
                .map(|output| output.warnings())
                .unwrap_or(&[]),
        );
        guard.attach_diagnostics(diagnostics.clone());
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();
        result.map_err(SemanticRequestControl::Compile)
    }

    /// Issue stable definition IDs on demand for the current semantic input.
    ///
    /// The authoritative semantic terminal already owns the final,
    /// post-classification definition universe. This projection never starts a
    /// peer declaration bind or reconstructs stable IDs from shells.
    pub(crate) fn stable_definitions(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<BoundDefinitionSet>, CompileErrors> {
        let mut guard = self.metrics.begin::<DefinitionQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_record = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.stable_definitions_attempt(
                options,
                attempt_id,
                &mut guard,
                &mut execution,
                &mut origin,
                &mut attempt_record,
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        let structural = attempt_record
            .clone()
            .map(Box::new)
            .map(QueryStructuralWork::Definition)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.publish_definition(0);
        self.metrics.synchronize();
        result
    }

    fn stable_definitions_attempt(
        &mut self,
        options: &CompileOptions,
        _attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        _origin: &mut Option<AttemptId>,
        attempt_record: &mut Option<DefinitionQueryRecord>,
    ) -> Result<Arc<BoundDefinitionSet>, CompileErrors> {
        self.require_successful_import_diagnostics()?;
        let imports = self.accepted_semantic_import_graph()?;
        let snapshot = self
            .published_snapshot
            .clone()
            .expect("definition query retains its exact source snapshot");
        let input =
            SemanticInputDescriptor::new(&snapshot, options.target, &options.preview_features);
        let _rir = match self.canonical_rir() {
            Ok(rir) => rir,
            Err(errors) => {
                let record = DefinitionQueryRecord {
                    input: input.clone(),
                    binding: DeclarationBindingWork::default(),
                    manifest: SemanticBindingManifestWork::default(),
                    issuance: BoundDefinitionWork::default(),
                    failed: true,
                };
                guard.accrue(QueryStructuralWork::Definition(Box::new(record.clone())));
                *attempt_record = Some(record);
                self.refresh_retention_metrics();
                return Err(errors);
            }
        };
        let merged = self.merge()?;
        let semantic = match self.canonical_semantic(options) {
            Ok(semantic) => semantic,
            Err(errors) => {
                let record = DefinitionQueryRecord {
                    input: input.clone(),
                    binding: DeclarationBindingWork::default(),
                    manifest: SemanticBindingManifestWork::default(),
                    issuance: BoundDefinitionWork::default(),
                    failed: true,
                };
                guard.accrue(QueryStructuralWork::Definition(Box::new(record.clone())));
                *attempt_record = Some(record);
                self.refresh_retention_metrics();
                return Err(errors);
            }
        };
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let computation = compute_stable_definitions(&merged, options, &imports, &semantic);
        let result = computation.result.clone();
        let record = DefinitionQueryRecord {
            input,
            binding: computation.binding,
            manifest: computation.manifest,
            issuance: computation.issuance,
            failed: result.is_err(),
        };
        guard.accrue(QueryStructuralWork::Definition(Box::new(record.clone())));
        *attempt_record = Some(record);
        self.refresh_retention_metrics();
        result
    }
}

fn import_semantic_body_warnings(
    body: &rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>,
    body_span: rue_span::Span,
) -> Vec<CompileWarning> {
    let locate = |anchor: &rue_air::SemanticBodyAnchor| {
        rue_span::Span::with_file(
            body_span.file_id,
            body_span.start + anchor.start,
            body_span.start + anchor.end,
        )
    };
    body.warnings
        .iter()
        .map(|warning| {
            let mut imported = CompileWarning::new(warning.kind.clone(), locate(&warning.anchor));
            for label in warning.labels.iter() {
                imported = imported.with_label(label.message.to_string(), locate(&label.anchor));
            }
            for note in warning.notes.iter() {
                imported = imported.with_note(note.to_string());
            }
            for help in warning.helps.iter() {
                imported = imported.with_help(help.to_string());
            }
            for suggestion in warning.suggestions.iter() {
                imported = imported.with_suggestion(
                    rue_error::Suggestion::new(
                        suggestion.message.to_string(),
                        locate(&suggestion.anchor),
                        suggestion.replacement.to_string(),
                    )
                    .with_applicability(suggestion.applicability),
                );
            }
            imported
        })
        .collect()
}

fn rooted_unused_function_warnings(
    graph: &RootedBodyGraph,
    warning_references: &BTreeSet<crate::StableDefinitionKey>,
) -> Vec<CompileWarning> {
    fn source_definition(
        instance: &crate::FunctionInstanceKey,
    ) -> Option<&crate::StableDefinitionKey> {
        match instance {
            crate::FunctionInstanceKey::Definition(definition) => Some(definition),
            crate::FunctionInstanceKey::Specialization { base, .. } => source_definition(base),
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_) => None,
        }
    }

    let mut referenced = graph
        .declaration_dependencies
        .iter()
        .filter_map(|dependency| {
            match &dependency.target {
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(key)
            | crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::TypeCallHead(
                key,
            )
            | crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                key,
            ) => Some(key.clone()),
            crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::BuiltinTypeCallHead(
                _,
            ) => None,
        }
        })
        .collect::<BTreeSet<_>>();
    referenced.extend(
        graph
            .closure
            .reached
            .iter()
            .filter_map(source_definition)
            .cloned(),
    );
    referenced.extend(warning_references.iter().cloned());

    graph
        .declarations
        .iter()
        .filter_map(|declaration| {
            let name = declaration.key.name();
            if declaration.key.kind() != crate::StableDefinitionKind::Function
                || name == "main"
                || declaration.key.module().is_trusted_standard_library()
                || declaration.is_public
                || name.starts_with('_')
                || referenced.contains(&declaration.key)
            {
                return None;
            }
            let module = graph
                .modules
                .iter()
                .find(|module| module.module_id() == declaration.key.module())?;
            let candidate = crate::declaration_candidate::DeclarationCandidateKey {
                module: declaration.key.module().clone(),
                category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
                name: Arc::from(name),
                owner: None,
                duplicate_discriminator: 0,
            };
            let locator = module.definitions().declaration_locator(&candidate)?;
            let function = module.ast().items.iter().find_map(|item| match item {
                rue_parser::ast::Item::Function(function)
                    if function.span == locator.declaration_span =>
                {
                    Some(function)
                }
                _ => None,
            })?;
            let allows_unused = function.directives.iter().any(|directive| {
                module.resolve_raw_symbol(directive.name.name) == "allow"
                    && directive.args.iter().any(|argument| match argument {
                        rue_parser::ast::DirectiveArg::Ident(argument) => {
                            module.resolve_raw_symbol(argument.name) == "unused_function"
                        }
                    })
            });
            if allows_unused {
                return None;
            }
            Some(
                CompileWarning::new(
                    rue_error::WarningKind::UnusedFunction(name.to_owned()),
                    locator.declaration_span,
                )
                .with_help(format!(
                    "if this is intentional, prefix it with an underscore: `_{name}`"
                )),
            )
        })
        .collect()
}

fn declaration_shell_failure_diagnostics(
    modules: &[Arc<crate::parsed_modules::ParsedModule>],
    failure: &crate::declaration_candidate::DeclarationShellFailure,
) -> CompileErrors {
    use crate::declaration_candidate::DeclarationShellFailure as F;
    let key = match failure {
        F::Absent(key) | F::Ambiguous(key) | F::ParserCapabilityMismatch(key) => Some(key),
        F::OccurrencesUnavailable(_) => None,
    };
    let span = key.and_then(|key| {
        modules
            .iter()
            .find(|module| module.module_id() == &key.module)
            .and_then(|module| module.definitions().declaration_locator(key))
            .map(|locator| locator.declaration_span)
    });
    let kind = ErrorKind::InternalError(format!(
        "query-owned declaration shell failed stable validation: {failure:?}"
    ));
    CompileErrors::from(match span {
        Some(span) => CompileError::new(kind, span),
        None => CompileError::without_span(kind),
    })
}

fn semantic_nucleus_failure_diagnostics(
    modules: &[Arc<crate::parsed_modules::ParsedModule>],
    declaration: Option<&crate::declaration_candidate::DeclarationCandidateKey>,
    failure: &crate::semantic_query_nucleus::SemanticNucleusFailure,
) -> CompileErrors {
    use crate::semantic_query_nucleus::SemanticNucleusFailure as F;
    if let F::DuplicateDeclarations(failures) = failure {
        let mut diagnostics = CompileErrors::new();
        for failure in failures.iter() {
            diagnostics.extend(semantic_nucleus_failure_diagnostics(
                modules,
                None,
                &F::DuplicateDeclaration {
                    kind: failure.kind.clone(),
                    first: failure.first.clone(),
                    duplicate: failure.duplicate.clone(),
                },
            ));
        }
        return diagnostics;
    }
    if let F::ForeignSignatureConflict(conflict) = failure {
        let locate = |declaration: &crate::declaration_candidate::DeclarationCandidateKey| {
            modules
                .iter()
                .find(|module| module.module_id() == &declaration.module)
                .and_then(|module| module.definitions().declaration_locator(declaration))
                .map(|locator| locator.declaration_span)
        };
        if let (Some(left_span), Some(right_span)) = (
            locate(&conflict.left.declaration),
            locate(&conflict.right.declaration),
        ) {
            let left = (left_span, &conflict.left);
            let right = (right_span, &conflict.right);
            let order = |(span, _): &(rue_span::Span, _)| (span.file_id.index(), span.start);
            let (first, second) = if order(&left) <= order(&right) {
                (left, right)
            } else {
                (right, left)
            };
            let spelled_alike = first.1.signature == second.1.signature;
            let mut error = CompileError::new(
                ErrorKind::ForeignSignatureConflict(Box::new(
                    rue_error::ForeignSignatureConflictError {
                        symbol: conflict.symbol.to_string(),
                        declared: second.1.signature.to_string(),
                        previously_declared: first.1.signature.to_string(),
                    },
                )),
                second.0,
            )
            .with_label("conflicting declaration of the same C symbol", second.0)
            .with_label("first declared here", first.0)
            .with_note(
                "an `extern \"C\"` declaration names an external C symbol, so every module that \
                 declares it describes the same function; only one definition is linked in",
            );
            if spelled_alike {
                error = error.with_note(
                    "the two signatures are spelled alike but resolve to different types: a struct \
                     or enum declared in each module is a distinct type, even under the same name",
                );
            }
            return CompileErrors::from(error.with_help(
                "make the declarations identical, or declare the symbol once and import that module",
            ));
        }
        return CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
            format!(
                "query-owned foreign-signature conflict could not be projected to source: {failure:?}"
            ),
        )));
    }
    if let (Some(declaration), F::DiagnosticAtParameter { kind, ordinal }) = (declaration, failure)
        && let Some(module) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
        && let Some(locator) = module.definitions().declaration_locator(declaration)
    {
        let parameters = module.ast().items.iter().find_map(|item| match item {
            rue_parser::ast::Item::Function(function)
                if function.span == locator.declaration_span =>
            {
                Some(function.params.as_slice())
            }
            rue_parser::ast::Item::Struct(structure) => structure
                .methods
                .iter()
                .find(|method| method.span == locator.declaration_span)
                .map(|method| method.params.as_slice()),
            rue_parser::ast::Item::Extern(block) => block
                .fns
                .iter()
                .find(|function| function.span == locator.declaration_span)
                .map(|function| function.params.as_slice()),
            _ => None,
        });
        if let Some(parameter) = parameters.and_then(|parameters| parameters.get(*ordinal as usize))
        {
            return CompileErrors::from(CompileError::new(kind.clone(), parameter.span));
        }
    }
    if let F::DiagnosticAtDeclaration { kind, declaration } = failure
        && let Some(span) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
            .and_then(|module| module.definitions().declaration_locator(declaration))
            .map(|locator| locator.declaration_span)
    {
        return CompileErrors::from(CompileError::new(kind.clone(), span));
    }
    if let F::DuplicateDeclaration {
        kind,
        first,
        duplicate,
    } = failure
        && let Some(module) = modules
            .iter()
            .find(|module| module.module_id() == &duplicate.module)
        && let Some(duplicate_span) = module
            .definitions()
            .declaration_locator(duplicate)
            .map(|locator| locator.declaration_span)
        && let Some(first_module) = modules
            .iter()
            .find(|module| module.module_id() == &first.module)
        && let Some(first_span) = first_module
            .definitions()
            .declaration_locator(first)
            .map(|locator| locator.declaration_span)
    {
        return CompileErrors::from(CompileError::new(kind.clone(), duplicate_span).with_label(
            format!("first defined in {}", first_module.physical_path()),
            first_span,
        ));
    }
    if let (Some(declaration), F::DiagnosticAtProducerRange { kind, start, end }) =
        (declaration, failure)
        && let Some(producer) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
            .and_then(|module| module.definitions().producer_fragment_span(declaration))
        && let (Some(start), Some(end)) = (
            producer.start.checked_add(*start),
            producer.start.checked_add(*end),
        )
        && start <= end
        && end <= producer.end
    {
        return CompileErrors::from(CompileError::new(
            kind.clone(),
            rue_span::Span::with_file(producer.file_id, start, end),
        ));
    }
    if let F::OwnershipGate { kind, gate } = failure {
        let primary_span = declaration.and_then(|key| {
            modules
                .iter()
                .find(|module| module.module_id() == &key.module)
                .and_then(|module| module.definitions().declaration_locator(key))
                .map(|locator| locator.declaration_span)
        });
        let mut error = match primary_span {
            Some(span) => CompileError::new(kind.clone(), span),
            None => CompileError::without_span(kind.clone()),
        };
        if let Some(application) = &gate.application
            && let Some(span) = modules
                .iter()
                .find(|module| module.module_id() == &application.declaration.module)
                .and_then(|module| {
                    module
                        .definitions()
                        .declaration_locator(&application.declaration)
                })
                .map(|locator| locator.declaration_span)
        {
            error = error.with_label("required by the type-constructor application here", span);
        }
        return CompileErrors::from(error);
    }
    if let (Some(declaration), F::Diagnostic(ErrorKind::CopyStructWithDestructor { type_name })) =
        (declaration, failure)
        && let Some(module) = modules
            .iter()
            .find(|module| module.module_id() == &declaration.module)
    {
        let destructor_span = module.ast().items.iter().find_map(|item| match item {
            rue_parser::ast::Item::DropFn(drop)
                if module.resolve_raw_symbol(drop.type_name.name) == type_name =>
            {
                Some(drop.span)
            }
            _ => None,
        });
        let copy_span = module.ast().items.iter().find_map(|item| match item {
            rue_parser::ast::Item::Struct(structure)
                if module.resolve_raw_symbol(structure.name.name) == type_name =>
            {
                structure
                    .directives
                    .iter()
                    .find(|directive| module.resolve_raw_symbol(directive.name.name) == "copy")
                    .map(|directive| directive.span)
            }
            _ => None,
        });
        if let Some(destructor_span) = destructor_span {
            let mut error = CompileError::new(
                ErrorKind::CopyStructWithDestructor {
                    type_name: type_name.clone(),
                },
                destructor_span,
            )
            .with_label("destructor defined here", destructor_span)
            .with_note(
                "`@copy` values are duplicated implicitly, so the destructor would run \
                     once per copy — cleaning up the same resource multiple times",
            )
            .with_help("remove the `@copy` attribute or remove the `drop fn`");
            if let Some(copy_span) = copy_span {
                error = error.with_label("type declared `@copy` here", copy_span);
            }
            return CompileErrors::from(error);
        }
    }
    let span = declaration.and_then(|key| {
        modules
            .iter()
            .find(|module| module.module_id() == &key.module)
            .and_then(|module| module.definitions().declaration_locator(key))
            .map(|locator| locator.declaration_span)
    });
    let (kind, help, note) = match failure {
        F::Diagnostic(kind) => (kind.clone(), None, None),
        F::DiagnosticAtParameter { kind, .. } => (kind.clone(), None, None),
        F::DiagnosticAtDeclaration { kind, .. } => (kind.clone(), None, None),
        F::DuplicateDeclaration { kind, .. } => (kind.clone(), None, None),
        F::DuplicateDeclarations(_) => unreachable!("duplicate batches return above"),
        F::ForeignSignatureConflict(_) => {
            unreachable!("foreign-signature conflicts return above")
        }
        F::DiagnosticAtProducerRange { kind, .. } => (kind.clone(), None, None),
        F::OwnershipGate { kind, .. } => (kind.clone(), None, None),
        F::DiagnosticWithHelp { kind, help } => (kind.clone(), Some(help.clone()), None),
        F::DiagnosticWithNote { kind, note } => (kind.clone(), None, Some(note.clone())),
        F::Cycle(nodes) => (
            ErrorKind::ConstInitializerCycle {
                cycle: nodes
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            },
            None,
            None,
        ),
        F::SignatureReentry { cycle, .. } => (
            ErrorKind::UnknownType(
                cycle
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            ),
            None,
            None,
        ),
        F::Resolution(message) if message.starts_with("unknown array length") => (
            ErrorKind::InvalidArrayLength {
                reason: message
                    .strip_prefix("unknown array length `")
                    .and_then(|name| name.strip_suffix('`'))
                    .map_or_else(
                        || message.to_string(),
                        |name| format!("'{name}' is not a compile-time constant"),
                    ),
            },
            None,
            None,
        ),
        F::Resolution(message) => (
            ErrorKind::ComptimeEvaluationFailed {
                reason: message.to_string(),
            },
            None,
            None,
        ),
        F::Shell(message) | F::Syntax(message) => (
            ErrorKind::InternalError(format!("semantic query invariant failed: {message}")),
            None,
            None,
        ),
    };
    let error = match span {
        Some(span) => CompileError::new(kind, span),
        None => CompileError::without_span(kind),
    };
    let error = match help {
        Some(help) => error.with_help(help.to_string()),
        None => error,
    };
    CompileErrors::from(match note {
        Some(note) => error.with_note(note.to_string()),
        None => error,
    })
}

fn well_known_option_resolution_diagnostics(
    modules: &[Arc<crate::parsed_modules::ParsedModule>],
    failure: &crate::revisioned_query_database::WellKnownOptionResolutionFailure,
) -> CompileErrors {
    use crate::revisioned_query_database::WellKnownOptionResolutionFailure as F;
    match failure {
        F::Incomplete {
            payload,
            prerequisite,
            detail,
        } => CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
            format!(
                "exact trusted Option({payload:?}) prerequisite resolution was incomplete{}: {detail}",
                prerequisite
                    .as_ref()
                    .map_or_else(String::new, |key| format!(" at {key:?}"))
            ),
        ))),
        F::Semantic { payload, failure } => {
            let mut errors = semantic_nucleus_failure_diagnostics(modules, None, failure);
            if errors.is_empty() {
                errors = CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
                    format!(
                        "trusted Option({payload:?}) resolution failed without diagnostics: {failure:?}"
                    ),
                )));
            }
            errors
        }
        F::WrongProjection { payload, detail } => CompileErrors::from(CompileError::without_span(
            ErrorKind::InternalError(format!(
                "trusted Option({payload:?}) resolution returned the wrong semantic projection: {detail}"
            )),
        )),
    }
}

fn semantic_diagnostic_input(
    input: &CodegenInputDescriptor,
    imports: CanonicalImportGraph,
) -> crate::ResolvedCodegenRevision {
    crate::ResolvedCodegenRevision::new(
        crate::ResolvedProgramRevision::new(input.semantic.clone(), imports),
        input.opt_level,
    )
}

fn programs_are_pointer_equivalent(left: &ParsedProgram, right: &ParsedProgram) -> bool {
    left.source_revision() == right.source_revision()
        && left.modules().len() == right.modules().len()
        && left
            .modules()
            .iter()
            .zip(right.modules())
            .all(|(left, right)| Arc::ptr_eq(left, right))
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

fn no_published_program() -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        "frontend query session has no successful parsed program".to_string(),
    )))
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
impl CompilerSession {
    /// Return the producer request that owns each currently retained ordinary
    /// body terminal named by `names`. A missing declaration or a declaration
    /// with no retained reached-body terminal is omitted.
    ///
    /// The scaling harness compares these stable provenance identities across
    /// revisions to prove the exact recomputed body set. Equal work counts alone
    /// cannot distinguish recomputing the intended consumers from recomputing
    /// the same number of unrelated bodies.
    pub(crate) fn retained_body_transaction_origins_for_test(
        &self,
        names: &[String],
    ) -> BTreeMap<String, u64> {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("the acceptance corpus has a semantic revision");
        self.queries
            .revisioned
            .retained_body_transaction_origins_for_test(revision, names)
    }

    /// Snapshot every retained body identity and its current observable
    /// transaction for the correctness oracle. The map includes stale cache
    /// identities with `None` when invalidation has made their terminal
    /// unobservable at the current revision.
    #[allow(dead_code)]
    pub(crate) fn retained_body_identity_states_for_test(
        &self,
        options: &CompileOptions,
    ) -> BTreeMap<String, Option<crate::BodyTransaction>> {
        let Some(revision) = self.queries.revisioned.current_semantic_revision() else {
            return BTreeMap::new();
        };
        self.queries
            .revisioned
            .retained_body_identity_states_for_test(
                revision,
                crate::semantic_query_nucleus::SemanticQueryConfiguration {
                    target: options.target,
                    preview_features: StablePreviewFeatures::new(&options.preview_features),
                },
            )
    }
}

#[cfg(test)]
mod tests {
    fn evict_diagnostic_index(session: &mut CompilerSession) {
        for revision in 0..=FRONTEND_DIAGNOSTIC_RETENTION_LIMIT {
            let source = snapshot(
                &[(
                    91,
                    "/eviction/main.rue",
                    "main.rue",
                    &format!("fn main() -> i32 {{ {revision} }}"),
                )],
                91,
            );
            let snapshot = Arc::new(FrontendDiagnosticSnapshot {
                source,
                stage: FrontendDiagnosticIdentity::Syntax,
                provenance: DiagnosticAttemptProvenance::Canonical,
                errors: Arc::from([]),
                warnings: Arc::from([]),
            });
            session.diagnostics.select_test_snapshot(snapshot);
        }
    }

    /// Publish `source` and, when it imports, commit its graph through a real
    /// discovery epoch served from the fixture's own modules. The returned work
    /// is the epoch's accumulated parse work, which is the parse an
    /// import-bearing revision actually performs.

    fn publish_with_test_imports(
        session: &mut CompilerSession,
        source: &SourceSnapshot,
    ) -> ParsedModulesWork {
        if !crate::test_support::fixture_has_imports(source).unwrap() {
            let update = session.update(source);
            let work = update.work();
            update.into_owner_result().unwrap();
            return work;
        }
        crate::test_support::TestDiscoveryHost::new(source)
            .unwrap()
            .drive(session)
            .unwrap()
            .parse_work
    }

    fn body_query_key(
        session: &mut CompilerSession,
        options: &CompileOptions,
        name: &str,
    ) -> crate::body_query::BodyQueryKey {
        let definitions = session.stable_definitions(options).unwrap();
        let definition = definitions
            .definitions()
            .iter()
            .find(|record| {
                record.stable_key().kind() == StableDefinitionKind::Function
                    && record.stable_key().name() == name
            })
            .unwrap()
            .stable_key()
            .clone();
        crate::body_query::BodyQueryKey {
            instance: crate::FunctionInstanceKey::Definition(definition),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: options.target,
                preview_features: StablePreviewFeatures::new(&options.preview_features),
            },
        }
    }

    /// The comptime arguments of a specialization of the definition named
    /// `source_name`, or `None` for any other callable.
    ///
    /// A live callable name is never a source name: an ordinary definition's
    /// internal symbol is module-qualified (RUE-1125) and a specialization
    /// appends its argument mangling to that. Tests therefore select a
    /// specialization through its durable identity.
    fn specialization_arguments<'a>(
        function: &'a FunctionWithCfg,
        source_name: &str,
    ) -> Option<&'a crate::CanonicalArguments> {
        let crate::FunctionInstanceKey::Specialization { base, arguments } =
            &function.semantic_identity
        else {
            return None;
        };
        let crate::FunctionInstanceKey::Definition(definition) = base.as_ref() else {
            return None;
        };
        (definition.name() == source_name).then_some(arguments)
    }

    fn retained_body_query_stamps(
        session: &CompilerSession,
        key: &crate::body_query::BodyQueryKey,
    ) -> (u64, u64, u64, u64) {
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        let cancellation = rue_query::CancellationToken::new();
        let transaction = session
            .queries
            .revisioned
            .body_transaction(revision, key.clone(), cancellation.clone())
            .unwrap();
        let body = session
            .queries
            .revisioned
            .canonical_body_projection(revision, key.clone(), cancellation.clone())
            .unwrap();
        let references = session
            .queries
            .revisioned
            .body_references_projection(revision, key.clone(), cancellation.clone())
            .unwrap();
        let produced_anonymous = session
            .queries
            .revisioned
            .body_produced_anonymous_projection(revision, key.clone(), cancellation)
            .unwrap();
        (
            transaction.stamp(),
            body.stamp(),
            references.stamp(),
            produced_anonymous.stamp(),
        )
    }

    fn retained_body_transaction(
        session: &CompilerSession,
        key: &crate::body_query::BodyQueryKey,
    ) -> (
        u64,
        rue_query::QueryTerminalKind,
        crate::body_query::BodyTransaction,
    ) {
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        let terminal = session
            .queries
            .revisioned
            .body_transaction(revision, key.clone(), rue_query::CancellationToken::new())
            .unwrap();
        let rue_query::QueryOutcome::Success(transaction) = terminal.outcome() else {
            unreachable!("BodyTransaction publishes typed values")
        };
        (terminal.stamp(), terminal.kind(), transaction.clone())
    }

    fn retained_body_closure_stamps(
        session: &CompilerSession,
        key: &crate::body_query::BodyQueryKey,
    ) -> (u64, u64) {
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        let crate::FunctionInstanceKey::Definition(definition) = &key.instance else {
            panic!("test closure root must be an ordinary definition")
        };
        let request = session
            .queries
            .revisioned
            .body_closure(
                revision,
                crate::body_query::BodyClosureQueryKey {
                    modules: Arc::from([definition.module().clone()]),
                    roots: Arc::from([key.instance.clone()]),
                    configuration: key.configuration.clone(),
                },
                rue_query::CancellationToken::new(),
            )
            .unwrap();
        let rue_query::QueryOutcome::Success(output) = request.terminal.outcome() else {
            unreachable!("BodyClosure publishes typed values")
        };
        let body = output
            .bodies
            .iter()
            .find(|body| body.key == *key)
            .expect("test closure contains its root body");
        (request.terminal.stamp(), body.bundle.stamp())
    }

    fn retained_body_source_locator(
        session: &CompilerSession,
        key: &crate::body_query::BodyQueryKey,
    ) -> (u64, crate::body_query::BodySourceLocator) {
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        let terminal = session
            .queries
            .revisioned
            .body_source_locator_projection(
                revision,
                key.clone(),
                rue_query::CancellationToken::new(),
            )
            .unwrap();
        let rue_query::QueryOutcome::Success(Some(locator)) = terminal.outcome() else {
            panic!("ordinary test body has a current source locator")
        };
        (terminal.stamp(), locator.clone())
    }

    fn retained_body_dependency_nodes(
        session: &CompilerSession,
        key: &crate::body_query::BodyQueryKey,
    ) -> Vec<String> {
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        session
            .queries
            .revisioned
            .body_transaction(revision, key.clone(), rue_query::CancellationToken::new())
            .unwrap()
            .dependencies()
            .iter()
            .map(|dependency| format!("{:?}", dependency.node))
            .collect()
    }

    /// A trusted-std `Option` snapshot for the well-known query-edge isolation
    /// regression: the root is `root_source`, and the trusted std `Option` module
    /// is provided at `\0rue-std/option.rue`, reached with
    /// `@import("std/option.rue")` (physical-suffix match).

    fn well_known_option_isolation_snapshot(root_source: &str) -> SourceSnapshot {
        well_known_option_snapshot_with_source(
            root_source,
            "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }",
        )
    }

    fn well_known_option_snapshot_with_source(
        root_source: &str,
        option_source: &str,
    ) -> SourceSnapshot {
        let root = FileId::new(1);
        let option = FileId::new(2);
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root,
            HashMap::from([
                (root, "/project/main.rue".to_owned()),
                (option, "/project/std/option.rue".to_owned()),
            ]),
            HashMap::from([
                (root, "main.rue".to_owned()),
                (option, "\0rue-std/option.rue".to_owned()),
            ]),
            HashSet::from([option]),
        )
        .unwrap();
        SourceSnapshot::new(
            metadata,
            vec![
                (root, Arc::new(root_source.to_owned())),
                (option, Arc::new(option_source.to_owned())),
            ],
        )
        .unwrap()
    }

    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    };

    use rue_span::FileId;

    use super::*;
    use crate::{
        FunctionWithCfg, ModuleId, OptLevel, PreviewFeature, PreviewFeatures, SourceMetadata,
        SourceSnapshot, StableDefinitionKey, StableDefinitionKind, Target,
    };

    #[test]
    fn phase_three_module_queries_close_the_exact_import_deletion_gate() {
        let production = include_str!("session.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap();
        assert!(!production.contains("RUE-1024 DELETION GATE"));
        let discovery = include_str!("import_discovery.rs");
        assert!(!discovery.contains("pub fn pending_requests("));
        let revisioned = include_str!("revisioned_query_database.rs");
        for family in [
            "compiler.parse-module",
            "compiler.module-index",
            "compiler.lookup-name",
            "compiler.resolve-import",
            "compiler.module-rir",
        ] {
            assert!(
                revisioned.contains(family),
                "missing canonical family {family}"
            );
        }
        assert!(!revisioned.contains("ImportModuleDemand"));
        assert!(!revisioned.contains("compiler.import-module-frontier"));
        assert_eq!(
            revisioned
                .matches("RUE-1026 DELETION GATE: this selected-revision compatibility")
                .count(),
            0
        );
        let unstable = include_str!("unstable.rs");
        assert_eq!(
            unstable
                .matches("Full-plan host compatibility adapter. RUE-1026")
                .count(),
            0
        );
    }

    #[test]
    fn import_discovery_has_no_public_bypass_authority() {
        let discovery = include_str!("import_discovery.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap();
        let session = include_str!("session.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap();
        assert!(!discovery.contains("RUE-1033 DELETION/REPLACEMENT GATE"));
        for declaration in [
            "pub fn import_discovery_plan(",
            "pub fn stage_import_discovery(",
            "pub fn close_import_discovery(",
        ] {
            assert!(
                !session.contains(declaration),
                "public import-discovery bypass returned: {declaration}"
            );
        }

        let unstable = include_str!("unstable.rs");
        let begin = unstable
            .split_once("pub fn begin_import_input_request(")
            .unwrap()
            .1
            .split_once(") -> crate::CompileResult<ImportInputRevision>")
            .unwrap()
            .0;
        assert!(!begin.contains("ImportObservationLedger"));
        assert!(!begin.contains("carried_ledger"));
        for boundary in [
            "pub fn stage_import_input_request(",
            "pub fn close_import_input_request(",
        ] {
            assert!(
                unstable.contains(boundary),
                "canonical import boundary is missing: {boundary}"
            );
        }
    }

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn c_ffi_options() -> CompileOptions {
        CompileOptions {
            preview_features: PreviewFeatures::from([PreviewFeature::CFfi]),
            ..CompileOptions::default()
        }
    }

    #[test]
    fn rooted_foreign_conflict_diagnostic_orders_sites_by_source_not_query_order() {
        let source = snapshot(
            &[
                (
                    10,
                    "/p/main.rue",
                    "main.rue",
                    "const a = @import(\"a.rue\");\n\
                     const b = @import(\"b.rue\");\n\
                     fn main() -> i32 { 0 }",
                ),
                (
                    40,
                    "/p/a.rue",
                    "a.rue",
                    "extern \"C\" { fn shared(x: i64) -> i64; }",
                ),
                (
                    2,
                    "/p/b.rue",
                    "b.rue",
                    "extern \"C\" { fn shared() -> bool; }",
                ),
            ],
            10,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let errors = session.rooted_cfg(&c_ffi_options()).unwrap_err();
        let error = errors
            .iter()
            .find(|error| matches!(error.kind, ErrorKind::ForeignSignatureConflict(_)))
            .unwrap_or_else(|| panic!("rooted declaration projection reports E1107: {errors:?}"));
        let ErrorKind::ForeignSignatureConflict(payload) = &error.kind else {
            unreachable!("just matched")
        };
        assert_eq!(payload.symbol, "shared");
        assert_eq!(payload.declared, "fn() -> bool");
        assert_eq!(payload.previously_declared, "fn(i64) -> i64");
        let primary = error.span().expect("E1107 has a primary declaration span");
        let first = error
            .diagnostic()
            .labels
            .iter()
            .find(|label| label.message == "first declared here")
            .expect("E1107 labels the first declaration")
            .span;
        assert!(
            (first.file_id.index(), first.start) < (primary.file_id.index(), primary.start),
            "diagnostic source order must not depend on projection traversal: {error:?}"
        );
    }

    #[test]
    fn rooted_foreign_conflict_explains_same_spelling_with_distinct_nominals() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "const a = @import(\"a.rue\");\n\
                     const b = @import(\"b.rue\");\n\
                     fn main() -> i32 { 0 }",
                ),
                (
                    2,
                    "/p/a.rue",
                    "a.rue",
                    "@repr(c)\n\
                     pub struct Point { x: i32, y: i32 }\n\
                     extern \"C\" { fn takes(p: Point) -> i32; }",
                ),
                (
                    3,
                    "/p/b.rue",
                    "b.rue",
                    "@repr(c)\n\
                     pub struct Point { x: i64 }\n\
                     extern \"C\" { fn takes(p: Point) -> i32; }",
                ),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let errors = session.rooted_cfg(&c_ffi_options()).unwrap_err();
        let error = errors
            .iter()
            .find(|error| matches!(error.kind, ErrorKind::ForeignSignatureConflict(_)))
            .unwrap_or_else(|| panic!("distinct nominal identities report E1107: {errors:?}"));
        assert!(error.diagnostic().notes.iter().any(|note| {
            note.0
                .contains("spelled alike but resolve to different types")
        }));
    }

    fn base() -> SourceSnapshot {
        snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        )
    }

    #[test]
    fn warm_compiler_queries_report_bounded_runtime_retention() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        let cold = session.unstable_metrics().retention();
        assert!(cold.retained_query_records > 0);
        assert!(cold.retained_bytes > 0);
        assert!(cold.dependency_pins > 0);
        assert!(cold.retained_bytes <= cold.retained_byte_budget);
        assert!(cold.dependency_pins <= cold.dependency_pin_budget);

        session.canonical_semantic(&options).unwrap();
        let warm = session.unstable_metrics().retention();
        assert_eq!(warm.retained_query_records, cold.retained_query_records);
        assert_eq!(warm.retained_bytes, cold.retained_bytes);
        assert_eq!(warm.dependency_pins, cold.dependency_pins);
        assert!(warm.peak_retained_bytes >= warm.retained_bytes);
        assert!(warm.peak_dependency_pins >= warm.dependency_pins);
    }

    #[test]
    fn absent_trusted_option_parks_the_rooted_attempt_with_exact_demand_and_anchor() {
        // RUE-1112: a freestanding program whose reached `main` body uses a
        // fallible intrinsic while NO trusted std module is present. The
        // rooted attempt must park with exactly the `option.rue` demand, anchored
        // on the demanding body (`main`), and must NOT run or publish any body
        // transaction — the unsatisfied prerequisite stops the worklist before it
        // enters `body_transaction`.
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();

        let park = match session.semantic_or_toolchain_park(&CompileOptions::default()) {
            SemanticParkOutcome::Parked(park) => park,
            SemanticParkOutcome::Ready(_) => {
                panic!("expected a trusted-toolchain park, got successful analysis")
            }
            SemanticParkOutcome::Errors(errors) => {
                panic!("expected a trusted-toolchain park, got errors: {errors:?}")
            }
        };

        // Exact demand set: exactly the trusted std `Option` module.
        let demands: Vec<&str> = park
            .demands()
            .iter()
            .map(crate::TrustedToolchainModuleDemand::logical_path)
            .collect();
        assert_eq!(demands, vec![crate::OPTION_MODULE_LOGICAL_PATH]);

        // Exact requester anchor: the demanding body's stable key (`main`).
        assert_eq!(park.requesters().len(), 1);
        let anchor = &park.requesters()[0];
        assert_eq!(anchor.name(), "main");
        assert_eq!(anchor.kind(), crate::StableDefinitionKind::Function);

        // No body transaction ran or published a terminal.
        assert!(
            !session.queries.revisioned.any_body_transaction_terminal(),
            "the park must precede any body transaction",
        );
    }

    #[test]
    fn already_reached_parks_batch_into_one_park_with_unioned_demands_and_anchors() {
        // RUE-1112 C2: two reached helper bodies demand different trusted modules
        // (a: parse -> Option; b: read_line -> Option+StrBuf) while no trusted std
        // is present. `main` reaches both, then the first to park must batch the
        // remaining already-reached body: ONE park carrying the UNION of absent
        // modules ([Option, StrBuf]) and BOTH requester anchors, so a single
        // successor acquisition satisfies everything.
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn a() -> i32 { let _ = @parse_i64(\"1\"); 0 }\n\
                 fn b() -> i32 { let _ = @read_line(); 0 }\n\
                 fn main() -> i32 { a() + b() }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();

        let park = match session.semantic_or_toolchain_park(&CompileOptions::default()) {
            SemanticParkOutcome::Parked(park) => park,
            SemanticParkOutcome::Ready(_) => panic!("expected a batched park, got ready analysis"),
            SemanticParkOutcome::Errors(errors) => {
                panic!("expected a batched park, got errors: {errors:?}")
            }
        };

        // Union of absent modules across both already-reached bodies, sorted.
        let demands: Vec<&str> = park
            .demands()
            .iter()
            .map(crate::TrustedToolchainModuleDemand::logical_path)
            .collect();
        assert_eq!(
            demands,
            vec![
                crate::OPTION_MODULE_LOGICAL_PATH,
                crate::STRBUF_MODULE_LOGICAL_PATH
            ]
        );

        // Both demanding bodies contribute a requester anchor. That both `a` and
        // `b` appear proves neither transacted before the park — each was still
        // pending and got projected into the one batch (`main`, which has no
        // fallible intrinsic, does run its transaction first, as expected).
        let anchors: std::collections::BTreeSet<&str> =
            park.requesters().iter().map(|key| key.name()).collect();
        assert_eq!(anchors, std::collections::BTreeSet::from(["a", "b"]));
    }

    // ---- RUE-1112: trusted-toolchain continuation + successor publication ----

    fn continuation_std_context() -> crate::ImportDiscoveryContext {
        crate::ImportDiscoveryContext::new(1, "/project", Some("/sdk"), "test-policy").unwrap()
    }

    fn continuation_metadata() -> crate::FileMetadataFingerprint {
        crate::FileMetadataFingerprint::new(10, 20, 30)
    }

    /// Drive `root_source` to a canonical import-discovery close, then run the
    /// rooted semantic attempt so its park atomically attaches the demanded-missing
    /// set to the closed continuation. Returns the session (now holding an
    /// AUTHORIZING continuation), its token, the empty closure-witness frontier, the
    /// predecessor snapshot, its accepted reads, and the assembler ready to add
    /// trusted leaves. Panics unless the attempt parked — the caller supplies a
    /// reached-fallible-intrinsic root whose demand set is the acquisition batch.
    ///
    /// This exercises the real protocol (close → park → attach → mint): demand
    /// authority is never seeded by direct field assignment, so a close whose
    /// attempt never parks yields no token.
    fn closed_continuation_for(
        root_source: &str,
    ) -> (
        CompilerSession,
        ClosedDiscoveryContinuation,
        crate::ImportDemandFrontier,
        SourceSnapshot,
        crate::AcceptedReadManifest,
        crate::DiscoverySourceAssembler,
    ) {
        let ctx = continuation_std_context();
        let mut assembler = crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            Arc::new(root_source.to_owned()),
        )
        .unwrap();
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let mut session = CompilerSession::new();
        let revision = session
            .begin_import_input_request(&snapshot, ctx.clone(), reads.clone())
            .unwrap();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                ctx.clone(),
                reads.shared_slice(),
                crate::ImportObservationLedger::default(),
            )
            .unwrap();
        let roots = plan.demand_roots();
        let frontier = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                crate::ImportDemandMode::Rooted,
                &roots,
            )
            .unwrap();
        assert!(
            frontier.requests().is_empty(),
            "a freestanding root closes with an empty frontier",
        );
        let ledger = session.import_observation_ledger(revision).unwrap();
        session.close_import_discovery(ledger).unwrap();
        // A bare close is non-authorizing: no demand set has been attached yet.
        assert!(
            session.closed_discovery_continuation().is_none(),
            "a close mints no token until a rooted park attaches a demanded set",
        );
        // The rooted attempt parks; the park attaches its exact demanded-missing
        // set to this closed state, making the continuation authorizing.
        match session.semantic_or_toolchain_park(&CompileOptions::default()) {
            SemanticParkOutcome::Parked(_) => {}
            SemanticParkOutcome::Ready(_) => {
                panic!("expected the reached fallible intrinsic to park the rooted attempt")
            }
            SemanticParkOutcome::Errors(errors) => {
                panic!("expected a trusted-toolchain park, got errors: {errors:?}")
            }
        }
        let token = session
            .closed_discovery_continuation()
            .expect("an attached rooted park makes the closed continuation authorizing");
        (session, token, frontier, snapshot, reads, assembler)
    }

    /// The common single-module case: a reached `@parse_i64` parks on exactly the
    /// trusted std `Option` module.
    fn closed_continuation() -> (
        CompilerSession,
        ClosedDiscoveryContinuation,
        crate::ImportDemandFrontier,
        SourceSnapshot,
        crate::AcceptedReadManifest,
        crate::DiscoverySourceAssembler,
    ) {
        closed_continuation_for("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }")
    }

    fn add_trusted_option(assembler: &mut crate::DiscoverySourceAssembler) {
        assembler
            .add_explicit(
                "/sdk/option.rue",
                "/sdk/option.rue",
                crate::PhysicalFileIdentity::new(2, 2),
                continuation_metadata(),
                Arc::new(
                    "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }".to_owned(),
                ),
            )
            .unwrap();
    }

    fn add_trusted_strbuf(assembler: &mut crate::DiscoverySourceAssembler) {
        assembler
            .add_explicit(
                "/sdk/strbuf.rue",
                "/sdk/strbuf.rue",
                crate::PhysicalFileIdentity::new(3, 3),
                continuation_metadata(),
                Arc::new("pub struct StrBuf { len: i64 }".to_owned()),
            )
            .unwrap();
    }

    #[test]
    fn trusted_successor_publishes_additive_leaf_in_same_generation() {
        let (mut session, token, frontier, predecessor, _reads, mut assembler) =
            closed_continuation();
        let predecessor_modules = predecessor.source_revision().modules().to_vec();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let successor_reads = assembler.accepted_read_manifest();

        let delta = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, successor_reads)
            .expect("a strictly-additive trusted successor publishes");
        // The publish mints an opaque delta authority bound to the appended set.
        // Its module identities are private; the successor stage/close derive and
        // verify them from the snapshot, so the host cannot edit them here.
        let published = delta.revision();

        // Same request generation as the predecessor close; the frontier round
        // advances by one (a successor of that same observation epoch).
        assert_eq!(
            published.request_generation,
            frontier.revision().request_generation
        );
        assert_eq!(
            published.frontier_round,
            frontier.revision().frontier_round + 1
        );

        // Every pre-existing module leaf is preserved byte-identical. Its exact
        // ModuleRevision — and therefore its SourceId, the parse key — reappears in
        // the successor, so no pre-existing module is re-read or reparsed across
        // acquisition; only the trusted Option leaf is appended.
        for old in &predecessor_modules {
            assert!(
                successor.source_revision().modules().contains(old),
                "pre-existing module {old:?} must be preserved byte-identical",
            );
        }
        assert_eq!(
            successor.source_revision().modules().len(),
            predecessor_modules.len() + 1,
        );
    }

    #[test]
    fn trusted_successor_reused_token_is_rejected() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        // The first publish consumes the single-use token.
        session
            .publish_trusted_toolchain_successor(
                token.clone(),
                &frontier,
                &successor,
                reads.clone(),
            )
            .unwrap();
        // Reusing it finds no outstanding continuation.
        let err = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .unwrap_err();
        assert!(
            err.first().unwrap().to_string().contains("already used"),
            "{err:?}",
        );
    }

    #[test]
    fn trusted_successor_stale_token_is_rejected() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        // Simulate a newer close superseding this token: advance the outstanding
        // state's nonce so the presented token no longer matches (stale).
        session.next_continuation_nonce += 7;
        session.continuation.as_mut().unwrap().nonce = session.next_continuation_nonce;
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let err = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .unwrap_err();
        assert!(
            err.first().unwrap().to_string().contains("stale"),
            "{err:?}"
        );
    }

    #[test]
    fn trusted_successor_new_request_invalidates_the_token() {
        let (mut session, token, frontier, predecessor, reads, mut assembler) =
            closed_continuation();
        // A fresh import-input request invalidates any outstanding continuation.
        session
            .begin_import_input_request(&predecessor, continuation_std_context(), reads)
            .unwrap();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let successor_reads = assembler.accepted_read_manifest();
        let err = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, successor_reads)
            .unwrap_err();
        assert!(
            err.first().unwrap().to_string().contains("already used"),
            "{err:?}",
        );
    }

    #[test]
    fn trusted_successor_mutated_predecessor_is_rejected() {
        let (mut session, token, frontier, _pred, _reads, _assembler) = closed_continuation();
        // A successor whose pre-existing root content differs is a mutated
        // predecessor: source evolution must be strictly additive.
        let ctx = continuation_std_context();
        let mut other = crate::DiscoverySourceAssembler::new(
            ctx,
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            Arc::new("fn main() -> i32 { 1 }".to_owned()),
        )
        .unwrap();
        add_trusted_option(&mut other);
        let successor = other.snapshot().unwrap();
        let reads = other.accepted_read_manifest();
        let err = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .unwrap_err();
        assert!(
            err.first()
                .unwrap()
                .to_string()
                .contains("strictly additive"),
            "{err:?}",
        );
    }

    #[test]
    fn trusted_successor_arbitrary_module_is_rejected() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        // StrBuf is a trusted module the park did NOT demand here (the reached
        // `@parse_i64` parks on Option only), so the added set {StrBuf} does not
        // equal the demanded set {Option} and may not ride in on this continuation.
        add_trusted_strbuf(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let err = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .unwrap_err();
        assert!(
            err.first()
                .unwrap()
                .to_string()
                .contains("must equal the rooted park's demanded missing set"),
            "{err:?}",
        );
    }

    #[test]
    fn trusted_successor_ready_close_is_non_authorizing() {
        // A close whose rooted semantic attempt is READY (no fallible intrinsic,
        // no park) attaches no demanded set, so the closed continuation mints no
        // token. Demand authority lives only in an attached park, so a ready close
        // can never inherit an earlier park's demand set and admit an uninvited
        // trusted leaf.
        let ctx = continuation_std_context();
        let mut assembler = crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            Arc::new("fn main() -> i32 { 0 }".to_owned()),
        )
        .unwrap();
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let mut session = CompilerSession::new();
        let revision = session
            .begin_import_input_request(&snapshot, ctx.clone(), reads.clone())
            .unwrap();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                ctx.clone(),
                reads.shared_slice(),
                crate::ImportObservationLedger::default(),
            )
            .unwrap();
        let roots = plan.demand_roots();
        let _frontier = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                crate::ImportDemandMode::Rooted,
                &roots,
            )
            .unwrap();
        let ledger = session.import_observation_ledger(revision).unwrap();
        session.close_import_discovery(ledger).unwrap();
        // The rooted attempt is ready: no park, so no demanded set is attached.
        assert!(matches!(
            session.semantic_or_toolchain_park(&CompileOptions::default()),
            SemanticParkOutcome::Ready(_)
        ));
        assert!(
            session.closed_discovery_continuation().is_none(),
            "a ready close is non-authorizing and mints no continuation token",
        );
    }

    #[test]
    fn trusted_successor_partial_batch_is_rejected_without_consuming_token() {
        // A reached `@read_line` parks on BOTH Option and StrBuf. A successor that
        // adds only Option is a partial batch — added {Option} does not equal the
        // demanded {Option, StrBuf} — so it is rejected. A rejection never consumes
        // the single-use token, so completing the batch and retrying with the same
        // token then publishes.
        let (mut session, token, frontier, _pred, _reads, mut assembler) =
            closed_continuation_for("fn main() -> i32 { let _ = @read_line(); 0 }");
        add_trusted_option(&mut assembler);
        let partial = assembler.snapshot().unwrap();
        let partial_reads = assembler.accepted_read_manifest();
        let err = session
            .publish_trusted_toolchain_successor(token.clone(), &frontier, &partial, partial_reads)
            .unwrap_err();
        assert!(
            err.first()
                .unwrap()
                .to_string()
                .contains("must equal the rooted park's demanded missing set"),
            "{err:?}",
        );
        // The token survived the rejection; completing the two-module batch and
        // retrying publishes with the SAME token.
        add_trusted_strbuf(&mut assembler);
        let full = assembler.snapshot().unwrap();
        let full_reads = assembler.accepted_read_manifest();
        session
            .publish_trusted_toolchain_successor(token, &frontier, &full, full_reads)
            .expect("the completed two-module batch publishes with the un-consumed token");
    }

    #[test]
    fn trusted_successor_altered_predecessor_provenance_is_rejected() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let full_reads = assembler.accepted_read_manifest();
        // Drop the predecessor root's accepted-read provenance, keeping only the
        // added leaf's: the old provenance is no longer byte-identical.
        let tampered: Vec<_> = full_reads
            .iter()
            .filter(|entry| entry.module().is_trusted_standard_library())
            .cloned()
            .collect();
        let err = session
            .publish_trusted_toolchain_successor(
                token,
                &frontier,
                &successor,
                crate::AcceptedReadManifest::from_entries(tampered),
            )
            .unwrap_err();
        assert!(
            err.first()
                .unwrap()
                .to_string()
                .contains("altered or removed"),
            "{err:?}",
        );
    }

    /// A successor-delta capability minted by one session cannot authorize a
    /// successor stage on a different session: the delta is bound to its issuing
    /// session, so a cross-session value is rejected without staging anything.
    #[test]
    fn successor_delta_from_another_session_is_rejected() {
        let (mut issuer, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let delta = issuer
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads.clone())
            .expect("a strictly-additive successor publishes");

        let mut other = CompilerSession::new();
        let err = other.stage_import_discovery_successor(&delta).unwrap_err();
        assert!(
            err.first()
                .unwrap()
                .to_string()
                .contains("different session"),
            "{err:?}",
        );
    }

    /// A successor-delta capability is single-generation: a new import-input
    /// request invalidates it, so a stale delta can neither stage nor close.
    #[test]
    fn stale_successor_delta_cannot_stage() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let delta = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads.clone())
            .expect("a strictly-additive successor publishes");

        // A fresh observation generation invalidates the outstanding delta.
        session
            .begin_import_input_request(&successor, continuation_std_context(), reads.clone())
            .unwrap();
        let err = session
            .stage_import_discovery_successor(&delta)
            .unwrap_err();
        assert!(
            err.first()
                .unwrap()
                .to_string()
                .contains("no outstanding successor-delta authority"),
            "{err:?}",
        );
    }

    /// The successor parse terminal is a REAL runtime query dependent of the
    /// exact predecessor parse terminal — the graph carries the
    /// successor-after-predecessor edge — and the successor close re-selects
    /// the staged terminal itself: same terminal identity, no second parse
    /// dispatch, and no second empty-extension publication.
    #[test]
    fn successor_close_reuses_the_staged_terminal_with_a_predecessor_edge() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        let predecessor_terminal = session
            .selected_parse_terminal()
            .expect("the committed close selects its staged parse terminal");
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let delta = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .expect("a strictly-additive successor publishes");
        session
            .stage_import_discovery_successor(&delta)
            .expect("the successor stages");
        let staged_terminal = session
            .selected_parse_terminal()
            .expect("the successor stage selects its parse terminal");
        assert!(
            !Arc::ptr_eq(&predecessor_terminal, &staged_terminal),
            "the successor stage computes its own terminal"
        );
        // (a) The successor terminal observes the exact predecessor parse
        // terminal as a runtime query dependency — the FULL captured identity
        // (node, incarnation, AND stamp), not an equivalent replacement under
        // the same display node — so red/green validation and leases flow
        // successor-after-predecessor through the graph.
        let observation = staged_terminal
            .dependencies()
            .iter()
            .find(|dependency| dependency.node == *predecessor_terminal.node())
            .unwrap_or_else(|| {
                panic!(
                    "the successor terminal must depend on the exact predecessor parse terminal: {:?}",
                    staged_terminal.dependencies(),
                )
            });
        assert_eq!(
            observation.incarnation,
            predecessor_terminal.node_incarnation(),
            "the dependency must carry the captured terminal's exact node incarnation"
        );
        assert_eq!(
            observation.stamp,
            predecessor_terminal.stamp(),
            "the dependency must carry the captured terminal's exact stamp"
        );
        // That the adoption touched no predecessor content-key Hash/Eq is
        // proven mechanically by the rue-query frozen-key regression
        // (`adoption_never_hashes_or_compares_the_predecessor_key`).
        // (b) The close re-selects the staged terminal itself: identical
        // terminal identity, no parse dispatch, and no second publication.
        let dispatched = session.parse_modules_dispatched();
        let materialized = session.parse_sources_materialized();
        session
            .close_import_discovery_successor(&delta)
            .expect("the successor closes");
        let adopted_terminal = session
            .selected_parse_terminal()
            .expect("the successor close selects the staged parse terminal");
        assert!(
            Arc::ptr_eq(&staged_terminal, &adopted_terminal),
            "the successor close must re-select the exact staged parse terminal"
        );
        assert_eq!(
            session.parse_modules_dispatched(),
            dispatched,
            "the successor close dispatches no parse work"
        );
        assert_eq!(
            session.parse_sources_materialized(),
            materialized,
            "the successor close materializes no whole-program projection"
        );
    }

    /// A strictly-additive successor adoption must leave the predecessor's
    /// immutable source leaf live: retained frontend terminals that correctly
    /// depend on it (however many variants are prewarmed) stay valid, and the
    /// acquisition contributes ZERO dependency-graph invalidation events —
    /// the successor becomes current without walking or invalidating the
    /// predecessor's retained downstream.
    #[test]
    fn successor_adoption_invalidates_no_retained_frontend_variants() {
        let acquisition_invalidations = |prewarm_retained_downstream: bool| -> u64 {
            let (mut session, token, frontier, _pred, _reads, mut assembler) =
                closed_continuation();
            if prewarm_retained_downstream {
                // Retain additional terminals depending on the predecessor's
                // source leaf. Semantic — and the definition/manifest variants
                // that observe it — cannot complete on this predecessor (the
                // reached fallible intrinsic parks semantic until
                // acquisition), so the retained downstream of the leaf is the
                // pre-semantic tier: merged RIR and canonical import
                // diagnostics.
                session
                    .rir()
                    .expect("pre-semantic RIR completes on the parked predecessor");
                session
                    .import_diagnostics()
                    .expect("import diagnostics retain on the closed predecessor");
            }
            add_trusted_option(&mut assembler);
            let successor = assembler.snapshot().unwrap();
            let reads = assembler.accepted_read_manifest();
            let delta = session
                .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
                .expect("a strictly-additive successor publishes");
            let before = session.frontend_query_invalidations();
            session
                .stage_import_discovery_successor(&delta)
                .expect("the successor stages");
            session
                .close_import_discovery_successor(&delta)
                .expect("the successor closes");
            session.frontend_query_invalidations() - before
        };
        let bare = acquisition_invalidations(false);
        let prewarmed = acquisition_invalidations(true);
        assert_eq!(
            bare, 0,
            "additive successor adoption must not invalidate retained frontend terminals"
        );
        assert_eq!(
            prewarmed, 0,
            "additive successor adoption must not invalidate retained frontend terminals regardless of how much retained downstream depends on the predecessor leaf"
        );
    }

    /// A successor-delta capability outstanding across an intervening source or
    /// presentation update is invalidated: the update replaced the retained
    /// parse artifact the successor would extend, so the stale capability can
    /// neither stage nor close — a mixed parsed program (foreign retained
    /// modules under the successor's claimed source revision) is never
    /// produced.
    #[test]
    fn intervening_presentation_update_invalidates_successor_delta() {
        let (mut session, token, frontier, _pred, _reads, mut assembler) = closed_continuation();
        add_trusted_option(&mut assembler);
        let successor = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let delta = session
            .publish_trusted_toolchain_successor(token, &frontier, &successor, reads)
            .expect("a strictly-additive successor publishes");

        // An intervening presentation update installs a successful parse of a
        // DIFFERENT snapshot (unrelated content and file order).
        let foreign = snapshot(
            &[
                (2, "/q/aux.rue", "aux.rue", "pub fn v() -> i32 { 2 }"),
                (1, "/q/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            1,
        );
        session
            .update_for_presentation(&foreign)
            .into_result()
            .expect("the foreign presentation update parses");

        let stage_err = session
            .stage_import_discovery_successor(&delta)
            .unwrap_err();
        assert!(
            stage_err
                .first()
                .unwrap()
                .to_string()
                .contains("no outstanding successor-delta authority"),
            "{stage_err:?}",
        );
        let close_err = session
            .close_import_discovery_successor(&delta)
            .unwrap_err();
        assert!(
            close_err
                .first()
                .unwrap()
                .to_string()
                .contains("no outstanding successor-delta authority"),
            "{close_err:?}",
        );
    }

    /// Substituted snapshots, contexts, provenance manifests, and ledgers are
    /// INEXPRESSIBLE at the successor stage/close: those APIs consume only the
    /// compiler-published view and the opaque capability. The one remaining host
    /// input surface on a same-generation lineage is the observation-batch
    /// publication, so the tampering regressions below attack through it; the
    /// overlay publication re-derives and justifies every addition, rejecting
    /// each attack before anything is published.
    ///
    /// Run one tampered batch publication against a closed lineage whose rooted
    /// frontier witness is empty, returning the rejection text.
    fn tampered_batch_error(
        build: impl FnOnce(&crate::ImportDiscoveryContext) -> crate::DiscoverySourceAssembler,
    ) -> String {
        let (mut session, _token, frontier, _pred, _reads, _assembler) = closed_continuation();
        let ctx = continuation_std_context();
        let mut tampered = build(&ctx);
        let snapshot = tampered.snapshot().unwrap();
        let reads = tampered.accepted_read_manifest();
        session
            .publish_import_observation_batch(&frontier, &snapshot, reads, Vec::new())
            .unwrap_err()
            .to_string()
    }

    /// A batch cannot INJECT a module: a snapshot carrying a module no accepted
    /// observation of that batch resolves is rejected at publication, so an
    /// unrelated module can never enter the published lineage (and therefore can
    /// never reach a successor stage/close, which read only the published view).
    #[test]
    fn observation_batch_rejects_an_injected_module() {
        let error = tampered_batch_error(|ctx| {
            let mut assembler = crate::DiscoverySourceAssembler::new(
                ctx.clone(),
                "/project/main.rue",
                "/project/main.rue",
                crate::PhysicalFileIdentity::new(1, 1),
                continuation_metadata(),
                Arc::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }".to_owned()),
            )
            .unwrap();
            // An extra module with provenance but NO justifying observation.
            add_trusted_option(&mut assembler);
            assembler
        });
        assert!(
            error.contains("must equal this step's authorized additions exactly"),
            "{error}",
        );
    }

    /// A batch cannot MUTATE a predecessor module under its ID: a snapshot whose
    /// root module has the same identity but different content is rejected at
    /// publication (the lineage is strictly additive at that boundary).
    #[test]
    fn observation_batch_rejects_a_mutated_predecessor_source() {
        let error = tampered_batch_error(|ctx| {
            crate::DiscoverySourceAssembler::new(
                ctx.clone(),
                "/project/main.rue",
                "/project/main.rue",
                crate::PhysicalFileIdentity::new(1, 1),
                continuation_metadata(),
                // Same root identity, DIFFERENT body.
                Arc::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 42 }".to_owned()),
            )
            .unwrap()
        });
        assert!(
            error.contains("mutates a predecessor module source"),
            "{error}",
        );
    }

    /// A batch cannot OMIT an accepted module: publishing the exact
    /// compiler-issued accepted observation for a newly resolved module while
    /// omitting that module from the successor snapshot is rejected — the
    /// additions must EQUAL the batch's accepted resolutions in both
    /// directions, so topology can never claim "resolved" without the module's
    /// source leaf behind it.
    #[test]
    fn observation_batch_rejects_omitting_an_accepted_module() {
        let ctx = continuation_std_context();
        let root_source = "const a = @import(\"a.rue\"); fn main() -> i32 { 0 }";
        let mut assembler = crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            Arc::new(root_source.to_owned()),
        )
        .unwrap();
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let mut session = CompilerSession::new();
        let revision = session
            .begin_import_input_request(&snapshot, ctx.clone(), reads.clone())
            .unwrap();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                ctx.clone(),
                reads.shared_slice(),
                crate::ImportObservationLedger::default(),
            )
            .unwrap();
        let roots = plan.demand_roots();
        let frontier = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                crate::ImportDemandMode::Rooted,
                &roots,
            )
            .unwrap();
        assert!(
            !frontier.requests().is_empty(),
            "an unresolved import demands host reads",
        );

        // Answer the frontier honestly for a.rue: the exact compiler-issued
        // accepted observation, absent elsewhere.
        let module_source = "pub fn value() -> i32 { 1 }";
        let observations: Vec<crate::ImportObservation> = frontier
            .requests()
            .iter()
            .map(|request| {
                if request.requested_path() == "/project/a.rue" {
                    crate::ImportObservation::accepted(
                        request.clone(),
                        crate::AcceptedImportSource::new(
                            Arc::from("/project/a.rue"),
                            Arc::from("/project/a.rue"),
                            crate::PhysicalFileIdentity::new(5, 5),
                            continuation_metadata(),
                            Arc::new(module_source.to_owned()),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                } else {
                    crate::ImportObservation::absent(request.clone())
                }
            })
            .collect();

        // A manifest carrying the resolved module's provenance, but a snapshot
        // OMITTING the module itself.
        let mut with_module = crate::DiscoverySourceAssembler::new(
            ctx.clone(),
            "/project/main.rue",
            "/project/main.rue",
            crate::PhysicalFileIdentity::new(1, 1),
            continuation_metadata(),
            Arc::new(root_source.to_owned()),
        )
        .unwrap();
        with_module
            .add_explicit(
                "/project/a.rue",
                "/project/a.rue",
                crate::PhysicalFileIdentity::new(5, 5),
                continuation_metadata(),
                Arc::new(module_source.to_owned()),
            )
            .unwrap();
        let reads_with_module = with_module.accepted_read_manifest();
        let err = session
            .publish_import_observation_batch(&frontier, &snapshot, reads_with_module, observations)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("must equal this step's authorized additions exactly"),
            "{err}",
        );
    }

    /// A batch cannot SUBSTITUTE predecessor provenance: an accepted-read entry
    /// for an existing module with altered physical identity is rejected at
    /// publication.
    #[test]
    fn observation_batch_rejects_substituted_provenance() {
        let error = tampered_batch_error(|ctx| {
            crate::DiscoverySourceAssembler::new(
                ctx.clone(),
                "/project/main.rue",
                "/project/main.rue",
                // Same content, DIFFERENT physical identity: the module revision
                // matches but its provenance record does not.
                crate::PhysicalFileIdentity::new(7, 7),
                continuation_metadata(),
                Arc::new("fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }".to_owned()),
            )
            .unwrap()
        });
        assert!(
            error.contains("mutates a predecessor accepted-read provenance"),
            "{error}",
        );
    }

    #[test]
    fn stable_no_filesystem_boundary_classifies_unsatisfied_toolchain_input_not_ice() {
        // RUE-1112 C3: the stable no-filesystem `canonical_semantic` entry cannot
        // acquire, so an unsatisfied trusted-toolchain demand for otherwise-valid
        // source is a deterministic CONTRACT failure, never an ICE (E9000).
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        let error = errors.first().expect("unsatisfied toolchain input error");
        assert!(
            matches!(
                error.kind,
                rue_error::ErrorKind::UnsatisfiedTrustedToolchainInput(_)
            ),
            "expected an unsatisfied-trusted-toolchain-input classification, got {:?}",
            error.kind
        );
        assert_ne!(error.kind.code(), rue_error::ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            error.kind.code(),
            rue_error::ErrorCode::UNSATISFIED_TRUSTED_TOOLCHAIN_INPUT
        );
    }

    #[test]
    fn provider_observation_counters_record_exact_production_work() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(2) }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .expect("the program analyzes");
        let metrics = crate::unstable::provider_observation_metrics(&session);
        assert_eq!(metrics.name_lookups, 3);
        assert_eq!(metrics.import_lookups, 0);
        assert_eq!(metrics.method_candidates, 0);
        assert_eq!(metrics.operator_candidates, 0);
        assert_eq!(metrics.declaration_facts, 25);
        assert!(metrics.identity_facts > 0, "{metrics:?}");
        assert!(metrics.signature_facts > 0, "{metrics:?}");
        assert!(metrics.materializations > 0, "{metrics:?}");
        assert_eq!(
            metrics.declaration_facts,
            metrics.identity_facts
                + metrics.signature_facts
                + metrics.type_facts
                + metrics.const_facts,
            "the declaration aggregate must be exactly partitioned by real fact families"
        );
        assert_eq!(metrics.anonymous_facts, 0);
        assert_eq!(metrics.producer_facts, 0);
        assert_eq!(metrics.toolchain_facts, 4);
    }

    #[test]
    fn published_lookup_root_lease_retains_production_body_lookups() {
        // Production body analysis publishes its exact lookup-name terminals
        // into the session lease. The lease owns retention independently from
        // the request-scoped provider that observed those terminals.
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(2) }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .expect("the program analyzes");

        let metrics = crate::unstable::lookup_pressure_metrics(&session);
        assert!(metrics.published_roots > 0, "{metrics:?}");
        assert!(metrics.leased_terminals > 0, "{metrics:?}");
        assert!(metrics.retained_logical_keys > 0, "{metrics:?}");
        assert!(metrics.retained_family_nodes > 0, "{metrics:?}");
        assert!(metrics.retained_family_terminals > 0, "{metrics:?}");
        assert_eq!(
            metrics.protected_growth, 0,
            "no lease supersession grew a family"
        );
        assert_eq!(
            metrics.evictions, 0,
            "no lease supersession evicted a terminal"
        );
        assert_eq!(metrics.rederivations_after_eviction, 0);
        // Exact lookup collection runs through the same provider-backed body
        // transaction as production analysis.
        assert!(
            crate::unstable::provider_observation_metrics(&session).name_lookups > 0,
            "production lookup terminals must be observed by the provider"
        );
    }

    #[test]
    fn successful_closure_retires_unreachable_and_deleted_body_lookup_roots() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
            )],
            1,
        );
        let unreachable = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } fn main() -> i32 { 0 }",
            )],
            1,
        );
        let deleted = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .expect("initial closure analyzes");
        let initial = crate::unstable::lookup_pressure_metrics(&session);
        assert_eq!(initial.published_roots, 2, "{initial:?}");

        session.update(&unreachable).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .expect("unreachable successor analyzes");
        let after_unreachable = crate::unstable::lookup_pressure_metrics(&session);
        assert_eq!(
            after_unreachable.published_roots, 1,
            "helper root must retire when it leaves the reached closure: {after_unreachable:?}"
        );

        session.update(&deleted).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .expect("deleted successor analyzes");
        let after_deleted = crate::unstable::lookup_pressure_metrics(&session);
        assert_eq!(
            after_deleted.published_roots, 1,
            "deleting an already-unreachable body cannot resurrect its root: {after_deleted:?}"
        );
    }

    #[test]
    fn nested_duplicate_parameter_diagnostics_rejoin_the_exact_current_occurrence() {
        for source_text in [
            "struct S {\n    fn m(self, a: i32, a: i32) {}\n}\nfn main() {}",
            "struct S {\n    fn make(a: i32, a: i32) {}\n}\nfn main() {}",
        ] {
            let duplicate_start = source_text
                .rfind("a: i32")
                .expect("fixture contains the duplicate parameter");
            let expected = rue_span::Span::with_file(
                FileId::new(1),
                duplicate_start as u32,
                (duplicate_start + "a: i32".len()) as u32,
            );
            let source = snapshot(&[(1, "/p/main.rue", "main.rue", source_text)], 1);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let errors = session
                .canonical_semantic(&CompileOptions::default())
                .unwrap_err();
            let error = errors.first().expect("duplicate parameter diagnostic");
            assert!(
                error.to_string().contains("duplicate parameter name 'a'"),
                "unexpected diagnostic: {error}"
            );
            assert_eq!(error.span(), Some(expected));
        }
    }

    #[test]
    fn anonymous_comptime_producer_failure_is_a_deterministic_diagnostic() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn empty() -> type {\n    struct { }\n}\nfn main() -> i32 {\n    let E = empty();\n    0\n}",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();

        let first = session
            .canonical_semantic(&CompileOptions::default())
            .expect_err("empty anonymous struct is rejected");
        let second = session
            .canonical_semantic(&CompileOptions::default())
            .expect_err("the retained producer failure remains deterministic");

        assert_eq!(first, second);
        assert_eq!(first.len(), 1, "unexpected diagnostics: {first:?}");
        let diagnostic = first.first().unwrap();
        assert!(
            matches!(&diagnostic.kind, ErrorKind::EmptyStruct),
            "producer failure must remain a source diagnostic, not request cancellation: {diagnostic:?}"
        );
    }

    #[test]
    fn keyed_destructor_validity_preserves_production_diagnostics_and_spans() {
        for (source_text, marker, message) in [
            (
                "drop fn Missing(self) {}\nfn main() {}",
                "drop fn Missing(self) {}",
                "unknown type 'Missing' in destructor",
            ),
            (
                "struct S {}\ndrop fn S(self) {}\ndrop fn S(self) {}\nfn main() {}",
                "drop fn S(self) {}",
                "duplicate destructor for type 'S'",
            ),
        ] {
            let declaration_start = source_text
                .rfind(marker)
                .expect("fixture contains the rejected destructor");
            let expected = rue_span::Span::with_file(
                FileId::new(1),
                declaration_start as u32,
                (declaration_start + marker.len()) as u32,
            );
            let source = snapshot(&[(1, "/p/main.rue", "main.rue", source_text)], 1);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let errors = session
                .canonical_semantic(&CompileOptions::default())
                .unwrap_err();
            let error = errors.first().expect("destructor validity diagnostic");
            assert!(
                error.to_string().contains(message),
                "unexpected diagnostic: {error}"
            );
            assert_eq!(error.span(), Some(expected));
        }
    }

    /// Generate `QUERY_TERMINAL_RETENTION_LIMIT + 1` distinct `CompileOptions`
    /// for terminal retention/eviction tests (one more key than the store can
    /// hold, forcing exactly one eviction). The low `feature_bits` bits of each
    /// mask pick a preview-feature subset; the remaining high bits index the
    /// compile target, so every mask is a distinct `(preview_features, target)`
    /// pair. Both dimensions are part of the semantic, definition, and
    /// dependency-manifest query keys — unlike `opt_level`, which keys only the
    /// semantic store — so the variants force one eviction in every terminal
    /// family under test.
    ///
    /// With `2` preview features the mask spans four subsets, and three targets
    /// carry them to `4 * 3 = 12` distinct keys, exactly `LIMIT + 1`. The
    /// assertion guards that the mask range spans no more `feature_bits`-wide
    /// blocks than there are targets; if a future feature-count drop breaks it,
    /// `QUERY_TERMINAL_RETENTION_LIMIT` must fall so `LIMIT + 1` still fits the
    /// available `(preview_features, target)` key space.
    #[allow(dead_code)]
    fn retention_variants() -> Vec<CompileOptions> {
        let feature_bits = PreviewFeature::all().len();
        let targets = Target::all();
        assert!(
            (QUERY_TERMINAL_RETENTION_LIMIT >> feature_bits) < targets.len(),
            "retention variants need a distinct (preview_features, target) key per mask"
        );
        (0..=QUERY_TERMINAL_RETENTION_LIMIT)
            .map(|mask| CompileOptions {
                preview_features: PreviewFeature::all()
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| mask & (1 << bit) != 0)
                    .map(|(_, feature)| *feature)
                    .collect(),
                target: targets[mask >> feature_bits],
                ..CompileOptions::default()
            })
            .collect()
    }

    #[test]
    fn nested_layout_change_invalidates_only_layout_consumers() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Inner { a: i32 }\nstruct Outer { inner: Inner }\nfn consume(value: Outer) -> i32 { value.inner.a }\nfn unaffected() -> i32 { 7 }\nfn main() -> i32 { consume(Outer { inner: Inner { a: 1 } }) + unaffected() }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Inner { a: i32, b: i32 }\nstruct Outer { inner: Inner }\nfn consume(value: Outer) -> i32 { value.inner.a }\nfn unaffected() -> i32 { 7 }\nfn main() -> i32 { consume(Outer { inner: Inner { a: 1, b: 2 } }) + unaffected() }",
            )],
            1,
        );
        let options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let consume_key = body_query_key(&mut session, &options, "consume");
        let consume_transaction = retained_body_transaction(&session, &consume_key).2;
        assert!(
            consume_transaction.references().0.iter().any(|reference| {
                matches!(
                    reference,
                    crate::body_query::BodyReference::Type(crate::TypeInstanceKey::Nominal(
                        crate::NominalInstanceKey::Named(definition)
                    )) if definition.name() == "Inner"
                )
            }),
            "{consume_transaction:?}"
        );
        let dependency_nodes = retained_body_dependency_nodes(&session, &consume_key);
        assert!(
            dependency_nodes
                .iter()
                .any(|node| node.contains("signature") && node.contains("Inner")),
            "{dependency_nodes:?}"
        );
        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(warm.work().cfg.cfg_reuses, 1);
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 2);
        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", warm.functions()),
            format!("{:?}", fresh.functions())
        );
    }

    #[test]
    fn pointer_only_consumer_ignores_pointee_layout_but_field_consumer_rebuilds() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Foo { a: i32 }\nfn pointer_only(value: ptr const Foo) -> i32 { 7 }\nfn field(value: Foo) -> i32 { value.a }\nfn main() -> i32 { let value = Foo { a: 1 }; checked { pointer_only(@raw(value)) + field(value) } }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Foo { a: i32, b: i32 }\nfn pointer_only(value: ptr const Foo) -> i32 { 7 }\nfn field(value: Foo) -> i32 { value.a }\nfn main() -> i32 { let value = Foo { a: 1, b: 2 }; checked { pointer_only(@raw(value)) + field(value) } }",
            )],
            1,
        );
        let options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(warm.work().cfg.cfg_reuses, 1);
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 2);
        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", warm.functions()),
            format!("{:?}", fresh.functions())
        );
    }

    #[test]
    fn cfg_reuse_is_per_function_and_preserves_exact_build_work() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn a() -> i32 { @dbg(\"same\"); @dbg(\"same\"); @dbg(\"alpha\"); 1 }\n\
                 fn b() -> i32 { @dbg(\"beta\"); 2 }\n\
                 fn main() -> i32 { @dbg(\"gamma\"); a() + b() }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "// move every retained body and perturb another body's string projection\n\
                 fn a() -> i32 { @dbg(\"same\"); @dbg(\"same\"); @dbg(\"alpha\"); 1 }\n\
                 fn b() -> i32 { @dbg(\"delta\"); @dbg(\"beta\"); 3 }\n\
                 fn main() -> i32 { @dbg(\"gamma\"); a() + b() }",
            )],
            1,
        );
        let options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.canonical_semantic(&options).unwrap();
        assert_eq!(cold.work().cfg.cfg_builds_attempted, 3);
        assert_eq!(cold.work().cfg.optimization_attempts, 3);
        let cold_atoms = cold
            .functions()
            .iter()
            .flat_map(|function| function.local_atoms.iter())
            .collect::<Vec<_>>();
        assert_eq!(cold_atoms.len(), 5);
        assert_eq!(
            cold_atoms
                .iter()
                .filter(|atom| atom.content.as_ref() == "same")
                .count(),
            2
        );
        assert!(cold_atoms.iter().any(|atom| atom.dense_id > 1));
        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            warm.work().cfg.cfg_reuses,
            2,
            "cfg work: {:?}",
            warm.work().cfg
        );
        assert_eq!(warm.work().cfg.cfg_import_successes, 2);
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 1);
        assert_eq!(warm.work().cfg.optimization_attempts, 1);
        assert_eq!(warm.work().cfg.optimized_level_attempts, 1);
        for function in warm.functions() {
            for atom in &function.local_atoms {
                assert_eq!(
                    warm.strings()
                        .get(atom.dense_id as usize)
                        .map(String::as_str),
                    Some(atom.content.as_ref())
                );
            }
        }

        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", warm.functions()),
            format!("{:?}", fresh.functions())
        );
        assert_eq!(
            format!("{:?}", warm.warnings()),
            format!("{:?}", fresh.warnings())
        );
    }

    #[test]
    fn cfg_local_materialization_preserves_body_callable_names() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn probe() -> u32 { @random_u32() }\nfn main() { probe(); }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let semantic = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let names = semantic
            .functions()
            .iter()
            .map(|function| (function.analyzed.name.as_str(), function.cfg.fn_name()))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            names.get("__rue_fn_main_2erue__probe"),
            Some(&"__rue_fn_main_2erue__probe")
        );
        assert_eq!(names.get("main"), Some(&"main"));
    }

    #[test]
    fn cfg_drop_facts_invalidate_same_layout_cleanup_changes() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Resource { value: i32 }\n\
                 fn main() -> i32 { let resource = Resource { value: 1 }; 0 }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Resource { value: i32 }\n\
                 drop fn Resource(self) { @dbg(self.value); }\n\
                 fn main() -> i32 { let resource = Resource { value: 1 }; 0 }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert!(
            warm.work().cfg.cfg_builds_attempted > 0,
            "same-layout drop-fact changes must rebuild affected CFGs: {:?}",
            warm.work().cfg
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", warm.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh.functions()))
        );
    }

    #[test]
    fn cfg_relocation_covers_runtime_param_drop_and_nominal_field_domains() {
        let program = |prefix: &str| {
            format!(
                "{prefix}\
                 struct Leaf {{ value: i32 }}\n\
                 drop fn Leaf(self) {{ @dbg(self.value); }}\n\
                 struct Holder {{ leaf: Leaf }}\n\
                 fn consume(value: Holder) -> i32 {{\n\
                     @dbg(value.leaf.value);\n\
                     value.leaf.value\n\
                 }}\n\
                 fn main() -> i32 {{ consume(Holder {{ leaf: Leaf {{ value: 7 }} }}) }}"
            )
        };
        let first_text = program("");
        let second_text = program(
            "struct Noise { pad: i64 }\n\
             fn noise(value: Noise) -> i64 { @assert(value.pad >= 0); value.pad }\n",
        );
        let first = snapshot(&[(1, "/p/main.rue", "main.rue", first_text.as_str())], 1);
        let second = snapshot(&[(1, "/p/main.rue", "main.rue", second_text.as_str())], 1);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            warm.work().cfg.cfg_builds_attempted,
            0,
            "live-domain relocation must not rebuild reusable unoptimized CFGs: {:?}",
            warm.work().cfg
        );
        assert_eq!(
            warm.work().cfg.cfg_reuses,
            5,
            "every unchanged reachable CFG must be reused: {:?}",
            warm.work().cfg
        );
        assert_eq!(
            warm.work().cfg.optimization_attempts,
            0,
            "complete relocation domains must reuse optimized terminals: {:?}",
            warm.work().cfg
        );
        assert_eq!(
            warm.work().cfg.cfg_import_successes,
            warm.work().cfg.cfg_reuses,
            "the collector must receive already-relocated optimized terminals: {:?}",
            warm.work().cfg
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", warm.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh.functions()))
        );
        assert_eq!(
            format!("{:?}", warm.warnings()),
            format!("{:?}", fresh.warnings())
        );
    }

    #[test]
    fn cfg_terminal_owned_domain_relocates_without_rebuild() {
        let first = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 7 }")],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "// relocate the retained body\nfn main() -> i32 { 7 }",
            )],
            1,
        );
        let options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(warm.work().cfg.cfg_reuse_candidates, 0);
        assert_eq!(warm.work().cfg.cfg_reuses, 1);
        assert_eq!(warm.work().cfg.cfg_import_attempts, 1);
        assert_eq!(warm.work().cfg.cfg_import_successes, 1);
        assert_eq!(warm.work().cfg.cfg_import_failures, 0);
        assert_eq!(warm.work().cfg.cfg_fallbacks, 0);
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 0);
        assert_eq!(warm.work().cfg.cfg_builds_succeeded, 0);
        assert_eq!(warm.work().cfg.cfg_builds_failed, 0);
        assert_eq!(warm.work().cfg.optimization_attempts, 0);
        assert_eq!(warm.work().cfg.optimization_completions, 0);
        assert_eq!(warm.work().cfg.optimized_level_attempts, 0);

        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", warm.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh.functions()))
        );
    }

    #[test]
    fn reused_parse_runtime_symbol_relocates_to_the_current_interner() {
        let program = |prefix: &str| {
            format!(
                "{prefix}\
                 const opt = @import(\"std/option.rue\");\n\
                 fn parse_runtime() -> i32 {{ let _ = @parse_i64(\"7\"); 0 }}\n\
                 fn main() -> i32 {{ parse_runtime() }}"
            )
        };
        let first_text = program("");
        let second_text =
            program("struct Noise { value: i64 }\nfn noise(value: Noise) -> i64 { value.value }\n");
        let first = well_known_option_isolation_snapshot(&first_text);
        let second = well_known_option_isolation_snapshot(&second_text);
        let options = CompileOptions::default();
        let runtime_symbols = |output: &crate::CanonicalSemanticOutput| {
            output
                .functions()
                .iter()
                .flat_map(|function| {
                    let cfg = &function.cfg;
                    cfg.blocks()
                        .iter()
                        .flat_map(|block| block.insts.iter())
                        .filter_map(|value| match cfg.get_inst(*value).data {
                            rue_cfg::CfgInstData::Intrinsic {
                                runtime: Some(rue_air::RuntimeCallKind::ParseI64),
                                name,
                                ..
                            } => Some(name),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };

        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &first);
        session.canonical_semantic(&options).unwrap();
        publish_with_test_imports(&mut session, &second);
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 0);
        assert!(warm.work().cfg.cfg_reuses >= 2, "{:?}", warm.work().cfg);
        assert_eq!(
            warm.work().cfg.optimization_attempts,
            0,
            "the collector must relocate the complete optimized terminal: {:?}",
            warm.work().cfg
        );
        let warm_symbols = runtime_symbols(&warm);
        assert_eq!(warm_symbols.len(), 1);
        let warm_rir = warm.rir_owner();
        assert_eq!(
            warm_rir
                .semantic_symbols()
                .interner()
                .resolve(&warm_symbols[0]),
            "parse_i64"
        );

        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &second);
        let fresh_output = fresh.canonical_semantic(&options).unwrap();
        let fresh_symbols = runtime_symbols(&fresh_output);
        assert_eq!(fresh_symbols.len(), 1);
        let fresh_rir = fresh_output.rir_owner();
        assert_eq!(
            fresh_rir
                .semantic_symbols()
                .interner()
                .resolve(&fresh_symbols[0]),
            "parse_i64"
        );
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", warm.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh_output.functions()))
        );
    }

    #[test]
    fn reused_print_runtime_call_relocates_to_the_current_helper_symbol() {
        let program = |prefix: &str| {
            format!(
                "{prefix}\
                 fn probe_print() {{ println(\"literal\"); }}\n\
                 fn main() -> i32 {{ probe_print(); 0 }}"
            )
        };
        let runtime_calls = |output: &crate::CanonicalSemanticOutput| {
            output
                .functions()
                .iter()
                .flat_map(|function| {
                    let cfg = &function.cfg;
                    cfg.blocks()
                        .iter()
                        .flat_map(|block| block.insts.iter())
                        .filter_map(|value| match cfg.get_inst(*value).data {
                            rue_cfg::CfgInstData::Call {
                                runtime: Some(runtime),
                                name,
                                ..
                            } => Some((runtime, name)),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let first_text = program("");
        let second_text = program("fn noise() -> i32 { let interner_churn = 1; interner_churn }\n");
        let first = snapshot(&[(1, "/p/main.rue", "main.rue", &first_text)], 1);
        let second = snapshot(&[(1, "/p/main.rue", "main.rue", &second_text)], 1);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.canonical_semantic(&options).unwrap();
        let cold_calls = runtime_calls(&cold);
        assert_eq!(cold_calls.len(), 1);
        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 0);
        assert_eq!(warm.work().cfg.cfg_reuses, 2);
        assert_eq!(warm.work().cfg.optimization_attempts, 0);
        let warm_calls = runtime_calls(&warm);
        assert_eq!(warm_calls.len(), 1);
        assert_ne!(
            cold_calls, warm_calls,
            "the inserted declaration must perturb the live runtime-call symbol"
        );
        let warm_rir = warm.rir_owner();
        for (runtime, symbol) in &warm_calls {
            assert_eq!(
                warm_rir.semantic_symbols().interner().resolve(symbol),
                runtime.helper().helper().symbol
            );
        }

        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh_output = fresh.canonical_semantic(&options).unwrap();
        let fresh_calls = runtime_calls(&fresh_output);
        assert_eq!(fresh_calls.len(), 1);
        let fresh_rir = fresh_output.rir_owner();
        for (runtime, symbol) in &fresh_calls {
            assert_eq!(
                fresh_rir.semantic_symbols().interner().resolve(symbol),
                runtime.helper().helper().symbol
            );
        }
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", warm.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh_output.functions()))
        );
    }

    #[test]
    fn opt_level_only_change_reuses_cfg_and_recomputes_optimization_per_function() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn left() -> i32 { 20 }\n\
                 fn right() -> i32 { 22 }\n\
                 fn main() -> i32 { left() + right() }",
            )],
            1,
        );
        let o0 = CompileOptions {
            opt_level: OptLevel::O0,
            ..CompileOptions::default()
        };
        let o1 = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();

        let cold = session.canonical_semantic(&o0).unwrap();
        assert_eq!(cold.functions().len(), 3);
        assert_eq!(cold.work().cfg.cfg_builds_attempted, 3);
        assert_eq!(cold.work().cfg.optimization_attempts, 3);

        let optimized = session.canonical_semantic(&o1).unwrap();
        assert_eq!(optimized.functions().len(), 3);
        assert_eq!(optimized.work().cfg.cfg_builds_attempted, 0);
        assert_eq!(optimized.work().cfg.cfg_builds_succeeded, 0);
        assert_eq!(optimized.work().cfg.cfg_builds_failed, 0);
        assert_eq!(optimized.work().cfg.cfg_reuses, 3);
        assert_eq!(optimized.work().cfg.cfg_import_attempts, 3);
        assert_eq!(optimized.work().cfg.cfg_import_successes, 3);
        assert_eq!(optimized.work().cfg.optimization_attempts, 3);
        assert_eq!(optimized.work().cfg.optimization_completions, 3);
        assert_eq!(optimized.work().cfg.optimized_level_attempts, 3);
    }

    #[test]
    fn specialized_cfg_reuse_is_stable_across_unrelated_body_edits() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn choose(comptime n: i32) -> i32 { n }\nfn b() -> i32 { 2 }\nfn main() -> i32 { choose(40) + b() }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn choose(comptime n: i32) -> i32 { n }\nfn b() -> i32 { 3 }\nfn main() -> i32 { choose(40) + b() }",
            )],
            1,
        );
        let options = CompileOptions {
            opt_level: OptLevel::O0,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert!(warm.functions().iter().any(|function| {
            matches!(
                function.semantic_identity,
                crate::FunctionInstanceKey::Specialization { .. }
            )
        }));
    }

    #[test]
    fn cfg_reuse_rejects_target_and_callable_identity_changes() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn old() -> i32 { 1 }\nfn stable() -> i32 { 2 }\nfn main() -> i32 { old() + stable() }",
            )],
            1,
        );
        let renamed = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn new() -> i32 { 1 }\nfn stable() -> i32 { 2 }\nfn main() -> i32 { new() + stable() }",
            )],
            1,
        );
        let host = Target::host().unwrap();
        let other = if host == Target::Aarch64Linux {
            Target::X86_64Linux
        } else {
            Target::Aarch64Linux
        };
        let first_options = CompileOptions {
            target: host,
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let other_options = CompileOptions {
            target: other,
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&first_options).unwrap();
        let cross_target = session.canonical_semantic(&other_options).unwrap();
        assert_eq!(cross_target.work().cfg.cfg_reuses, 0);
        assert_eq!(cross_target.work().cfg.cfg_builds_attempted, 3);
        session.update(&renamed).into_result().unwrap();
        let changed = session.canonical_semantic(&other_options).unwrap();
        assert_eq!(changed.work().cfg.cfg_reuses, 1);
        assert_eq!(changed.work().cfg.cfg_builds_attempted, 2);
        let mut fresh = CompilerSession::new();
        fresh.update(&renamed).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&other_options).unwrap();
        assert_eq!(
            format!("{:?}", changed.functions()),
            format!("{:?}", fresh.functions())
        );
    }

    #[test]
    fn value_constants_install_from_the_semantic_nucleus_without_fallback() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const n: i32 = 1; fn main() -> i32 { n }",
            )],
            1,
        );
        let second = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const n: i32 = 1; fn main() -> i32 { n + 1 }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let main = body_query_key(&mut session, &options, "main");
        let (first_stamp, _, first_transaction) = retained_body_transaction(&session, &main);
        session.update(&second).into_result().unwrap();
        let output = session.canonical_semantic(&options).unwrap();
        let (second_stamp, _, second_transaction) = retained_body_transaction(&session, &main);
        assert_ne!(first_stamp, second_stamp);
        assert!(matches!(
            first_transaction,
            crate::body_query::BodyTransaction::Success { .. }
        ));
        assert!(matches!(
            second_transaction,
            crate::body_query::BodyTransaction::Success { .. }
        ));
        assert_eq!(output.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(output.work().binding.durable_install_invocations, 1);
        assert_eq!(output.work().declaration_reuse.durable_records_reused, 2);
    }

    fn assert_semantic_artifact_parity(
        session: &CompilerSession,
        actual: &CanonicalSemanticOutput,
        fresh: &CanonicalSemanticOutput,
    ) {
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", actual.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh.functions()))
        );
        assert_eq!(actual.strings(), fresh.strings());
        assert_eq!(
            format!("{:?}", actual.warnings()),
            format!("{:?}", fresh.warnings())
        );
        let diagnostics = session
            .latest_diagnostics()
            .expect("semantic query publishes diagnostics");
        assert!(diagnostics.is_success());
        assert_eq!(
            format!("{:?}", diagnostics.warnings()),
            format!("{:?}", fresh.warnings())
        );
    }

    fn normalize_session_local_spurs(value: String) -> String {
        let mut normalized = String::with_capacity(value.len());
        let mut rest = value.as_str();
        while let Some(start) = rest.find("Spur(") {
            normalized.push_str(&rest[..start]);
            normalized.push_str("Spur(_)");
            let after = &rest[start + "Spur(".len()..];
            let Some(end) = after.find(')') else {
                normalized.push_str(after);
                return normalized;
            };
            rest = &after[end + 1..];
        }
        normalized.push_str(rest);
        normalized
    }

    fn assert_body_artifact_parity(
        actual: &CanonicalSemanticOutput,
        fresh: &CanonicalSemanticOutput,
    ) {
        assert_eq!(
            format!("{:?}", actual.functions()),
            format!("{:?}", fresh.functions())
        );
        assert_eq!(actual.strings(), fresh.strings());
        assert_eq!(
            format!("{:?}", actual.warnings()),
            format!("{:?}", fresh.warnings())
        );
        assert_eq!(actual.type_pool().stats(), fresh.type_pool().stats());
    }

    fn assert_diagnostic_parity(actual: &CompilerSession, fresh: &CompilerSession) {
        let actual = actual.latest_diagnostics().unwrap();
        let fresh = fresh.latest_diagnostics().unwrap();
        assert_eq!(
            format!("{:?}", actual.stage()),
            format!("{:?}", fresh.stage())
        );
        assert_eq!(
            format!("{:?}", actual.errors()),
            format!("{:?}", fresh.errors())
        );
        assert_eq!(
            format!("{:?}", actual.warnings()),
            format!("{:?}", fresh.warnings())
        );
    }

    #[test]
    fn composite_generic_signature_reuses_across_relocation_and_specialization_edit() {
        let source = |file, physical: &str, value| {
            snapshot(
                &[(
                    file,
                    physical,
                    "main.rue",
                    &format!(
                        "fn first(comptime T: type, values: [[T; 2]; 2]) -> T {{ values[0][0] }} fn main() -> i32 {{ first(i32, [[1, 2], [3, {value}]]) }}"
                    ),
                )],
                file,
            )
        };
        let first = source(1, "/old/main.rue", 4);
        let relocated_edit = source(99, "/new/main.rue", 5);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&relocated_edit).into_result().unwrap();
        let reused = session.canonical_semantic(&options).unwrap();
        assert_eq!(reused.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(reused.work().binding.durable_payloads_installed, 2);
        assert_eq!(reused.work().declaration_reuse.durable_records_reused, 2);
        assert_eq!(
            reused.work().declaration_reuse.declaration_prefix_fallbacks,
            0
        );

        let mut fresh = CompilerSession::new();
        fresh.update(&relocated_edit).into_result().unwrap();
        let ordinary = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &reused, &ordinary);
        assert_diagnostic_parity(&session, &fresh);
    }

    #[test]
    fn semantic_nucleus_installs_nested_generic_signatures_without_fallback() {
        let source = |value| {
            snapshot(
                &[(
                    1,
                    "/p/main.rue",
                    "main.rue",
                    &format!(
                        "fn first(comptime T: type, values: [[T; 2]; 2]) -> T {{ values[0][0] }} fn main() -> i32 {{ first(i32, [[1, 2], [3, {value}]]) }}"
                    ),
                )],
                1,
            )
        };
        let first = source(4);
        let edited = source(5);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&edited).into_result().unwrap();
        let actual = session.canonical_semantic(&options).unwrap();
        assert_eq!(actual.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(actual.work().declaration_reuse.install_invocations, 1);
        assert_eq!(actual.work().declaration_reuse.durable_records_reused, 2);
        assert_eq!(actual.work().declaration_reuse.fallbacks, 0);
        assert_eq!(
            actual.work().declaration_reuse.declaration_prefix_fallbacks,
            0
        );
        assert_eq!(
            actual.work().declaration_reuse.declaration_prefixes_built,
            1
        );

        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let ordinary = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &actual, &ordinary);
        assert_eq!(actual.type_pool().stats(), ordinary.type_pool().stats());
        assert_diagnostic_parity(&session, &fresh);
    }

    #[test]
    fn comptime_named_method_reuses_declarations_while_body_reuse_fails_closed() {
        let source = |body: &str| snapshot(&[(1, "/p/main.rue", "main.rue", body)], 1);
        let first = source(
            "struct Value { fn choose(borrow self, comptime n: i32) -> i32 { n } } fn main() -> i32 { let value = Value {}; value.choose(1) }",
        );
        let edited = source(
            "struct Value { fn choose(borrow self, comptime n: i32) -> i32 { n + 1 } } fn main() -> i32 { let value = Value {}; value.choose(1) }",
        );
        let supported = source("fn main() -> i32 { 1 }");
        let supported_edit = source("fn main() -> i32 { 2 }");
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.canonical_semantic(&options).unwrap();
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(cold.work().binding.durable_install_invocations, 1);

        session.update(&edited).into_result().unwrap();
        let ordinary = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            ordinary.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(ordinary.work().binding.durable_install_invocations, 1);
        assert_eq!(ordinary.work().binding.durable_payloads_installed, 3);
        assert_eq!(ordinary.work().declaration_reuse.durable_records_reused, 3);
        assert_eq!(
            ordinary
                .work()
                .declaration_reuse
                .declaration_prefix_fallbacks,
            0
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let expected = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &ordinary, &expected);

        // Moving to a different declaration universe seeds a new baseline, and
        // its next body edit can reuse normally.
        session.update(&supported).into_result().unwrap();
        let seeded = session.canonical_semantic(&options).unwrap();
        assert_eq!(seeded.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(seeded.work().binding.durable_install_invocations, 1);
        session.update(&supported_edit).into_result().unwrap();
        let recovered = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            recovered.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(recovered.work().binding.durable_payloads_installed, 1);
    }

    #[test]
    fn anonymous_structural_body_operations_export_durably_after_declaration_reuse() {
        let first = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn Box(comptime T: type) -> type { struct { value: T, fn get(borrow self) -> T { self.value } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 1 }; value.get() }",
            )],
            1,
        );
        let edited = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn Box(comptime T: type) -> type { struct { value: T, fn get(borrow self) -> T { self.value } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 2 }; value.get() }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.canonical_semantic(&options).unwrap();
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(cold.work().binding.durable_install_invocations, 1);

        session.update(&edited).into_result().unwrap();
        let ordinary = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            ordinary.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(ordinary.work().binding.durable_install_invocations, 1);
        assert_eq!(ordinary.work().binding.durable_payloads_installed, 2);
        assert_eq!(ordinary.work().declaration_reuse.durable_records_reused, 2);
        assert_eq!(
            ordinary
                .work()
                .declaration_reuse
                .declaration_prefix_fallbacks,
            0
        );
        // Type producers are query inputs, not runtime function bodies. The
        // reached executable set is `main` plus the anonymous `get` method.
        assert_eq!(ordinary.functions().len(), 2);
        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let expected = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &ordinary, &expected);

        let supported = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
            1,
        );
        let supported_edit = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 2 }")],
            1,
        );
        session.update(&supported).into_result().unwrap();
        let seeded = session.canonical_semantic(&options).unwrap();
        assert_eq!(seeded.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(seeded.work().binding.durable_install_invocations, 1);
        session.update(&supported_edit).into_result().unwrap();
        let recovered = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            recovered.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(recovered.work().binding.durable_payloads_installed, 1);
    }

    #[test]
    fn signature_target_and_failed_body_changes_fail_closed_and_recovery_reuses() {
        let base = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i32 { 1 } fn main() { value(); }",
            )],
            1,
        );
        let signature = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { 1 } fn main() { value(); }",
            )],
            1,
        );
        let broken_body = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { missing } fn main() { value(); }",
            )],
            1,
        );
        let recovered = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { 2 } fn main() { value(); }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&base).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&signature).into_result().unwrap();
        let changed = session.canonical_semantic(&options).unwrap();
        assert_eq!(changed.work().binding.declaration_resolution_invocations, 0);

        session.update(&broken_body).into_result().unwrap();
        assert!(session.canonical_semantic(&options).is_err());
        session.update(&recovered).into_result().unwrap();
        let recovered = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            recovered.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(recovered.work().declaration_reuse.durable_records_reused, 2);

        let mut other_target = options.clone();
        other_target.target = *Target::all()
            .iter()
            .find(|target| **target != options.target)
            .unwrap();
        let target_changed = session.canonical_semantic(&other_target).unwrap();
        assert_eq!(
            target_changed
                .work()
                .binding
                .declaration_resolution_invocations,
            0
        );
    }

    #[test]
    fn root_relocation_file_id_and_logical_changes_invalidate_correctly() {
        let base = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            1,
        );
        let root_only = snapshot(
            &[
                (1, "/old/a.rue", "a.rue", "fn a() {}"),
                (2, "/old/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let relocated = snapshot(
            &[
                (1, "/new/a.rue", "a.rue", "fn a() {}"),
                (2, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            2,
        );
        let reassigned = snapshot(
            &[
                (11, "/new/a.rue", "a.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let renamed = snapshot(
            &[
                (11, "/new/a2.rue", "a2.rue", "fn a() {}"),
                (12, "/new/b.rue", "b.rue", "fn b() {}"),
            ],
            12,
        );
        let mut session = CompilerSession::new();
        session.update(&base).into_result().unwrap();
        session.canonical_rir().unwrap();

        let root = session.update(&root_only);
        assert!(root.downstream_invalidated());
        assert_eq!(root.work().modules_reused, 2);
        root.into_result().unwrap();
        session.canonical_rir().unwrap();
        let moved = session.update(&relocated);
        assert!(moved.downstream_invalidated());
        assert_eq!(moved.work().modules_rebound, 2);
        moved.into_result().unwrap();
        session.canonical_rir().unwrap();
        let ids = session.update(&reassigned);
        assert!(ids.downstream_invalidated());
        assert_eq!(ids.work().modules_reparsed, 0);
        assert_eq!(ids.work().modules_rebound, 2);
        ids.into_result().unwrap();
        session.canonical_rir().unwrap();
        let rename = session.update(&renamed);
        assert!(rename.downstream_invalidated());
        assert_eq!(rename.invalidation().added.len(), 1);
        assert_eq!(rename.invalidation().removed.len(), 1);
        // ParseModule is keyed by stable logical module identity. A logical
        // rename is a removed leaf plus a new demanded leaf, so its syntax is
        // recomputed even when the source bytes happen to match.
        assert_eq!(rename.work().modules_reparsed, 1);
    }

    #[test]
    fn retained_body_failure_reprojects_spans_after_leading_trivia_edit() {
        let first_text = "fn main() -> i32 { missing_name }";
        let shifted_text = "// newly inserted leading trivia\n\nfn main() -> i32 { missing_name }";
        let first = snapshot(&[(1, "/p/main.rue", "main.rue", first_text)], 1);
        let shifted = snapshot(&[(1, "/p/main.rue", "main.rue", shifted_text)], 1);
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();

        session.update(&valid).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let key = body_query_key(&mut session, &options, "main");
        session.update(&first).into_result().unwrap();
        let first_errors = session.canonical_semantic(&options).unwrap_err();
        let (first_stamp, _, _) = retained_body_transaction(&session, &key);
        let first_closure_stamps = retained_body_closure_stamps(&session, &key);
        let (first_locator_stamp, _) = retained_body_source_locator(&session, &key);
        assert_eq!(
            first_errors
                .first()
                .and_then(|error| error.span())
                .unwrap()
                .start,
            u32::try_from(first_text.find("missing_name").unwrap()).unwrap(),
        );

        session.update(&shifted).into_result().unwrap();
        let shifted_errors = session.canonical_semantic(&options).unwrap_err();
        let (shifted_stamp, _, _) = retained_body_transaction(&session, &key);
        let shifted_closure_stamps = retained_body_closure_stamps(&session, &key);
        let (shifted_locator_stamp, _) = retained_body_source_locator(&session, &key);
        assert_eq!(
            shifted_stamp, first_stamp,
            "a locator-only edit must reuse the semantic body transaction",
        );
        assert_eq!(
            shifted_closure_stamps, first_closure_stamps,
            "positioned diagnostic payload must not restamp the semantic body closure",
        );
        assert_ne!(
            shifted_locator_stamp, first_locator_stamp,
            "diagnostics obtain the shifted position from the independent locator projection",
        );
        let shifted_span = shifted_errors
            .first()
            .and_then(|error| error.span())
            .unwrap();
        assert_eq!(
            shifted_span.start,
            u32::try_from(shifted_text.find("missing_name").unwrap()).unwrap(),
        );
        assert_eq!(shifted_span.file_id, crate::FileId::new(1));
    }

    #[test]
    fn whitespace_above_definition_reuses_semantic_shards_and_body_closure() {
        let first_text = "fn main() -> i32 { 0 }";
        let shifted_text = "// position-only leading trivia\n\nfn main() -> i32 { 0 }";
        let first = snapshot(&[(1, "/p/main.rue", "main.rue", first_text)], 1);
        let shifted = snapshot(&[(1, "/p/main.rue", "main.rue", shifted_text)], 1);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();

        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let key = body_query_key(&mut session, &options, "main");
        let first_body_stamps = retained_body_query_stamps(&session, &key);
        let first_closure_stamps = retained_body_closure_stamps(&session, &key);
        let (first_locator_stamp, first_locator) = retained_body_source_locator(&session, &key);

        session.update(&shifted).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        let shifted_body_stamps = retained_body_query_stamps(&session, &key);
        let shifted_closure_stamps = retained_body_closure_stamps(&session, &key);
        let (shifted_locator_stamp, shifted_locator) = retained_body_source_locator(&session, &key);

        assert_eq!(
            shifted_body_stamps, first_body_stamps,
            "position-only edits keep the body transaction and its semantic projections green",
        );
        assert_eq!(
            shifted_closure_stamps, first_closure_stamps,
            "the aggregate body-analysis bundle and body closure stay green",
        );
        assert_ne!(
            shifted_locator_stamp, first_locator_stamp,
            "the independently stamped source-locator projection refreshes",
        );
        assert_eq!(first_locator.declaration_start, 0);
        assert_eq!(
            shifted_locator.declaration_start,
            u32::try_from(shifted_text.find("fn main").unwrap()).unwrap(),
        );
        assert_eq!(
            shifted_locator.body_start,
            u32::try_from(shifted_text.find("{ 0 }").unwrap()).unwrap(),
        );
        assert_eq!(warm.work().body_analysis.body_analyses_computed, 0);
        assert_eq!(warm.work().body_analysis.body_analyses_reused, 1);

        let merge = session.unstable_metrics().merge_metrics();
        assert_eq!(merge.definition_shards_indexed, 1);
        assert_eq!(merge.definition_shards_reused, 1);
        assert_eq!(merge.definition_shards_rebuilt, 0);
        let merged = session.merge().unwrap();
        let main = merged
            .definitions()
            .definitions()
            .find(|definition| definition.name_key().name() == "main")
            .unwrap();
        assert_eq!(
            main.declaration_span().start,
            u32::try_from(shifted_text.find("fn main").unwrap()).unwrap(),
            "reusing the position-free shard must still rebuild current navigation records",
        );
    }

    #[test]
    fn many_shallow_specializations_compile() {
        // Regression (RUE-1083): breadth is not depth. A program may reach far
        // more than `MAX_SPECIALIZATION_ROUNDS` distinct specializations as long
        // as each sits at a shallow instantiation depth. Here `tag` has a
        // compile-time-known base case, so every `tag(k)` is a leaf
        // specialization at nesting depth 1; `main` reaches
        // `MAX_SPECIALIZATION_ROUNDS + 8` of them. The retired total-count budget
        // failed this program with E1200; the chain-depth budget compiles it.
        let count = rue_air::specialize::MAX_SPECIALIZATION_ROUNDS + 8;
        let mut body = String::from("fn main() -> i32 {\n    let mut total = 0;\n");
        for k in 0..count {
            body.push_str(&format!("    total = total + tag({k});\n"));
        }
        body.push_str("    total\n}\n");
        let program = format!("fn tag(comptime n: i32) -> i32 {{ n }}\n{body}");
        let valid = snapshot(&[(1, "/p/main.rue", "main.rue", program.as_str())], 1);
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .expect("many shallow specializations must compile");
    }

    #[test]
    fn cross_body_specialization_chain_still_overflows() {
        // The chain-depth budget must still reject unbounded cross-body
        // instantiation chains: `deepen<n>` instantiates `deepen<n + 1>`, so
        // each body publishes a strictly deeper specialization and the nesting
        // depth grows without bound. This must fail with the same E1200
        // (`maximum nesting depth`) diagnostic as before.
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn deepen(comptime n: i32) -> i32 { deepen(n + 1) }\n\
                 fn main() -> i32 { deepen(0) }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&invalid).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        assert!(
            matches!(
                errors.first().map(|error| &error.kind),
                Some(ErrorKind::ComptimeEvaluationFailed { reason })
                    if reason.contains("maximum nesting depth")
            ),
            "runaway cross-body specialization chain must overflow with E1200"
        );
    }

    #[test]
    fn file_const_anonymous_types_use_epoch_local_comptime_producers() {
        for source in [
            r#"
const T: type = struct { value: i32 };
fn main() -> i32 {
    let value: T = T { value: 42 };
    value.value
}
"#,
            r#"
const T: type = enum { A, B(i32) };
fn main() -> i32 { 0 }
"#,
        ] {
            let source = snapshot(&[(7, "/p/main.rue", "main.rue", source)], 7);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap();
            let definitions = session
                .stable_definitions(&CompileOptions::default())
                .unwrap();
            assert!(definitions.definitions().iter().any(|record| {
                record.stable_key().kind() == StableDefinitionKind::ValueConst
                    && record.stable_key().name() == "T"
            }));
        }
    }

    #[test]
    fn query_and_retired_air_paths_reject_the_same_declaration_failures() {
        for text in [
            "const value: i32 = 1; const value: i32 = 2; fn main() {}",
            "struct Value {} drop fn Value(self) {} drop fn Value(self) {} fn main() {}",
            "drop fn Missing(self) {} fn main() {}",
        ] {
            let source = snapshot(&[(1, "/main.rue", "main.rue", text)], 1);
            let stages = crate::test_support::test_frontend_stages(&source).unwrap();
            let _merged = &stages.merged;
            let rir = &stages.rir;
            let retired = match rue_air::Sema::new_synthetic(
                rir.rir(),
                rir.semantic_symbols().interner(),
                PreviewFeatures::new(),
            )
            .bind_declarations_for_test()
            {
                Err(errors) => errors,
                Ok(_) => panic!("retired AIR path unexpectedly accepted failure fixture"),
            };
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let query = match session.canonical_semantic(&CompileOptions::default()) {
                Err(errors) => errors,
                Ok(_) => panic!("query path unexpectedly accepted failure fixture"),
            };
            let messages =
                |errors: CompileErrors| errors.iter().map(ToString::to_string).collect::<Vec<_>>();
            assert_eq!(messages(retired), messages(query));
        }
    }

    #[test]
    fn published_queries_support_stable_tooling_lookups() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        let published = session.update(&source).into_owner_result().unwrap();

        let module_id = ModuleId::from_logical_path("a.rue").unwrap();
        let module = published.module(&module_id).expect("module by stable ID");
        assert_eq!(module.module_id(), &module_id);
        assert!(
            published
                .module(&ModuleId::from_logical_path("missing.rue").unwrap())
                .is_none()
        );

        let definitions = session.stable_definitions(&options).unwrap();
        let record = &definitions.definitions()[0];
        assert!(std::ptr::eq(
            definitions
                .definition_by_key(record.stable_key())
                .expect("definition by stable key"),
            record
        ));
    }

    #[test]
    fn import_graph_requires_committed_discovery_for_import_bearing_revisions() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/app/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let error = session.import_graph(None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("require a committed discovery graph")
        );
        assert_eq!(session.work().imports.executions, 0);
    }

    #[test]
    fn committed_discovery_graph_is_consumed_without_resolution_fallback() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn main() -> i32 { let s = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/p/helper.rue", "helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        assert!(session.import_graph(None).is_ok());
        assert_eq!(session.work().imports.executions, 0);
    }

    #[test]
    fn empty_import_graph_is_send_sync_and_concurrently_readable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalImportGraphOutput>();
        let mut session = CompilerSession::new();
        session.update(&base()).into_result().unwrap();
        let graph = session.import_graph(None).unwrap();
        assert!(graph.graph().records().is_empty());
        std::thread::spawn(move || assert!(graph.validation().is_valid()))
            .join()
            .unwrap();
    }

    #[test]
    fn parse_family_reselects_canonical_origin_after_diagnostic_index_eviction() {
        let source = base();
        let mut session = CompilerSession::new();
        let origin = session.update(&source).diagnostics().clone();

        evict_diagnostic_index(&mut session);
        assert!(
            session
                .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
                .is_none()
        );
        let publications = session.work().diagnostic_publications;
        let invalidations = session.work().diagnostic_invalidations;
        let reuses = session.work().diagnostic_reuses;

        let exact = session.update(&source);

        assert!(Arc::ptr_eq(exact.diagnostics(), &origin));
        assert_eq!(exact.work(), ParsedModulesWork::default());
        assert_eq!(session.work().diagnostic_publications, publications);
        assert_eq!(session.work().diagnostic_invalidations, invalidations);
        assert_eq!(session.work().diagnostic_reuses, reuses + 1);
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &origin
        ));
    }

    #[test]
    fn parse_family_reselects_presentation_origin_after_diagnostic_index_eviction() {
        let source = base();
        let mut session = CompilerSession::new();
        let origin = session
            .update_for_presentation(&source)
            .diagnostics()
            .clone();

        evict_diagnostic_index(&mut session);
        assert!(
            session
                .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
                .is_none()
        );
        let publications = session.work().diagnostic_publications;
        let invalidations = session.work().diagnostic_invalidations;
        let reuses = session.work().diagnostic_reuses;

        let exact = session.update_for_presentation(&source);

        assert!(Arc::ptr_eq(exact.diagnostics(), &origin));
        assert_eq!(exact.work(), ParsedModulesWork::default());
        assert_eq!(session.work().diagnostic_publications, publications);
        assert_eq!(session.work().diagnostic_invalidations, invalidations);
        assert_eq!(session.work().diagnostic_reuses, reuses + 1);
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &origin
        ));
    }

    #[test]
    fn reselected_parse_terminal_is_the_only_baseline_for_the_next_miss() {
        let a = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/helper.rue", "helper.rue", "fn helper() -> i32 { 0 }"),
            ],
            1,
        );
        let b = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/helper.rue", "helper.rue", "fn helper() -> i32 { 0 }"),
            ],
            1,
        );
        let c = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/helper.rue", "helper.rue", "fn helper() -> i32 { 2 }"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&a).into_result().unwrap();
        session.update(&b).into_result().unwrap();

        let reselected = session.update(&a);
        assert_eq!(reselected.work(), ParsedModulesWork::default());

        let next = session.update(&c);
        assert_eq!(next.work().modules_reused, 1);
        assert_eq!(next.work().modules_reparsed, 1);
    }

    #[test]
    fn direct_import_cache_reselects_its_origin_after_diagnostic_index_eviction() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let origin = session.import_diagnostics().unwrap();
        let stage = origin.identity().clone();

        evict_diagnostic_index(&mut session);
        assert!(
            session
                .most_recent_diagnostics_for(&source, &stage)
                .is_none()
        );
        let publications = session.work().diagnostic_publications;

        let reused = session.import_diagnostics().unwrap();

        assert!(Arc::ptr_eq(&reused, &origin));
        assert_eq!(session.work().import_diagnostics.executions, 1);
        assert_eq!(session.work().import_diagnostics.reuses, 1);
        assert_eq!(session.work().diagnostic_publications, publications);
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&source, &stage)
                .unwrap(),
            &origin
        ));
    }

    #[test]
    fn specialized_reuse_survives_relocation_file_ids_and_input_order() {
        let original = snapshot(
            &[
                (
                    71,
                    "/old/main.rue",
                    "main.rue",
                    "const lib = @import(\"lib.rue\"); fn main() -> i32 { lib.id(i32, 42) }",
                ),
                (
                    72,
                    "/old/lib.rue",
                    "lib.rue",
                    "pub fn id(comptime T: type, value: T) -> T { value }",
                ),
            ],
            71,
        );
        let relocated = snapshot(
            &[
                (
                    4,
                    "/new/lib.rue",
                    "lib.rue",
                    "pub fn id(comptime T: type, value: T) -> T { value }",
                ),
                (
                    9,
                    "/new/main.rue",
                    "main.rue",
                    "const lib = @import(\"lib.rue\"); fn main() -> i32 { lib.id(i32, 42) }",
                ),
            ],
            9,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &original);
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        publish_with_test_imports(&mut session, &relocated);
        let options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let reused = session.canonical_semantic(&options).unwrap();
        let mut fresh_session = CompilerSession::new();
        publish_with_test_imports(&mut fresh_session, &relocated);
        let fresh = fresh_session.canonical_semantic(&options).unwrap();
        assert_eq!(
            reused
                .functions()
                .iter()
                .map(|function| (&function.semantic_identity, function.machine_name.as_str()))
                .collect::<Vec<_>>(),
            fresh
                .functions()
                .iter()
                .map(|function| (&function.semantic_identity, function.machine_name.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(reused.strings(), fresh.strings());
        assert_eq!(
            format!("{:?}", reused.warnings()),
            format!("{:?}", fresh.warnings())
        );
        assert_diagnostic_parity(&session, &fresh_session);
    }

    #[test]
    fn specialized_target_and_preview_boundaries_fail_closed_exactly() {
        let source = snapshot(
            &[(
                42,
                "/p/main.rue",
                "main.rue",
                "fn id(comptime T: type, value: T) -> T { value } fn main() -> i32 { id(i32, 42) }",
            )],
            42,
        );
        let run = |options: CompileOptions| {
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap();
            session.canonical_semantic(&options).unwrap()
        };
        let other_target = *Target::all()
            .iter()
            .find(|target| **target != CompileOptions::default().target)
            .unwrap();
        let target = run(CompileOptions {
            target: other_target,
            ..CompileOptions::default()
        });
        assert_eq!(target.functions().len(), 2);

        let preview = run(CompileOptions {
            preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
            ..CompileOptions::default()
        });
        assert_eq!(preview.functions().len(), 2);
    }

    #[test]
    fn warning_specializations_recompute_once_and_are_never_published() {
        let source = snapshot(
            &[(
                42,
                "/p/main.rue",
                "main.rue",
                "fn noisy(comptime n: i32) -> i32 { let unused = 0; n } fn main() -> i32 { noisy(1) + noisy(1) }",
            )],
            42,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let cold = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(cold.warnings().len(), 1);

        let warm = session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..CompileOptions::default()
            })
            .unwrap();
        assert_eq!(warm.warnings().len(), 1);
        assert_eq!(
            format!("{:?}", warm.warnings()),
            format!("{:?}", cold.warnings())
        );
    }

    #[test]
    fn callable_alias_is_rejected_as_comptime_value_argument() {
        let source = snapshot(
            &[(
                42,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 } const F = helper; fn Witness(comptime T: type, comptime value: T) -> type { struct { marker: i32 } } fn bad(value: Witness(type, F)) -> i32 { value.marker } fn main() -> i32 { 0 }",
            )],
            42,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        assert!(errors.iter().any(|error| {
            error
                .to_string()
                .contains("callable alias cannot be passed as a comptime value argument")
        }));
    }

    #[test]
    fn nested_specialized_bodies_reuse_and_close_over_changed_callees() {
        let source_text = "fn inner(comptime T: type, value: T) -> T { value }\n\
             fn outer(comptime T: type, value: T) -> T { inner(T, value) }\n\
             fn main() -> i32 { outer(i32, 41) + outer(i32, 1) }";
        let source = snapshot(&[(42, "/p/main.rue", "main.rue", source_text)], 42);
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        let optimized = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        session.canonical_semantic(&optimized).unwrap();

        let unrelated_text = format!("{source_text}\nfn unrelated() -> i32 {{ 7 }}");
        let unrelated = snapshot(
            &[(42, "/p/main.rue", "main.rue", unrelated_text.as_str())],
            42,
        );
        session.update(&unrelated).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        let changed_text = "fn inner(comptime T: type, value: T) -> T { let copy = value; copy }\n\
             fn outer(comptime T: type, value: T) -> T { inner(T, value) }\n\
             fn main() -> i32 { outer(i32, 41) + outer(i32, 1) }\n\
             fn unrelated() -> i32 { 7 }";
        let changed_source = snapshot(&[(42, "/p/main.rue", "main.rue", changed_text)], 42);
        session.update(&changed_source).into_result().unwrap();
        let changed = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh = CompilerSession::new();
        fresh.update(&changed_source).into_result().unwrap();
        let fresh = fresh
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", changed.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh.functions()))
        );
    }

    #[test]
    fn recursive_specialized_candidates_reenter_the_fixed_point_once_each() {
        let source = snapshot(
            &[(
                42,
                "/p/main.rue",
                "main.rue",
                r#"fn fib(comptime n: i32) -> i32 {
                       if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
                   }
                   fn main() -> i32 { fib(5) + fib(5) }"#,
            )],
            42,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        let optimized = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let warm = session.canonical_semantic(&optimized).unwrap();
        assert_eq!(warm.functions().len(), 7);
    }

    #[test]
    fn evaluated_away_named_const_provenance_invalidates_only_affected_instance() {
        let first_text = "const answer: i32 = 41;\n\
             fn choose(comptime use_answer: bool) -> i32 {\n\
                 if use_answer { answer } else { 1 }\n\
             }\n\
             fn main() -> i32 { choose(true) + choose(false) }";
        let first = snapshot(&[(42, "/p/main.rue", "main.rue", first_text)], 42);
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let choose_true = cold
            .functions()
            .iter()
            .find(|function| {
                specialization_arguments(function, "choose").is_some_and(|arguments| {
                    arguments.values.as_ref() == [crate::CanonicalArgumentValue::Bool(true)]
                })
            })
            .unwrap();
        let choose_true_key = crate::body_query::BodyQueryKey {
            instance: choose_true.semantic_identity.clone(),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: CompileOptions::default().target,
                preview_features: StablePreviewFeatures::new(
                    &CompileOptions::default().preview_features,
                ),
            },
        };
        let transaction = retained_body_transaction(&session, &choose_true_key).2;
        assert!(
            transaction.references().0.iter().any(|reference| matches!(
                reference,
                crate::body_query::BodyReference::Definition(definition)
                    if definition.name() == "answer"
            )),
            "{transaction:?}"
        );
        let dependency_nodes = retained_body_dependency_nodes(&session, &choose_true_key);
        assert!(
            dependency_nodes
                .iter()
                .any(|node| node.contains("const:") && node.contains("answer")),
            "{dependency_nodes:?}"
        );

        let changed_text = first_text.replace("41", "42");
        let changed_source = snapshot(
            &[(42, "/p/main.rue", "main.rue", changed_text.as_str())],
            42,
        );
        session.update(&changed_source).into_result().unwrap();
        let changed = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh = CompilerSession::new();
        fresh.update(&changed_source).into_result().unwrap();
        let fresh = fresh
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(
            format!("{:?}", changed.functions()),
            format!("{:?}", fresh.functions())
        );
    }

    #[test]
    fn specialized_drop_provenance_invalidates_only_the_owning_instance() {
        let first_text = "fn cleanup() {}\n\
             struct Resource { value: i32 }\n\
             drop fn Resource(self) { cleanup(); }\n\
             fn borrowed(comptime n: i32, borrow resource: Resource) -> i32 {\n\
                 resource.value + n\n\
             }\n\
             fn owned(comptime n: i32, resource: Resource) -> i32 {\n\
                 resource.value + n\n\
             }\n\
             fn main() -> i32 {\n\
                 let left = Resource { value: 20 };\n\
                 let right = Resource { value: 20 };\n\
                 borrowed(1, borrow left) + owned(1, right)\n\
             }";
        let first = snapshot(&[(43, "/p/main.rue", "main.rue", first_text)], 43);
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        let changed_text = first_text.replace("cleanup();", "cleanup(); let marker = 0;");
        let changed_source = snapshot(
            &[(43, "/p/main.rue", "main.rue", changed_text.as_str())],
            43,
        );
        session.update(&changed_source).into_result().unwrap();
        let changed = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh_session = CompilerSession::new();
        fresh_session.update(&changed_source).into_result().unwrap();
        let fresh = fresh_session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_body_artifact_parity(&changed, &fresh);
        assert_diagnostic_parity(&session, &fresh_session);
    }

    #[test]
    fn unreachable_composite_body_candidate_is_never_imported() {
        let original = snapshot(
            &[(
                71,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> [i32; 2] { [1, 2] }\nfn main() -> i32 { helper()[0] }",
            )],
            71,
        );
        let edited = snapshot(
            &[(
                71,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> [i32; 2] { [1, 2] }\nfn main() -> i32 { 0 }",
            )],
            71,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&edited).into_result().unwrap();
        let reused = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let fresh = fresh
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(
            format!("{:?}", reused.functions()),
            format!("{:?}", fresh.functions())
        );
        assert_eq!(reused.type_pool().stats(), fresh.type_pool().stats());
    }

    #[test]
    fn exact_semantic_cache_hit_restores_its_successful_body_baseline() {
        let a = snapshot(
            &[(
                73,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() }",
            )],
            73,
        );
        let b = snapshot(
            &[(
                73,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 2 }\nfn main() -> i32 { helper() }",
            )],
            73,
        );
        let a_prime = snapshot(
            &[(
                73,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 1 }\nfn main() -> i32 { helper() + 1 }",
            )],
            73,
        );
        let mut session = CompilerSession::new();
        session.update(&a).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&b).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&a).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&a_prime).into_result().unwrap();
        let output = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh = CompilerSession::new();
        fresh.update(&a_prime).into_result().unwrap();
        let fresh = fresh
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(
            format!("{:?}", output.functions()),
            format!("{:?}", fresh.functions())
        );
    }

    #[test]
    fn mutual_recursion_edit_rebuilds_the_cycle_and_callers_only() {
        let original = snapshot(
            &[(
                82,
                "/p/main.rue",
                "main.rue",
                r#"
            fn a(n: i32) -> i32 { if n == 0 { 0 } else { b(n - 1) } }
            fn b(n: i32) -> i32 { if n == 0 { 0 } else { a(n - 1) } }
            fn spare() -> i32 { 7 }
            fn main() -> i32 { a(2) + spare() }
        "#,
            )],
            82,
        );
        let edited = snapshot(
            &[(
                82,
                "/p/main.rue",
                "main.rue",
                r#"
            fn a(n: i32) -> i32 { if n == 0 { 1 } else { b(n - 1) } }
            fn b(n: i32) -> i32 { if n == 0 { 0 } else { a(n - 1) } }
            fn spare() -> i32 { 7 }
            fn main() -> i32 { a(2) + spare() }
        "#,
            )],
            82,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&edited).into_result().unwrap();
        let actual = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh_session = CompilerSession::new();
        fresh_session.update(&edited).into_result().unwrap();
        let fresh = fresh_session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_body_artifact_parity(&actual, &fresh);
        assert_diagnostic_parity(&session, &fresh_session);
    }

    #[test]
    fn recursive_body_edit_rebuilds_self_and_transitive_caller_only() {
        let original = snapshot(
            &[(
                83,
                "/p/main.rue",
                "main.rue",
                "fn recurse(n: i32) -> i32 { if n == 0 { 0 } else { recurse(n - 1) } } fn spare() -> i32 { 3 } fn main() -> i32 { recurse(2) + spare() }",
            )],
            83,
        );
        let edited = snapshot(
            &[(
                83,
                "/p/main.rue",
                "main.rue",
                "fn recurse(n: i32) -> i32 { if n == 0 { 1 } else { recurse(n - 1) } } fn spare() -> i32 { 3 } fn main() -> i32 { recurse(2) + spare() }",
            )],
            83,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&edited).into_result().unwrap();
        let actual = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        let mut fresh_session = CompilerSession::new();
        fresh_session.update(&edited).into_result().unwrap();
        let fresh = fresh_session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_body_artifact_parity(&actual, &fresh);
        assert_diagnostic_parity(&session, &fresh_session);
    }

    #[test]
    fn body_reuse_survives_relocation_file_ids_and_input_permutation() {
        let original = snapshot(
            &[
                (
                    91,
                    "/one/main.rue",
                    "main.rue",
                    "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
                ),
                (92, "/one/dead.rue", "dead.rue", "fn dead() -> i32 { 2 }"),
            ],
            91,
        );
        let relocated = snapshot(
            &[
                (4, "/else/dead.rue", "dead.rue", "fn dead() -> i32 { 2 }"),
                (
                    7,
                    "/else/main.rue",
                    "main.rue",
                    "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
                ),
            ],
            7,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&relocated).into_result().unwrap();
        let options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let actual = session.canonical_semantic(&options).unwrap();
        let mut fresh_session = CompilerSession::new();
        fresh_session.update(&relocated).into_result().unwrap();
        let fresh = fresh_session.canonical_semantic(&options).unwrap();
        assert_body_artifact_parity(&actual, &fresh);
        assert_diagnostic_parity(&session, &fresh_session);
    }

    #[test]
    fn target_preview_root_and_signature_changes_reject_body_artifacts() {
        let source = snapshot(
            &[(
                101,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i32 { 1 } fn main() -> i32 { value(); 0 }",
            )],
            101,
        );
        let signature = snapshot(
            &[(
                101,
                "/p/main.rue",
                "main.rue",
                "fn value() -> i64 { 1 } fn main() -> i32 { value(); 0 }",
            )],
            101,
        );
        let run = |options: CompileOptions, next: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap();
            session.update(next).into_result().unwrap();
            session.canonical_semantic(&options).unwrap()
        };
        let other_target = *Target::all()
            .iter()
            .find(|target| **target != CompileOptions::default().target)
            .unwrap();
        let target = run(
            CompileOptions {
                target: other_target,
                ..CompileOptions::default()
            },
            &source,
        );
        assert_eq!(target.functions().len(), 2);
        let preview = run(
            CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..CompileOptions::default()
            },
            &source,
        );
        assert_eq!(preview.functions().len(), 2);
        let signature = run(CompileOptions::default(), &signature);
        assert_eq!(signature.functions().len(), 2);

        // Both files carry a top-level `main` so either can serve as the root
        // (RUE-920: a non-root `main` is an ordinary, inert function). Only the
        // designated root's `main` is the entry point, so switching the root
        // still forces a body re-analysis without a duplicate-main error.
        let both_roots = snapshot(
            &[
                (101, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (102, "/p/other.rue", "other.rue", "fn main() -> i32 { 2 }"),
            ],
            101,
        );
        let other_root = snapshot(
            &[
                (101, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (102, "/p/other.rue", "other.rue", "fn main() -> i32 { 2 }"),
            ],
            102,
        );
        let mut session = CompilerSession::new();
        session.update(&both_roots).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&other_root).into_result().unwrap();
        let root = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(root.functions().len(), 1);
    }

    #[test]
    fn malformed_well_known_option_repairs_to_fresh_canonical_semantics() {
        let options = CompileOptions::default();
        let program = r#"
const opt = @import("std/option.rue");
fn main() -> i32 {
    let O = opt.Option(i32);
    match @parse_i32("42") {
        O.Some(value) => value,
        O.None => 0,
    }
}
"#;
        let malformed = well_known_option_snapshot_with_source(
            program,
            "pub fn Option(comptime T: type) -> type { missing }",
        );
        let repaired = well_known_option_isolation_snapshot(program);

        let mut warm = CompilerSession::new();
        publish_with_test_imports(&mut warm, &malformed);
        let errors = warm
            .canonical_semantic(&options)
            .expect_err("the malformed trusted Option specialization must fail");
        assert!(
            errors.to_string().contains("missing"),
            "the failed attempt must retain the trusted declaration diagnostic: {errors}"
        );
        assert!(
            warm.queries.revisioned.any_body_transaction_terminal(),
            "typed body-control classification must be published atomically"
        );
        assert!(
            !warm.queries.revisioned.any_body_reference_terminal(),
            "the malformed attempt must not publish a body-reference projection"
        );
        publish_with_test_imports(&mut warm, &repaired);
        let warm_repaired = warm
            .canonical_semantic(&options)
            .expect("the repaired successor must compile");

        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &repaired);
        let fresh_repaired = fresh
            .canonical_semantic(&options)
            .expect("the repaired snapshot must compile fresh");

        assert_eq!(
            warm_repaired.unstable_parity_snapshot(),
            fresh_repaired.unstable_parity_snapshot(),
            "malformed-state warming must not change repaired canonical semantics"
        );
        assert!(warm.queries.revisioned.any_body_transaction_terminal());
    }

    /// Query-edge isolation. Two reached bodies demand DIFFERENT well-known
    /// `Option` payloads: `left` uses `@parse_i32` (payload i32), `right` uses
    /// `@parse_i64` (payload i64). Each body derives its OWN exact payload set from
    /// its canonical raw body and roots only that payload's `Option`
    /// specialization, so:
    ///
    /// 1. `left`'s dependency edges reach the i32 `Option` specialization and NOT
    ///    the i64 one; `right`'s reach the i64 specialization and NOT the i32 one.
    ///    A body therefore cannot inherit failure or cancellation from an
    ///    unrelated body's specialization — it has no edge to it.
    /// 2. Invalidating the i64 specialization's owning body (editing `right`)
    ///    leaves `left`'s terminal identity (its published stamp) unchanged: with
    ///    no edge to the churned specialization, `left` is reused, not recomputed.
    #[test]
    fn sibling_body_retains_no_edge_to_a_distinct_payload_specialization() {
        let options = CompileOptions::default();

        // Locate the ComptimeCall specialization edges by the payload spelled in
        // the dependency node's Debug rendering. The two bodies demand distinct
        // payloads, so distinct nodes carry the i32 and i64 type arguments.
        let has_i32_option_edge = |nodes: &[String]| {
            nodes.iter().any(|node| {
                node.contains("comptime:") && node.contains("Option") && node.contains("I32)")
            })
        };
        let has_i64_option_edge = |nodes: &[String]| {
            nodes.iter().any(|node| {
                node.contains("comptime:") && node.contains("Option") && node.contains("I64)")
            })
        };

        let program_v1 = r#"
const opt = @import("std/option.rue");
fn left(s: str) -> opt.Option(i32) {
    let O = opt.Option(i32);
    O.Some(@parse_i32(s)?)
}
fn right(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let OA = opt.Option(i32);
    let OB = opt.Option(i64);
    let a = match left("1") { OA.Some(v) => v, OA.None => 0 };
    let b = match right("2") { OB.Some(v) => @intCast(v), OB.None => 0 };
    a + b
}
"#;

        let source_v1 = well_known_option_isolation_snapshot(program_v1);
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source_v1);
        session.canonical_semantic(&options).unwrap();

        let left_key = body_query_key(&mut session, &options, "left");
        let right_key = body_query_key(&mut session, &options, "right");

        let left_nodes = retained_body_dependency_nodes(&session, &left_key);
        let right_nodes = retained_body_dependency_nodes(&session, &right_key);

        assert!(
            has_i32_option_edge(&left_nodes),
            "left (i32) must have an edge to the Option(i32) specialization: {left_nodes:?}",
        );
        assert!(
            !has_i64_option_edge(&left_nodes),
            "left (i32) must have NO edge to the sibling's Option(i64) specialization: {left_nodes:?}",
        );
        assert!(
            has_i64_option_edge(&right_nodes),
            "right (i64) must have an edge to the Option(i64) specialization: {right_nodes:?}",
        );
        assert!(
            !has_i32_option_edge(&right_nodes),
            "right (i64) must have NO edge to the sibling's Option(i32) specialization: {right_nodes:?}",
        );

        // `left`'s terminal identity before the sibling churns.
        let left_stamp_v1 = retained_body_transaction(&session, &left_key).0;

        // Invalidate the i64 specialization's owning body: edit ONLY `right`,
        // keeping its i64 payload demand. `left`'s raw body and its i32 demand are
        // untouched, and it has no edge to the i64 specialization, so its terminal
        // must be reused with an unchanged stamp.
        let program_v2 = r#"
const opt = @import("std/option.rue");
fn left(s: str) -> opt.Option(i32) {
    let O = opt.Option(i32);
    O.Some(@parse_i32(s)?)
}
fn right(s: str) -> opt.Option(i64) {
    let O = opt.Option(i64);
    let _churn = 7 + 8;
    O.Some(@parse_i64(s)?)
}
fn main() -> i32 {
    let OA = opt.Option(i32);
    let OB = opt.Option(i64);
    let a = match left("1") { OA.Some(v) => v, OA.None => 0 };
    let b = match right("2") { OB.Some(v) => @intCast(v), OB.None => 0 };
    a + b
}
"#;
        let source_v2 = well_known_option_isolation_snapshot(program_v2);
        publish_with_test_imports(&mut session, &source_v2);
        session.canonical_semantic(&options).unwrap();

        let left_key_v2 = body_query_key(&mut session, &options, "left");
        let left_stamp_v2 = retained_body_transaction(&session, &left_key_v2).0;

        assert_eq!(
            left_stamp_v1, left_stamp_v2,
            "editing the i64-owning sibling must not disturb left's terminal identity: \
             left has no edge to the i64 specialization",
        );
    }

    #[allow(dead_code)]
    fn projected_anonymous_nominals(
        session: &mut CompilerSession,
        options: &CompileOptions,
    ) -> Arc<[crate::durable_semantics::DurableAnonymousNominal]> {
        let merged = session.merge().unwrap();
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        session
            .queries
            .revisioned
            .projected_declaration_semantics(
                revision,
                merged.ast(),
                options.target,
                &options.preview_features,
                rue_query::CancellationToken::new(),
            )
            .unwrap()
            .anonymous_nominals
    }

    #[allow(dead_code)]
    fn specialized_anonymous_producer(
        nominal: &crate::durable_semantics::DurableAnonymousNominal,
    ) -> Option<(&StableDefinitionKey, &crate::CanonicalArguments)> {
        let crate::StableProducerId::Function(function) = &nominal.identity.producer else {
            return None;
        };
        let crate::FunctionInstanceKey::Specialization { base, arguments } = function.as_ref()
        else {
            return None;
        };
        let crate::FunctionInstanceKey::Definition(definition) = base.as_ref() else {
            return None;
        };
        Some((definition, arguments))
    }

    #[allow(dead_code)]
    fn nested_option_result_facts(
        facts: &[crate::durable_semantics::DurableAnonymousNominal],
    ) -> (
        &crate::durable_semantics::DurableAnonymousNominal,
        &crate::durable_semantics::DurableAnonymousNominal,
    ) {
        let by_name = |name: &str| {
            facts
                .iter()
                .find(|fact| {
                    specialized_anonymous_producer(fact)
                        .is_some_and(|(definition, _)| definition.name() == name)
                })
                .unwrap_or_else(|| panic!("missing anonymous fact owned by {name}: {facts:?}"))
        };
        (by_name("Option"), by_name("Result"))
    }

    #[allow(dead_code)]
    fn assert_nested_result_owns_option_argument(
        option: &crate::durable_semantics::DurableAnonymousNominal,
        result: &crate::durable_semantics::DurableAnonymousNominal,
        ordered_facts: &[crate::durable_semantics::DurableAnonymousNominal],
    ) {
        let (_, option_arguments) = specialized_anonymous_producer(option).unwrap();
        let (_, result_arguments) = specialized_anonymous_producer(result).unwrap();
        assert_eq!(option.identity.arguments, *option_arguments);
        assert_eq!(result.identity.arguments, *result_arguments);
        assert!(matches!(
            result_arguments.types.first(),
            Some(crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Anonymous(identity)
            )) if identity == &option.identity
        ));
        let option_position = ordered_facts
            .iter()
            .position(|fact| fact.identity == option.identity)
            .unwrap();
        let result_position = ordered_facts
            .iter()
            .position(|fact| fact.identity == result.identity)
            .unwrap();
        assert!(
            option_position < result_position,
            "the dependency fact must precede its nested consumer: {ordered_facts:?}"
        );
    }

    /// Warm-session locality of callable identity (RUE-1125).
    ///
    /// Inserting an unreachable, same-named free function into an unrelated
    /// module must not touch an existing function at all. Identity is derived
    /// from a declaration's own module and source name, so `helpers.value`
    /// keeps its semantic identity, its body/declaration terminals, its
    /// dependency set, its machine symbol, and its presentation name, and its
    /// body is reused rather than recomputed.
    #[test]
    fn an_unrelated_same_named_declaration_does_not_disturb_a_warm_body() {
        const ROOT: &str = "const helpers = @import(\"helpers.rue\");\n\
             const spare = @import(\"spare.rue\");\n\
             fn main() -> i32 { helpers.value() + spare.unrelated() }";
        const HELPERS: &str = "pub fn value() -> i32 { 10 }";
        const SPARE: &str = "pub fn unrelated() -> i32 { 20 }";
        let program = |spare: &str| {
            snapshot(
                &[
                    (1, "/p/main.rue", "main.rue", ROOT),
                    (2, "/p/helpers.rue", "helpers.rue", HELPERS),
                    (3, "/p/spare.rue", "spare.rue", spare),
                ],
                1,
            )
        };
        let options = CompileOptions::default();

        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &program(SPARE));
        let cold = session.canonical_semantic(&options).unwrap();
        let value = body_query_key(&mut session, &options, "value");

        // Everything RUE-1125 requires to be a function of `value`'s own
        // declaration: its query terminals, its dependency set, its emitted
        // symbols, and how it is presented.
        let observed = |session: &CompilerSession,
                        semantic: &Arc<crate::CanonicalSemanticOutput>| {
            let function = semantic
                .functions()
                .iter()
                .find(|function| function.definition_source_name() == Some("value"))
                .expect("value is reached from main");
            (
                retained_body_query_stamps(session, &value),
                retained_body_closure_stamps(session, &value),
                retained_body_dependency_nodes(session, &value),
                function.semantic_identity.clone(),
                function.legacy_name.clone(),
                function.machine_name.clone(),
            )
        };
        let before = observed(&session, &cold);
        assert_eq!(
            before.4, "__rue_fn_helpers_2erue__value",
            "an ordinary free function is module-qualified from the start"
        );
        let codegen = |session: &mut CompilerSession, semantic| {
            session
                .codegen_units(
                    semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            session
                .codegen_executions()
                .iter()
                .find(|(identity, _)| *identity == before.3)
                .map(|(_, execution)| *execution)
                .expect("value publishes a codegen unit")
        };
        assert_eq!(
            codegen(&mut session, &cold),
            rue_query::RequestExecution::Computed
        );

        // Insert an unreachable free function with the same source name into a
        // module `value` has no relationship with. Appending leaves every
        // existing span in `spare.rue` untouched.
        let edited = format!("{SPARE}\n@allow(unused_function)\npub fn value() -> i32 {{ 99 }}\n");
        publish_with_test_imports(&mut session, &program(&edited));
        let warm = session.canonical_semantic(&options).unwrap();
        let after = observed(&session, &warm);

        assert_eq!(
            before.0, after.0,
            "the body transaction, canonical body, reference, and produced-anonymous \
             terminals must all keep their stamps"
        );
        assert_eq!(
            before.1, after.1,
            "the body closure and its bundle must keep their stamps"
        );
        assert_eq!(before.2, after.2, "the dependency set must be unchanged");
        assert_eq!(
            (&before.3, &before.4, &before.5),
            (&after.3, &after.4, &after.5),
            "semantic identity, internal symbol, and machine symbol must be unchanged"
        );
        assert_eq!(
            warm.work().body_analysis.body_analyses_computed,
            0,
            "no body may be recomputed: {:?}",
            warm.work().body_analysis
        );
        assert_eq!(
            codegen(&mut session, &warm),
            rue_query::RequestExecution::Reused,
            "the machine-code terminal must be reused, not re-emitted"
        );

        // The declaration terminal and the presentation identity are equally
        // unaffected, and the new declaration really is present and distinct.
        let declarations = |session: &mut CompilerSession| {
            session
                .stable_definitions(&options)
                .unwrap()
                .definitions()
                .iter()
                .filter(|record| {
                    record.stable_key().kind() == StableDefinitionKind::Function
                        && record.stable_key().name() == "value"
                })
                .map(|record| record.stable_key().clone())
                .collect::<Vec<_>>()
        };
        let bound = declarations(&mut session);
        let helpers_value = match &before.3 {
            crate::FunctionInstanceKey::Definition(key) => key.clone(),
            other => panic!("value is an ordinary definition: {other:?}"),
        };
        assert!(
            bound.contains(&helpers_value),
            "helpers.value keeps its declaration identity: {bound:?}"
        );
        assert!(
            bound
                .iter()
                .any(|key| key.module().logical_path() == "spare.rue"),
            "the inserted declaration is really bound, as its own module's: {bound:?}"
        );
        assert_eq!(bound.len(), 2, "{bound:?}");
        let presented = session
            .semantic(&options)
            .unwrap()
            .function_views()
            .map(|function| function.name().to_owned())
            .collect::<Vec<_>>();
        assert!(
            presented.contains(&"value".to_owned()),
            "presentation names the declaration, not its internal symbol: {presented:?}"
        );

        // Warm and fresh must agree on the whole artifact.
        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &program(&edited));
        let expected = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &warm, &expected);
    }

    #[test]
    fn body_query_stamps_preserve_caller_and_reference_values_across_body_only_edits() {
        let options = CompileOptions::default();
        let first = SourceSnapshot::single(
            "main.rue",
            "fn helper() -> i32 { 1 } fn main() -> i32 { helper() }",
        )
        .unwrap();
        let second = SourceSnapshot::single(
            "main.rue",
            "fn helper() -> i32 { 2 } fn main() -> i32 { helper() }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let main = body_query_key(&mut session, &options, "main");
        let helper = body_query_key(&mut session, &options, "helper");
        let first_main = retained_body_query_stamps(&session, &main);
        let first_helper = retained_body_query_stamps(&session, &helper);

        session.update(&second).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let second_main = retained_body_query_stamps(&session, &main);
        let second_helper = retained_body_query_stamps(&session, &helper);

        assert_eq!(
            first_main, second_main,
            "callee bodies are not caller inputs"
        );
        assert_ne!(first_helper.0, second_helper.0);
        assert_ne!(first_helper.1, second_helper.1);
        assert_eq!(first_helper.2, second_helper.2);
        assert_eq!(first_helper.3, second_helper.3);
    }

    #[test]
    fn codegen_units_observe_exact_owned_dependencies_and_reuse_unchanged_callers() {
        let options = CompileOptions::default();
        let first = SourceSnapshot::single(
            "main.rue",
            "fn helper() -> i32 { 1 } fn main() -> i32 { if false { helper() } else { 0 } }",
        )
        .unwrap();
        let second = SourceSnapshot::single(
            "main.rue",
            "fn helper() -> i32 { 2 } fn main() -> i32 { if false { helper() } else { 0 } }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        let compile_units = |session: &mut CompilerSession| {
            let semantic = session.canonical_semantic(&options).unwrap();
            session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap();
            semantic
        };

        session.update(&first).into_result().unwrap();
        let cold = compile_units(&mut session);
        assert_eq!(session.codegen_collections(), cold.functions().len());
        assert!(
            session
                .codegen_executions()
                .iter()
                .all(|(_, execution)| { *execution == rue_query::RequestExecution::Computed })
        );
        for (_, work) in session.codegen_attempt_work() {
            let amount = |label: &str| {
                work.iter()
                    .find_map(|(candidate, amount)| {
                        (candidate.as_ref() == label).then_some(*amount)
                    })
                    .unwrap_or(0)
            };
            assert_eq!(amount("codegen.dependencies.optimized-cfg"), 1);
            assert_eq!(amount("codegen.lowering.local"), 1);
            assert_eq!(amount("codegen.unit.successes"), 1);
        }
        let cold_main = cold
            .functions()
            .iter()
            .find(|function| function.legacy_name == "main")
            .unwrap();
        let cold_main_work = session
            .codegen_attempt_work()
            .iter()
            .find(|(identity, _)| identity == &cold_main.semantic_identity)
            .unwrap();
        assert!(cold_main_work.1.iter().any(|(label, amount)| {
            label.as_ref() == "codegen.domain.symbol-aliases" && *amount > 0
        }));

        session.update(&second).into_result().unwrap();
        let warm = compile_units(&mut session);
        assert_eq!(session.codegen_collections(), warm.functions().len());
        let identity_for = |name: &str| {
            warm.functions()
                .iter()
                .find(|function| function.definition_source_name() == Some(name))
                .unwrap()
                .semantic_identity
                .clone()
        };
        let execution_for = |identity: &crate::FunctionInstanceKey| {
            session
                .codegen_executions()
                .iter()
                .find_map(|(candidate, execution)| (candidate == identity).then_some(*execution))
                .unwrap()
        };
        assert_eq!(
            execution_for(&identity_for("main")),
            rue_query::RequestExecution::Reused,
            "an optimized-away alias does not make a callee implementation an exact caller dependency"
        );
        assert_eq!(
            execution_for(&identity_for("helper")),
            rue_query::RequestExecution::Computed
        );
        let main_work = session
            .codegen_attempt_work()
            .iter()
            .find(|(identity, _)| identity == &identity_for("main"))
            .unwrap();
        assert!(
            main_work.1.is_empty(),
            "reused codegen terminals perform no local lowering or dependency collection"
        );
    }

    #[test]
    fn cfg_fails_closed_when_an_exact_call_abi_dependency_fails() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn helper(value: i32) -> i32 { value } fn main() -> i32 { helper(42) }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = crate::cfg_query::with_test_call_abi_failure_injection(|| {
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap_err()
        });
        let rendered = errors.to_string();
        assert!(rendered.contains("call ABI unavailable"), "{rendered}");
        assert!(rendered.contains("injected call ABI failure"), "{rendered}");
    }

    #[test]
    fn owned_codegen_domains_preserve_legacy_backend_bytes_on_both_architectures() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn add(left: i32, right: i32) -> i32 { left + right }\n\
             fn main() -> i32 { let message = \"owned\"; @dbg(message); add(20, 22) }",
        )
        .unwrap();
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            let options = CompileOptions {
                target,
                ..CompileOptions::default()
            };
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let semantic = session.canonical_semantic(&options).unwrap();
            let foreign = crate::backend::collect_foreign_symbols(
                semantic.rir_owner().rir(),
                semantic.rir_owner().semantic_symbols().interner(),
            );
            let owned = session
                .codegen_units(
                    &semantic,
                    &options,
                    rue_codegen::BackendArtifactRequest::default(),
                )
                .unwrap()
                .into_iter()
                .map(|unit| unit.unit.backend_product())
                .collect::<Vec<_>>();
            let legacy = crate::backend::generate_backend_products(
                semantic.functions(),
                semantic.type_pool(),
                semantic.strings(),
                semantic.rir_owner().semantic_symbols().interner(),
                &options,
                &foreign,
                rue_codegen::BackendArtifactRequest::default(),
            )
            .unwrap();
            assert_eq!(owned.len(), legacy.len());
            for (owned, legacy) in owned.iter().zip(&legacy) {
                assert_eq!(owned.machine_name, legacy.machine_name);
                assert_eq!(owned.machine_code.code, legacy.machine_code.code);
                assert_eq!(owned.machine_code.strings, legacy.machine_code.strings);
                assert_eq!(
                    format!("{:?}", owned.machine_code.relocations),
                    format!("{:?}", legacy.machine_code.relocations)
                );
            }
        }
    }

    #[test]
    fn unchanged_consumer_observes_function_produced_anonymous_fact_changes() {
        let first = SourceSnapshot::single(
            "main.rue",
            "const N: i32 = 1; fn Make() -> type { struct { values: [i32; N] } } fn size(comptime T: type) -> i32 { @size_of(T) } fn main() -> i32 { size(Make()) }",
        )
        .unwrap();
        let second = SourceSnapshot::single(
            "main.rue",
            "const N: i32 = 2; fn Make() -> type { struct { values: [i32; N] } } fn size(comptime T: type) -> i32 { @size_of(T) } fn main() -> i32 { size(Make()) }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.canonical_semantic(&options).unwrap();
        let main = body_query_key(&mut session, &options, "main");
        let make_definition = body_query_key(&mut session, &options, "Make");
        let make = crate::body_query::BodyQueryKey {
            instance: crate::FunctionInstanceKey::Specialization {
                base: Box::new(make_definition.instance),
                arguments: crate::CanonicalArguments::default(),
            },
            configuration: main.configuration.clone(),
        };
        let size = cold
            .functions()
            .iter()
            .find(|function| specialization_arguments(function, "size").is_some())
            .unwrap();
        let size = crate::body_query::BodyQueryKey {
            instance: size.semantic_identity.clone(),
            configuration: main.configuration.clone(),
        };
        let first_make_stamps = retained_body_query_stamps(&session, &make);
        let first_size_stamps = retained_body_query_stamps(&session, &size);
        let make_dependencies = retained_body_dependency_nodes(&session, &make);
        assert!(
            make_dependencies
                .iter()
                .any(|dependency| dependency.contains("const:") && dependency.contains(":N:")),
            "{make_dependencies:?}"
        );
        let main_transaction = retained_body_transaction(&session, &main).2;
        let main_dependencies = retained_body_dependency_nodes(&session, &main);
        assert!(
            main_dependencies.iter().any(|dependency| {
                dependency.contains("body-produced-anonymous") && dependency.contains("Make")
            }),
            "transaction={main_transaction:?}; dependencies={main_dependencies:?}"
        );

        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        let second_make_stamps = retained_body_query_stamps(&session, &make);
        let second_size_stamps = retained_body_query_stamps(&session, &size);
        assert_ne!(first_make_stamps.3, second_make_stamps.3);
        assert_ne!(first_size_stamps.0, second_size_stamps.0);

        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let expected = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &warm, &expected);
    }

    #[test]
    fn deferred_value_type_constructor_positions_publish_a_complete_body_closure() {
        let source = SourceSnapshot::single(
            "main.rue",
            r#"
                fn Witness(comptime T: type, comptime value: T) -> type {
                    struct { payload: T }
                }

                fn Wrap(comptime T: type) -> type {
                    struct { inner: T }
                }

                fn read(w: Witness(i32, 7)) -> i32 { w.payload }

                fn main() -> i32 {
                    let W = Witness(i32, 7);
                    let Wrapped = Wrap(Witness(i32, 7));
                    let wrapped = Wrapped { inner: W { payload: 42 } };
                    read(wrapped.inner)
                }
            "#,
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
    }

    #[test]
    fn anonymous_specialization_dependency_priority_prevents_lexical_starvation() {
        let source = SourceSnapshot::single(
            "main.rue",
            r#"
                fn ABox(comptime T: type) -> type { struct { item: T } }
                fn ZItem() -> type { struct { value: i32 } }
                fn main() -> i32 {
                    let Item = ZItem();
                    let Box = ABox(Item);
                    let boxed = Box { item: Item { value: 42 } };
                    boxed.item.value
                }
            "#,
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        session.canonical_semantic(&options).unwrap();
        let main = body_query_key(&mut session, &options, "main");
        let transaction = retained_body_transaction(&session, &main).2;
        let mut producers = BTreeSet::new();
        for reference in transaction.references().0.iter() {
            match reference {
                crate::body_query::BodyReference::Callable(function) => {
                    if let Some(definition) = stable_function_definition_root(function) {
                        producers.insert(definition.name().to_owned());
                    }
                }
                crate::body_query::BodyReference::Type(ty)
                | crate::body_query::BodyReference::DropGlue(ty) => {
                    let owner = crate::FunctionInstanceKey::DropGlue(Box::new(ty.clone()));
                    producers.extend(
                        crate::revisioned_query_database::collect_instance_anonymous_nominals(
                            &owner,
                        )
                        .iter()
                        .filter_map(|identity| {
                            stable_producer_definition_root(&identity.producer)
                                .map(|definition| definition.name().to_owned())
                        }),
                    );
                }
                crate::body_query::BodyReference::Definition(definition) => {
                    producers.insert(definition.name().to_owned());
                }
            }
        }
        assert!(producers.contains("ABox"));
        assert!(producers.contains("ZItem"));
    }

    #[test]
    fn negative_body_lookup_recomputes_when_a_declaration_is_added() {
        let missing = SourceSnapshot::single("main.rue", "fn main() -> i32 { helper() }").unwrap();
        let resolved = SourceSnapshot::single(
            "main.rue",
            "fn main() -> i32 { helper() } fn helper() -> i32 { 42 }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut warm = CompilerSession::new();
        warm.update(&missing).into_result().unwrap();
        assert!(warm.canonical_semantic(&options).is_err());
        let main = crate::body_query::BodyQueryKey {
            instance: crate::FunctionInstanceKey::Definition(
                crate::StableDefinitionKey::from_stable_parts(
                    crate::ModuleId::from_logical_path("main.rue").unwrap(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::Function,
                    "main",
                    None,
                ),
            ),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: options.target,
                preview_features: StablePreviewFeatures::new(&options.preview_features),
            },
        };
        let dependencies = retained_body_dependency_nodes(&warm, &main);
        assert!(
            dependencies
                .iter()
                .any(|node| node.contains("lookup-name") && node.contains("helper")),
            "{dependencies:?}"
        );
        assert!(
            dependencies
                .iter()
                .all(|node| !node.contains("module-declaration-set")),
            "{dependencies:?}"
        );

        warm.update(&resolved).into_result().unwrap();
        let warm_output = warm.canonical_semantic(&options).unwrap();
        let mut fresh = CompilerSession::new();
        fresh.update(&resolved).into_result().unwrap();
        let fresh_output = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", warm_output.functions()),
            format!("{:?}", fresh_output.functions())
        );
        assert_eq!(warm_output.functions().len(), 2);
    }

    #[test]
    fn qualified_negative_body_lookup_recomputes_when_imported_member_is_added() {
        let main = r#"const lib = @import("lib.rue"); fn main() -> i32 { lib.helper() }"#;
        let missing = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", main),
                (2, "/p/lib.rue", "lib.rue", "pub const value: i32 = 1;"),
            ],
            1,
        );
        let resolved = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", main),
                (2, "/p/lib.rue", "lib.rue", "pub fn helper() -> i32 { 42 }"),
            ],
            1,
        );
        let options = CompileOptions::default();
        let mut warm = CompilerSession::new();
        publish_with_test_imports(&mut warm, &missing);
        assert!(warm.canonical_semantic(&options).is_err());

        publish_with_test_imports(&mut warm, &resolved);
        let warm_output = warm.canonical_semantic(&options).unwrap();
        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &resolved);
        let fresh_output = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", warm_output.functions()),
            format!("{:?}", fresh_output.functions())
        );
        assert_eq!(warm_output.functions().len(), 2);
    }

    #[test]
    fn body_query_values_survive_relocation_and_input_order() {
        let program = "fn helper() -> i32 { 41 } fn main() -> i32 { helper() + 1 }";
        let first = snapshot(&[(1, "/old/main.rue", "main.rue", program)], 1);
        let relocated = snapshot(&[(91, "/new/main.rue", "main.rue", program)], 91);
        let options = CompileOptions::default();
        let build = |source: &SourceSnapshot| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session.canonical_semantic(&options).unwrap();
            let key = body_query_key(&mut session, &options, "main");
            let transaction = retained_body_transaction(&session, &key).2;
            (key, transaction)
        };
        let (first_key, first_transaction) = build(&first);
        let (relocated_key, relocated_transaction) = build(&relocated);
        assert_eq!(first_key, relocated_key);
        assert!(crate::body_query::transaction_equal(
            &first_transaction,
            &relocated_transaction,
        ));
    }

    #[test]
    fn recursive_body_query_publishes_a_terminal_without_a_query_cycle() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn recurse(n: i32) -> i32 { if n == 0 { 0 } else { recurse(n - 1) } } fn main() -> i32 { recurse(4) }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let key = body_query_key(&mut session, &options, "recurse");
        let transaction = retained_body_transaction(&session, &key).2;
        assert!(matches!(
            transaction,
            crate::body_query::BodyTransaction::Success { .. }
        ));
        assert!(
            transaction
                .references()
                .0
                .iter()
                .any(|reference| match reference {
                    crate::body_query::BodyReference::Callable(instance) =>
                        instance == &key.instance,
                    crate::body_query::BodyReference::Definition(definition) => {
                        matches!(
                            &key.instance,
                            crate::FunctionInstanceKey::Definition(owner) if owner == definition
                        )
                    }
                    crate::body_query::BodyReference::Type(_)
                    | crate::body_query::BodyReference::DropGlue(_) => false,
                })
        );
    }

    #[test]
    fn reachable_comptime_specialization_is_composed_from_its_body_terminal() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn make(comptime N: i32) -> [i32; N] { [7; N] } fn main() -> i32 { let a: [i32; 3] = make(1 + 2); a[0] + a[1] + a[2] }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let output = session.canonical_semantic(&options).unwrap();

        assert!(
            output.functions().iter().any(|function| matches!(
                function.semantic_identity,
                crate::FunctionInstanceKey::Specialization { .. }
            )),
            "the reachable specialization must be composed into canonical output"
        );
    }

    #[test]
    fn target_and_preview_configuration_select_distinct_body_terminals() {
        let source = SourceSnapshot::single("main.rue", "fn main() -> i32 { 42 }").unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();

        let default_options = CompileOptions::default();
        session.canonical_semantic(&default_options).unwrap();
        let default_key = body_query_key(&mut session, &default_options, "main");
        let default = retained_body_transaction(&session, &default_key);

        let configured_options = CompileOptions {
            target: if default_options.target == crate::Target::X86_64Linux {
                crate::Target::Aarch64Linux
            } else {
                crate::Target::X86_64Linux
            },
            preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
            ..CompileOptions::default()
        };
        session.canonical_semantic(&configured_options).unwrap();
        let configured_key = body_query_key(&mut session, &configured_options, "main");
        let configured = retained_body_transaction(&session, &configured_key);

        assert_ne!(default_key, configured_key);
        assert!(matches!(
            default.2,
            crate::body_query::BodyTransaction::Success { .. }
        ));
        assert!(matches!(
            configured.2,
            crate::body_query::BodyTransaction::Success { .. }
        ));
    }

    #[test]
    fn ordinary_body_transaction_runs_from_exact_input_and_provider_facts() {
        let source = SourceSnapshot::single(
            "main.rue",
            "fn helper() -> i32 { 40 } fn main() -> i32 { helper() + 2 }",
        )
        .unwrap();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        session.canonical_semantic(&options).unwrap();

        let key = body_query_key(&mut session, &options, "main");
        let transaction = retained_body_transaction(&session, &key).2;
        let crate::body_query::BodyTransaction::Success {
            body, references, ..
        } = transaction
        else {
            panic!("provider-backed ordinary body must publish success");
        };
        assert!(matches!(
            body.as_ref(),
            crate::body_query::CanonicalBody::Ordinary { owner, .. }
                if owner.name() == "main"
        ));
        assert!(references.0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Callable(
                crate::FunctionInstanceKey::Definition(definition)
            ) if definition.name() == "helper"
        )));
        let dependencies = retained_body_dependency_nodes(&session, &key);
        assert!(
            dependencies
                .iter()
                .any(|dependency| dependency.contains("compiler.body-input")),
            "{dependencies:?}"
        );
    }

    #[test]
    fn failed_body_transaction_retains_every_positive_provider_reference() {
        let source = SourceSnapshot::single(
            "main.rue",
            "struct S { fn value(self) -> i32 { 1 } }\n\
             const C: i32 = 2;\n\
             fn helper() -> i32 { 3 }\n\
             fn main() -> i32 {\n\
                 let s = S {};\n\
                 let resolved = helper() + s.value() + C;\n\
                 resolved + missing\n\
             }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        assert!(session.canonical_semantic(&options).is_err());
        let key = crate::body_query::BodyQueryKey {
            instance: crate::FunctionInstanceKey::Definition(
                crate::StableDefinitionKey::from_stable_parts(
                    crate::ModuleId::from_logical_path("main.rue").unwrap(),
                    crate::StableDefinitionNamespace::Value,
                    crate::StableDefinitionKind::Function,
                    Arc::from("main"),
                    None,
                ),
            ),
            configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: options.target,
                preview_features: StablePreviewFeatures::new(&options.preview_features),
            },
        };
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("failed semantic request retains its revision");
        let terminal = session
            .queries
            .revisioned
            .body_transaction(revision, key, rue_query::CancellationToken::new())
            .expect("deterministic body error publishes a typed terminal");
        let rue_query::QueryOutcome::Success(
            crate::body_query::BodyTransaction::DeterministicFailure { references, .. },
        ) = terminal.outcome()
        else {
            panic!(
                "expected deterministic body failure: {:?}",
                terminal.outcome()
            );
        };
        assert!(references.0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Callable(
                crate::FunctionInstanceKey::Definition(definition)
            ) if definition.name() == "helper"
        )));
        assert!(references.0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Callable(
                crate::FunctionInstanceKey::Definition(definition)
            ) if definition.name() == "value"
        )));
        assert!(references.0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Definition(definition)
                if definition.name() == "C"
        )));
        assert!(references.0.iter().any(|reference| matches!(
            reference,
            crate::body_query::BodyReference::Type(crate::TypeInstanceKey::Nominal(
                crate::NominalInstanceKey::Named(definition)
            )) if definition.name() == "S"
        )));
    }

    #[test]
    fn body_callable_dependencies_select_exact_shell_candidate_sets() {
        let source = SourceSnapshot::single(
            "main.rue",
            "extern \"C\" { fn foreign() -> i32; }\n\
             fn helper(value: i32) -> i32 { value + 1 }\n\
             fn unrelated() -> i32 { 7 }\n\
             fn main() -> i32 { helper(1) + checked { foreign() } }",
        )
        .unwrap();
        let options = CompileOptions {
            preview_features: PreviewFeatures::from([PreviewFeature::CFfi]),
            ..CompileOptions::default()
        };
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let main = body_query_key(&mut session, &options, "main");
        let main_transaction = retained_body_transaction(&session, &main).2;
        let dependencies = retained_body_dependency_nodes(&session, &main);

        for name in ["helper", "foreign"] {
            assert!(
                dependencies
                    .iter()
                    .any(|node| { node.contains("compiler.lookup-name") && node.contains(name) }),
                "body transaction must observe the exact provider lookup for {name}: \
                 transaction={main_transaction:?}; dependencies={dependencies:?}"
            );
        }
        for (category, name) in [("Function", "helper"), ("ExternFunction", "foreign")] {
            assert!(
                dependencies.iter().any(|node| {
                    node.contains("compiler.raw-declaration-signature")
                        && node.contains(&format!(":{category}:"))
                        && node.contains(name)
                }),
                "body transaction must observe the exactly selected signature for {name}: \
                 transaction={main_transaction:?}; dependencies={dependencies:?}"
            );
        }
        for (category, name) in [("ExternFunction", "helper"), ("Function", "foreign")] {
            assert!(
                dependencies.iter().all(|node| {
                    !(node.contains("compiler.raw-declaration-signature")
                        && node.contains(&format!(":{category}:"))
                        && node.contains(name))
                }),
                "the unselected opposite-category signature must stay behind the lookup for \
                 {name}: dependencies={dependencies:?}"
            );
        }
        assert!(
            dependencies.iter().any(|node| {
                node.contains("compiler.semantic-nucleus")
                    && node.contains("signature:")
                    && node.contains(":Function:")
                    && node.contains("helper")
            }),
            "{dependencies:?}"
        );
        assert!(
            dependencies.iter().any(|node| {
                node.contains("compiler.semantic-nucleus")
                    && node.contains("signature:")
                    && node.contains(":ExternFunction:")
                    && node.contains("foreign")
            }),
            "{dependencies:?}"
        );
        assert!(
            dependencies.iter().all(|node| !node.contains("unrelated")),
            "an unrelated declaration must not become a body dependency: {dependencies:?}"
        );
        assert!(
            dependencies
                .iter()
                .all(|node| !node.contains("compiler.declaration-occurrence-index")),
            "the module-wide occurrence index must stay behind the stable classifier: \
             {dependencies:?}"
        );

        let ambiguous = SourceSnapshot::single(
            "main.rue",
            "extern \"C\" { fn foreign() -> i32; fn helper(value: i32) -> i32; }\n\
             fn helper(value: i32) -> i32 { value + 1 }\n\
             fn unrelated() -> i32 { 7 }\n\
             fn main() -> i32 { helper(1) + checked { foreign() } }",
        )
        .unwrap();
        session.update(&ambiguous).into_result().unwrap();
        assert!(session.canonical_semantic(&options).is_err());
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("failed semantic attempt publishes its revision");
        let result = session.queries.revisioned.body_transaction(
            revision,
            main.clone(),
            rue_query::CancellationToken::new(),
        );
        let terminal = result.expect("ambiguous callable publishes a typed body failure");
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            )
        ));

        let mut fresh = CompilerSession::new();
        fresh.update(&ambiguous).into_result().unwrap();
        assert!(fresh.canonical_semantic(&options).is_err());
        let fresh_revision = fresh
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("fresh failed semantic attempt publishes its revision");
        let fresh_result = fresh.queries.revisioned.body_transaction(
            fresh_revision,
            main,
            rue_query::CancellationToken::new(),
        );
        let terminal = fresh_result.expect("fresh ambiguity publishes a typed body failure");
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            )
        ));
    }

    #[test]
    fn body_candidate_classifier_invalidates_same_category_duplicates_warm_and_fresh() {
        let valid = SourceSnapshot::single(
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\n\
             fn main() -> i32 { helper(1) }",
        )
        .unwrap();
        let duplicate = SourceSnapshot::single(
            "main.rue",
            "fn helper(value: i32) -> i32 { value + 1 }\n\
             fn helper(value: i32) -> i32 { value + 2 }\n\
             fn main() -> i32 { helper(1) }",
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let main = body_query_key(&mut session, &options, "main");
        let dependencies = retained_body_dependency_nodes(&session, &main);
        assert!(
            dependencies
                .iter()
                .any(|node| { node.contains("compiler.lookup-name") && node.contains("helper") }),
            "{dependencies:?}"
        );

        session.update(&duplicate).into_result().unwrap();
        assert!(session.canonical_semantic(&options).is_err());
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("failed semantic attempt publishes its revision");
        let result = session.queries.revisioned.body_transaction(
            revision,
            main.clone(),
            rue_query::CancellationToken::new(),
        );
        let terminal = result.expect("warm duplicate publishes a typed body failure");
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            )
        ));

        let mut fresh = CompilerSession::new();
        fresh.update(&duplicate).into_result().unwrap();
        assert!(fresh.canonical_semantic(&options).is_err());
        let fresh_revision = fresh
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("fresh failed semantic attempt publishes its revision");
        let fresh_result = fresh.queries.revisioned.body_transaction(
            fresh_revision,
            main,
            rue_query::CancellationToken::new(),
        );
        let terminal = fresh_result.expect("fresh duplicate publishes a typed body failure");
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(
                crate::body_query::BodyTransaction::DeterministicFailure { .. }
            )
        ));
    }

    #[test]
    fn unreachable_body_is_not_requested_by_production_reachability() {
        let source =
            SourceSnapshot::single("main.rue", "fn dead() -> i32 { 1 } fn main() -> i32 { 42 }")
                .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let dead = body_query_key(&mut session, &options, "dead");
        assert!(
            !session.queries.revisioned.has_retained_body_key(&dead),
            "an unreachable body must not have a retained transaction"
        );
    }
}
