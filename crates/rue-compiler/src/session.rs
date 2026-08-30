//! In-process canonical parse, merge, and RIR query orchestration.

use ahash::{AHashMap, AHashSet};
use rue_air::Node;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::{
    CanonicalImportGraph, CanonicalImportGraphValidation, CanonicalMergeWork,
    CanonicalMergedProgram, CanonicalRirOutput, CanonicalRirWork, CodegenInputDescriptor,
    CompileError, CompileErrors, CompileOptions, CompileWarning, ErrorKind, ModuleResolutionInputs,
    ParseInvalidationSummary, ParsedModulesWork, SemanticInputDescriptor, SourceRevision,
    SourceSnapshot, StablePreviewFeatures,
    canonical_lower::project_candidate_module_rirs_with_work,
    canonical_merge::merge_parsed_modules_reusing_indexes,
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

// Source-level partitions of this one owner (RUE-1852), following the RUE-1673
// playbook: `CompilerSession` and its orchestration stay here, and the support
// machinery that merely accumulated around them moves out. The glob re-exports
// keep every `crate::session::…` path and every item's own visibility exactly
// as they were, so the split is not an API change.
mod discovery_continuation;
mod frontend_queries;
mod metrics;
mod rooted_artifacts;

pub use discovery_continuation::*;
pub(crate) use frontend_queries::*;
pub use metrics::*;
pub use rooted_artifacts::*;

// The whole module tree as one string, for the structural gates that assert a
// retired construct has not come back. They used to read `include_str!(
// "session.rs")`, which after this split would have stopped seeing most of
// what they were pinning -- a gate that still passes over source no longer
// holding the code it guards is the vacuous pass RUE-1152 is about.
#[cfg(test)]
pub(crate) const SESSION_SOURCE: &str = concat!(
    include_str!("session/metrics.rs"),
    include_str!("session/rooted_artifacts.rs"),
    include_str!("session/discovery_continuation.rs"),
    include_str!("session/frontend_queries.rs"),
    // Last on purpose: the gates that want production-only source split this
    // string on session.rs's own `mod tests` marker and keep what precedes it,
    // so every partition has to sit ahead of that cut.
    include_str!("session.rs"),
    "\n#[cfg(test)]\nmod tests {\n",
    include_str!("session/tests.rs"),
    "\n}\n",
);

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
    /// Exact immutable import-input view this stage consumed. Production
    /// successor staging matches this against the next view's compiler-owned
    /// parent transition instead of rescanning accumulated sources and ledgers.
    input_revision: Option<crate::ImportInputRevision>,
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
    /// Exact parse terminal produced while staging this open revision. An
    /// ordinary host batch extends this terminal, never an ambient selected
    /// parse from another retained compilation.
    staging_parse_terminal: Option<Arc<rue_query::QueryTerminal<ParseQueryRecord>>>,
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
    fn input_revision(&self) -> Option<crate::ImportInputRevision> {
        self.input_revision
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
    pub fn diagnostics(&self) -> &Arc<FrontendDiagnosticSnapshot> {
        &self.diagnostics
    }
}

#[derive(Debug, Default)]
pub struct CompilerSession {
    identity: Arc<()>,
    /// Test-only injection for the canonical compilation-owned symbol bound.
    /// Production leaves this unset and uses the published u32 ceiling.
    #[cfg(test)]
    interner_limit: Option<usize>,
    /// Test-only bound for the request-local CFG symbol universe.
    #[cfg(test)]
    cfg_interner_limit: Option<usize>,
    /// Test-only deterministic CFG terminal failure for accessor propagation
    /// diagnostics. Production never sets this hook.
    #[cfg(test)]
    cfg_accessor_failure: bool,
    /// Test-only differential-oracle perturbation requested through the
    /// unstable test bridge. It corrupts a canonical projection at the next
    /// observation point without reviving a retired selected-result store.
    oracle_fault: Option<crate::unstable::DifferentialOracleFault>,
    /// Protocol context only while the typed import-closure query is open.
    /// Closed attempts live exclusively in their plan or closure terminal.
    open_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    /// The exact snapshot and accepted-read manifest pair whose fail-closed
    /// agreement this session has already proved, so a stage extending that pair
    /// re-proves only its own appended entries. Both values are immutable, so
    /// holding them is what makes the structural-extension proof below sound.
    validated_accepted_reads: Option<(SourceSnapshot, crate::AcceptedReadManifest)>,
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
    /// Exact session-facing selectors that existed before the current rooted
    /// import-input request began or before a trusted successor overlay was
    /// published. Candidate parsing and import diagnostics may move these
    /// selectors while the request is open; a superseded filesystem observation
    /// restores this snapshot after the revisioned input database reselects its
    /// committed root. Immutable terminals computed by the discarded request may
    /// remain retained, but none stays selected.
    import_request_checkpoint: Option<ImportRequestCheckpoint>,
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
    #[cfg(test)]
    object_projection_executions: Vec<(crate::FunctionInstanceKey, rue_query::RequestExecution)>,
    #[cfg(test)]
    object_projection_collections: usize,
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

#[derive(Debug)]
pub(crate) enum PipelineRequestControl {
    Compile(CompileErrors),
    Abort(rue_query::QueryAbort),
    Parked(Box<crate::ParkedToolchainModules>),
}

impl From<CompileErrors> for PipelineRequestControl {
    fn from(errors: CompileErrors) -> Self {
        Self::Compile(errors)
    }
}

impl From<CompileError> for PipelineRequestControl {
    fn from(error: CompileError) -> Self {
        Self::Compile(error.into())
    }
}

pub(crate) fn pipeline_abort_errors(context: &str, abort: rue_query::QueryAbort) -> CompileErrors {
    CompileError::without_span(ErrorKind::InternalError(format!(
        "{context} query aborted: {abort:?}"
    )))
    .into()
}

impl From<CompileErrors> for SemanticRequestControl {
    fn from(errors: CompileErrors) -> Self {
        Self::Compile(errors)
    }
}

impl CompilerSession {
    #[cfg(test)]
    pub(crate) fn cancel_constraint_generation_after_nodes_for_test(
        &self,
        nodes: usize,
    ) -> crate::revisioned_query_database::TestConstraintGenerationCancellationGuard {
        self.queries
            .revisioned
            .cancel_constraint_generation_after_nodes_for_test(nodes)
    }

    #[cfg(test)]
    pub(crate) fn constraint_generation_visits_for_test(&self) -> usize {
        self.queries
            .revisioned
            .constraint_generation_visits_for_test()
    }

    #[cfg(test)]
    pub(crate) fn any_successful_body_transaction_for_test(&self) -> bool {
        self.queries
            .revisioned
            .any_successful_body_transaction_for_test()
    }

    #[cfg(test)]
    pub(crate) fn empty_body_closure_work_for_test(
        &self,
        options: &CompileOptions,
    ) -> (crate::CandidateBodyPlanWork, crate::CandidateBodyPlanWork) {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("empty body-closure test requires a semantic revision");
        let request = self
            .queries
            .revisioned
            .body_closure(
                revision,
                crate::body_query::BodyClosureQueryKey {
                    modules: Arc::from([]),
                    roots: Arc::from([]),
                    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration {
                        target: options.target.clone(),
                        preview_features: StablePreviewFeatures::new(&options.preview_features),
                    },
                },
                rue_query::CancellationToken::new(),
            )
            .expect("empty body-closure query must publish");
        (
            request.candidate_body_plan_work,
            request.candidate_body_materialization_work,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_query_concurrency(workers: usize) -> Self {
        let mut session = Self::default();
        session.queries.revisioned =
            crate::revisioned_query_database::RevisionedQueryDatabase::with_query_concurrency(
                workers,
            );
        session
    }

    /// Construct a canonical session with a bounded shared symbol space for
    /// deterministic resource-limit regression tests. The bound is owned by
    /// the query database and therefore reaches the worker threads that run
    /// canonical materialization.
    #[cfg(test)]
    pub(crate) fn with_interner_limit(max_entries: usize) -> Self {
        let mut session = Self::default();
        session.interner_limit = Some(max_entries);
        session.queries.revisioned =
            crate::revisioned_query_database::RevisionedQueryDatabase::with_interner_limit(
                max_entries,
            );
        session
    }

    #[cfg(test)]
    pub(crate) fn with_cfg_interner_limit(max_entries: usize) -> Self {
        let mut session = Self::default();
        session.cfg_interner_limit = Some(max_entries);
        session
    }

    #[cfg(test)]
    pub(crate) fn with_cfg_accessor_failure() -> Self {
        let mut session = Self::default();
        session.cfg_accessor_failure = true;
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
            crate::unstable::DifferentialOracleFault::Semantic
            | crate::unstable::DifferentialOracleFault::CfgTransformation => {
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
            "import-diagnostics" | "merge" | "rir" | "semantic" | "definitions" | "parse" => {}
            family => unreachable!("unknown query guard family {family}"),
        }
        self.metrics.synchronize();
        std::panic::resume_unwind(payload)
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

    fn capture_import_request_checkpoint(&self) -> ImportRequestCheckpoint {
        ImportRequestCheckpoint {
            validated_accepted_reads: self.validated_accepted_reads.clone(),
            continuation: self.continuation.clone(),
            discovery_attempt: self.queries.discovery_attempt.clone(),
            prior_discovery: self.queries.prior_discovery.clone(),
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
        if self.import_request_checkpoint.is_none() {
            self.import_request_checkpoint = Some(self.capture_import_request_checkpoint());
        }
        // A fresh observation generation invalidates any outstanding
        // trusted-toolchain continuation and successor-delta authority (RUE-1112).
        self.continuation = None;
        self.successor_delta_nonce = None;
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
                let Some(predecessor) = self.open_discovery.as_deref().filter(|artifact| {
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
        self.import_plan_groups_constructed
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
        let same_revision = self.open_discovery.as_ref().filter(|artifact| {
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
        let extension = self.validated_accepted_reads.as_ref().and_then(
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
        self.validated_accepted_reads = Some((snapshot.clone(), accepted_reads.clone()));
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
                self.open_discovery.as_deref().filter(|attempt| {
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
        self.open_discovery = None;
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
            self.queries.record_discovery_attempt(attempted_artifact);
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
                self.queries.record_discovery_attempt(attempted_artifact);
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
                self.import_plan_groups_constructed = self
                    .import_plan_groups_constructed
                    .saturating_add(constructed);
                plan
            }),
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
        // Only a subsequent rooted body-closure park attaches its exact missing-demand
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
        self.queries.revisioned.commit_import_request();
        self.import_request_checkpoint = None;
        Ok(artifact)
    }

    /// Mint the trusted-toolchain continuation token for the current successful
    /// import-discovery close, if one is outstanding AND authorizing (RUE-1112).
    /// A closed state becomes authorizing only once a rooted body-closure park
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
            self.import_request_checkpoint.is_none(),
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
        self.import_request_checkpoint = Some(checkpoint);
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
        self.metrics.set_runtime(FrontendRuntimeMetrics {
            query: runtime.into(),
            semantic_reachability: self.queries.revisioned.body_reachability_metrics(),
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

    /// Select the parse this presentation shows after an import-discovery
    /// close. A trusted successor re-selects its retained successor parse
    /// terminal; an ordinary close selects the presentation order and runs the
    /// parse update, whose per-module requests reuse the retained module
    /// terminals. The discovery parse itself is deliberately NOT handed over:
    /// adopting it wholesale would bypass parse-terminal publication
    /// (RUE-1144).
    fn select_parse_for_presentation(
        &mut self,
        snapshot: &SourceSnapshot,
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
        promote_selection: bool,
    ) -> (
        ParseQueryRecord,
        Arc<dyn AttemptView>,
        QueryAttemptExecution,
        ParsedModulesWork,
        ParseInvalidationSummary,
        Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
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
        // The per-module parses run OUTSIDE the outer query body below, as
        // top-level requests, so the outer `compiler.parse` node never records
        // them as dependencies (RUE-1145). This is deliberate, and safe only
        // because the node is key-identified: `ExactSourceInput` embeds every
        // file's exact content identity, so any source change selects a
        // different node and its recorded edge set is never what invalidates
        // it. The single synthetic whole-source leaf recorded in the closure is
        // NOT a real dependency graph — no consumer may read this node's edges
        // to learn which modules it consumed. (The successor path is different:
        // it adopts the predecessor terminal and records the appended modules'
        // input leaves, so its edges are real.) This shim node is deleted by
        // ADR-0063 Phase 12 (RUE-1033); recording real module edges here would
        // mean growing an out-of-band observation API in rue-query for a node
        // scheduled for removal.
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
                    // Key-identified-only node: this synthetic leaf exists so
                    // the terminal has a non-empty input set, not to model the
                    // per-module parses consumed above (RUE-1145; see the
                    // deletion-gate comment at `parse_program`).
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
        if promote_selection {
            self.queries.revisioned.select_parse(&attempt);
        } else {
            self.queries.revisioned.select_parse_candidate(&attempt);
        }
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()))
            .clone();
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
        (record, view, execution, work, invalidation, terminal)
    }

    /// Reconcile one successor parse extension without side effects: the
    /// retained parse artifact this stage extends, its presentation order, and
    /// the appended (module, file) pairs. The compiler-owned input transition
    /// proves the exact parent revision and appended revisions; the retained
    /// terminal must still be adoptable, rooted identically, and have the exact
    /// predecessor presentation order. A record from an intervening update is
    /// rejected. Everything here is O(appended); predecessor contents are
    /// carried by the immutable revision rather than rescanned or re-hashed.
    fn prepare_successor_parse(
        &self,
        snapshot: &SourceSnapshot,
        delta: &Arc<[crate::ModuleRevision]>,
        terminal: Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
    ) -> Result<PreparedSuccessorParse, CompileErrors> {
        let reject = |message: &str| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("incremental import parse rejected: {message}"),
            )))
        };
        let Ok(predecessor_terminal) = self
            .queries
            .revisioned
            .parse_family()
            .adoptable_terminal(&terminal)
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
        // The private input transition already proves predecessor identity and
        // unchanged carried sources. Snapshot assemblers may compact their
        // persistent segments, so pointer ancestry is not a semantic
        // requirement here; requiring it would reject an otherwise exact host
        // batch after compaction. Root identity plus the exact appended segment
        // is rechecked below without walking the predecessor prefix.
        let predecessor_snapshot = record.snapshot.clone();
        if snapshot.source_revision().root() != predecessor_snapshot.source_revision().root() {
            return Err(reject(
                "the retained parse artifact belongs to a different root module",
            ));
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
        Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
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
        self.queries.revisioned.select_parse_candidate(&attempt);
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("parse query aborted: {:?}", attempt.abort()))
            .clone();
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
        (record, view, execution, work, invalidation, terminal)
    }

    fn parse_staging_snapshot(
        &mut self,
        snapshot: &SourceSnapshot,
        successor: Option<(
            crate::ImportInputRevision,
            &Arc<[crate::ModuleRevision]>,
            Arc<rue_query::QueryTerminal<ParseQueryRecord>>,
        )>,
    ) -> (
        Result<Arc<ParsedProgram>, CompileErrors>,
        ParsedModulesWork,
        Option<ParseQueryRecord>,
        Option<Arc<rue_query::QueryTerminal<ParseQueryRecord>>>,
    ) {
        // A successor stage MUST extend its verified predecessor: a failed
        // predecessor binding rejects the stage rather than silently falling
        // back to a full content-keyed build under successor authority.
        let prepared_successor = match successor {
            Some((revision, delta, terminal)) => {
                match self.prepare_successor_parse(snapshot, delta, terminal) {
                    Ok(prepared) => Some((revision, prepared)),
                    Err(errors) => {
                        return (Err(errors), ParsedModulesWork::default(), None, None);
                    }
                }
            }
            None => None,
        };
        let staged_successor = prepared_successor.is_some();
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        let (record, view, execution, work, _invalidation, terminal) = match prepared_successor {
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
                self.execute_parse_query(snapshot, presentation, attempt_id, false)
            }
        };
        guard.started();
        let result = record.result.clone();
        guard.attach_diagnostics(record.diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        let retained = staged_successor.then(|| record.clone());
        (result, work, retained, Some(terminal))
    }

    fn run_parse_update(
        &mut self,
        snapshot: &SourceSnapshot,
        presentation: DiagnosticAttemptProvenance,
    ) -> CompilerSessionUpdate {
        let mut guard = self.metrics.begin_unprojected("parse");
        let attempt_id = guard.id;
        let (record, view, execution, parse_work, invalidation, _) =
            self.execute_parse_query(snapshot, presentation, attempt_id, true);
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
                // An open discovery artifact survives only an EXACT
                // republication of its own snapshot. Source revisions exclude
                // physical paths and presentation order, so a same-revision
                // replacement update (relocated or reordered files) must still
                // invalidate the artifact — otherwise its later close would
                // republish the superseded snapshot, rolling physical and
                // presentation state backward (RUE-1823).
                if self
                    .open_discovery
                    .as_deref()
                    .is_some_and(|artifact| !artifact.snapshot.is_same_exact_snapshot(snapshot))
                {
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
                // An open discovery artifact survives only an EXACT
                // republication of its own snapshot. Source revisions exclude
                // physical paths and presentation order, so a same-revision
                // replacement update (relocated or reordered files) must still
                // invalidate the artifact — otherwise its later close would
                // republish the superseded snapshot, rolling physical and
                // presentation state backward (RUE-1823).
                if self
                    .open_discovery
                    .as_deref()
                    .is_some_and(|artifact| !artifact.snapshot.is_same_exact_snapshot(snapshot))
                {
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
                    self.queries
                        .revisioned
                        .compose_candidate_module_rirs(revision, module_ids)
                };
                match module_rirs {
                    Ok(modules) => {
                        let projected = {
                            let _span = tracing::info_span!("rir_projection").entered();
                            project_candidate_module_rirs_with_work(merged, &modules, query_work, {
                                #[cfg(test)]
                                {
                                    self.interner_limit
                                        .unwrap_or(rue_lexer::MAX_INTERNED_STRINGS)
                                }
                                #[cfg(not(test))]
                                {
                                    rue_lexer::MAX_INTERNED_STRINGS
                                }
                            })
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
        let _declaration_graph_collection_span =
            tracing::info_span!("declaration_graph_collection", phase = "semantic_analysis")
                .entered();
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
        drop(_declaration_graph_collection_span);
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
        request.accrue_reachability_work(&mut work.body_analysis);
        request.accrue_candidate_body_plan_work(&mut work);
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
        let _body_graph_projection_span =
            tracing::info_span!("body_graph_projection", phase = "semantic_analysis").entered();
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

        let mut anonymous = BTreeMap::new();
        for fact in projection.anonymous_nominals.iter() {
            if let Err(identity) =
                crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, fact)
            {
                errors.push(CompileError::without_span(ErrorKind::OutputPublication(
                    format!("conflicting anonymous facts for {identity:?}"),
                )));
            }
        }
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
                    .body_source_basis_projection(
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
                    if let Err(identity) =
                        crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, fact)
                    {
                        errors.push(CompileError::without_span(ErrorKind::OutputPublication(
                            format!("conflicting anonymous facts for {identity:?}"),
                        )));
                    }
                }
            }
            if let Some(crate::body_query::ProducedAnonymous::Produced(produced)) =
                &bundle.produced_anonymous
            {
                for fact in produced.0.iter() {
                    if let Err(identity) =
                        crate::durable_semantics::merge_anonymous_nominal(&mut anonymous, fact)
                    {
                        errors.push(CompileError::without_span(ErrorKind::OutputPublication(
                            format!("conflicting anonymous facts for {identity:?}"),
                        )));
                    }
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
            declaration_index: projection.declaration_index,
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
        cancellation: rue_query::CancellationToken,
    ) -> Result<BTreeSet<crate::StableDefinitionKey>, PipelineRequestControl> {
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
        let declarations = graph
            .declarations
            .iter()
            .filter(|declaration| declaration.key.kind().owns_body())
            .collect::<Vec<_>>();
        if declarations.is_empty() {
            self.metrics
                .set_warning_references(crate::unstable::WarningReferenceMetrics::default());
            return Ok(referenced);
        }
        let keys = declarations
            .iter()
            .map(|declaration| {
                crate::body_query::BodyQueryKey::new(
                    crate::FunctionInstanceKey::Definition(declaration.key.clone()),
                    graph.configuration.clone(),
                )
            })
            .collect::<Vec<_>>()
            .into();
        let (attempt, child_executions) = self.queries.revisioned.warning_body_reference_frontier(
            graph.revision,
            keys,
            cancellation,
        );
        let batch_execution = attempt.execution();
        let mut warning_work = crate::unstable::WarningReferenceMetrics {
            frontier_items: declarations.len(),
            frontier_batches: 1,
            frontier_batch_overhead: attempt
                .work()
                .iter()
                .find_map(|(name, count)| {
                    (name.as_ref() == "warning-reference.frontier.overhead")
                        .then_some(*count as usize)
                })
                .unwrap_or(0),
            ..crate::unstable::WarningReferenceMetrics::default()
        };
        for child in child_executions.iter().flatten() {
            match child.execution {
                rue_query::RequestExecution::Computed => warning_work.children_computed += 1,
                rue_query::RequestExecution::Reused => warning_work.children_reused += 1,
                rue_query::RequestExecution::Joined => warning_work.children_joined += 1,
                rue_query::RequestExecution::Aborted if child.canceled => {
                    warning_work.children_canceled += 1;
                }
                rue_query::RequestExecution::Aborted => {}
            }
        }
        self.metrics.set_warning_references(warning_work);
        let executions = child_executions
            .into_iter()
            .map(|execution| {
                execution
                    .map(|execution| execution.execution)
                    .unwrap_or(batch_execution)
            })
            .collect::<Vec<_>>();
        let terminal = attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(batch) = terminal.outcome() else {
            unreachable!("WarningBodyReferenceFrontier publishes typed values")
        };
        assert_eq!(batch.values.len(), declarations.len());
        for ((declaration, projected), execution) in declarations
            .into_iter()
            .zip(batch.values.iter())
            .zip(executions.into_iter())
        {
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
        if self.oracle_fault == Some(crate::unstable::DifferentialOracleFault::Semantic) {
            self.oracle_fault.take();
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InternalError("differential semantic fault".into()),
            )));
        }
        match self.rooted_cfg_with_cancellation(options, rue_query::CancellationToken::new()) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("rooted CFG", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_pre_optimization_cfg(
        &mut self,
        options: &CompileOptions,
    ) -> Result<RootedPreOptimizationCfgOutput, CompileErrors> {
        match self.rooted_cfg_artifact_with_cancellation(
            options,
            rue_query::CancellationToken::new(),
            true,
            std::convert::identity,
            |_| unreachable!("a pre-optimization request cannot publish a post artifact"),
        ) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("pre-optimization rooted CFG", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_cfg_with_cancellation(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCfgOutput, PipelineRequestControl> {
        self.rooted_cfg_artifact_with_cancellation(
            options,
            cancellation,
            false,
            |_| unreachable!("a post-optimization request cannot publish a raw artifact"),
            std::convert::identity,
        )
    }

    fn rooted_cfg_artifact_with_cancellation<T>(
        &mut self,
        options: &CompileOptions,
        cancellation: rue_query::CancellationToken,
        pre_optimization: bool,
        publish_pre: impl FnOnce(RootedPreOptimizationCfgOutput) -> T,
        publish_post: impl FnOnce(RootedCfgOutput) -> T,
    ) -> Result<T, PipelineRequestControl> {
        let graph = match self.rooted_body_graph_with_cancellation(options, cancellation.clone()) {
            Ok(graph) => graph,
            Err(SemanticRequestControl::Compile(errors)) => {
                return Err(PipelineRequestControl::Compile(errors));
            }
            Err(SemanticRequestControl::Parked(park)) => {
                return Err(PipelineRequestControl::Parked(park));
            }
            Err(SemanticRequestControl::Abort(abort)) => {
                return Err(PipelineRequestControl::Abort(abort));
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
                .map(|owner| crate::FunctionInstanceKey::DropGlue(Node::new(owner))),
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
            // Probed by identity when a body's callable facts are selected,
            // never iterated. An ordered map here charged a recursive
            // `FunctionInstanceKey` comparison per level of every probe.
            .collect::<ahash::AHashMap<_, _>>();
        let mut cfg_inputs = Vec::with_capacity(identities.len());
        let warning_references = self.rooted_warning_references(&graph, cancellation.clone())?;
        let mut warnings = rooted_unused_function_warnings(&graph, &warning_references);
        let _cfg_collection_span =
            tracing::info_span!("optimized_cfg_collection", phase = "cfg_and_optimization")
                .entered();
        let (materialization_index, index_work) =
            crate::local_semantic_materialization::LocalFactSelectionIndex::new(
                &graph.declaration_index,
                &graph.declarations,
                &graph.anonymous_nominals,
            )
            .map_err(|error| {
                CompileError::without_span(ErrorKind::OutputPublication(format!(
                    "CFG materialization index rejected anonymous facts: {error:?}"
                )))
            })?;
        work.cfg.materialization_index_builds += 1;
        work.cfg.materialization_declarations_scanned += index_work.declarations_scanned;
        work.cfg.materialization_anonymous_nominals_scanned +=
            index_work.anonymous_nominals_scanned;
        work.cfg.materialization_type_nodes_scanned += index_work.type_nodes_scanned;
        // One table for this pass, covering both the body loop below and the
        // drop-glue loop after it: drop glue for a type reached from several
        // bodies selects the same closure each time.
        let mut fact_closures =
            crate::local_semantic_materialization::LocalMaterializationFactInterner::default();
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
                .body_source_basis_projection(
                    graph.revision,
                    closure_body.key.clone(),
                    cancellation.clone(),
                )
                .map_err(PipelineRequestControl::Abort)?;
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
            work.cfg.materialization_fact_selections += 1;
            let materialization =
                crate::local_semantic_materialization::select_materialization_facts(
                    &closure_body.key.instance,
                    semantic_body,
                    &materialization_index,
                    &callable_symbols,
                    &mut fact_closures,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "CFG materialization fact selection failed: {error:?}"
                        )),
                        body_span,
                    )
                })?;
            work.cfg.materialization_declarations_selected += materialization.declarations.len();
            work.cfg.materialization_anonymous_nominals_selected +=
                materialization.anonymous_nominals.len();
            work.cfg.materialization_callables_selected += materialization.callables.len();
            work.cfg.materialization_nominal_metadata_selected +=
                materialization.nominal_metadata.len();
            work.cfg.materialization_modules_selected += materialization.modules.len();
            work.cfg.materialization_builtin_nominals_selected +=
                materialization.builtin_nominals.len();
            work.cfg.materialization_required_types_selected +=
                materialization.required_types.len();
            cfg_inputs.push((
                closure_body.key.instance.clone(),
                crate::cfg_query::CfgSemanticInput::Body {
                    input: Arc::new(crate::cfg_query::CfgBodyInput {
                        function: closure_body.key.instance.clone(),
                        canonical: body.clone(),
                        body_span,
                        #[cfg(test)]
                        interner_limit: self.cfg_interner_limit,
                        #[cfg(test)]
                        force_failure: self.cfg_accessor_failure && semantic_body.is_accessor,
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
            work.cfg.materialization_fact_selections += 1;
            let identity = crate::FunctionInstanceKey::DropGlue(Node::new(owner.clone()));
            let materialization =
                crate::local_semantic_materialization::select_drop_glue_materialization_facts(
                    owner,
                    facts,
                    &materialization_index,
                    &callable_symbols,
                    &mut fact_closures,
                )
                .map_err(|error| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "drop-glue materialization fact selection failed: {error:?}"
                        )),
                        fallback_span,
                    )
                })?;
            work.cfg.materialization_declarations_selected += materialization.declarations.len();
            work.cfg.materialization_anonymous_nominals_selected +=
                materialization.anonymous_nominals.len();
            work.cfg.materialization_callables_selected += materialization.callables.len();
            work.cfg.materialization_nominal_metadata_selected +=
                materialization.nominal_metadata.len();
            work.cfg.materialization_modules_selected += materialization.modules.len();
            work.cfg.materialization_builtin_nominals_selected +=
                materialization.builtin_nominals.len();
            work.cfg.materialization_required_types_selected +=
                materialization.required_types.len();
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
        work.cfg.materialization_fact_closures_allocated += fact_closures.allocated;
        work.cfg.materialization_fact_closures_reused += fact_closures.reused;
        // The selected facts now own everything carried by CFG memo keys. Do
        // not retain the request-wide lookup tables across CFG evaluation.
        drop(materialization_index);
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
        if pre_optimization {
            let raw_requests = cfg_inputs
                .iter()
                .map(|(function, _, body_span)| {
                    (
                        function.clone(),
                        raw_accessor_keys
                            .get(function)
                            .expect("every CFG input has one raw key")
                            .clone(),
                        *body_span,
                    )
                })
                .collect::<Vec<_>>();
            let raw_keys = raw_requests
                .iter()
                .map(|(_, key, _)| key.clone())
                .collect::<Vec<_>>()
                .into();
            #[cfg(test)]
            self.rooted_cfg_executions.clear();
            let (raw_cfg_batch, attempt) =
                self.queries
                    .revisioned
                    .raw_cfg_batch(graph.revision, raw_keys, cancellation);
            let batch_execution = attempt.execution();
            let executions = if batch_execution == rue_query::RequestExecution::Computed {
                let executions = attempt
                    .nested_attempts()
                    .iter()
                    .filter(|attempt| attempt.node().family() == "compiler.cfg")
                    .map(rue_query::NestedQueryAttempt::execution)
                    .collect::<Vec<_>>();
                assert_eq!(executions.len(), raw_requests.len());
                executions
            } else {
                vec![batch_execution; raw_requests.len()]
            };
            let batch_work = |name: &str| {
                attempt
                    .work()
                    .iter()
                    .find_map(|(kind, count)| (kind.as_ref() == name).then_some(*count as usize))
                    .unwrap_or(0)
            };
            work.cfg.prerequisite_stable_types_scanned +=
                batch_work("cfg.prerequisite.stable-types-scanned");
            work.cfg.prerequisite_layout_requests += batch_work("cfg.prerequisite.layout-requests");
            work.cfg.prerequisite_drop_glue_requests +=
                batch_work("cfg.prerequisite.drop-glue-requests");
            work.cfg.retained_interner_charge_scans +=
                batch_work("cfg.retained-interner-charge-scans");
            work.cfg.retained_interner_entries_scanned +=
                batch_work("cfg.retained-interner-entries-scanned");
            work.cfg.retained_interner_utf8_bytes_scanned +=
                batch_work("cfg.retained-interner-utf8-bytes-scanned");
            work.cfg.cfg_builds_attempted += batch_work("cfg.build.attempts");
            work.cfg.cfg_builds_succeeded += batch_work("cfg.build.successes");
            work.cfg.cfg_builds_failed += batch_work("cfg.build.failures");
            work.cfg.air_instructions_consumed += batch_work("cfg.air.instructions");
            work.cfg.cfg_warnings_emitted += batch_work("cfg.warnings");
            let cfg_reuses = executions
                .iter()
                .filter(|execution| {
                    matches!(
                        execution,
                        rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                    )
                })
                .count();
            work.cfg.cfg_reuse_candidates += cfg_reuses;
            work.cfg.cfg_reuses += cfg_reuses;

            let batch_terminal = attempt
                .into_result()
                .map_err(PipelineRequestControl::Abort)?;
            let rue_query::QueryOutcome::Success(batch) = batch_terminal.outcome() else {
                unreachable!("RawCfgBatch publishes typed values")
            };
            assert_eq!(batch.values.len(), raw_requests.len());
            let mut cfgs = Vec::with_capacity(raw_requests.len());
            for (((function, cfg_key, body_span), value), _execution) in raw_requests
                .into_iter()
                .zip(batch.values.iter())
                .zip(executions)
            {
                #[cfg(test)]
                self.rooted_cfg_executions
                    .push((function.clone(), _execution));
                let record = match value {
                    crate::cfg_query::CfgValue::Available(record) => record.clone(),
                    crate::cfg_query::CfgValue::Failure {
                        errors,
                        body_span: old_span,
                    } => {
                        return Err(PipelineRequestControl::Compile(
                            crate::cfg_query::import_errors(errors, *old_span, body_span),
                        ));
                    }
                    crate::cfg_query::CfgValue::AccessorFailure { .. } => {
                        unreachable!("raw CFG queries do not publish accessor-splice failures")
                    }
                };
                let local_air_payload = record.air.payload_store_stats();
                work.cfg.local_epochs += 1;
                work.cfg.local_air_instructions += record.air.instructions().len();
                work.cfg.local_air_payload_bytes += local_air_payload
                    .word_store_logical_bytes
                    .saturating_add(local_air_payload.projection_store_logical_bytes)
                    .saturating_add(local_air_payload.place_store_logical_bytes);
                work.cfg.local_type_entries += record.type_pool.len();
                work.cfg.local_aggregate_type_aliases += record.local_aggregate_type_aliases;
                work.cfg.local_materialized_type_handles += record.local_materialized_type_handles;
                work.cfg.local_interner_entries += record.interner.len();
                work.cfg.local_interner_utf8_bytes += record.interner.utf8_bytes();
                work.cfg.local_strings += record.strings.len();
                work.cfg.local_atoms += record.local_atoms.len();
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
                cfgs.push(RootedPreOptimizationCfgUnit {
                    function,
                    cfg_key,
                    record,
                });
            }
            drop(_cfg_collection_span);
            cfgs.sort_by(|left, right| left.function.cmp(&right.function));
            sort_rooted_warnings(&graph, &mut warnings);
            let source = self
                .published_snapshot
                .clone()
                .ok_or_else(|| PipelineRequestControl::Compile(no_published_program()))?;
            let input = CodegenInputDescriptor {
                semantic: SemanticInputDescriptor::new(
                    &source,
                    options.target,
                    &options.preview_features,
                ),
                opt_level: options.opt_level.into(),
            };
            let imports = self
                .accepted_semantic_import_graph()
                .map_err(PipelineRequestControl::Compile)?;
            let diagnostics = self.publish_diagnostics(
                &source,
                FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(&input, imports)),
                None,
                &warnings,
            );
            self.diagnostics.select_snapshot(&diagnostics);
            self.refresh_retention_metrics();
            return Ok(publish_pre(RootedPreOptimizationCfgOutput {
                cfgs,
                raw_cfg_batch,
                _raw_cfg_terminal: batch_terminal,
                warnings,
                work,
            }));
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
        let mut accessor_roots = accessor_subgraph.roots;
        let mut accessor_dependencies = accessor_subgraph.dependencies;
        let accessor_functions = accessor_subgraph.accessors;
        let cfg_requests = cfg_inputs
            .into_iter()
            .filter(|(function, _, _)| !accessor_functions.contains(function))
            .map(|(function, _, body_span)| {
                let cfg = accessor_roots
                    .remove(&function)
                    .expect("validated accessor subgraph has one root per executable function");
                let optimized_cfg_key = crate::cfg_query::OptimizedCfgQueryKey::new(
                    cfg,
                    options.opt_level,
                    accessor_dependencies
                        .remove(&function)
                        .expect("validated accessor subgraph has dependencies for every root"),
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
        let (cfg_batch_key, attempt) = self.queries.revisioned.optimized_cfg_batch(
            graph.revision,
            optimized_keys,
            std::iter::once(crate::FunctionInstanceKey::Definition(graph.main.clone()))
                .chain(
                    graph
                        .c_export_roots
                        .iter()
                        .cloned()
                        .map(crate::FunctionInstanceKey::Definition),
                )
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
            cancellation,
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
        let nested_cfg_attempts = attempt
            .nested_attempts()
            .iter()
            .filter(|attempt| attempt.node().family() == "compiler.cfg")
            .collect::<Vec<_>>();
        let nested_cfg_reuses = nested_cfg_attempts
            .iter()
            .filter(|attempt| {
                matches!(
                    attempt.execution(),
                    rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                )
            })
            .count();
        let mut backend_work = BackendQueryWork::default();
        for execution in &executions {
            backend_work.observe(*execution);
        }
        let batch_work = |name: &str| {
            attempt
                .work()
                .iter()
                .find_map(|(kind, count)| (kind.as_ref() == name).then_some(*count as usize))
                .unwrap_or(0)
        };
        work.cfg.prerequisite_stable_types_scanned +=
            batch_work("cfg.prerequisite.stable-types-scanned");
        work.cfg.prerequisite_layout_requests += batch_work("cfg.prerequisite.layout-requests");
        work.cfg.prerequisite_drop_glue_requests +=
            batch_work("cfg.prerequisite.drop-glue-requests");
        work.cfg.retained_interner_charge_scans += batch_work("cfg.retained-interner-charge-scans");
        work.cfg.retained_interner_entries_scanned +=
            batch_work("cfg.retained-interner-entries-scanned");
        work.cfg.retained_interner_utf8_bytes_scanned +=
            batch_work("cfg.retained-interner-utf8-bytes-scanned");
        work.cfg.cfg_builds_attempted += batch_work("cfg.build.attempts");
        work.cfg.cfg_builds_succeeded += batch_work("cfg.build.successes");
        work.cfg.cfg_builds_failed += batch_work("cfg.build.failures");
        work.cfg.air_instructions_consumed += batch_work("cfg.air.instructions");
        work.cfg.optimization_attempts += batch_work("cfg.optimize.attempts");
        work.cfg.optimization_completions += batch_work("cfg.optimize.successes");
        work.cfg.optimized_level_attempts += batch_work("cfg.optimize.nonzero-level");
        work.cfg.optimization_loops_analyzed += batch_work("cfg.optimize.loops-analyzed");
        work.cfg.optimization_loops_unrolled += batch_work("cfg.optimize.loops-unrolled");
        work.cfg.optimization_budget_refusals += batch_work("cfg.optimize.budget-refusals");
        work.cfg.optimization_inline_budget_refusals +=
            batch_work("cfg.general-inline-budget-refusals");
        work.cfg.optimization_inline_importability_refusals +=
            batch_work("cfg.general-inline-importability-refusals");
        work.cfg.optimization_inline_importability_checks +=
            batch_work("cfg.general-inline-importability-checks");
        work.cfg.optimization_inline_import_attempts +=
            batch_work("cfg.general-inline-import-attempts");
        work.cfg.optimization_inline_interner_stages +=
            batch_work("cfg.general-inline-interner-stages");
        work.cfg.optimization_inline_growth_preflights +=
            batch_work("cfg.general-inline-growth-preflights");
        let optimizer_code_growth = batch_work("cfg.optimize.code-growth-used");
        let optimizer_code_growth_blocks = batch_work("cfg.optimize.code-growth-blocks-used");
        let reoptimization_code_growth = batch_work("cfg.reoptimize.code-growth-used");
        let reoptimization_code_growth_blocks =
            batch_work("cfg.reoptimize.code-growth-blocks-used");
        let inline_code_growth = batch_work("cfg.general-inline-code-growth");
        let inline_code_growth_blocks = batch_work("cfg.general-inline-code-growth-blocks");
        work.cfg.optimization_code_growth_used +=
            optimizer_code_growth + inline_code_growth + reoptimization_code_growth;
        work.cfg.optimization_code_growth_blocks_used += optimizer_code_growth_blocks
            + inline_code_growth_blocks
            + reoptimization_code_growth_blocks;
        work.cfg.optimization_inline_code_growth_used += inline_code_growth;
        work.cfg.optimization_inline_code_growth_blocks_used += inline_code_growth_blocks;
        work.cfg.optimization_reoptimization_attempts += batch_work("cfg.reoptimize.attempts");
        work.cfg.optimization_reoptimization_completions +=
            batch_work("cfg.reoptimize.completions");
        work.cfg.optimization_reoptimization_code_growth_used += reoptimization_code_growth;
        work.cfg.optimization_reoptimization_code_growth_blocks_used +=
            reoptimization_code_growth_blocks;
        work.cfg.cfg_warnings_emitted += batch_work("cfg.warnings");
        let optimized_reuses = executions
            .iter()
            .filter(|execution| {
                matches!(
                    execution,
                    rue_query::RequestExecution::Reused | rue_query::RequestExecution::Joined
                )
            })
            .count();
        let cfg_reuses = if nested_cfg_attempts.is_empty() {
            optimized_reuses
        } else {
            nested_cfg_reuses
        };
        work.cfg.cfg_reuse_candidates += cfg_reuses;
        work.cfg.cfg_reuses += cfg_reuses;
        if let Some(terminal) = attempt.terminal() {
            self.queries.revisioned.retain_backend_optimized_cfg_batch(
                &mut backend_root,
                &cfg_batch_key,
                terminal,
            );
        }
        let batch = attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("OptimizedCfgBatch publishes typed values")
        };
        assert_eq!(batch.values.len(), cfg_requests.len());
        let unreachable_functions = batch.unreachable_functions.iter().collect::<BTreeSet<_>>();
        for (((function, optimized_cfg_key, body_span), value), _execution) in cfg_requests
            .into_iter()
            .zip(batch.values.iter())
            .zip(executions)
        {
            #[cfg(test)]
            self.rooted_cfg_executions
                .push((function.clone(), _execution));
            let record = match value {
                crate::cfg_query::CfgValue::Available(record) => record.clone(),
                crate::cfg_query::CfgValue::Failure {
                    errors,
                    body_span: old_span,
                } => {
                    return Err(PipelineRequestControl::Compile(
                        crate::cfg_query::import_errors(errors, *old_span, body_span),
                    ));
                }
                crate::cfg_query::CfgValue::AccessorFailure { errors, origin, .. } => {
                    return Err(PipelineRequestControl::Compile(
                        crate::cfg_query::import_accessor_failure(
                            errors,
                            origin,
                            &optimized_cfg_key,
                        ),
                    ));
                }
            };
            let local_air_payload = record.air.payload_store_stats();
            work.cfg.local_epochs += 1;
            work.cfg.local_air_instructions += record.air.instructions().len();
            work.cfg.local_air_payload_bytes += local_air_payload
                .word_store_logical_bytes
                .saturating_add(local_air_payload.projection_store_logical_bytes)
                .saturating_add(local_air_payload.place_store_logical_bytes);
            work.cfg.local_type_entries += record.type_pool.len();
            work.cfg.local_aggregate_type_aliases += record.local_aggregate_type_aliases;
            work.cfg.local_materialized_type_handles += record.local_materialized_type_handles;
            work.cfg.local_interner_entries += record.interner.len();
            work.cfg.local_interner_utf8_bytes += record.interner.utf8_bytes();
            work.cfg.local_strings += record.strings.len();
            work.cfg.local_atoms += record.local_atoms.len();
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
            // Reachability only controls backend publication. Diagnostics and
            // completed-work accounting belong to every successfully queried
            // body, including a callee removed after inlining.
            if unreachable_functions.contains(&function) {
                continue;
            }
            cfgs.push(RootedCfgUnit {
                function,
                optimized_cfg_key,
                record,
                body_span,
            });
        }
        if self.oracle_fault == Some(crate::unstable::DifferentialOracleFault::CfgTransformation) {
            self.oracle_fault.take();
            let main = cfgs
                .iter_mut()
                .find(|unit| unit.function == main_identity)
                .expect("successful rooted CFG publishes main");
            let record = Arc::make_mut(&mut main.record);
            if !record
                .cfg
                .inject_differential_comparison_fault(&record.type_pool)
            {
                return Err(CompileError::without_span(ErrorKind::InternalError(
                    "differential CFG transformation fault had no equality comparison to corrupt"
                        .into(),
                ))
                .into());
            }
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

        // Presentation order for warnings is module path, then span, then the
        // rendered text. Building that key costs a module lookup and two
        // rendered strings, so it is decorated once per warning rather than
        // twice per comparison. The first module wins a duplicated file id,
        // matching the linear scan this replaces.
        let mut module_ids: AHashMap<rue_span::FileId, &str> =
            AHashMap::with_capacity(graph.modules.len());
        for module in graph.modules.iter() {
            module_ids
                .entry(module.file_id())
                .or_insert_with(|| module.module_id().as_str());
        }
        let mut keyed = warnings
            .drain(..)
            .map(|warning| {
                let span = warning.span();
                let key = (
                    span.and_then(|span| module_ids.get(&span.file_id).copied())
                        .unwrap_or(""),
                    span.map(|span| span.start).unwrap_or(0),
                    span.map(|span| span.end).unwrap_or(0),
                    warning.to_string(),
                    format!("{:?}", warning.diagnostic()),
                );
                (key, warning)
            })
            .collect::<Vec<_>>();
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        warnings.extend(keyed.into_iter().map(|(_, warning)| warning));
        warnings.dedup();

        let source = self
            .published_snapshot
            .clone()
            .ok_or_else(|| PipelineRequestControl::Compile(no_published_program()))?;
        let input = CodegenInputDescriptor {
            semantic: SemanticInputDescriptor::new(
                &source,
                options.target,
                &options.preview_features,
            ),
            opt_level: options.opt_level.into(),
        };
        let imports = self
            .accepted_semantic_import_graph()
            .map_err(PipelineRequestControl::Compile)?;
        let diagnostics = self.publish_diagnostics(
            &source,
            FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(&input, imports)),
            None,
            &warnings,
        );
        self.diagnostics.select_snapshot(&diagnostics);
        self.refresh_retention_metrics();

        Ok(publish_post(RootedCfgOutput {
            graph,
            cfgs,
            optimized_cfg_batch: cfg_batch_key,
            warnings,
            work,
            backend_work,
            backend_root,
        }))
    }

    pub(crate) fn rooted_codegen(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<RootedCodegenOutput, CompileErrors> {
        match self.rooted_codegen_with_cancellation(
            options,
            request,
            rue_query::CancellationToken::new(),
        ) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("rooted codegen", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_codegen_with_cancellation(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenOutput, PipelineRequestControl> {
        let ready =
            self.rooted_codegen_ready_with_cancellation(options, request, cancellation.clone())?;
        self.rooted_objects_ready_with_cancellation(ready, cancellation)
    }

    /// Complete the retained codegen boundary for an ordinary internal link.
    /// ObjectProjectionBatch is intentionally not requested: the linker
    /// consumes the retained CodegenUnits directly. Byte consumers continue
    /// through `rooted_objects_ready_with_cancellation` below.
    pub(crate) fn rooted_codegen_internal_with_cancellation(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenOutput, PipelineRequestControl> {
        let ready =
            self.rooted_codegen_ready_with_cancellation(options, request, cancellation.clone())?;
        let RootedCodegenReadyOutput {
            graph,
            units,
            cfgs,
            warnings,
            work,
            cfg_work,
            codegen_work,
            backend_root,
            codegen_batch_key,
        } = ready;
        let exports = collect_rooted_exports(&graph, &cfgs);
        if cancellation.is_canceled() {
            return Err(PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        self.queries
            .revisioned
            .publish_backend_root(
                graph.revision,
                backend_root,
                crate::revisioned_query_database::BackendRootPublicationInput::Codegen(
                    codegen_batch_key,
                ),
            )
            .map_err(PipelineRequestControl::Abort)?;
        Ok(RootedCodegenOutput {
            input: RootedCodegenInput::Structured,
            units,
            objects: Vec::new(),
            cfgs,
            exports,
            warnings,
            work,
            cfg_work,
            codegen_work,
            object_projection_work: BackendQueryWork::default(),
        })
    }

    /// Collect the rooted reached set's canonical CodegenUnits while retaining
    /// the exact unpublished backend-root candidate for object projection.
    pub(crate) fn rooted_codegen_ready(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<RootedCodegenReadyOutput, CompileErrors> {
        match self.rooted_codegen_ready_with_cancellation(
            options,
            request,
            rue_query::CancellationToken::new(),
        ) {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("codegen-ready", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_codegen_ready_with_cancellation(
        &mut self,
        options: &CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenReadyOutput, PipelineRequestControl> {
        let RootedCfgOutput {
            graph,
            cfgs,
            optimized_cfg_batch,
            warnings,
            work,
            backend_work: cfg_work,
            mut backend_root,
        } = self.rooted_cfg_with_cancellation(options, cancellation.clone())?;

        let codegen_keys = cfgs
            .iter()
            .map(|cfg| {
                crate::codegen_query::CodegenUnitQueryKey::new_with_batch(
                    cfg.optimized_cfg_key.clone(),
                    options.target,
                    request,
                    options.opt_level,
                    (!cfg.record.durable_reuse_allowed)
                        .then(|| std::sync::Arc::new(optimized_cfg_batch.clone())),
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
            self.object_projection_executions.clear();
            self.object_projection_collections = 0;
        }
        let _codegen_collection_span =
            tracing::info_span!("codegen_collection", phase = "backend").entered();
        let (codegen_batch_key, attempt) =
            self.queries
                .revisioned
                .codegen_unit_batch(graph.revision, codegen_keys, cancellation);
        let batch_execution = attempt.execution();
        let child_attempts = if batch_execution == rue_query::RequestExecution::Computed {
            let attempts = attempt
                .nested_attempts()
                .iter()
                .filter(|attempt| attempt.node().family() == "compiler.codegen-unit")
                .map(rue_query::NestedQueryAttempt::execution)
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
        #[cfg(test)]
        let child_attempt_work = if batch_execution == rue_query::RequestExecution::Computed {
            Some(
                attempt
                    .nested_attempts()
                    .iter()
                    .filter(|attempt| attempt.node().family() == "compiler.codegen-unit")
                    .map(|attempt| attempt.work().to_vec())
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        let mut codegen_work = BackendQueryWork::default();
        if let Some(terminal) = attempt.terminal() {
            self.queries.revisioned.retain_backend_codegen_batch(
                &mut backend_root,
                &codegen_batch_key,
                terminal,
            );
        }
        let batch = attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(batch) = batch.outcome() else {
            unreachable!("CodegenUnitBatch publishes typed terminals")
        };
        assert_eq!(batch.values.len(), cfgs.len());
        for (index, (cfg, value)) in cfgs.iter().zip(batch.values.iter()).enumerate() {
            let execution = child_attempts
                .as_ref()
                .map_or(batch_execution, |attempts| attempts[index]);
            codegen_work.observe(execution);
            #[cfg(test)]
            {
                self.codegen_executions
                    .push((cfg.function.clone(), execution));
                self.codegen_attempt_work.push((
                    cfg.function.clone(),
                    child_attempt_work
                        .as_ref()
                        .map_or_else(Vec::new, |attempts| attempts[index].clone()),
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
                    return Err(PipelineRequestControl::Compile(errors.clone()));
                }
            }
        }
        drop(_codegen_collection_span);
        Ok(RootedCodegenReadyOutput {
            graph,
            units,
            cfgs,
            warnings,
            work,
            cfg_work,
            codegen_work,
            backend_root,
            codegen_batch_key,
        })
    }

    /// Continue one compiler-issued codegen-ready capability through retained
    /// per-unit object projection and atomically publish the backend root.
    pub(crate) fn rooted_objects_ready(
        &mut self,
        ready: RootedCodegenReadyOutput,
    ) -> Result<RootedCodegenOutput, CompileErrors> {
        match self
            .rooted_objects_ready_with_cancellation(ready, rue_query::CancellationToken::new())
        {
            Ok(output) => Ok(output),
            Err(PipelineRequestControl::Compile(errors)) => Err(errors),
            Err(PipelineRequestControl::Abort(abort)) => {
                Err(pipeline_abort_errors("objects-ready", abort))
            }
            Err(PipelineRequestControl::Parked(park)) => {
                Err(unresolved_toolchain_park_errors(&park))
            }
        }
    }

    pub(crate) fn rooted_objects_ready_with_cancellation(
        &mut self,
        ready: RootedCodegenReadyOutput,
        cancellation: rue_query::CancellationToken,
    ) -> Result<RootedCodegenOutput, PipelineRequestControl> {
        let RootedCodegenReadyOutput {
            graph,
            units,
            cfgs,
            warnings,
            work,
            cfg_work,
            codegen_work,
            mut backend_root,
            codegen_batch_key,
        } = ready;
        let object_keys = codegen_batch_key
            .keys
            .iter()
            .cloned()
            .map(crate::object_query::ObjectProjectionQueryKey::new)
            .collect::<Vec<_>>()
            .into();
        let (object_batch_key, object_attempt) = self.queries.revisioned.object_projection_batch(
            graph.revision,
            object_keys,
            cancellation.clone(),
        );
        let object_batch_execution = object_attempt.execution();
        let object_child_attempts =
            if object_batch_execution == rue_query::RequestExecution::Computed {
                let attempts = object_attempt
                    .nested_attempts()
                    .iter()
                    .filter(|attempt| attempt.node().family() == "compiler.object-projection")
                    .map(|attempt| attempt.execution())
                    .collect::<Vec<_>>();
                assert_eq!(
                    attempts.len(),
                    cfgs.len(),
                    "an evaluated ObjectProjection batch records one direct child per key"
                );
                Some(attempts)
            } else {
                None
            };
        let mut object_projection_work = BackendQueryWork::default();
        if let Some(terminal) = object_attempt.terminal() {
            self.queries
                .revisioned
                .retain_backend_object_projection_batch(
                    &mut backend_root,
                    &object_batch_key,
                    terminal,
                );
        }
        let object_batch = object_attempt
            .into_result()
            .map_err(PipelineRequestControl::Abort)?;
        let rue_query::QueryOutcome::Success(object_batch) = object_batch.outcome() else {
            unreachable!("ObjectProjectionBatch publishes typed terminals")
        };
        assert_eq!(object_batch.values.len(), units.len());
        let mut objects = Vec::with_capacity(units.len());
        for (index, (collected, value)) in units.iter().zip(object_batch.values.iter()).enumerate()
        {
            let execution = object_child_attempts
                .as_ref()
                .map_or(object_batch_execution, |attempts| attempts[index]);
            object_projection_work.observe(execution);
            #[cfg(test)]
            self.object_projection_executions
                .push((collected.function.clone(), execution));
            match value {
                crate::object_query::ObjectProjectionValue::Available(object) => {
                    objects.push(crate::object_query::CollectedObjectProjection {
                        function: collected.function.clone(),
                        unit: collected.unit.clone(),
                        object: object.clone(),
                    });
                    #[cfg(test)]
                    {
                        self.object_projection_collections += 1;
                    }
                }
                crate::object_query::ObjectProjectionValue::Failure(errors) => {
                    return Err(PipelineRequestControl::Compile(errors.clone()));
                }
            }
        }
        let exports = collect_rooted_exports(&graph, &cfgs);
        if cancellation.is_canceled() {
            return Err(PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        self.queries
            .revisioned
            .publish_backend_root(
                graph.revision,
                backend_root,
                crate::revisioned_query_database::BackendRootPublicationInput::Objects(
                    object_batch_key,
                ),
            )
            .map_err(PipelineRequestControl::Abort)?;
        Ok(RootedCodegenOutput {
            input: RootedCodegenInput::Projected,
            units,
            objects,
            cfgs,
            exports,
            warnings,
            work,
            cfg_work,
            codegen_work,
            object_projection_work,
        })
    }

    /// Collect reached canonical codegen terminals for tests which inspect the
    /// pre-object boundary. Production object and link consumers use
    /// `rooted_codegen`'s query-native image root; this adapter enumerates the
    /// semantic functions only so focused tests can inspect units without
    /// constructing a `ProgramImage`.
    #[cfg(test)]
    pub(crate) fn codegen_units(
        &mut self,
        semantic: &RootedCfgOutput,
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
        let mut units = Vec::with_capacity(semantic.cfgs.len());
        #[cfg(test)]
        {
            self.codegen_executions.clear();
            self.codegen_attempt_work.clear();
            self.codegen_collections = 0;
        }
        for function in &semantic.cfgs {
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
                    .push((function.function.clone(), attempt.execution()));
                self.codegen_attempt_work
                    .push((function.function.clone(), attempt.work().to_vec()));
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
                        function: function.function.clone(),
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
    pub(crate) fn object_projection_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.object_projection_executions
    }

    #[cfg(test)]
    pub(crate) fn object_projection_collections(&self) -> usize {
        self.object_projection_collections
    }

    #[cfg(test)]
    pub(crate) fn backend_root_metrics(
        &self,
    ) -> crate::revisioned_query_database::PublishedBackendRootMetrics {
        self.queries.revisioned.backend_root_metrics_for_test()
    }

    #[cfg(test)]
    pub(crate) fn raw_cfg_handoff_is_published(
        &self,
        output: &RootedPreOptimizationCfgOutput,
    ) -> bool {
        self.queries
            .revisioned
            .raw_cfg_handoff_matches_terminal_for_test(&output._raw_cfg_terminal)
    }

    #[cfg(test)]
    pub(crate) fn backend_cfg_key_is_retained(&self, key: &crate::cfg_query::CfgQueryKey) -> bool {
        self.queries
            .revisioned
            .backend_cfg_key_is_retained_for_test(key)
    }

    #[cfg(test)]
    pub(crate) fn raw_cfg_record_for_test(
        &self,
        key: crate::cfg_query::CfgQueryKey,
    ) -> Arc<crate::cfg_query::CfgRecord> {
        let revision = self
            .queries
            .revisioned
            .current_semantic_revision()
            .expect("raw CFG inspection requires a published semantic revision");
        let terminal = self
            .queries
            .revisioned
            .cfg(revision, key, rue_query::CancellationToken::new())
            .into_result()
            .expect("retained raw CFG request must not abort");
        let rue_query::QueryOutcome::Success(crate::cfg_query::CfgValue::Available(record)) =
            terminal.outcome()
        else {
            panic!("raw CFG inspection requires a successful record")
        };
        record.clone()
    }

    #[cfg(test)]
    pub(crate) fn object_projection_key_is_retained(
        &self,
        key: &crate::object_query::ObjectProjectionQueryKey,
    ) -> bool {
        self.queries
            .revisioned
            .object_projection_key_is_retained_for_test(key)
    }

    #[cfg(test)]
    pub(crate) fn query_evictions_for_test(&self) -> u64 {
        self.queries.revisioned.query_evictions_for_test()
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
        self.open_discovery = None;
        self.successor_delta_nonce = None;
        self.queries
            .revisioned
            .restore_import_revision_after_abort(
                committed.as_ref().and_then(|(revision, _, _, _)| *revision),
            )?;
        if let Some(checkpoint) = self.import_request_checkpoint.take() {
            self.validated_accepted_reads = checkpoint.validated_accepted_reads;
            self.continuation = checkpoint.continuation;
            self.queries.discovery_attempt = checkpoint.discovery_attempt;
            self.queries.prior_discovery = checkpoint.prior_discovery;
            self.batch_diagnostic_order = checkpoint.batch_diagnostic_order;
            self.diagnostics = checkpoint.diagnostics;
            // A fresh rooted import request invalidates a provisional trusted
            // successor delta permanently. Restoring only its nonce after the
            // revisioned database has reselected the committed predecessor
            // would manufacture a live-looking capability whose exact overlay
            // revision and lineage no longer exist.
            self.successor_delta_nonce = None;
            self.refresh_retention_metrics();
            return Ok(());
        }
        self.validated_accepted_reads = committed
            .as_ref()
            .map(|(_, snapshot, accepted_reads, _)| (snapshot.clone(), accepted_reads.clone()));
        self.continuation = committed.and_then(|(revision, snapshot, accepted_reads, ledger)| {
            revision.map(|revision| {
                self.next_continuation_nonce += 1;
                ContinuationState {
                    nonce: self.next_continuation_nonce,
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

    /// One module's warning-collection lookups: the module itself, plus the
    /// span index of its function items, built on first use so a program whose
    /// candidates touch one module never indexes the rest.
    struct CandidateModule<'a> {
        module: &'a crate::parsed_modules::ParsedModule,
        functions: Option<AHashMap<rue_span::Span, &'a rue_parser::ast::Function>>,
    }

    // Candidate declarations are keyed by module and located by declaration
    // span, so both lookups are indexed rather than scanned per candidate. The
    // first module and the first item win a duplicated key, matching the linear
    // scans these replace.
    let mut modules: AHashMap<&crate::ModuleId, CandidateModule<'_>> =
        AHashMap::with_capacity(graph.modules.len());
    for module in graph.modules.iter() {
        modules
            .entry(module.module_id())
            .or_insert_with(|| CandidateModule {
                module,
                functions: None,
            });
    }

    let mut warnings = Vec::new();
    for declaration in graph.declarations.iter() {
        let name = declaration.key.name();
        if declaration.key.kind() != crate::StableDefinitionKind::Function
            || name == "main"
            || declaration.key.module().is_trusted_standard_library()
            || declaration.is_public
            || name.starts_with('_')
            || referenced.contains(&declaration.key)
        {
            continue;
        }
        let Some(entry) = modules.get_mut(declaration.key.module()) else {
            continue;
        };
        let module = entry.module;
        let candidate = crate::declaration_candidate::DeclarationCandidateKey {
            module: declaration.key.module().clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from(name),
            owner: None,
            duplicate_discriminator: 0,
        };
        let Some(locator) = module.definitions().declaration_locator(&candidate) else {
            continue;
        };
        let functions = entry.functions.get_or_insert_with(|| {
            let items = &module.ast().items;
            let mut spans = AHashMap::with_capacity(items.len());
            for item in items.iter() {
                if let rue_parser::ast::Item::Function(function) = item {
                    spans.entry(function.span).or_insert(function);
                }
            }
            spans
        });
        let Some(function) = functions.get(&locator.declaration_span).copied() else {
            continue;
        };
        let allows_unused = function.directives.iter().any(|directive| {
            module.resolve_raw_symbol(directive.name.name) == "allow"
                && directive.args.iter().any(|argument| match argument {
                    rue_parser::ast::DirectiveArg::Ident(argument) => {
                        module.resolve_raw_symbol(argument.name) == "unused_function"
                    }
                })
        });
        if allows_unused {
            continue;
        }
        warnings.push(
            CompileWarning::new(
                rue_error::WarningKind::UnusedFunction(name.to_owned()),
                locator.declaration_span,
            )
            .with_help(format!(
                "if this is intentional, prefix it with an underscore: `_{name}`"
            )),
        );
    }
    warnings
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
    if let F::DiagnosticAtProducerRange {
        kind,
        producer: producer_key,
        start,
        end,
    } = failure
        && let Some(producer) = modules
            .iter()
            .find(|module| module.module_id() == &producer_key.module)
            .and_then(|module| module.definitions().declaration_locator(producer_key))
            .map(|locator| locator.declaration_span)
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

impl CompilerSession {
    /// Return the producer request that owns each currently retained ordinary
    /// body terminal named by `names`. A missing declaration or a declaration
    /// with no retained reached-body terminal is omitted.
    ///
    /// The scaling harness compares these stable provenance identities across
    /// revisions to prove the exact recomputed body set. Equal work counts alone
    /// cannot distinguish recomputing the intended consumers from recomputing
    /// the same number of unrelated bodies.
    #[cfg(test)]
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
mod tests;
