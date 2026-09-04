//! In-process canonical parse, merge, and RIR query orchestration.

use ahash::AHashMap;
#[cfg(test)]
use rue_air::Node;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::{
    CanonicalImportGraph, CanonicalImportGraphValidation, CanonicalMergeWork,
    CanonicalMergedProgram, CanonicalRirOutput, CanonicalRirWork, CodegenInputDescriptor,
    CompileError, CompileErrors, CompileOptions, CompileWarning, ErrorKind, ModuleResolutionInputs,
    ParseInvalidationSummary, ParsedModulesWork, SourceRevision, SourceSnapshot,
    StablePreviewFeatures, parsed_modules::ParsedProgram, validate_canonical_import_graph,
};

pub(crate) use crate::diagnostic_attempt_store::FRONTEND_DIAGNOSTIC_RETENTION_LIMIT;
use crate::diagnostic_attempt_store::{
    DiagnosticAttemptProvenance, DiagnosticAttemptStore, FrontendDiagnosticIdentity,
    FrontendDiagnosticSnapshot, ImportDiagnosticInputDescriptor,
};
use crate::retained_charge::RetainedCharge;
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as QueryAttemptExecution, AttemptOutcomeKind,
    QUERY_TERMINAL_RETENTION_LIMIT, TerminalKind, TypedQueryFamily,
};

/// Capability for constructing the session's canonical revisioned query
/// database. Its private field and constructor keep safe construction inside
/// this module tree; source inventory pins the sole use to `frontend_queries`.
pub(crate) struct RevisionedQueryDatabaseConstructionToken {
    _private: (),
}

impl RevisionedQueryDatabaseConstructionToken {
    fn new() -> Self {
        Self { _private: () }
    }
}

// Source-level partitions of this one owner. `CompilerSession` remains the
// sole session/query-graph owner, while its implementation is grouped into
// owner-controlled capabilities with the stable façade declared here.
mod discovery_continuation;
mod frontend_queries;
mod import_discovery_owner;
mod metrics;
mod metrics_attempts;
mod program_artifacts;
mod revision_lifecycle;
mod rooted_artifacts;
mod rooted_projections;

pub use discovery_continuation::*;
pub(crate) use frontend_queries::*;
use import_discovery_owner::ImportDiscoveryOwner;
pub use metrics::*;
use program_artifacts::no_published_program;
pub use rooted_artifacts::*;
#[cfg(test)]
use rooted_projections::{stable_function_definition_root, stable_producer_definition_root};

// Production-only authority for structural gates. Every implementation
// partition is explicit so owner-local test modules cannot truncate the scan.
#[cfg(test)]
pub(crate) const SESSION_PRODUCTION_SOURCE: &str = concat!(
    include_str!("session/metrics.rs"),
    include_str!("session/rooted_artifacts.rs"),
    include_str!("session/discovery_continuation.rs"),
    include_str!("session/frontend_queries.rs"),
    include_str!("session/revision_lifecycle.rs"),
    include_str!("session/import_discovery_owner.rs"),
    include_str!("session/metrics_attempts.rs"),
    include_str!("session/program_artifacts.rs"),
    include_str!("session/rooted_projections.rs"),
    include_str!("session.rs"),
);

// Whole session tree, including integration and owner-local tests, for gates
// which deliberately inventory test-only hooks as well as production code.
#[cfg(test)]
pub(crate) const SESSION_SOURCE: &str = concat!(
    include_str!("session/metrics.rs"),
    include_str!("session/rooted_artifacts.rs"),
    include_str!("session/discovery_continuation.rs"),
    include_str!("session/frontend_queries.rs"),
    include_str!("session/revision_lifecycle.rs"),
    include_str!("session/import_discovery_owner.rs"),
    include_str!("session/metrics_attempts.rs"),
    include_str!("session/program_artifacts.rs"),
    include_str!("session/rooted_projections.rs"),
    include_str!("session.rs"),
    include_str!("session/import_discovery_owner/tests.rs"),
    include_str!("session/program_artifacts/tests.rs"),
    include_str!("session/tests.rs"),
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
    /// Opaque import lifecycle and selector authority. Its fields are private
    /// to `import_discovery_owner`, making sibling reachability compiler-enforced.
    imports: ImportDiscoveryOwner,
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
    CompileError::without_span(ErrorKind::InternalError(abort_internal_message(
        context, &abort,
    )))
    .into()
}

/// Renders one query abort as the message of an internal-error diagnostic.
///
/// A refused physical worker thread is a condition of the host the compiler is
/// running on rather than a defect in the compiler or the program, so it keeps
/// its own contracted sentence: it names the operating system's refusal and the
/// worker budget that was live, and a driver or harness can recognize it
/// without parsing a structural dump. Every other abort keeps the structural
/// rendering, which is a compiler-internal condition worth reading as one.
pub(crate) fn abort_internal_message(context: &str, abort: &rue_query::QueryAbort) -> String {
    match abort {
        rue_query::QueryAbort::WorkerSpawn(failure) => failure.to_string(),
        abort => format!("{context} query aborted: {abort:?}"),
    }
}

impl From<CompileErrors> for SemanticRequestControl {
    fn from(errors: CompileErrors) -> Self {
        Self::Compile(errors)
    }
}

#[cfg(test)]
mod tests;
