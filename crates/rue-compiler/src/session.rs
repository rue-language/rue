//! In-process canonical parse, merge, and RIR query orchestration.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use rue_air::{DeclarationBindingWork, SemanticBindingManifestWork};
use rue_span::Span;
use sha2::{Digest, Sha256};

use crate::{
    BoundDefinitionSet, BoundDefinitionWork, CanonicalImportGraph, CanonicalImportGraphValidation,
    CanonicalImportResolution, CanonicalMergeWork, CanonicalMergedProgram, CanonicalRirOutput,
    CanonicalRirWork, CanonicalSemanticOutput, CanonicalSemanticWork, CodegenInputDescriptor,
    CompileError, CompileErrors, CompileOptions, CompileWarning, DurableDeclarationSemantic,
    ErrorKind, ModuleResolutionInputs, ParseInvalidationSummary, ParsedModulesWork,
    SemanticInputDescriptor, SourceRevision, SourceSnapshot, StableDefinitionKey,
    StableDefinitionKind, StableDefinitionNamespace, StableOptLevel, StablePreviewFeatures,
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
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as QueryAttemptExecution, AttemptView,
    QUERY_TERMINAL_RETENTION_LIMIT, TerminalKind, TypedEquivalentLookupFamily, TypedQueryFamily,
    TypedQueryStore, TypedSecondaryLookupFamily,
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
    /// Constant-size selected and last-good terminal protection.
    pub protected_query_records: usize,
    /// Retained terminals currently referenced by reverse dependency edges.
    pub dependency_pins: usize,
    /// Bounded validation stamps whose artifacts have been evicted.
    pub validation_tombstones: usize,
    /// Disappeared graph nodes pinned by retained reverse dependency edges
    /// after their family tombstones have left bounded store retention.
    pub graph_retained_disappeared_nodes: usize,
    /// Lifetime artifact evictions across all typed query families.
    pub query_evictions: usize,
    /// Bounded canceled, duplicate, and cyclic query-attempt history.
    pub aborted_query_attempts: usize,
    /// Retained direct import-diagnostic query terminals.
    pub import_query_entries: usize,
    /// Lifetime direct import-diagnostic query evictions.
    pub import_query_evictions: usize,
    /// Retained semantic query terminals.
    pub semantic_query_entries: usize,
    /// Lifetime semantic query evictions.
    pub semantic_query_evictions: usize,
    /// Retained stable-definition query terminals.
    pub definition_query_entries: usize,
    /// Lifetime stable-definition query evictions.
    pub definition_query_evictions: usize,
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
    /// Distinct dependency manifests strongly owned by all session caches.
    pub dependency_manifests: usize,
    /// Recent semantic invalidation plans strongly owned by the session.
    pub invalidation_plans: usize,
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
    pub dependency_manifests: FrontendQueryWork,
    pub dependency_manifest_records_visited: usize,
    pub dependency_manifest_import_records_visited: usize,
    pub invalidation_plans: FrontendQueryWork,
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
    DependencyManifest(Box<SemanticDependencyManifestWork>),
    Invalidation(SemanticInvalidationWork),
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
        attempt.dependencies(),
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
    dependencies: Vec<crate::query_graph::ObservedDependency>,
    diagnostics: Option<Arc<FrontendDiagnosticSnapshot>>,
    structural: QueryStructuralWork,
    cancel_requested: bool,
}

impl QueryComputationGuard {
    fn started(&mut self) {}

    fn accrue(&mut self, structural: QueryStructuralWork) {
        self.structural = structural;
    }

    fn structural(&self) -> QueryStructuralWork {
        self.structural.clone()
    }

    fn bind(&mut self, attempt: Arc<dyn AttemptView>) {
        self.attempt = Some(attempt);
    }

    fn observe(
        &mut self,
        dependencies: impl IntoIterator<Item = crate::query_graph::ObservedDependency>,
    ) {
        self.dependencies.extend(dependencies);
    }

    fn attach_diagnostics(&mut self, diagnostics: Arc<FrontendDiagnosticSnapshot>) {
        self.diagnostics = Some(diagnostics);
    }

    fn request_cancel(&mut self) {
        self.cancel_requested = true;
    }

    fn finish<T, E>(
        self,
        _execution: QueryAttemptExecution,
        _reuse_origin: Option<AttemptId>,
        _result: &Result<T, E>,
        _structural: QueryStructuralWork,
    ) -> AttemptId {
        if let Some(attempt) = self.attempt {
            self.sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .index(self.family, attempt);
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
    fn allocate_attempt_id(&self) -> AttemptId {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .allocate()
    }

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
            dependencies: Vec::new(),
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
            dependencies: Vec::new(),
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
            QueryStructuralWork::DependencyManifest(work) => {
                self.aggregate.dependency_manifest_records_visited +=
                    work.definition_records_visited;
                self.aggregate.dependency_manifest_import_records_visited +=
                    work.import_records_visited;
            }
            QueryStructuralWork::Invalidation(_) => {}
            QueryStructuralWork::Merge(_) | QueryStructuralWork::Rir(_) => {}
        }
    }

    #[cfg(test)]
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

    fn project_dependency_invalidations(
        &mut self,
        graph: &crate::query_graph::QueryGraph,
        changed_existing_revision: bool,
    ) {
        if changed_existing_revision {
            self.aggregate.downstream_invalidations += 1;
        }
        let semantic = graph.invalidation_count::<SemanticQuery>();
        let definitions = graph.invalidation_count::<DefinitionQuery>();
        self.aggregate.semantic_entries_invalidated +=
            semantic.saturating_sub(self.projected_semantic_invalidations);
        self.aggregate.definition_entries_invalidated +=
            definitions.saturating_sub(self.projected_definition_invalidations);
        self.projected_semantic_invalidations = semantic;
        self.projected_definition_invalidations = definitions;
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

/// Maximum number of recent invalidation plans owned by a frontend session.
///
/// Each entry strongly owns both input manifests. Oldest insertion is evicted
/// first; weak references are intentionally not used because a plan's
/// dependency inputs must remain sound for as long as the cached plan exists.
pub const FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticDependencyManifestWork {
    pub definition_records_visited: usize,
    pub import_records_visited: usize,
    pub free_function_events_translated: usize,
    pub specialization_origins_validated: usize,
    pub named_method_events_translated: usize,
    pub named_destructor_events_translated: usize,
    pub declaration_type_events_translated: usize,
    pub declaration_type_call_head_events_translated: usize,
    pub builtin_type_call_head_inputs_translated: usize,
    pub named_const_events_translated: usize,
    pub implicit_named_destructor_events_translated: usize,
    pub body_owner_events_translated: usize,
    pub body_named_events_translated: usize,
    pub body_dependency_records_built: usize,
    pub durable_bodies: crate::DurableBodyWork,
    pub extra_rir_instructions_visited: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableFreeFunctionDependency {
    pub caller: StableDefinitionKey,
    pub callee: StableDefinitionKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableNamedMethodDependencyTarget {
    FreeFunction(StableDefinitionKey),
    NamedMethod(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedMethodDependency {
    pub caller: StableDefinitionKey,
    pub target: StableNamedMethodDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedDestructorDependency {
    pub caller: StableDefinitionKey,
    pub target: StableNamedMethodDependencyTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableImplicitNamedDestructorDependency {
    pub source: StableDefinitionKey,
    pub target: StableDefinitionKey,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDeclarationTypeDependency {
    pub source: StableDefinitionKey,
    pub target: StableDefinitionKey,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDeclarationTypeCallHeadDependency {
    pub source: StableDefinitionKey,
    pub callable: StableDefinitionKey,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableBuiltinTypeCallHeadInput {
    pub source: StableDefinitionKey,
    pub builtin: rue_air::BuiltinTypeCallHead,
    pub kind: rue_air::DeclarationTypeDependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableNamedConstDependencyTarget {
    ValueConst(StableDefinitionKey),
    FreeFunction(StableDefinitionKey),
    NamedType(StableDefinitionKey),
    ModuleBinding(StableDefinitionKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedConstDependency {
    pub source: StableDefinitionKey,
    pub target: StableNamedConstDependencyTarget,
}

/// Complete stable inputs observed for one successfully analyzed ordinary body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StableBodyDependencyInputRecord {
    owner: StableDefinitionKey,
    fingerprint: StableDefinitionInputFingerprint,
    target: crate::Target,
    preview_features: StablePreviewFeatures,
    direct_dependency_inputs: Arc<[StableDefinitionInputFingerprint]>,
    builtin_type_call_heads: Arc<[StableBuiltinTypeCallHeadInput]>,
    blockers: Arc<[SemanticDependencyBlocker]>,
}

impl StableBodyDependencyInputRecord {
    pub fn owner(&self) -> &StableDefinitionKey {
        &self.owner
    }
    pub fn fingerprint(&self) -> &StableDefinitionInputFingerprint {
        &self.fingerprint
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn reusable_boundary_supported(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Versioned digest of one immutable semantic input fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDefinitionFingerprint([u8; 32]);

impl StableDefinitionFingerprint {
    #[cfg(test)]
    pub(crate) fn for_test(byte: u8) -> Self {
        Self([byte; 32])
    }
}

/// Precision of the parser-authored source partition represented by a
/// definition fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionFingerprintPrecision {
    SignatureAndBody,
    SignatureAndInitializer,
    /// All declaration bytes are semantic signature input and there is no
    /// independently executable payload.
    ExactSignature,
}

/// Immutable, relocation-independent inputs for one stable definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDefinitionInputFingerprint {
    /// Schema version for persisted consumers. Bump when domains or partition
    /// semantics change.
    pub schema_version: u16,
    pub key: StableDefinitionKey,
    /// Stable identity and visibility metadata, excluding source locations.
    pub declaration: StableDefinitionFingerprint,
    /// Signature/header bytes, or the full declaration under conservative
    /// precision.
    pub signature: StableDefinitionFingerprint,
    /// Function/method/destructor body or const initializer when exact parser
    /// boundaries are available.
    pub body_or_initializer: Option<StableDefinitionFingerprint>,
    pub precision: StableDefinitionFingerprintPrecision,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableModuleImportDependency {
    Resolved {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
        target: crate::ModuleId,
    },
    Missing {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
    },
    Ambiguous {
        importer: crate::ModuleId,
        normalized_specifier: Arc<str>,
        file_module: crate::ModuleId,
        directory_module: crate::ModuleId,
    },
}

/// A semantic dependency surface whose captured edges may be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticDependencySurface {
    BodyOwner,
    FreeFunctionCall,
    NonGenericNamedMethodCall,
    GenericNamedMethodCall,
    NamedDestructorCall,
    ImplicitNamedDestructor,
    DeclarationType,
    DeclarationTypeCallHead,
    SupportedTypeCallHead,
    NamedValueConst,
}

/// The production evidence which prevents a dependency surface from being trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticDependencyIncompleteReason {
    AnonymousBodyOwnerUnavailable,
    CallerEndpointUnavailable,
    GenericSubstitutionIdentityUnavailable,
    DestructorEndpointUnavailable,
    AnonymousDropOwnerUnavailable,
    ResolvedTypeIdentityUnavailable,
    TypeCallHeadIdentityUnavailable,
    UnsupportedDynamicTypeCallHead,
    ConstEndpointUnavailable,
}

/// A deterministic, stable-keyed reason that semantic reuse must fail closed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticDependencyBlocker {
    owner: Option<StableDefinitionKey>,
    surface: SemanticDependencySurface,
    reason: SemanticDependencyIncompleteReason,
}

impl SemanticDependencyBlocker {
    pub fn owner(&self) -> Option<&StableDefinitionKey> {
        self.owner.as_ref()
    }
    pub fn surface(&self) -> SemanticDependencySurface {
        self.surface
    }
    pub fn reason(&self) -> SemanticDependencyIncompleteReason {
        self.reason
    }
}

/// A non-empty, sorted set of stable dependency blockers.
///
/// Construction is centralized here so the retained slice is the one
/// canonical value used for planning, accessors, and presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NonEmptySemanticDependencyBlockers {
    blockers: Arc<[SemanticDependencyBlocker]>,
}

impl NonEmptySemanticDependencyBlockers {
    fn from_blockers(mut blockers: Vec<SemanticDependencyBlocker>) -> Option<Self> {
        blockers.sort();
        blockers.dedup();
        (!blockers.is_empty()).then(|| Self {
            blockers: blockers.into(),
        })
    }

    fn as_slice(&self) -> &[SemanticDependencyBlocker] {
        &self.blockers
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DurableBodyCandidateState {
    Complete,
    Incomplete(NonEmptySemanticDependencyBlockers),
}

impl DurableBodyCandidateState {
    fn from_blockers(blockers: Vec<SemanticDependencyBlocker>) -> Self {
        match NonEmptySemanticDependencyBlockers::from_blockers(blockers) {
            Some(blockers) => Self::Incomplete(blockers),
            None => Self::Complete,
        }
    }

    fn blockers(&self) -> &[SemanticDependencyBlocker] {
        match self {
            Self::Complete => &[],
            Self::Incomplete(blockers) => blockers.as_slice(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SemanticDependencyGraphState {
    Complete,
    Incomplete(NonEmptySemanticDependencyBlockers),
}

impl SemanticDependencyGraphState {
    fn from_blockers(blockers: Vec<SemanticDependencyBlocker>) -> Self {
        match NonEmptySemanticDependencyBlockers::from_blockers(blockers) {
            Some(blockers) => Self::Incomplete(blockers),
            None => Self::Complete,
        }
    }

    fn blockers(&self) -> &[SemanticDependencyBlocker] {
        match self {
            Self::Complete => &[],
            Self::Incomplete(blockers) => blockers.as_slice(),
        }
    }

    fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }

    fn surface_complete(&self, surface: SemanticDependencySurface) -> bool {
        match self {
            Self::Complete => true,
            Self::Incomplete(blockers) => blockers
                .as_slice()
                .iter()
                .all(|blocker| blocker.surface != surface),
        }
    }

    /// Feed every incomplete surface into invalidation planning. The match on
    /// `SemanticDependencySurface` is deliberately exhaustive: adding a new
    /// surface cannot compile until this fold accounts for it.
    fn fold_planning_blockers(&self, blockers: &mut BTreeSet<SemanticDependencyBlocker>) {
        let Self::Incomplete(incomplete) = self else {
            return;
        };
        for blocker in incomplete.as_slice() {
            match blocker.surface {
                SemanticDependencySurface::BodyOwner
                | SemanticDependencySurface::FreeFunctionCall
                | SemanticDependencySurface::NonGenericNamedMethodCall
                | SemanticDependencySurface::GenericNamedMethodCall
                | SemanticDependencySurface::NamedDestructorCall
                | SemanticDependencySurface::ImplicitNamedDestructor
                | SemanticDependencySurface::DeclarationType
                | SemanticDependencySurface::DeclarationTypeCallHead
                | SemanticDependencySurface::SupportedTypeCallHead
                | SemanticDependencySurface::NamedValueConst => {
                    blockers.insert(blocker.clone());
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NonEmptyDefinitionFailures {
    failures: Arc<[rue_error::CompileError]>,
}

impl NonEmptyDefinitionFailures {
    fn from_errors(errors: &CompileErrors) -> Self {
        assert!(
            !errors.is_empty(),
            "failed stable-definition query must carry at least one error"
        );
        Self {
            failures: errors.as_slice().to_vec().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SemanticDefinitionUniverseIncompleteReason {
    StableDefinitionsFailed(NonEmptyDefinitionFailures),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SemanticDefinitionUniverseState {
    Complete,
    Incomplete(SemanticDefinitionUniverseIncompleteReason),
}

#[derive(Clone, PartialEq, Eq)]
pub struct SemanticDependencyInputManifest {
    input: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
    definitions: Arc<[StableDefinitionKey]>,
    definition_fingerprints: Arc<[StableDefinitionInputFingerprint]>,
    module_imports: Arc<[StableModuleImportDependency]>,
    free_function_dependencies: Arc<[StableFreeFunctionDependency]>,
    named_method_dependencies: Arc<[StableNamedMethodDependency]>,
    named_destructor_dependencies: Arc<[StableNamedDestructorDependency]>,
    implicit_named_destructor_dependencies: Arc<[StableImplicitNamedDestructorDependency]>,
    declaration_type_dependencies: Arc<[StableDeclarationTypeDependency]>,
    declaration_type_call_head_dependencies: Arc<[StableDeclarationTypeCallHeadDependency]>,
    builtin_type_call_head_inputs: Arc<[StableBuiltinTypeCallHeadInput]>,
    named_const_dependencies: Arc<[StableNamedConstDependency]>,
    body_dependencies: Arc<[StableBodyDependencyInputRecord]>,
    durable_ordinary_bodies: Arc<[crate::DurableOrdinaryBody]>,
    /// Completeness of durable body candidates, including owner-specific
    /// blockers. This is distinct from whole-graph invalidation completeness.
    durable_body_candidate_state: DurableBodyCandidateState,
    dependency_graph_state: SemanticDependencyGraphState,
    definition_universe_state: SemanticDefinitionUniverseState,
    work: SemanticDependencyManifestWork,
}

impl SemanticDependencyInputManifest {
    /// Cheap hash discriminant consistent with `PartialEq`. `Eq` compares the
    /// full contents, so hashing only these lengths is sound: equal manifests
    /// share them, and any collision is resolved by exact `Eq`.
    fn hash_discriminant<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.definitions.len().hash(state);
        self.definition_fingerprints.len().hash(state);
        self.module_imports.len().hash(state);
        self.body_dependencies.len().hash(state);
    }
}

impl std::fmt::Debug for SemanticDependencyInputManifest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticDependencyInputManifest")
            .field("input", &self.input)
            .field("imports", &self.imports)
            .field("definitions", &self.definitions)
            .field("definition_fingerprints", &self.definition_fingerprints)
            .field("module_imports", &self.module_imports)
            .field(
                "free_function_dependencies",
                &self.free_function_dependencies,
            )
            .field("named_method_dependencies", &self.named_method_dependencies)
            .field(
                "named_destructor_dependencies",
                &self.named_destructor_dependencies,
            )
            .field(
                "implicit_named_destructor_dependencies",
                &self.implicit_named_destructor_dependencies,
            )
            .field(
                "declaration_type_dependencies",
                &self.declaration_type_dependencies,
            )
            .field(
                "declaration_type_call_head_dependencies",
                &self.declaration_type_call_head_dependencies,
            )
            .field(
                "builtin_type_call_head_inputs",
                &self.builtin_type_call_head_inputs,
            )
            .field("named_const_dependencies", &self.named_const_dependencies)
            .field("body_dependencies", &self.body_dependencies)
            .field("durable_ordinary_bodies", &self.durable_ordinary_bodies)
            .field("body_dependency_blockers", &self.body_dependency_blockers())
            .field("dependency_blockers", &self.dependency_blockers())
            .field(
                "definition_universe_complete",
                &self.definition_universe_complete(),
            )
            .field("work", &self.work)
            .finish()
    }
}

impl SemanticDependencyInputManifest {
    pub fn input(&self) -> &SemanticInputDescriptor {
        &self.input
    }
    pub fn imports(&self) -> &CanonicalImportGraph {
        &self.imports
    }
    pub fn definitions(&self) -> &[StableDefinitionKey] {
        &self.definitions
    }
    pub fn definition_fingerprints(&self) -> &[StableDefinitionInputFingerprint] {
        &self.definition_fingerprints
    }
    pub fn module_imports(&self) -> &[StableModuleImportDependency] {
        &self.module_imports
    }
    pub fn free_function_dependencies(&self) -> &[StableFreeFunctionDependency] {
        &self.free_function_dependencies
    }
    pub fn free_function_caller_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::FreeFunctionCall)
    }
    pub fn named_method_dependencies(&self) -> &[StableNamedMethodDependency] {
        &self.named_method_dependencies
    }
    pub fn non_generic_named_method_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::NonGenericNamedMethodCall)
    }
    pub fn generic_named_method_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::GenericNamedMethodCall)
    }
    pub fn named_destructor_dependencies(&self) -> &[StableNamedDestructorDependency] {
        &self.named_destructor_dependencies
    }
    pub fn named_destructor_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::NamedDestructorCall)
    }
    pub fn implicit_named_destructor_dependencies(
        &self,
    ) -> &[StableImplicitNamedDestructorDependency] {
        &self.implicit_named_destructor_dependencies
    }
    pub fn implicit_named_destructor_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::ImplicitNamedDestructor)
    }
    pub fn declaration_type_dependencies(&self) -> &[StableDeclarationTypeDependency] {
        &self.declaration_type_dependencies
    }
    pub fn declaration_type_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::DeclarationType)
    }
    pub fn declaration_type_call_head_dependencies(
        &self,
    ) -> &[StableDeclarationTypeCallHeadDependency] {
        &self.declaration_type_call_head_dependencies
    }
    pub fn declaration_type_call_head_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::DeclarationTypeCallHead)
    }
    pub fn builtin_type_call_head_inputs(&self) -> &[StableBuiltinTypeCallHeadInput] {
        &self.builtin_type_call_head_inputs
    }
    pub fn supported_type_call_heads_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::SupportedTypeCallHead)
    }
    pub fn named_const_dependencies(&self) -> &[StableNamedConstDependency] {
        &self.named_const_dependencies
    }
    pub fn body_dependencies(&self) -> &[StableBodyDependencyInputRecord] {
        &self.body_dependencies
    }
    /// Explicitly unstable equality status for durable-cache instrumentation.
    pub fn unstable_durable_artifact_status(&self) -> crate::unstable::DurableArtifactStatus {
        crate::unstable::DurableArtifactStatus::from_debug(&self.durable_ordinary_bodies)
    }
    pub fn body_dependency_blockers(&self) -> &[SemanticDependencyBlocker] {
        self.durable_body_candidate_state.blockers()
    }
    pub fn named_value_const_dependencies_complete(&self) -> bool {
        self.surface_complete(SemanticDependencySurface::NamedValueConst)
    }
    pub fn semantic_dependency_graph_complete(&self) -> bool {
        self.dependency_graph_state.is_complete()
    }
    pub fn dependency_blockers(&self) -> &[SemanticDependencyBlocker] {
        self.dependency_graph_state.blockers()
    }
    pub fn definition_universe_complete(&self) -> bool {
        matches!(
            self.definition_universe_state,
            SemanticDefinitionUniverseState::Complete
        )
    }
    #[cfg(test)]
    pub(crate) fn work(&self) -> SemanticDependencyManifestWork {
        self.work
    }
    /// Return owned counters for unstable dependency-manifest instrumentation.
    pub fn unstable_metrics(&self) -> crate::unstable::DependencyManifestMetrics {
        crate::unstable::DependencyManifestMetrics::from_work(self.work)
    }

    fn surface_complete(&self, surface: SemanticDependencySurface) -> bool {
        self.dependency_graph_state.surface_complete(surface)
    }
}

/// A reason why semantic results cannot soundly be reused across two manifests.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticFullInvalidationReason {
    RootChanged,
    ModuleImportsChanged,
    TargetChanged,
    PreviewFeaturesChanged,
    IncompleteDefinitionUniverse,
    IncompleteDependencyGraph(Arc<[SemanticDependencyBlocker]>),
}

/// Explicit work performed while planning semantic invalidation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticInvalidationWork {
    pub definition_fingerprints_compared: usize,
    pub dependency_edges_visited: usize,
    pub reverse_closure_nodes_visited: usize,
    pub extra_rir_instructions_visited: usize,
}

/// Immutable, stable-keyed invalidation decision for two semantic input manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticInvalidationScope {
    Full {
        reasons: Arc<[SemanticFullInvalidationReason]>,
    },
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticInvalidationPlan {
    scope: SemanticInvalidationScope,
    added: Arc<[StableDefinitionKey]>,
    removed: Arc<[StableDefinitionKey]>,
    changed: Arc<[StableDefinitionKey]>,
    invalidated: Arc<[StableDefinitionKey]>,
    reusable: Arc<[StableDefinitionKey]>,
    work: SemanticInvalidationWork,
}

impl SemanticInvalidationPlan {
    pub fn scope(&self) -> &SemanticInvalidationScope {
        &self.scope
    }
    pub fn added(&self) -> &[StableDefinitionKey] {
        &self.added
    }
    pub fn removed(&self) -> &[StableDefinitionKey] {
        &self.removed
    }
    pub fn changed(&self) -> &[StableDefinitionKey] {
        &self.changed
    }
    pub fn invalidated(&self) -> &[StableDefinitionKey] {
        &self.invalidated
    }
    pub fn reusable(&self) -> &[StableDefinitionKey] {
        &self.reusable
    }
    pub fn work(&self) -> SemanticInvalidationWork {
        self.work
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
    accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
    graph: Option<Arc<CanonicalImportGraphOutput>>,
    diagnostics: CompileErrors,
    diagnostic_snapshot: Option<Arc<FrontendDiagnosticSnapshot>>,
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
    pub(crate) fn accepted_read_manifest(&self) -> &[crate::AcceptedReadManifestEntry] {
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
    /// Protocol context only while the typed import-closure query is open.
    /// Closed attempts live exclusively in their plan or closure terminal.
    open_discovery: Option<Arc<ImportDiscoveryRevisionArtifact>>,
    queries: FrontendQueryDatabase,
    #[cfg(test)]
    supplied_test_import_graph: Option<CanonicalImportGraph>,
    #[cfg(test)]
    cancel_merge_before_commit: bool,
    #[cfg(test)]
    cancel_semantic_after_dependency: bool,
    #[cfg(test)]
    cancel_semantic_before_publication: bool,
    published: Option<Arc<ParsedProgram>>,
    published_snapshot: Option<SourceSnapshot>,
    batch_diagnostic_order: Option<Vec<crate::ModuleId>>,
    definition_shard_baseline: Option<crate::DefinitionSnapshot>,
    metrics: CompilerSessionMetrics,
    diagnostics: DiagnosticAttemptStore,
    #[cfg(test)]
    durable_declaration_cache: Option<DurableDeclarationCache>,
    #[cfg(test)]
    last_successful_cfg_cache: Option<Arc<[crate::queries::DurableCfgArtifact]>>,
}

/// Producer-nominal projection (ADR-0066): an anonymous member's owner is
/// projected onto a canonical *anchor* spelling of its own producer. There is
/// no cross-producer structural equivalence class and no stable-minimum
/// representative across producers: two anonymous types are reconciled here only
/// when they share the exact same producer, kind, and canonical arguments and
/// differ solely in their definition-relative structural anchor (the
/// anchor-prefix projection a body reached under different contexts can
/// produce). Distinct producers or distinct specialization arguments — including
/// every `Wrapper(n)` in an unbounded recursion — are never merged, so genuine
/// specialization runaway still accrues nesting depth and is diagnosed by the
/// depth budget rather than collapsing onto a shared representative.
fn canonicalize_reached_anonymous_member(
    instance: &crate::FunctionInstanceKey,
    produced: &BTreeMap<
        crate::AnonymousNominalKey,
        crate::durable_semantics::DurableAnonymousNominal,
    >,
    declarations: &[crate::durable_semantics::DurableAnonymousNominal],
) -> crate::FunctionInstanceKey {
    let crate::FunctionInstanceKey::AnonymousMember { owner, member } = instance else {
        return instance.clone();
    };
    let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity)) =
        owner.as_ref()
    else {
        return instance.clone();
    };
    // Fail closed: only reconcile when this exact producer identity has been
    // observed. When it has not, keep the instance untouched rather than
    // guessing a representative (a wrong guess would silently miscompile).
    if produced.get(identity).is_none()
        && !declarations
            .iter()
            .any(|nominal| nominal.identity == *identity)
    {
        return instance.clone();
    }
    // Reconcile only exact-producer anchor variants: same producer, kind, and
    // canonical arguments, differing solely in their definition-relative
    // structural anchor. This is anchor-prefix projection, NOT structural
    // equivalence — fields, variants, methods, and captured values never enter,
    // and two distinct producers (including every `Wrapper(n)` in an unbounded
    // recursion) are never merged, so genuine specialization runaway still
    // accrues nesting depth for the depth budget (ADR-0066).
    let representative = produced
        .values()
        .map(|nominal| &nominal.identity)
        .chain(declarations.iter().map(|nominal| &nominal.identity))
        .chain(std::iter::once(identity))
        .filter(|candidate| {
            candidate.kind == identity.kind
                && candidate.producer == identity.producer
                && candidate.arguments == identity.arguments
        })
        .min_by(|left, right| stable_anonymous_nominal_cmp(left, right))
        .expect("the target identity is always a candidate")
        .clone();
    crate::FunctionInstanceKey::AnonymousMember {
        owner: Box::new(crate::TypeInstanceKey::Nominal(
            crate::NominalInstanceKey::Anonymous(representative),
        )),
        member: member.clone(),
    }
}

fn stable_anonymous_nominal_cmp(
    left: &crate::AnonymousNominalKey,
    right: &crate::AnonymousNominalKey,
) -> std::cmp::Ordering {
    stable_producer_definition_root(&left.producer)
        .cmp(&stable_producer_definition_root(&right.producer))
        .then_with(|| left.cmp(right))
}

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

fn stable_producer_definition_root(
    producer: &crate::StableProducerId,
) -> Option<&crate::StableDefinitionKey> {
    match producer {
        crate::StableProducerId::Definition(definition) => Some(definition),
        crate::StableProducerId::Function(function) => stable_function_definition_root(function),
    }
}

fn produced_anonymous_survives_closure_restart(
    instance: &crate::FunctionInstanceKey,
    produced: &BTreeMap<
        crate::AnonymousNominalKey,
        crate::durable_semantics::DurableAnonymousNominal,
    >,
    declarations: &[crate::durable_semantics::DurableAnonymousNominal],
) -> bool {
    canonicalize_reached_anonymous_member(instance, produced, declarations) == *instance
}

fn enqueue_owned_destructor_closure(
    ty: &crate::TypeInstanceKey,
    definitions: &crate::BoundDefinitionSet,
    declarations: &[crate::durable_semantics::DurableDeclarationSemantic],
    produced_anonymous: &BTreeMap<
        crate::AnonymousNominalKey,
        crate::durable_semantics::DurableAnonymousNominal,
    >,
    declaration_anonymous: &[crate::durable_semantics::DurableAnonymousNominal],
    pending: &mut std::collections::BTreeSet<crate::FunctionInstanceKey>,
) {
    fn named(
        owner: &crate::StableDefinitionKey,
        definitions: &crate::BoundDefinitionSet,
        declarations: &[crate::durable_semantics::DurableDeclarationSemantic],
        produced_anonymous: &BTreeMap<
            crate::AnonymousNominalKey,
            crate::durable_semantics::DurableAnonymousNominal,
        >,
        declaration_anonymous: &[crate::durable_semantics::DurableAnonymousNominal],
        pending: &mut std::collections::BTreeSet<crate::FunctionInstanceKey>,
        visited: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
    ) {
        let identity =
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(owner.clone()));
        if !visited.insert(identity) {
            return;
        }
        for record in definitions.definitions() {
            let candidate = record.stable_key();
            let Some(candidate_owner) = candidate.owner() else {
                continue;
            };
            if candidate.kind() == crate::StableDefinitionKind::Destructor
                && candidate_owner.module() == owner.module()
                && candidate_owner.kind() == owner.kind()
                && candidate_owner.name() == owner.name()
            {
                pending.insert(crate::FunctionInstanceKey::Definition(candidate.clone()));
            }
        }
        let Some(declaration) = declarations
            .iter()
            .find(|declaration| declaration.key == *owner)
        else {
            return;
        };
        match &declaration.payload {
            crate::durable_semantics::DurableDeclarationPayload::Struct { fields, .. } => {
                for (_, field) in fields.iter() {
                    durable(
                        field,
                        definitions,
                        declarations,
                        produced_anonymous,
                        declaration_anonymous,
                        pending,
                        visited,
                    );
                }
            }
            crate::durable_semantics::DurableDeclarationPayload::Enum { variants } => {
                for (_, fields) in variants.iter() {
                    for field in fields.iter() {
                        durable(
                            field,
                            definitions,
                            declarations,
                            produced_anonymous,
                            declaration_anonymous,
                            pending,
                            visited,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn anonymous(
        owner: &crate::AnonymousNominalKey,
        definitions: &crate::BoundDefinitionSet,
        declarations: &[crate::durable_semantics::DurableDeclarationSemantic],
        produced_anonymous: &BTreeMap<
            crate::AnonymousNominalKey,
            crate::durable_semantics::DurableAnonymousNominal,
        >,
        declaration_anonymous: &[crate::durable_semantics::DurableAnonymousNominal],
        pending: &mut std::collections::BTreeSet<crate::FunctionInstanceKey>,
        visited: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
    ) {
        let identity =
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(owner.clone()));
        if !visited.insert(identity.clone()) {
            return;
        }
        let Some(nominal) = produced_anonymous.get(owner).or_else(|| {
            declaration_anonymous
                .iter()
                .find(|nominal| nominal.identity == *owner)
        }) else {
            return;
        };
        match &nominal.shape {
            crate::durable_semantics::DurableAnonymousNominalShape::Struct { fields, methods } => {
                if methods
                    .iter()
                    .any(|method| method.has_self && method.name.as_ref() == "__drop")
                {
                    pending.insert(crate::FunctionInstanceKey::AnonymousMember {
                        owner: Box::new(identity),
                        member: crate::AnonymousMemberKey {
                            kind: crate::AnonymousMemberKind::Destructor,
                            name: Arc::from("__drop"),
                        },
                    });
                }
                for (_, field) in fields.iter() {
                    durable(
                        field,
                        definitions,
                        declarations,
                        produced_anonymous,
                        declaration_anonymous,
                        pending,
                        visited,
                    );
                }
            }
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { variants } => {
                for (_, fields) in variants.iter() {
                    for field in fields.iter() {
                        durable(
                            field,
                            definitions,
                            declarations,
                            produced_anonymous,
                            declaration_anonymous,
                            pending,
                            visited,
                        );
                    }
                }
            }
        }
    }

    fn durable(
        ty: &crate::durable_semantics::DurableType,
        definitions: &crate::BoundDefinitionSet,
        declarations: &[crate::durable_semantics::DurableDeclarationSemantic],
        produced_anonymous: &BTreeMap<
            crate::AnonymousNominalKey,
            crate::durable_semantics::DurableAnonymousNominal,
        >,
        declaration_anonymous: &[crate::durable_semantics::DurableAnonymousNominal],
        pending: &mut std::collections::BTreeSet<crate::FunctionInstanceKey>,
        visited: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
    ) {
        match ty {
            crate::durable_semantics::DurableType::Nominal(owner) => named(
                owner,
                definitions,
                declarations,
                produced_anonymous,
                declaration_anonymous,
                pending,
                visited,
            ),
            crate::durable_semantics::DurableType::AnonymousNominal(owner) => anonymous(
                owner,
                definitions,
                declarations,
                produced_anonymous,
                declaration_anonymous,
                pending,
                visited,
            ),
            crate::durable_semantics::DurableType::Array { element, .. } => durable(
                element,
                definitions,
                declarations,
                produced_anonymous,
                declaration_anonymous,
                pending,
                visited,
            ),
            _ => {}
        }
    }

    fn instance(
        ty: &crate::TypeInstanceKey,
        definitions: &crate::BoundDefinitionSet,
        declarations: &[crate::durable_semantics::DurableDeclarationSemantic],
        produced_anonymous: &BTreeMap<
            crate::AnonymousNominalKey,
            crate::durable_semantics::DurableAnonymousNominal,
        >,
        declaration_anonymous: &[crate::durable_semantics::DurableAnonymousNominal],
        pending: &mut std::collections::BTreeSet<crate::FunctionInstanceKey>,
        visited: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
    ) {
        match ty {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(owner)) => named(
                owner,
                definitions,
                declarations,
                produced_anonymous,
                declaration_anonymous,
                pending,
                visited,
            ),
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(owner)) => {
                anonymous(
                    owner,
                    definitions,
                    declarations,
                    produced_anonymous,
                    declaration_anonymous,
                    pending,
                    visited,
                );
            }
            crate::TypeInstanceKey::Array { element, .. } => instance(
                element,
                definitions,
                declarations,
                produced_anonymous,
                declaration_anonymous,
                pending,
                visited,
            ),
            _ => {}
        }
    }

    instance(
        ty,
        definitions,
        declarations,
        produced_anonymous,
        declaration_anonymous,
        pending,
        &mut std::collections::BTreeSet::new(),
    );
}

#[derive(Debug)]
enum SemanticRequestControl {
    Compile(CompileErrors),
    Abort(rue_query::QueryAbort),
}

impl From<CompileErrors> for SemanticRequestControl {
    fn from(errors: CompileErrors) -> Self {
        Self::Compile(errors)
    }
}

/// The single typed frontend query database owned by `CompilerSession`.
///
/// Query algorithms stay in the session; this value owns terminals and their
/// dependency/reverse-dependency state only.
#[derive(Debug)]
struct FrontendQueryDatabase {
    /// Canonical Phase 1 execution substrate. The legacy stores below are
    /// removed family-by-family as callers move through its selected-state shim.
    revisioned: crate::revisioned_query_database::RevisionedQueryDatabase,
    // RUE-1033 COMPATIBILITY: these selected-state plan/closure records retain
    // legacy diagnostic attempts only. Syntax, name lookup, exact occurrence
    // resolution, and module lowering are authoritative runtime families.
    import_plans: TypedQueryStore<ImportPlanQuery>,
    import_closures: TypedQueryStore<ImportClosureQuery>,
    import_diagnostics: TypedQueryStore<ImportDiagnosticQuery>,
    merge: TypedQueryStore<MergeQuery>,
    rir: TypedQueryStore<RirQuery>,
    semantic: TypedQueryStore<SemanticQuery>,
    definitions: TypedQueryStore<DefinitionQuery>,
    manifests: TypedQueryStore<DependencyManifestQuery>,
    invalidation_plans: TypedQueryStore<InvalidationPlanQuery>,
    graph: crate::query_graph::QueryGraph,
    import_plan_inputs: crate::query_graph::TypedLeafStore<ImportPlanQueryKey>,
    import_closure_inputs: crate::query_graph::TypedLeafStore<ImportClosureQueryKey>,
    source_inputs: crate::query_graph::TypedLeafStore<ExactSourceInput>,
    import_inputs: crate::query_graph::TypedLeafStore<CanonicalImportGraph>,
    target_inputs: crate::query_graph::TypedLeafStore<crate::Target>,
    preview_inputs: crate::query_graph::TypedLeafStore<StablePreviewFeatures>,
    optimization_inputs: crate::query_graph::TypedLeafStore<StableOptLevel>,
}

impl Default for FrontendQueryDatabase {
    fn default() -> Self {
        Self {
            revisioned: crate::revisioned_query_database::RevisionedQueryDatabase::default(),
            import_plans: TypedQueryStore::default(),
            import_closures: TypedQueryStore::default(),
            import_diagnostics: TypedQueryStore::default(),
            merge: TypedQueryStore::default(),
            rir: TypedQueryStore::default(),
            semantic: TypedQueryStore::default(),
            definitions: TypedQueryStore::default(),
            manifests: TypedQueryStore::default(),
            invalidation_plans: TypedQueryStore::default(),
            graph: crate::query_graph::QueryGraph::default(),
            import_plan_inputs: crate::query_graph::TypedLeafStore::new(
                QUERY_TERMINAL_RETENTION_LIMIT,
            ),
            import_closure_inputs: crate::query_graph::TypedLeafStore::new(
                QUERY_TERMINAL_RETENTION_LIMIT,
            ),
            source_inputs: crate::query_graph::TypedLeafStore::new(QUERY_TERMINAL_RETENTION_LIMIT),
            import_inputs: crate::query_graph::TypedLeafStore::new(QUERY_TERMINAL_RETENTION_LIMIT),
            target_inputs: crate::query_graph::TypedLeafStore::new(QUERY_TERMINAL_RETENTION_LIMIT),
            preview_inputs: crate::query_graph::TypedLeafStore::new(QUERY_TERMINAL_RETENTION_LIMIT),
            optimization_inputs: crate::query_graph::TypedLeafStore::new(
                QUERY_TERMINAL_RETENTION_LIMIT,
            ),
        }
    }
}

impl FrontendQueryDatabase {
    fn publish_source(&mut self, source: ExactSourceInput) -> bool {
        let previous = self.source_inputs.selected(&self.graph);
        let current = self.source_inputs.publish(&mut self.graph, source);
        previous.is_some_and(|previous| previous != current)
    }

    fn publish_import_graph(&mut self, imports: CanonicalImportGraph) {
        self.import_inputs.publish(&mut self.graph, imports);
    }

    fn publish_request_inputs(&mut self, options: &CompileOptions) {
        self.target_inputs
            .publish_retained(&mut self.graph, options.target);
        self.preview_inputs.publish_retained(
            &mut self.graph,
            StablePreviewFeatures::new(&options.preview_features),
        );
        self.optimization_inputs
            .publish_retained(&mut self.graph, options.opt_level.into());
    }
}

#[derive(Debug, Clone)]
struct DurableDeclarationCache {
    semantics: Arc<[DurableDeclarationSemantic]>,
}

#[derive(Debug, Clone)]
struct InvalidationPlanQueryKey {
    previous: Arc<SemanticDependencyInputManifest>,
    current: Arc<SemanticDependencyInputManifest>,
}

impl PartialEq for InvalidationPlanQueryKey {
    fn eq(&self, other: &Self) -> bool {
        (Arc::ptr_eq(&self.previous, &other.previous) || self.previous == other.previous)
            && (Arc::ptr_eq(&self.current, &other.current) || self.current == other.current)
    }
}

impl Eq for InvalidationPlanQueryKey {}

impl std::hash::Hash for InvalidationPlanQueryKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // `Eq` compares the full manifest contents, so any subset of those
        // fields is a valid hash source: equal keys necessarily agree on the
        // discriminant, and hash collisions between distinct keys resolve
        // through exact `Eq` in the memo map. A cheap length discriminant keeps
        // this low-cardinality control family off a full-manifest hash.
        self.previous.hash_discriminant(state);
        self.current.hash_discriminant(state);
    }
}

#[derive(Debug, Clone)]
struct InvalidationPlanCacheEntry {
    key: InvalidationPlanQueryKey,
    plan: Arc<SemanticInvalidationPlan>,
}

#[derive(Debug, Clone)]
struct DependencyManifestCacheEntry {
    key: DependencyManifestQueryKey,
    result: Result<Arc<SemanticDependencyInputManifest>, CompileErrors>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ParseQueryKey {
    source: ExactSourceInput,
    /// Caller-owned source table order. This is presentation state rather than
    /// module identity, but a selected parse record retains the exact snapshot
    /// and diagnostic source table, so order changes must reproject the outer
    /// record while granular module terminals remain reusable.
    file_order: Arc<[crate::FileId]>,
    presentation: DiagnosticAttemptProvenance,
}

impl ParseQueryKey {
    pub(crate) fn source(&self) -> &ExactSourceInput {
        &self.source
    }
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
        record.snapshot.source_revision() == &record.key.source.revision
            && record.snapshot.metadata() == &record.key.source.metadata
            && record
                .snapshot
                .files()
                .map(|source| source.file_id)
                .eq(record.key.file_order.iter().copied())
            && match &record.result {
                Ok(program) => program.source_revision() == &record.key.source.revision,
                Err(_) => true,
            }
            && record.diagnostics.source_revision() == &record.key.source.revision
            && record.diagnostics.identity() == &FrontendDiagnosticIdentity::Syntax
            && record.diagnostics.provenance == record.key.presentation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ImportPlanQueryKey {
    source: ExactSourceInput,
    context: crate::ImportDiscoveryContext,
    policy_version: u32,
    accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
    carried_ledger: crate::ImportObservationLedger,
}

#[derive(Debug, Clone)]
struct ImportPlanQueryRecord {
    key: ImportPlanQueryKey,
    result: Result<crate::ImportDiscoveryPlan, CompileErrors>,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
    attempted_artifact: Option<Arc<ImportDiscoveryRevisionArtifact>>,
}

#[derive(Debug)]
struct ImportPlanQuery;

impl TypedQueryFamily for ImportPlanQuery {
    type Key = ImportPlanQueryKey;
    type Record = ImportPlanQueryRecord;
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
        left.result == right.result
    }

    fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
        diagnostic_batches_equal(&left.diagnostics, &right.diagnostics)
    }

    fn diagnostics(record: &Self::Record) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        Some(&record.diagnostics)
    }

    fn record_is_consistent(record: &Self::Record) -> bool {
        record.diagnostics.source_revision() == &record.key.source.revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ImportClosureQueryKey {
    source: ExactSourceInput,
    context: crate::ImportDiscoveryContext,
    policy_version: u32,
    accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
    plan: crate::ImportDiscoveryPlan,
    ledger: crate::ImportObservationLedger,
}

#[derive(Debug, Clone)]
struct ImportClosureQueryRecord {
    key: ImportClosureQueryKey,
    result: Result<Arc<CanonicalImportGraphOutput>, CompileErrors>,
    artifact: Arc<ImportDiscoveryRevisionArtifact>,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

#[derive(Debug)]
struct ImportClosureQuery;

impl TypedQueryFamily for ImportClosureQuery {
    type Key = ImportClosureQueryKey;
    type Record = ImportClosureQueryRecord;
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
            (Ok(left), Ok(right)) => left.input() == right.input() && left.graph() == right.graph(),
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
        record.artifact.snapshot().source_revision() == &record.key.source.revision
            && record.diagnostics.source_revision() == &record.key.source.revision
            && match &record.result {
                Ok(graph) => graph.input().sources == record.key.source.revision,
                Err(_) => true,
            }
    }
}

#[derive(Debug, Clone)]
struct MergeCacheEntry {
    key: MergeQueryKey,
    result: Result<Arc<CanonicalMergedProgram>, CompileErrors>,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

#[derive(Debug, Clone)]
struct RirCacheEntry {
    key: RirQueryKey,
    result: Result<Arc<CanonicalRirOutput>, CompileErrors>,
    merged: Option<Arc<CanonicalMergedProgram>>,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MergeQueryKey {
    source: ExactSourceInput,
    presentation: Option<Arc<[crate::ModuleId]>>,
}

#[derive(Debug)]
struct MergeQuery;

fn compile_errors_equal(left: &CompileErrors, right: &CompileErrors) -> bool {
    left.iter().eq(right.iter())
}

fn query_control_error<T>(error: crate::typed_query_store::BeginSelectedError) -> T {
    std::panic::panic_any(error)
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

impl TypedQueryFamily for MergeQuery {
    type Key = MergeQueryKey;
    type Record = MergeCacheEntry;
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
            // Exact source bytes and metadata plus presentation order are the
            // complete deterministic merge input. Work counters and compact
            // allocation identities are intentionally not part of equality.
            (Ok(_), Ok(_)) => left.key == right.key,
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
        let artifact_matches = match &record.result {
            Ok(merged) => merged.ast().source_revision() == &record.key.source.revision,
            Err(_) => true,
        };
        artifact_matches
            && record.diagnostics.source_revision() == &record.key.source.revision
            && record.diagnostics.identity() == &FrontendDiagnosticIdentity::Merge
    }
}

impl TypedSecondaryLookupFamily for MergeQuery {
    type SecondaryKey = SourceRevision;

    fn matches_secondary(record: &Self::Record, key: &Self::SecondaryKey) -> bool {
        &record.key.source.revision == key
    }
}

impl TypedEquivalentLookupFamily for MergeQuery {
    fn rekey_equivalent(record: &Self::Record, key: Self::Key) -> Option<Self::Record> {
        if record.result.is_err() || !record.diagnostics.is_success() {
            return None;
        }
        let mut record = record.clone();
        record.key = key;
        Some(record)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RirQueryKey {
    source: SourceRevision,
}

#[derive(Debug)]
struct RirQuery;

impl TypedQueryFamily for RirQuery {
    type Key = RirQueryKey;
    type Record = RirCacheEntry;
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
            (Ok(left), Ok(right)) => Arc::ptr_eq(left, right) || left.structurally_eq(right),
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
        (match &record.result {
            Ok(output) => output.source_revision() == &record.key.source,
            Err(_) => true,
        }) && record.diagnostics.source_revision() == &record.key.source
            && record.diagnostics.identity()
                == &FrontendDiagnosticIdentity::Rir(record.key.source.clone())
            && record
                .merged
                .as_ref()
                .is_none_or(|merged| merged.ast().source_revision() == &record.key.source)
    }
}

#[derive(Debug, Clone)]
struct DirectImportDiagnosticCacheEntry {
    key: ImportDiagnosticInputDescriptor,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
}

#[derive(Debug)]
struct ImportDiagnosticQuery;

impl TypedQueryFamily for ImportDiagnosticQuery {
    type Key = ImportDiagnosticInputDescriptor;
    type Record = DirectImportDiagnosticCacheEntry;
    const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

    fn key(record: &Self::Record) -> &Self::Key {
        &record.key
    }

    fn terminal_kind(record: &Self::Record) -> TerminalKind {
        if record.diagnostics.is_success() {
            TerminalKind::Success
        } else {
            TerminalKind::Failure
        }
    }

    fn outcome_equal(_left: &Self::Record, _right: &Self::Record) -> bool {
        // This projection's value is unit; its entire observable answer is the
        // attached diagnostic batch.
        true
    }

    fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
        diagnostic_batches_equal(&left.diagnostics, &right.diagnostics)
    }

    fn diagnostics(record: &Self::Record) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        Some(&record.diagnostics)
    }

    fn record_is_consistent(record: &Self::Record) -> bool {
        record.diagnostics.source_revision() == record.key.source_revision()
            && record.diagnostics.identity()
                == &FrontendDiagnosticIdentity::Import(record.key.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SemanticQueryKey {
    input: CodegenInputDescriptor,
    imports: CanonicalImportGraph,
}

/// Complete semantic binding identity shared by every optimization variant.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticBindingLookupKey {
    input: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
}

#[derive(Debug, Clone)]
struct SemanticCacheEntry {
    key: SemanticQueryKey,
    result: Result<Arc<CanonicalSemanticOutput>, CompileErrors>,
    rir: Option<Arc<CanonicalRirOutput>>,
    diagnostics: Arc<FrontendDiagnosticSnapshot>,
    durable_declaration_cache: Option<DurableDeclarationCache>,
    successful_cfg_cache: Option<Arc<[crate::queries::DurableCfgArtifact]>>,
    oracle_injected: bool,
}

#[derive(Debug)]
struct SemanticQuery;

impl TypedQueryFamily for SemanticQuery {
    type Key = SemanticQueryKey;
    type Record = SemanticCacheEntry;
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
            // Semantic analysis is deterministic over the complete typed key.
            // This is an exhaustive equality proof and excludes request-local
            // compact indices, allocation identity, and work observations.
            (Ok(_), Ok(_)) => left.key == right.key,
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
        let artifact_matches = match &record.result {
            Ok(output) => output.input() == &record.key.input,
            Err(_) => true,
        };
        let expected_stage = FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(
            &record.key.input,
            record.key.imports.clone(),
        ));
        record.key.input.semantic.sources.root() == record.key.imports.root()
            && (artifact_matches || record.oracle_injected)
            && record.diagnostics.source_revision() == &record.key.input.semantic.sources
            && record.diagnostics.identity() == &expected_stage
    }
}

impl TypedSecondaryLookupFamily for SemanticQuery {
    type SecondaryKey = SemanticBindingLookupKey;

    fn matches_secondary(record: &Self::Record, key: &Self::SecondaryKey) -> bool {
        record.result.is_ok()
            && record.key.input.semantic == key.input
            && record.key.imports == key.imports
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DefinitionQueryKey {
    input: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
}

#[derive(Debug, Clone)]
struct DefinitionCacheEntry {
    key: DefinitionQueryKey,
    output: DefinitionQueryOutput,
}

/// Immutable output produced at the stable-definition computation boundary.
///
/// The provenance is reconstructed from the arguments actually passed to the
/// phase rather than supplied by the cache publisher. This lets the typed store
/// reject publication under a different query key.
#[derive(Debug, Clone)]
struct DefinitionQueryOutput {
    provenance: DefinitionQueryKey,
    result: Result<Arc<BoundDefinitionSet>, CompileErrors>,
}

#[derive(Debug)]
struct DefinitionComputation {
    output: DefinitionQueryOutput,
    binding: DeclarationBindingWork,
    manifest: SemanticBindingManifestWork,
    issuance: BoundDefinitionWork,
}

#[derive(Debug)]
struct DefinitionQuery;

impl TypedQueryFamily for DefinitionQuery {
    type Key = DefinitionQueryKey;
    type Record = DefinitionCacheEntry;
    const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

    fn key(record: &Self::Record) -> &Self::Key {
        &record.key
    }

    fn terminal_kind(record: &Self::Record) -> TerminalKind {
        if record.output.result.is_ok() {
            TerminalKind::Success
        } else {
            TerminalKind::Failure
        }
    }

    fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
        match (&left.output.result, &right.output.result) {
            (Ok(left), Ok(right)) => Arc::ptr_eq(left, right) || left.structurally_eq(right),
            (Err(left), Err(right)) => compile_errors_equal(left, right),
            _ => false,
        }
    }

    fn diagnostics_equal(_left: &Self::Record, _right: &Self::Record) -> bool {
        true
    }

    fn record_is_consistent(record: &Self::Record) -> bool {
        record.key == record.output.provenance
            && record.key.input.sources.root() == record.key.imports.root()
            && match &record.output.result {
                Ok(definitions) => definitions.source_revision() == &record.key.input.sources,
                Err(_) => true,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DependencyManifestQueryKey {
    input: SemanticInputDescriptor,
    imports: CanonicalImportGraph,
}

#[derive(Debug)]
struct DependencyManifestQuery;

impl TypedQueryFamily for DependencyManifestQuery {
    type Key = DependencyManifestQueryKey;
    type Record = DependencyManifestCacheEntry;
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
            (Ok(left), Ok(right)) => Arc::ptr_eq(left, right) || left == right,
            (Err(left), Err(right)) => compile_errors_equal(left, right),
            _ => false,
        }
    }

    fn diagnostics_equal(_left: &Self::Record, _right: &Self::Record) -> bool {
        true
    }

    fn record_is_consistent(record: &Self::Record) -> bool {
        match &record.result {
            Ok(manifest) => {
                manifest.input == record.key.input && manifest.imports == record.key.imports
            }
            Err(_) => true,
        }
    }
}

#[derive(Debug)]
struct InvalidationPlanQuery;

impl TypedQueryFamily for InvalidationPlanQuery {
    type Key = InvalidationPlanQueryKey;
    type Record = InvalidationPlanCacheEntry;
    const MAX_TERMINALS: usize = FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT;

    fn key(record: &Self::Record) -> &Self::Key {
        &record.key
    }

    fn terminal_kind(_record: &Self::Record) -> TerminalKind {
        TerminalKind::Success
    }

    fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
        Arc::ptr_eq(&left.plan, &right.plan) || left.plan == right.plan
    }

    fn diagnostics_equal(_left: &Self::Record, _right: &Self::Record) -> bool {
        true
    }

    fn record_is_consistent(_record: &Self::Record) -> bool {
        true
    }
}

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
session_query_metrics_family!(
    DependencyManifestQuery,
    "dependency-manifests",
    dependency_manifests
);
session_query_metrics_family!(
    InvalidationPlanQuery,
    "invalidation-plans",
    invalidation_plans
);

/// Explicit compiler inputs read by a terminal attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExactSourceInput {
    revision: SourceRevision,
    metadata: crate::SourceMetadata,
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
    let provenance = DefinitionQueryKey {
        input: SemanticInputDescriptor::new(
            merged.definitions().source_snapshot(),
            options.target,
            &options.preview_features,
        ),
        imports: imports.clone(),
    };
    // A semantic terminal normally belongs to this exact source descriptor.
    // Re-stamp the projection at this query boundary so typed fault injection
    // remains a detectable stale value rather than publishing an internally
    // inconsistent definition terminal.
    let definitions = semantic
        .body_owner_issuer()
        .projected_for_source_revision(merged.ast().source_revision());
    let work = semantic.work();
    let binding = work.binding;
    let manifest = work.manifest;
    let issuance = definitions.work();
    let result = Ok(Arc::new(definitions));
    DefinitionComputation {
        output: DefinitionQueryOutput { provenance, result },
        binding,
        manifest,
        issuance,
    }
}

impl CompilerSession {
    /// Corrupt actual retained/selectable query state for the differential
    /// oracle. This is deliberately typed and narrow; production callers have
    /// no reason to use it.
    #[doc(hidden)]
    pub(crate) fn inject_stale_query_for_oracle(
        &mut self,
        fault: crate::unstable::DifferentialOracleFault,
    ) -> bool {
        match fault {
            crate::unstable::DifferentialOracleFault::Semantic => {
                let records = self.queries.semantic.records().cloned().collect::<Vec<_>>();
                let Some(current) = records.last().cloned() else {
                    return false;
                };
                let Some(stale) = records
                    .iter()
                    .rev()
                    .skip(1)
                    .find(|record| record.result.is_ok() && record.key != current.key)
                else {
                    return false;
                };
                let mut injected = current;
                injected.result = stale.result.clone();
                injected.rir = stale.rir.clone();
                injected.durable_declaration_cache = stale.durable_declaration_cache.clone();
                injected.successful_cfg_cache = stale.successful_cfg_cache.clone();
                injected.oracle_injected = true;
                self.queries.semantic.insert_with_dependencies(
                    &mut self.queries.graph,
                    injected,
                    [],
                );
                true
            }
            crate::unstable::DifferentialOracleFault::Diagnostic => {
                let Some(latest) = self.diagnostics.latest().cloned() else {
                    return false;
                };
                let stale = self
                    .queries
                    .semantic
                    .records()
                    .map(|record| record.diagnostics.clone())
                    .find(|diagnostics| {
                        !Arc::ptr_eq(diagnostics, &latest)
                            && !diagnostic_batches_equal(diagnostics, &latest)
                    });
                let Some(stale) = stale else {
                    return false;
                };
                self.diagnostics.select_snapshot(&stale);
                true
            }
            crate::unstable::DifferentialOracleFault::Import => {
                let Some(current) = self
                    .queries
                    .import_closures
                    .selected_record(&self.queries.graph)
                    .cloned()
                else {
                    return false;
                };
                let stale = self
                    .queries
                    .import_closures
                    .records()
                    .find(|record| {
                        record.key != current.key
                            && record.artifact.source_revision()
                                != current.artifact.source_revision()
                    })
                    .map(|record| record.key.clone());
                let Some(stale) = stale else {
                    return false;
                };
                self.queries
                    .import_closures
                    .select(&mut self.queries.graph, stale);
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
        let reason = if let Some(reason) = payload
            .downcast_ref::<crate::typed_query_store::BeginSelectedError>()
            .copied()
        {
            match reason {
                crate::typed_query_store::BeginSelectedError::DuplicateInFlight => {
                    AbortedQueryReason::DuplicateInFlight
                }
                crate::typed_query_store::BeginSelectedError::DependencyCycle => {
                    AbortedQueryReason::DependencyCycle
                }
                crate::typed_query_store::BeginSelectedError::KeyNotSelected
                | crate::typed_query_store::BeginSelectedError::AlreadyTerminal => {
                    AbortedQueryReason::Canceled
                }
            }
        } else {
            AbortedQueryReason::Canceled
        };
        let work = guard.structural();
        let dependencies = guard.dependencies.clone();
        let diagnostics = guard.diagnostics.clone();
        let attempt = match guard.family {
            "import-diagnostics" => self.queries.import_diagnostics.computing_key().map(|key| {
                self.queries.import_diagnostics.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work.clone(),
                    dependencies.clone(),
                    diagnostics.clone(),
                )
            }),
            "merge" => self.queries.merge.computing_key().map(|key| {
                self.queries.merge.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work.clone(),
                    dependencies.clone(),
                    diagnostics.clone(),
                )
            }),
            "rir" => self.queries.rir.computing_key().map(|key| {
                self.queries.rir.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work.clone(),
                    dependencies.clone(),
                    diagnostics.clone(),
                )
            }),
            "semantic" => self.queries.semantic.computing_key().map(|key| {
                self.queries.semantic.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work.clone(),
                    dependencies.clone(),
                    diagnostics.clone(),
                )
            }),
            "definitions" => self.queries.definitions.computing_key().map(|key| {
                self.queries.definitions.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work.clone(),
                    dependencies.clone(),
                    diagnostics.clone(),
                )
            }),
            "dependency-manifests" => self.queries.manifests.computing_key().map(|key| {
                self.queries.manifests.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work.clone(),
                    dependencies.clone(),
                    diagnostics.clone(),
                )
            }),
            "invalidation-plans" => self.queries.invalidation_plans.computing_key().map(|key| {
                self.queries.invalidation_plans.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    guard.id.0,
                    reason,
                    work,
                    dependencies,
                    diagnostics,
                )
            }),
            "imports" | "parse" => None,
            family => unreachable!("unknown query guard family {family}"),
        };
        if let Some(attempt) = attempt {
            guard.bind(attempt);
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
            #[cfg(test)]
            if let Some(graph) = &self.supplied_test_import_graph {
                return Ok(graph.clone());
            }
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
            let inputs = ModuleResolutionInputs::new(
                program.root().clone(),
                program
                    .modules()
                    .iter()
                    .map(|module| crate::ModuleResolutionInput {
                        module: module.module_id().clone(),
                        physical_path: Arc::from(module.physical_path()),
                    })
                    .collect(),
            )
            .map_err(CompileErrors::from)?;
            CanonicalImportGraph::from_supplied(program.root().clone(), Vec::new(), &inputs)
                .map_err(|validation| {
                    CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(format!(
                            "invalid import-free semantic graph: {validation:?}"
                        )),
                    ))
                })?
        };
        Ok(graph)
    }
    pub fn published(&self) -> Option<crate::SyntaxView> {
        self.published.as_ref().cloned().map(crate::SyntaxView::new)
    }

    pub(crate) fn published_owner(&self) -> Option<&Arc<ParsedProgram>> {
        self.published.as_ref()
    }

    /// Explicitly supply accepted canonical records to lower-layer unit tests.
    /// Production import-bearing sessions can only consume the atomically
    /// committed discovery artifact.
    #[cfg(test)]
    pub(crate) fn adopt_test_import_graph(&mut self, graph: CanonicalImportGraph) {
        self.queries
            .revisioned
            .adopt_test_import_graph(graph.clone());
        self.supplied_test_import_graph = Some(graph);
    }
    /// Derive the pre-closure import plan for the session's current parsed
    /// revision.
    ///
    /// This is retained only as part of the RUE-1033 legacy supported import
    /// path. It is not freshness- or speculation-safe, and new consumers must
    /// use the unstable begin/frontier/publish protocol.
    pub fn import_discovery_plan(
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

    /// Begins one fresh rooted external-input request with granular immutable
    /// module source, accepted-read provenance, and observation leaves.
    /// Successor carry is available only through batch publication.
    pub(crate) fn begin_import_input_request(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
    ) -> crate::CompileResult<crate::ImportInputRevision> {
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
        accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
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
    #[cfg(test)]
    pub(crate) fn discovery_attempt(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.discovery_attempt_artifact()
    }

    pub(crate) fn discovery_attempt_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.open_discovery
            .as_ref()
            .or_else(|| {
                self.queries
                    .import_plans
                    .selected_record(&self.queries.graph)
                    .and_then(|record| record.attempted_artifact.as_ref())
            })
            .or_else(|| {
                self.queries
                    .import_closures
                    .selected_record(&self.queries.graph)
                    .map(|record| &record.artifact)
            })
    }
    #[cfg(test)]
    pub(crate) fn last_good_discovery(&self) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.last_good_discovery_artifact()
    }

    pub(crate) fn last_good_discovery_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        self.queries
            .import_closures
            .last_good_record()
            .map(|record| &record.artifact)
    }

    pub(crate) fn committed_import_discovery_artifact(
        &self,
    ) -> Option<&Arc<ImportDiscoveryRevisionArtifact>> {
        let source = self.published.as_ref()?.source_revision();
        self.queries
            .import_closures
            .selected_record(&self.queries.graph)
            .filter(|record| record.result.is_ok() && record.artifact.source_revision() == source)
            .map(|record| &record.artifact)
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
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.import_diagnostics_attempt(attempt_id, &mut guard, &mut execution, &mut origin)
        }));
        let result = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        if execution == QueryAttemptExecution::Reused && origin.is_none() {
            execution = QueryAttemptExecution::Adopted;
        }
        guard.finish(execution, origin, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        result
    }

    fn import_diagnostics_attempt(
        &mut self,
        attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        origin: &mut Option<AttemptId>,
    ) -> Result<Arc<FrontendDiagnosticSnapshot>, CompileErrors> {
        let diagnostics = if let Some(attempt) = self.discovery_attempt_artifact() {
            let diagnostics = attempt.diagnostic_snapshot.as_ref().ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "open import discovery work has no canonical diagnostic batch".into(),
                )))
            })?;
            *execution = QueryAttemptExecution::Reused;
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
                accepted_reads: Arc::from([]),
            };
            if let Some((cached, handle)) = self
                .queries
                .import_diagnostics
                .request_selected(&mut self.queries.graph, input.clone(), attempt_id.0)
                .unwrap_or_else(query_control_error)
            {
                *execution = QueryAttemptExecution::Reused;
                *origin = Some(handle.origin_attempt_id());
                self.diagnostics.select(handle.as_view());
                guard.bind(handle.as_view());
                cached.diagnostics.clone()
            } else {
                let program = self.published.as_ref().ok_or_else(no_published_program)?;
                let errors = crate::ImportDiscoveryPlan::shape_diagnostics(program);
                *execution = QueryAttemptExecution::Computed;
                guard.started();
                let diagnostics = self.publish_diagnostics(
                    &source,
                    FrontendDiagnosticIdentity::Import(input.clone()),
                    Some(&errors),
                    &[],
                );
                let source_dependency = self
                    .queries
                    .source_inputs
                    .selected(&self.queries.graph)
                    .expect("import diagnostics retain their exact source input");
                let handle = self
                    .queries
                    .import_diagnostics
                    .publish_selected(
                        &mut self.queries.graph,
                        DirectImportDiagnosticCacheEntry {
                            key: input,
                            diagnostics: diagnostics.clone(),
                        },
                        [source_dependency],
                    )
                    .unwrap_or_else(query_control_error);
                self.diagnostics.select(handle.as_view());
                guard.bind(handle.as_view());
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

    /// Parse an immutable staging snapshot without publishing it to semantic
    /// or dependency queries.
    ///
    /// The carried ledger and this operation are retained only for the
    /// RUE-1033 legacy compatibility boundary. They are not freshness- or
    /// speculation-safe; new consumers must use the unstable canonical
    /// begin/frontier/publish protocol.
    pub fn stage_import_discovery(
        &mut self,
        snapshot: &SourceSnapshot,
        context: crate::ImportDiscoveryContext,
        accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
        carried_ledger: crate::ImportObservationLedger,
    ) -> Result<crate::ImportDiscoveryPlan, CompileErrors> {
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
        // From this point the selected typed plan terminal owns any closed
        // failure. Reinstall protocol context only if staging reaches Open.
        self.open_discovery = None;
        let source_revision = snapshot.source_revision().clone();
        let plan_key = ImportPlanQueryKey {
            source: ExactSourceInput::new(snapshot),
            context: context.clone(),
            policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            accepted_reads: accepted_reads.clone(),
            carried_ledger: carried_ledger.clone(),
        };
        let (plan_dependency, publish_plan) = self.select_import_plan_query(plan_key.clone());
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
            });
            self.publish_import_plan_query(
                plan_key,
                Err(errors.clone()),
                attempted_artifact
                    .diagnostic_snapshot
                    .as_ref()
                    .unwrap()
                    .clone(),
                Some(attempted_artifact),
                plan_dependency,
                publish_plan,
            );
            return Err(errors);
        }
        let (parse_result, staged_work) = self.parse_staging_snapshot(snapshot);
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
                });
                self.publish_import_plan_query(
                    plan_key,
                    Err(errors.clone()),
                    attempted_artifact
                        .diagnostic_snapshot
                        .as_ref()
                        .unwrap()
                        .clone(),
                    Some(attempted_artifact),
                    plan_dependency,
                    publish_plan,
                );
                return Err(errors);
            }
        };
        let plan = match crate::ImportDiscoveryPlan::new(&program, context.clone()) {
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
                });
                self.publish_import_plan_query(
                    plan_key,
                    Err(errors.clone()),
                    attempted_artifact
                        .diagnostic_snapshot
                        .as_ref()
                        .unwrap()
                        .clone(),
                    Some(attempted_artifact),
                    plan_dependency,
                    publish_plan,
                );
                return Err(errors);
            }
        };
        let plan_diagnostics = if publish_plan {
            let shape_diagnostics = crate::ImportDiscoveryPlan::shape_diagnostics(&program);
            self.publish_import_diagnostics(
                snapshot,
                Some(context.clone()),
                Some(plan.clone()),
                carried_ledger.clone(),
                accepted_reads.clone(),
                &shape_diagnostics,
            )
        } else {
            let diagnostics = self
                .queries
                .import_plans
                .selected_record(&self.queries.graph)
                .expect("selected terminal import plan retains diagnostics")
                .diagnostics
                .clone();
            self.reuse_diagnostics(diagnostics.clone());
            diagnostics
        };
        self.publish_import_plan_query(
            plan_key,
            Ok(plan.clone()),
            plan_diagnostics,
            None,
            plan_dependency,
            publish_plan,
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
        }));
        Ok(plan)
    }

    /// Close the current staging revision. Missing, ambiguous, and malformed
    /// imports retain a closed attempted artifact; only a diagnostic-free graph
    /// is atomically adopted as the committed compiler revision.
    ///
    /// Caller-supplied closure is retained only for the RUE-1033 legacy
    /// compatibility boundary. It is not freshness- or speculation-safe; new
    /// consumers must publish compiler-ordered canonical frontier batches.
    #[cfg(not(test))]
    pub fn close_import_discovery(
        &mut self,
        ledger: crate::ImportObservationLedger,
    ) -> Result<Arc<crate::ImportDiscoveryView>, CompileErrors> {
        self.close_import_discovery_artifact(ledger)
            .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact)))
    }

    #[cfg(test)]
    pub(crate) fn close_import_discovery(
        &mut self,
        ledger: crate::ImportObservationLedger,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
        self.close_import_discovery_artifact(ledger)
    }

    fn close_import_discovery_artifact(
        &mut self,
        ledger: crate::ImportObservationLedger,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
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
        let roots = crate::ImportDemandRoots::whole_plan(&plan);
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
                        ImportDiscoveryRevisionStatus::ClosedAttempted,
                        None,
                        &errors,
                    );
                    return Err(errors);
                }
            },
            // RUE-1033 LEGACY EMBEDDER GATE: callers that bypass the canonical
            // begin/frontier/publish protocol retain their historical plan
            // groups until that entire public boundary is removed together.
            None => plan.groups().to_vec(),
        };
        if let Err(error) =
            crate::import_discovery::validate_exact_import_ledger(&exact_groups, &ledger)
        {
            let errors = CompileErrors::from(error);
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        if !crate::import_discovery::exact_import_pending_requests(&exact_groups, &ledger)
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
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        let diagnostics =
            crate::import_discovery::exact_import_diagnostics(&program, &exact_groups, &ledger);
        if crate::import_discovery::exact_import_has_failures(&exact_groups, &ledger) {
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &diagnostics,
            );
            return Err(diagnostics);
        }

        let resolution = match ModuleResolutionInputs::new(
            program.root().clone(),
            program
                .modules()
                .iter()
                .map(|module| crate::ModuleResolutionInput {
                    module: module.module_id().clone(),
                    physical_path: Arc::from(module.physical_path()),
                })
                .collect(),
        ) {
            Ok(resolution) => resolution,
            Err(error) => {
                let errors = CompileErrors::from(error);
                self.publish_failed_import_attempt(
                    open,
                    plan,
                    ledger,
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
        let reduced = match crate::import_discovery::reduce_exact_import_graph(
            program.root().clone(),
            &exact_groups,
            &ledger,
            &open.accepted_reads,
        ) {
            Ok(graph) => graph,
            Err(error) => {
                let errors = CompileErrors::from(error);
                self.publish_failed_import_attempt(
                    open,
                    plan,
                    ledger,
                    ImportDiscoveryRevisionStatus::ClosedAttempted,
                    None,
                    &errors,
                );
                return Err(errors);
            }
        };
        let validation = validate_canonical_import_graph(&reduced, &input.resolution);
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
        );
        if let Err(errors) = adoption.into_result() {
            self.publish_failed_import_attempt(
                open,
                plan,
                ledger,
                ImportDiscoveryRevisionStatus::ClosedAttempted,
                None,
                &errors,
            );
            return Err(errors);
        }
        let (closure_key, closure_dependencies, publish_closure) =
            self.select_import_closure_query(&open, &plan, &ledger);
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
        self.publish_import_closure_query(
            closure_key,
            Ok(graph),
            artifact.clone(),
            closure_dependencies,
            publish_closure,
        );
        self.open_discovery = None;
        Ok(artifact)
    }

    fn publish_failed_import_attempt(
        &mut self,
        open: ImportDiscoveryRevisionArtifact,
        plan: crate::ImportDiscoveryPlan,
        ledger: crate::ImportObservationLedger,
        status: ImportDiscoveryRevisionStatus,
        graph: Option<Arc<CanonicalImportGraphOutput>>,
        errors: &CompileErrors,
    ) -> Arc<ImportDiscoveryRevisionArtifact> {
        debug_assert_ne!(status, ImportDiscoveryRevisionStatus::ClosedValid);
        let (closure_key, closure_dependencies, publish_closure) =
            self.select_import_closure_query(&open, &plan, &ledger);
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
        self.publish_import_closure_query(
            closure_key,
            Err(errors.clone()),
            artifact.clone(),
            closure_dependencies,
            publish_closure,
        );
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
                    DiagnosticAttemptProvenance::Presentation(order.clone().into())
                }),
            FrontendDiagnosticIdentity::Merge if errors.is_some() => self
                .batch_diagnostic_order
                .as_ref()
                .map_or(DiagnosticAttemptProvenance::Canonical, |order| {
                    DiagnosticAttemptProvenance::Presentation(order.clone().into())
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
        accepted_reads: Arc<[crate::AcceptedReadManifestEntry]>,
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

        let stores = [
            self.queries.revisioned.parse_retention(),
            self.queries.import_plans.retention(&self.queries.graph),
            self.queries.import_closures.retention(&self.queries.graph),
            self.queries
                .import_diagnostics
                .retention(&self.queries.graph),
            self.queries.merge.retention(&self.queries.graph),
            self.queries.rir.retention(&self.queries.graph),
            self.queries.semantic.retention(&self.queries.graph),
            self.queries.definitions.retention(&self.queries.graph),
            self.queries.manifests.retention(&self.queries.graph),
            self.queries
                .invalidation_plans
                .retention(&self.queries.graph),
        ];

        let mut pinned_attempts = BTreeSet::new();
        pinned_attempts.extend(self.queries.revisioned.parse.origin_attempt_ids());
        pinned_attempts.extend(self.queries.import_plans.origin_attempt_ids());
        pinned_attempts.extend(self.queries.import_closures.origin_attempt_ids());
        pinned_attempts.extend(self.queries.import_diagnostics.origin_attempt_ids());
        pinned_attempts.extend(self.queries.merge.origin_attempt_ids());
        pinned_attempts.extend(self.queries.rir.origin_attempt_ids());
        pinned_attempts.extend(self.queries.semantic.origin_attempt_ids());
        pinned_attempts.extend(self.queries.definitions.origin_attempt_ids());
        pinned_attempts.extend(self.queries.manifests.origin_attempt_ids());
        pinned_attempts.extend(self.queries.invalidation_plans.origin_attempt_ids());
        self.metrics.set_pinned_origins(pinned_attempts);

        let mut manifests = BTreeSet::new();
        for entry in self.queries.manifests.records() {
            if let Ok(manifest) = &entry.result {
                manifests.insert(Arc::as_ptr(manifest) as usize);
            }
        }
        for entry in self.queries.invalidation_plans.records() {
            manifests.insert(Arc::as_ptr(&entry.key.previous) as usize);
            manifests.insert(Arc::as_ptr(&entry.key.current) as usize);
        }
        self.metrics.set_retention(FrontendRetentionMetrics {
            retained_query_records: stores.iter().map(|store| store.retained).sum(),
            protected_query_records: stores.iter().map(|store| store.protected).sum(),
            dependency_pins: stores.iter().map(|store| store.pinned).sum(),
            validation_tombstones: stores.iter().map(|store| store.tombstones).sum(),
            graph_retained_disappeared_nodes: self.queries.graph.retained_disappeared_count(),
            query_evictions: stores.iter().map(|store| store.evictions).sum(),
            aborted_query_attempts: self.queries.revisioned.parse.retained_aborted_len()
                + self.queries.import_plans.aborted_len()
                + self.queries.import_closures.aborted_len()
                + self.queries.import_diagnostics.aborted_len()
                + self.queries.merge.aborted_len()
                + self.queries.rir.aborted_len()
                + self.queries.semantic.aborted_len()
                + self.queries.definitions.aborted_len()
                + self.queries.manifests.aborted_len()
                + self.queries.invalidation_plans.aborted_len(),
            import_query_entries: self.queries.import_diagnostics.len(),
            import_query_evictions: self.queries.import_diagnostics.evictions(),
            semantic_query_entries: self.queries.semantic.len(),
            semantic_query_evictions: self.queries.semantic.evictions(),
            definition_query_entries: self.queries.definitions.len(),
            definition_query_evictions: self.queries.definitions.evictions(),
            diagnostic_entries: diagnostics.entries,
            diagnostic_source_attempts: diagnostics.source_attempts,
            diagnostic_source_bytes: diagnostics.source_bytes,
            dependency_manifests: manifests.len(),
            invalidation_plans: self.queries.invalidation_plans.len(),
        });
    }

    pub fn update(&mut self, snapshot: &SourceSnapshot) -> CompilerSessionUpdate {
        #[cfg(test)]
        {
            self.supplied_test_import_graph = None;
        }
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
        #[cfg(test)]
        {
            self.supplied_test_import_graph = None;
        }
        self.select_diagnostic_presentation(Some(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        ));
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    fn adopt_discovery_program_for_presentation(
        &mut self,
        snapshot: &SourceSnapshot,
        _program: Arc<ParsedProgram>,
        _work: ParsedModulesWork,
    ) -> CompilerSessionUpdate {
        #[cfg(test)]
        {
            self.supplied_test_import_graph = None;
        }
        self.select_diagnostic_presentation(Some(
            snapshot
                .files()
                .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
                .collect(),
        ));
        let provenance = self.syntax_diagnostic_provenance();
        self.run_parse_update(snapshot, provenance)
    }

    fn parse_baseline(&self) -> Option<Arc<ParsedProgram>> {
        self.queries
            .revisioned
            .parse
            .last_good_record()
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
                DiagnosticAttemptProvenance::Presentation(order.clone().into())
            })
    }

    fn select_diagnostic_presentation(&mut self, order: Option<Vec<crate::ModuleId>>) {
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
        let source = ExactSourceInput::new(snapshot);
        let key = ParseQueryKey {
            source: source.clone(),
            file_order: snapshot
                .files()
                .map(|source| source.file_id)
                .collect::<Vec<_>>()
                .into(),
            presentation: presentation.clone(),
        };
        let revision = self.queries.revisioned.source_revision(&source, snapshot);
        let demanded_modules = match &presentation {
            DiagnosticAttemptProvenance::Canonical => snapshot
                .source_revision()
                .modules()
                .iter()
                .map(|source| source.module.clone())
                .collect::<Vec<_>>(),
            DiagnosticAttemptProvenance::Presentation(order) => order.to_vec(),
        };
        let (modular_result, modular_work) = self.queries.revisioned.parse_program(
            revision,
            snapshot.source_revision().root(),
            demanded_modules,
        );
        let prepared = self.queries.revisioned.parse.prepare(key.clone());
        let baseline = self.parse_baseline();
        let attempt = prepared.execute(revision, attempt_id, |context| {
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
                provenance: key.presentation.clone(),
                errors: result
                    .as_ref()
                    .err()
                    .map_or_else(|| Arc::from([]), |errors| errors.as_slice().to_vec().into()),
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
        let view = self.queries.revisioned.parse.attempt_view(
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
    ) -> (Result<Arc<ParsedProgram>, CompileErrors>, ParsedModulesWork) {
        let mut guard = self.metrics.begin_unprojected("parse");
        let order = snapshot
            .files()
            .map(|source| snapshot.module_id(source.file_id).unwrap().clone())
            .collect::<Vec<_>>();
        self.select_diagnostic_presentation(Some(order.clone()));
        let presentation = DiagnosticAttemptProvenance::Presentation(order.into());
        let attempt_id = guard.id;
        let (record, view, execution, work, _invalidation) =
            self.execute_parse_query(snapshot, presentation, attempt_id);
        guard.started();
        let result = record.result.clone();
        guard.attach_diagnostics(record.diagnostics.clone());
        guard.bind(view);
        guard.finish(execution, None, &result, QueryStructuralWork::None);
        self.metrics.synchronize();
        (result, work)
    }

    fn select_import_plan_query(
        &mut self,
        key: ImportPlanQueryKey,
    ) -> (crate::query_graph::ObservedDependency, bool) {
        let dependency = self
            .queries
            .import_plan_inputs
            .publish_retained(&mut self.queries.graph, key.clone());
        let attempt_id = self.metrics.allocate_attempt_id();
        let reused = self
            .queries
            .import_plans
            .request_selected(&mut self.queries.graph, key, attempt_id.0)
            .unwrap_or_else(query_control_error);
        if let Some((_, handle)) = &reused {
            self.diagnostics.select(handle.as_view());
        }
        let publish = reused.is_none();
        (dependency, publish)
    }

    fn publish_import_plan_query(
        &mut self,
        key: ImportPlanQueryKey,
        result: Result<crate::ImportDiscoveryPlan, CompileErrors>,
        diagnostics: Arc<FrontendDiagnosticSnapshot>,
        attempted_artifact: Option<Arc<ImportDiscoveryRevisionArtifact>>,
        dependency: crate::query_graph::ObservedDependency,
        publish: bool,
    ) {
        if publish {
            let handle = self
                .queries
                .import_plans
                .publish_selected(
                    &mut self.queries.graph,
                    ImportPlanQueryRecord {
                        key,
                        result,
                        diagnostics,
                        attempted_artifact,
                    },
                    [dependency],
                )
                .unwrap_or_else(query_control_error);
            self.diagnostics.select(handle.as_view());
        }
    }

    fn select_import_closure_query(
        &mut self,
        open: &ImportDiscoveryRevisionArtifact,
        plan: &crate::ImportDiscoveryPlan,
        ledger: &crate::ImportObservationLedger,
    ) -> (
        ImportClosureQueryKey,
        Vec<crate::query_graph::ObservedDependency>,
        bool,
    ) {
        let plan_key = ImportPlanQueryKey {
            source: ExactSourceInput::new(&open.snapshot),
            context: open.context.clone(),
            policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            accepted_reads: open.accepted_reads.clone(),
            carried_ledger: open.ledger.clone(),
        };
        let plan_dependency = self
            .queries
            .import_plans
            .handle(&plan_key)
            .expect("an open discovery revision retains its typed plan terminal")
            .observed();
        let key = ImportClosureQueryKey {
            source: plan_key.source,
            context: open.context.clone(),
            policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            accepted_reads: open.accepted_reads.clone(),
            plan: plan.clone(),
            ledger: ledger.clone(),
        };
        let input_dependency = self
            .queries
            .import_closure_inputs
            .publish_retained(&mut self.queries.graph, key.clone());
        let attempt_id = self.metrics.allocate_attempt_id();
        let reused = self
            .queries
            .import_closures
            .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
            .unwrap_or_else(query_control_error);
        if let Some((_, handle)) = &reused {
            self.diagnostics.select(handle.as_view());
        }
        let publish = reused.is_none();
        (key, vec![plan_dependency, input_dependency], publish)
    }

    fn publish_import_closure_query(
        &mut self,
        key: ImportClosureQueryKey,
        result: Result<Arc<CanonicalImportGraphOutput>, CompileErrors>,
        artifact: Arc<ImportDiscoveryRevisionArtifact>,
        dependencies: Vec<crate::query_graph::ObservedDependency>,
        publish: bool,
    ) {
        if publish {
            let diagnostics = artifact
                .diagnostic_snapshot
                .as_ref()
                .expect("closed discovery terminals retain diagnostics")
                .clone();
            let handle = self
                .queries
                .import_closures
                .publish_selected(
                    &mut self.queries.graph,
                    ImportClosureQueryRecord {
                        key,
                        result,
                        artifact,
                        diagnostics,
                    },
                    dependencies,
                )
                .unwrap_or_else(query_control_error);
            self.diagnostics.select(handle.as_view());
        }
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
                    let changed = self.queries.publish_source(ExactSourceInput::new(snapshot));
                    debug_assert!(!changed);
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
                    self.queries.publish_source(ExactSourceInput::new(snapshot));
                    self.metrics.project_dependency_invalidations(
                        &self.queries.graph,
                        downstream_invalidated,
                    );
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
        if let Ok(output) = &result {
            self.queries.publish_import_graph(output.graph().clone());
        }
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
        #[cfg(test)]
        if let Some(graph) = self.supplied_test_import_graph.as_ref() {
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
            let validation = validate_canonical_import_graph(graph, &resolution);
            *execution = QueryAttemptExecution::Reused;
            return Ok(Arc::new(CanonicalImportGraphOutput {
                input: ImportGraphInputDescriptor {
                    sources: parsed.source_revision().clone(),
                    resolution,
                    std_dir: std_dir.map(Arc::from),
                },
                graph: graph.clone(),
                validation,
            }));
        }
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
        let graph = CanonicalImportGraph::from_supplied(
            parsed.root().clone(),
            Vec::new(),
            &input.resolution,
        )
        .map_err(|validation| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                format!("invalid import-free graph: {validation:?}"),
            )))
        })?;
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
        if guard.cancel_requested {
            let key = self
                .queries
                .merge
                .computing_key()
                .expect("canceled merge retains its computing key");
            let attempt = self.queries.merge.record_aborted_attempt(
                &mut self.queries.graph,
                key,
                attempt_id.0,
                AbortedQueryReason::Canceled,
                structural.clone(),
                guard.dependencies.clone(),
                guard.diagnostics.clone(),
            );
            guard.bind(attempt);
        }
        guard.finish(execution, origin, &result, structural);
        self.metrics.synchronize();
        result
    }

    fn merge_attempt(
        &mut self,
        attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<CanonicalMergeWork>,
    ) -> Result<Arc<CanonicalMergedProgram>, CompileErrors> {
        self.require_closed_discovery()?;
        let parsed = self.published.clone().ok_or_else(no_published_program)?;
        let key = MergeQueryKey {
            source: ExactSourceInput::new(
                self.published_snapshot
                    .as_ref()
                    .expect("a published parsed program retains its exact source snapshot"),
            ),
            presentation: self.batch_diagnostic_order.clone().map(Arc::from),
        };
        if let Some((entry, handle)) = self
            .queries
            .merge
            .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
            .unwrap_or_else(query_control_error)
        {
            let result = entry.result.clone();
            let diagnostics = entry.diagnostics.clone();
            let cached_origin = handle.origin_attempt_id();
            *execution = QueryAttemptExecution::Reused;
            *origin = Some(cached_origin);
            self.diagnostics.select(handle.as_view());
            guard.bind(handle.as_view());
            guard.observe(
                self.queries
                    .graph
                    .direct_dependencies::<MergeQuery>(handle.observed().node),
            );
            guard.attach_diagnostics(diagnostics.clone());
            self.reuse_diagnostics(diagnostics);
            return result;
        }
        let source_dependency = self
            .queries
            .source_inputs
            .selected(&self.queries.graph)
            .expect("merge retains its exact source input");
        // The runtime parse terminal already validated this exact leaf. Merge
        // remains on the compatibility projection until its family migrates.
        guard.observe([source_dependency]);
        if let Some(entry) = self.queries.merge.publish_selected_equivalent(
            &mut self.queries.graph,
            key.clone(),
            &key.source.revision,
            [source_dependency],
        ) {
            *execution = QueryAttemptExecution::Reused;
            *origin = Some(
                self.queries
                    .merge
                    .handle(&key)
                    .expect("equivalent merge publication retains its origin")
                    .origin_attempt_id(),
            );
            let handle = self
                .queries
                .merge
                .handle(&key)
                .expect("equivalent merge publication retains its attempt");
            self.diagnostics.select(handle.as_view());
            guard.bind(handle.as_view());
            guard.observe([source_dependency]);
            guard.attach_diagnostics(entry.diagnostics.clone());
            self.reuse_diagnostics(entry.diagnostics.clone());
            return entry.result;
        }
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let runtime_revision = self
            .queries
            .revisioned
            .parse
            .last_good_record()
            .expect("merge has a successful parse terminal")
            .runtime_revision;
        let projected_indexes = self
            .queries
            .revisioned
            .projected_module_indexes(runtime_revision, &parsed);
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
        let merged = projected_indexes
            .and_then(|indexes| {
                merge_parsed_modules_reusing_indexes(
                    &parsed,
                    &indexes,
                    self.definition_shard_baseline.as_ref(),
                    self.batch_diagnostic_order.as_deref(),
                )
            })
            .map(Arc::new);
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
        let handle = self
            .queries
            .merge
            .publish_selected_attempt(
                &mut self.queries.graph,
                MergeCacheEntry {
                    key,
                    result: merged.clone(),
                    diagnostics,
                },
                [source_dependency],
                guard.structural(),
                *execution,
            )
            .unwrap_or_else(query_control_error);
        self.diagnostics.select(handle.as_view());
        guard.bind(handle.as_view());
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

    pub(crate) fn selected_semantic_rir_owner(&self) -> Option<Arc<CanonicalRirOutput>> {
        self.queries
            .semantic
            .selected_record(&self.queries.graph)
            .and_then(|record| record.rir.clone())
    }

    fn rir_attempt(
        &mut self,
        attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<CanonicalRirWork>,
    ) -> Result<Arc<CanonicalRirOutput>, CompileErrors> {
        self.require_successful_import_diagnostics()?;
        let source = self
            .published
            .as_ref()
            .ok_or_else(no_published_program)?
            .source_revision()
            .clone();
        let key = RirQueryKey {
            source: source.clone(),
        };
        if let Some((cached, handle)) = self
            .queries
            .rir
            .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
            .unwrap_or_else(query_control_error)
        {
            *execution = QueryAttemptExecution::Reused;
            *origin = Some(handle.origin_attempt_id());
            self.diagnostics.select(handle.as_view());
            guard.bind(handle.as_view());
            guard.observe(
                self.queries
                    .graph
                    .direct_dependencies::<RirQuery>(handle.observed().node),
            );
            guard.attach_diagnostics(cached.diagnostics.clone());
            self.reuse_diagnostics(cached.diagnostics.clone());
            return cached.result;
        }
        let merged = self.merge();
        let result = match &merged {
            Ok(merged) => {
                *execution = QueryAttemptExecution::Computed;
                guard.started();
                let revision = self
                    .queries
                    .revisioned
                    .parse
                    .last_good_record()
                    .expect("published syntax has a successful parse terminal")
                    .runtime_revision;
                let module_ids = merged
                    .ast()
                    .modules()
                    .iter()
                    .map(|module| module.module_id().clone())
                    .collect::<Vec<_>>();
                let (module_rirs, query_work) =
                    self.queries.revisioned.module_rirs(revision, module_ids);
                match module_rirs {
                    Ok(modules) => {
                        match project_module_rirs_with_work(merged, &modules, query_work) {
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
        let merge_dependency = self
            .queries
            .merge
            .selected_handle(&self.queries.graph)
            .expect("RIR query observes the current merge terminal")
            .observed();
        guard.observe([merge_dependency]);
        guard.attach_diagnostics(diagnostics.clone());
        let handle = self
            .queries
            .rir
            .publish_selected_attempt(
                &mut self.queries.graph,
                RirCacheEntry {
                    key,
                    result: result.clone(),
                    merged: merged.ok(),
                    diagnostics,
                },
                [merge_dependency],
                guard.structural(),
                *execution,
            )
            .unwrap_or_else(query_control_error);
        self.diagnostics.select(handle.as_view());
        guard.bind(handle.as_view());
        self.refresh_retention_metrics();
        result
    }

    /// Analyze the current published revision without issuing stable definition IDs.
    /// Query semantic analysis and optimized CFGs through immutable views.
    pub fn semantic(
        &mut self,
        options: &CompileOptions,
    ) -> Result<Arc<crate::SemanticView>, CompileErrors> {
        let owner = self.canonical_semantic(options)?;
        let rir = self
            .selected_semantic_rir_owner()
            .expect("successful semantic query retains its exact RIR terminal");
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
            Err(SemanticRequestControl::Abort(abort)) => {
                panic!("uncanceled semantic request aborted: {abort:?}")
            }
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
        if matches!(result, Err(SemanticRequestControl::Abort(_))) {
            guard.request_cancel();
        }
        let structural = attempt_record
            .clone()
            .map(Box::new)
            .map(QueryStructuralWork::Semantic)
            .unwrap_or(QueryStructuralWork::None);
        if guard.cancel_requested {
            if let Some(key) = self.queries.semantic.computing_key() {
                let attempt = self.queries.semantic.record_aborted_attempt(
                    &mut self.queries.graph,
                    key,
                    attempt_id.0,
                    AbortedQueryReason::Canceled,
                    structural.clone(),
                    guard.dependencies.clone(),
                    guard.diagnostics.clone(),
                );
                guard.bind(attempt);
            }
        }
        guard.finish(execution, origin, &result, structural);
        self.metrics.publish_semantic(self.queries.semantic.len());
        self.metrics.synchronize();
        result
    }

    fn semantic_attempt(
        &mut self,
        options: &CompileOptions,
        cancellation: &rue_query::CancellationToken,
        attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        origin: &mut Option<AttemptId>,
        attempt_record: &mut Option<SemanticQueryRecord>,
    ) -> Result<Arc<CanonicalSemanticOutput>, SemanticRequestControl> {
        if cancellation.is_canceled() {
            return Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled,
            ));
        }
        self.require_successful_import_diagnostics()?;
        let imports = self.accepted_semantic_import_graph()?;
        self.queries.publish_import_graph(imports.clone());
        self.queries.publish_request_inputs(options);
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
        let key = SemanticQueryKey {
            input: input.clone(),
            imports: imports.clone(),
        };
        if let Some((entry, handle)) = self
            .queries
            .semantic
            .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
            .unwrap_or_else(query_control_error)
        {
            let result = entry.result.clone();
            *execution = QueryAttemptExecution::Reused;
            *origin = Some(handle.origin_attempt_id());
            self.diagnostics.select(handle.as_view());
            guard.bind(handle.as_view());
            guard.observe(
                self.queries
                    .graph
                    .direct_dependencies::<SemanticQuery>(handle.observed().node),
            );
            guard.attach_diagnostics(entry.diagnostics.clone());
            #[cfg(test)]
            if result.is_ok() {
                self.last_successful_cfg_cache = entry.successful_cfg_cache.clone();
                self.durable_declaration_cache = entry.durable_declaration_cache.clone();
            }
            self.reuse_diagnostics(entry.diagnostics.clone());
            if cancellation.is_canceled() {
                return Err(SemanticRequestControl::Abort(
                    rue_query::QueryAbort::Canceled,
                ));
            }
            return result.map_err(SemanticRequestControl::Compile);
        }
        let durable_baseline = self.queries.semantic.last_good_record().cloned();
        let previous_cfg_cache = durable_baseline
            .as_ref()
            .and_then(|entry| entry.successful_cfg_cache.clone())
            .unwrap_or_else(|| Arc::from([]));
        #[cfg(test)]
        let previous_cfg_cache = self
            .last_successful_cfg_cache
            .clone()
            .unwrap_or(previous_cfg_cache);

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
                let dependency = self
                    .queries
                    .rir
                    .selected_handle(&self.queries.graph)
                    .expect("semantic rejection observes a deterministic RIR failure")
                    .observed();
                guard.observe([dependency]);
                guard.attach_diagnostics(diagnostics.clone());
                let handle = self
                    .queries
                    .semantic
                    .publish_selected_attempt(
                        &mut self.queries.graph,
                        SemanticCacheEntry {
                            key,
                            result: Err(errors.clone()),
                            rir: None,
                            diagnostics,
                            durable_declaration_cache: None,
                            successful_cfg_cache: None,
                            oracle_injected: false,
                        },
                        [dependency],
                        guard.structural(),
                        *execution,
                    )
                    .unwrap_or_else(query_control_error);
                self.diagnostics.select(handle.as_view());
                guard.bind(handle.as_view());
                self.refresh_retention_metrics();
                return Err(SemanticRequestControl::Compile(errors));
            }
        };
        let merged = self
            .queries
            .rir
            .selected_record(&self.queries.graph)
            .and_then(|entry| entry.merged.clone())
            .expect("successful RIR terminal retains its exact merge input");
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
        let query_shells = self.queries.revisioned.projected_declaration_shells(
            runtime_revision,
            merged.ast(),
            cancellation.clone(),
        );
        let mut body_query_shells = None;
        let mut prepared = match query_shells {
            Ok(query_shells) => {
                body_query_shells = Some(query_shells.clone());
                prepare_query_declaration_shells(&merged, &rir, options, &imports, &query_shells)
            }
            Err(crate::revisioned_query_database::DeclarationShellBatchFailure::Query(abort)) => {
                return Err(SemanticRequestControl::Abort(abort));
            }
            Err(crate::revisioned_query_database::DeclarationShellBatchFailure::Stable(
                failure,
            )) => Err(CanonicalSemanticFailure::declaration(
                declaration_shell_failure_diagnostics(merged.ast(), &failure),
                CanonicalSemanticWork::default(),
            )),
        };
        let (
            query_declarations,
            query_anonymous_nominals,
            query_declaration_dependencies,
            query_c_export_roots,
        ) = match self.queries.revisioned.projected_declaration_semantics(
            runtime_revision,
            merged.ast(),
            options.target,
            &options.preview_features,
            cancellation.clone(),
        ) {
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
                        merged.ast(),
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
        let mut body_query_errors = BTreeMap::new();
        let mut body_query_reference_cache = BTreeMap::new();
        // RUE-1027 production boundary: derive the reached callable frontier
        // serially from the references owned by each body transaction. Ordinary
        // call recursion is worklist state, never a query dependency cycle.
        // RUE-1028 replaces this deterministic coordinator with database-owned
        // reachability and parallel frontier scheduling.
        if let (Some(query_shells), Ok(prepared_definitions)) =
            (body_query_shells.as_ref(), prepared.as_ref())
        {
            let mut declaration_candidates = BTreeMap::new();
            for shell in query_shells.iter() {
                let Some(definition) = prepared_definitions
                    .definitions()
                    .definitions()
                    .iter()
                    .find(|record| {
                        let key = record.stable_key();
                        let kind_matches = key.kind() == shell.identity.kind
                            || matches!(
                                (key.kind(), shell.identity.kind),
                                (
                                    crate::StableDefinitionKind::ValueConst,
                                    crate::StableDefinitionKind::ModuleBinding
                                ) | (
                                    crate::StableDefinitionKind::ModuleBinding,
                                    crate::StableDefinitionKind::ValueConst
                                )
                            );
                        key.module().as_str() == shell.identity.module_path.as_ref()
                            && kind_matches
                            && key.name() == shell.identity.name.as_ref()
                            && key.owner().map(|owner| owner.name())
                                == shell.identity.owner.as_deref()
                    })
                    .map(|record| record.stable_key().clone())
                else {
                    continue;
                };
                use crate::StableDefinitionKind as K;
                use crate::declaration_candidate::DeclarationCandidateCategory as C;
                let category = match definition.kind() {
                    K::Function if shell.is_extern => C::ExternFunction,
                    K::Function => C::Function,
                    K::Struct => C::Struct,
                    K::Enum => C::Enum,
                    K::ValueConst | K::ModuleBinding => C::ConstCandidate,
                    K::Destructor => C::Destructor,
                    K::Method => C::Method,
                    K::AssociatedFunction => C::AssociatedFunction,
                };
                let owner = definition.owner().map(|owner| {
                    crate::declaration_candidate::DeclarationCandidateOwner {
                        category: match owner.kind() {
                            K::Struct => C::Struct,
                            K::Enum => C::Enum,
                            _ => unreachable!("callable owners are nominal types"),
                        },
                        name: Arc::from(owner.name()),
                    }
                });
                declaration_candidates.insert(
                    definition.clone(),
                    crate::declaration_candidate::DeclarationCandidateKey {
                        module: definition.module().clone(),
                        category,
                        name: Arc::from(definition.name()),
                        owner,
                        duplicate_discriminator: 0,
                    },
                );
            }
            for declaration in query_declarations
                .as_deref()
                .expect("body traversal follows a successful declaration projection")
            {
                if matches!(
                    declaration.key.kind(),
                    crate::StableDefinitionKind::ValueConst
                        | crate::StableDefinitionKind::ModuleBinding
                ) {
                    declaration_candidates
                        .entry(declaration.key.clone())
                        .or_insert_with(|| {
                            crate::declaration_candidate::DeclarationCandidateKey {
                                module: declaration.key.module().clone(),
                                category: crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
                                name: Arc::from(declaration.key.name()),
                                owner: None,
                                duplicate_discriminator: 0,
                            }
                        });
                }
            }
            let declaration_candidates = Arc::new(declaration_candidates);
            let mut declaration_modules = BTreeSet::from([imports.root().clone()]);
            for record in imports.records() {
                declaration_modules.insert(record.importer().clone());
                if let crate::CanonicalImportResolution::Resolved(target) = record.resolution() {
                    declaration_modules.insert(target.clone());
                }
            }
            let declaration_modules: Arc<[crate::ModuleId]> =
                declaration_modules.into_iter().collect::<Vec<_>>().into();
            let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
                target: options.target,
                preview_features: StablePreviewFeatures::new(&options.preview_features),
            };
            let mut pending = std::collections::BTreeSet::new();
            for record in prepared_definitions.definitions().definitions() {
                let stable = record.stable_key();
                if (stable.kind() == crate::StableDefinitionKind::Function
                    && stable.name() == "main"
                    && stable.module() == merged.ast().root())
                    || stable.kind() == crate::StableDefinitionKind::Destructor
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
            let no_body_produced_anonymous = BTreeMap::new();
            for record in prepared_definitions.definitions().definitions() {
                let stable = record.stable_key();
                if matches!(
                    stable.kind(),
                    crate::StableDefinitionKind::Struct | crate::StableDefinitionKind::Enum
                ) {
                    enqueue_owned_destructor_closure(
                        &crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Named(
                            stable.clone(),
                        )),
                        prepared_definitions.definitions(),
                        query_declarations
                            .as_deref()
                            .expect("body traversal follows a successful declaration projection"),
                        &no_body_produced_anonymous,
                        &query_anonymous_nominals,
                        &mut pending,
                    );
                }
            }
            let roots = pending;
            let mut stable_produced_seed = BTreeMap::new();
            let callable_has_source_body = |callable: &crate::FunctionInstanceKey| {
                let mut current = callable;
                loop {
                    match current {
                        crate::FunctionInstanceKey::Definition(definition) => {
                            let has_runtime_result = query_declarations
                                .as_deref()
                                .is_some_and(|declarations| {
                                    declarations.iter().any(|declaration| {
                                        declaration.key == *definition
                                            && (matches!(
                                                &declaration.payload,
                                                crate::durable_semantics::DurableDeclarationPayload::Callable {
                                                    result,
                                                    ..
                                                } if !matches!(result, crate::durable_semantics::DurableType::ComptimeType)
                                            ) || matches!(
                                                &declaration.payload,
                                                crate::durable_semantics::DurableDeclarationPayload::Destructor
                                            ))
                                    })
                                });
                            return has_runtime_result
                                && declaration_candidates.get(definition).is_some_and(|candidate| {
                                    candidate.category
                                        != crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
                                });
                        }
                        crate::FunctionInstanceKey::Specialization { base, .. } => {
                            current = base;
                        }
                        crate::FunctionInstanceKey::AnonymousMember { .. } => return true,
                        crate::FunctionInstanceKey::DropGlue(_) => return false,
                    }
                }
            };
            let callable_has_query_body = |callable: &crate::FunctionInstanceKey| {
                if let crate::FunctionInstanceKey::Specialization { base, .. } = callable {
                    let mut base = base.as_ref();
                    while let crate::FunctionInstanceKey::Specialization { base: next, .. } = base {
                        base = next;
                    }
                    if let crate::FunctionInstanceKey::Definition(definition) = base {
                        return declaration_candidates.get(definition).is_some_and(|candidate| {
                            candidate.category
                                != crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
                        });
                    }
                }
                callable_has_source_body(callable)
            };
            let callable_has_any_body = |callable: &crate::FunctionInstanceKey| match callable {
                crate::FunctionInstanceKey::Definition(definition) => {
                    let concrete_definition = query_declarations.as_deref().is_some_and(
                            |declarations| {
                                declarations.iter().any(|declaration| {
                                    declaration.key == *definition
                                        && match &declaration.payload {
                                            crate::durable_semantics::DurableDeclarationPayload::Callable {
                                                parameters,
                                                ..
                                            } => parameters
                                                .iter()
                                                .all(|parameter| !parameter.is_comptime),
                                            crate::durable_semantics::DurableDeclarationPayload::Destructor => true,
                                            _ => false,
                                        }
                                })
                            },
                        );
                    concrete_definition
                            && declaration_candidates.get(definition).is_some_and(|candidate| {
                                candidate.category
                                    != crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
                            })
                }
                crate::FunctionInstanceKey::Specialization { base, .. } => {
                    let mut base = base.as_ref();
                    while let crate::FunctionInstanceKey::Specialization { base: next, .. } = base {
                        base = next;
                    }
                    matches!(base, crate::FunctionInstanceKey::Definition(definition) if declaration_candidates.get(definition).is_some_and(|candidate| {
                        candidate.category
                            != crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
                    }))
                }
                crate::FunctionInstanceKey::AnonymousMember { .. } => true,
                crate::FunctionInstanceKey::DropGlue(_) => false,
            };
            // One coordinator span covers the whole body-traversal closure,
            // including every `continue 'closure` restart, so `--time-passes`
            // attributes the aggregate reach-and-analyze cost that RUE-1083
            // traced to the previously unspanned semantic-query path. The
            // per-body pipeline stages nest beneath it as timed leaves.
            let _body_queries_span = tracing::info_span!("body_queries").entered();
            'closure: loop {
                body_query_reference_cache.clear();
                let mut pending = roots.clone();
                let mut priority_pending = Vec::new();
                let mut deferred_dependency_chain = BTreeSet::new();
                let mut visited = std::collections::BTreeSet::new();
                let mut queried_ordinary = Vec::new();
                let mut queried_specialized = Vec::new();
                let mut queried_anonymous = Vec::new();
                // Chain-depth budget for cross-body specialization runaway.
                //
                // Invariant: `instance_depth[S]` is the length of the shortest
                // chain of *specialization* instantiation edges on any traversal
                // path from a root to `S`. Root bodies (`main`, destructors,
                // C-exports, owned-destructor closures) are depth 0. Traversing a
                // body at depth `d` publishes each referenced callable at depth
                // `d + 1` when that callable is itself a specialization, or at
                // depth `d` otherwise (ordinary/anonymous bodies pass the count
                // through without consuming it). This reproduces the old
                // whole-program expansion-wave semantics
                // (`MAX_SPECIALIZATION_ROUNDS` waves), where one wave of newly
                // discovered specializations advanced the counter once and
                // ordinary sema worklists did not: the depth of a specialization
                // equals the wave in which it would first have been produced.
                // Breadth (arbitrarily many specializations at shallow depth) is
                // therefore free; only genuine nested instantiation chains
                // (`f<T>` -> `f<Wrap<T>>` -> `f<Wrap<Wrap<T>>>` -> ...) accrue
                // depth and eventually exceed the budget.
                //
                // All state is re-initialized per `'closure` restart, so the map
                // is simply recomputed from scratch each time an anonymous
                // representative changes; no cross-restart carry is required.
                let mut instance_depth: BTreeMap<crate::FunctionInstanceKey, usize> =
                    roots.iter().map(|root| (root.clone(), 0usize)).collect();
                let record_depth =
                    |instance_depth: &mut BTreeMap<crate::FunctionInstanceKey, usize>,
                     key: &crate::FunctionInstanceKey,
                     depth: usize| {
                        instance_depth
                            .entry(key.clone())
                            .and_modify(|existing| *existing = (*existing).min(depth))
                            .or_insert(depth);
                    };
                let child_depth = |parent_depth: usize, child: &crate::FunctionInstanceKey| {
                    if matches!(child, crate::FunctionInstanceKey::Specialization { .. }) {
                        parent_depth + 1
                    } else {
                        parent_depth
                    }
                };
                body_produced_anonymous = stable_produced_seed.clone();
                body_query_errors.clear();
                let mut representative_changed = false;
                while let Some(mut instance) =
                    priority_pending.pop().or_else(|| pending.pop_first())
                {
                    let popped_depth = instance_depth.get(&instance).copied().unwrap_or(0);
                    instance = canonicalize_reached_anonymous_member(
                        &instance,
                        &body_produced_anonymous,
                        &query_anonymous_nominals,
                    );
                    // Canonicalization may rewrite the popped key to its
                    // canonical anonymous representative; carry the depth onto
                    // the canonical key so it survives the rewrite.
                    record_depth(&mut instance_depth, &instance, popped_depth);
                    let current_depth = instance_depth
                        .get(&instance)
                        .copied()
                        .unwrap_or(popped_depth);
                    let deferred_producers =
                        crate::revisioned_query_database::collect_instance_anonymous_nominals(
                            &instance,
                        )
                        .into_iter()
                        .filter_map(|identity| match identity.producer {
                            crate::StableProducerId::Function(producer)
                                if callable_has_any_body(&producer)
                                    && !visited.contains(producer.as_ref()) =>
                            {
                                Some(*producer)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if !deferred_producers.is_empty() {
                        if deferred_producers
                            .iter()
                            .any(|producer| deferred_dependency_chain.contains(producer))
                        {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(crate::CompileError::without_span(
                                    rue_error::ErrorKind::InternalError(format!(
                                        "anonymous producer dependency cycle while scheduling {instance:?}"
                                    )),
                                )),
                            );
                            break;
                        }
                        deferred_dependency_chain.insert(instance.clone());
                        // `pending` is an ordered set, so reinserting a
                        // lexically earlier consumer there can starve its
                        // later-sorting producer forever. The priority stack
                        // keeps this consumer below every exact producer and
                        // therefore retries it only after those bodies have
                        // published terminals.
                        for producer in deferred_producers.iter() {
                            record_depth(
                                &mut instance_depth,
                                producer,
                                child_depth(current_depth, producer),
                            );
                        }
                        priority_pending.push(instance);
                        priority_pending.extend(deferred_producers);
                        queried_body_work.deferred_producer_retries += 1;
                        continue;
                    }
                    deferred_dependency_chain.remove(&instance);
                    if !visited.insert(instance.clone()) {
                        continue;
                    }
                    if matches!(instance, crate::FunctionInstanceKey::Specialization { .. })
                        && current_depth > rue_air::specialize::MAX_SPECIALIZATION_ROUNDS
                    {
                        // A reached callable may never be omitted from the
                        // published closure. In the one-body query model the
                        // coordinator, rather than one persistent specializer,
                        // observes the transitive runaway and therefore owns
                        // the ordinary comptime-depth diagnostic. We fail only
                        // when this instance's *nesting depth* (its shortest
                        // specialization instantiation chain from a root)
                        // exceeds the budget, matching the per-body comptime
                        // evaluator's `> MAX_SPECIALIZATION_ROUNDS` check and
                        // permitting arbitrarily many shallow specializations.
                        let name = stable_function_definition_root(&instance)
                            .map(crate::StableDefinitionKey::name)
                            .unwrap_or("<anonymous>");
                        body_query_errors.insert(
                            instance.clone(),
                            crate::CompileErrors::from(crate::CompileError::without_span(
                                rue_error::ErrorKind::ComptimeEvaluationFailed {
                                    reason: format!(
                                        "specialization of '{name}' exceeded the maximum nesting depth ({}); is a comptime-recursive function missing a compile-time-known base case, or a generic function recursively instantiating itself with new types?",
                                        rue_air::specialize::MAX_SPECIALIZATION_ROUNDS
                                    ),
                                },
                            )),
                        );
                        break;
                    }
                    let key = crate::body_query::BodyQueryKey {
                        instance: instance.clone(),
                        configuration: configuration.clone(),
                    };
                    let producer_body_terminal_required = match &instance {
                        crate::FunctionInstanceKey::AnonymousMember { owner, .. } => {
                            if let crate::TypeInstanceKey::Nominal(
                                crate::NominalInstanceKey::Anonymous(identity),
                            ) = owner.as_ref()
                                && let crate::StableProducerId::Function(producer) =
                                    &identity.producer
                            {
                                callable_has_any_body(producer)
                                    && (matches!(
                                        producer.as_ref(),
                                        crate::FunctionInstanceKey::Specialization { .. }
                                    ) || !query_anonymous_nominals
                                        .iter()
                                        .any(|nominal| nominal.identity == *identity))
                            } else {
                                false
                            }
                        }
                        _ => false,
                    };
                    let transaction = self.queries.revisioned.body_transaction(
                        runtime_revision,
                        key.clone(),
                        declaration_candidates.clone(),
                        declaration_modules.clone(),
                        producer_body_terminal_required,
                        cancellation.clone(),
                        |selected_anonymous| {
                            queried_body_work.bodies_attempted += 1;
                            // Every cold reached-body analysis re-prepares,
                            // re-projects, and re-installs the entire declaration
                            // universe inside `analyze_body_query` before it
                            // touches this single body. The per-body
                            // declaration-context counters are accrued INSIDE that
                            // call, at each stage's own source, as the stage
                            // actually performs the work — never from these input
                            // slice lengths, which would charge a body for
                            // installation it may never reach. A local stage-work
                            // struct captures exactly what ran (partial on a
                            // stage failure) and is folded into the running total
                            // afterwards. Warm reuse never enters this closure, so
                            // reused bodies add nothing here.
                            let mut stage_context =
                                rue_air::PerBodyDeclarationContextWork::default();
                            let specialized = matches!(
                                &key.instance,
                                crate::FunctionInstanceKey::Specialization { .. }
                            );
                            if specialized {
                                queried_body_work.specialized_bodies_attempted += 1;
                            }
                            let mut body_anonymous = query_anonymous_nominals
                                .iter()
                                .cloned()
                                .map(|nominal| (nominal.identity.clone(), nominal))
                                .collect::<BTreeMap<_, _>>();
                            body_anonymous.extend(
                                selected_anonymous
                                    .iter()
                                    .cloned()
                                    .map(|nominal| (nominal.identity.clone(), nominal)),
                            );
                            let body_anonymous = body_anonymous.into_values().collect::<Vec<_>>();
                            let transaction = crate::canonical_semantic::analyze_body_query(
                                &merged,
                                &rir,
                                options,
                                &imports,
                                query_shells,
                                query_declarations.as_deref().expect(
                                    "body traversal follows a successful declaration projection",
                                ),
                                &body_anonymous,
                                &key,
                                cancellation,
                                &mut stage_context,
                            );
                            // Fold the stage-sourced per-body declaration-context
                            // work (whatever actually ran) into the running total.
                            let context = &mut queried_body_work.per_body_declaration_context;
                            context.cold_body_preparations += stage_context.cold_body_preparations;
                            context.shells_prepared += stage_context.shells_prepared;
                            context.semantics_installed += stage_context.semantics_installed;
                            context.projections_performed += stage_context.projections_performed;
                            context.endpoints_installed += stage_context.endpoints_installed;
                            match &transaction {
                                Ok(crate::body_query::BodyTransaction::Success {
                                    body, ..
                                }) => {
                                    queried_body_work.bodies_succeeded += 1;
                                    let body = match body.as_ref() {
                                        crate::body_query::CanonicalBody::Ordinary {
                                            body, ..
                                        }
                                        | crate::body_query::CanonicalBody::Anonymous {
                                            body,
                                            ..
                                        }
                                        | crate::body_query::CanonicalBody::Specialization {
                                            body,
                                            ..
                                        } => body,
                                    };
                                    queried_body_work.air_instructions_produced +=
                                        body.instructions.len();
                                    queried_body_work.local_strings_produced += body.strings.len();
                                    if specialized {
                                        queried_body_work.specialized_bodies_succeeded += 1;
                                        queried_body_work.specialized_body_exports_attempted += 1;
                                        queried_body_work.specialized_body_exports_succeeded += 1;
                                        queried_body_work
                                            .specialized_body_export_instructions_emitted +=
                                            body.instructions.len();
                                        queried_body_work.specialized_body_export_places_emitted +=
                                            body.places.len();
                                        queried_body_work
                                            .specialized_body_export_strings_emitted +=
                                            body.strings.len();
                                    } else {
                                        queried_body_work.ordinary_body_exports_attempted += 1;
                                        queried_body_work.ordinary_body_exports_succeeded += 1;
                                        queried_body_work
                                            .ordinary_body_export_instructions_emitted +=
                                            body.instructions.len();
                                        queried_body_work.ordinary_body_export_places_emitted +=
                                            body.places.len();
                                        queried_body_work.ordinary_body_export_strings_emitted +=
                                            body.strings.len();
                                    }
                                }
                                Ok(crate::body_query::BodyTransaction::DeterministicFailure {
                                    ..
                                }) => {
                                    queried_body_work.bodies_failed += 1;
                                    if specialized {
                                        queried_body_work.specialized_bodies_failed += 1;
                                    }
                                }
                                Err(_) => {}
                            }
                            transaction
                        },
                    );
                    let transaction = match transaction {
                        Ok(terminal) => terminal,
                        Err(crate::revisioned_query_database::BodyTransactionRequestFailure::DeferredAnonymousProducers(
                            producers,
                        )) => {
                            let mut scheduled = false;
                            priority_pending.push(instance.clone());
                            for producer in producers.iter() {
                                if !visited.contains(producer) {
                                    record_depth(
                                        &mut instance_depth,
                                        producer,
                                        child_depth(current_depth, producer),
                                    );
                                    priority_pending.push(producer.clone());
                                    scheduled = true;
                                }
                            }
                            if scheduled {
                                visited.remove(&instance);
                                queried_body_work.deferred_producer_retries += 1;
                                continue;
                            }
                            priority_pending.pop();
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(crate::CompileError::without_span(
                                    rue_error::ErrorKind::InternalError(format!(
                                        "reached body query {instance:?} could not observe an already-reached anonymous producer"
                                    )),
                                )),
                            );
                            break;
                        }
                        Err(crate::revisioned_query_database::BodyTransactionRequestFailure::Query(
                            rue_query::QueryAbort::Canceled,
                        )) if !cancellation.is_canceled() => {
                            body_query_errors.insert(
                                instance.clone(),
                                crate::CompileErrors::from(crate::CompileError::without_span(
                                    rue_error::ErrorKind::InternalError(format!(
                                        "reached body query {instance:?} did not publish a terminal"
                                    )),
                                )),
                            );
                            break;
                        }
                        Err(crate::revisioned_query_database::BodyTransactionRequestFailure::Query(
                            abort,
                        )) => return Err(SemanticRequestControl::Abort(abort)),
                    };
                    let rue_query::QueryOutcome::Success(transaction) = transaction.outcome()
                    else {
                        unreachable!("BodyTransaction publishes typed values")
                    };
                    body_query_reference_cache
                        .insert(instance.clone(), transaction.references().clone());
                    if let crate::body_query::BodyTransaction::DeterministicFailure {
                        errors, ..
                    } = &transaction
                    {
                        body_query_errors.insert(instance.clone(), errors.clone());
                    }
                    if matches!(
                        transaction,
                        crate::body_query::BodyTransaction::Success { .. }
                    ) {
                        let produced = match self
                            .queries
                            .revisioned
                            .body_produced_anonymous_projection(
                                runtime_revision,
                                key.clone(),
                                cancellation.clone(),
                            ) {
                            Ok(produced) => produced,
                            Err(rue_query::QueryAbort::Canceled) if !cancellation.is_canceled() => {
                                body_query_errors.insert(
                                    instance.clone(),
                                    crate::CompileErrors::from(crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} anonymous-projection did not publish a terminal"
                                        )),
                                    )),
                                );
                                break;
                            }
                            Err(abort) => return Err(SemanticRequestControl::Abort(abort)),
                        };
                        let rue_query::QueryOutcome::Success(produced) = produced.outcome() else {
                            unreachable!("BodyProducedAnonymous publishes typed values")
                        };
                        let previous_representatives = visited
                            .iter()
                            .map(|reached| {
                                (
                                    reached.clone(),
                                    canonicalize_reached_anonymous_member(
                                        reached,
                                        &body_produced_anonymous,
                                        &query_anonymous_nominals,
                                    ),
                                )
                            })
                            .collect::<Vec<_>>();
                        body_produced_anonymous.extend(
                            produced
                                .0
                                .iter()
                                .cloned()
                                .map(|nominal| (nominal.identity.clone(), nominal)),
                        );
                        // Keep facts from every producer which remains its own
                        // canonical representative under the now-complete
                        // fact set. This retracts abandoned member-produced
                        // nested nominals while preserving a surviving
                        // canonical member's facts across the restart.
                        stable_produced_seed.retain(|_, nominal| {
                            match &nominal.identity.producer {
                                crate::StableProducerId::Definition(_) => true,
                                crate::StableProducerId::Function(producer) => {
                                    produced_anonymous_survives_closure_restart(
                                        producer,
                                        &body_produced_anonymous,
                                        &query_anonymous_nominals,
                                    )
                                }
                            }
                        });
                        if produced_anonymous_survives_closure_restart(
                            &instance,
                            &body_produced_anonymous,
                            &query_anonymous_nominals,
                        ) {
                            stable_produced_seed.extend(
                                produced
                                    .0
                                    .iter()
                                    .cloned()
                                    .map(|nominal| (nominal.identity.clone(), nominal)),
                            );
                        }
                        representative_changed =
                            previous_representatives.iter().any(|(reached, previous)| {
                                canonicalize_reached_anonymous_member(
                                    reached,
                                    &body_produced_anonymous,
                                    &query_anonymous_nominals,
                                ) != *previous
                            });
                        if representative_changed {
                            // Anonymous method bodies and their edges are not part
                            // of structural equality. Discard this in-flight
                            // closure and traverse again from roots against the
                            // new producer-owned representative set.
                            break;
                        }
                        for nominal in produced.0.iter() {
                            if let crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                                methods,
                                ..
                            } = &nominal.shape
                                && methods.iter().any(|method| {
                                    method.has_self && method.name.as_ref() == "__drop"
                                })
                            {
                                let drop_glue = crate::FunctionInstanceKey::AnonymousMember {
                                    owner: Box::new(crate::TypeInstanceKey::Nominal(
                                        crate::NominalInstanceKey::Anonymous(
                                            nominal.identity.clone(),
                                        ),
                                    )),
                                    member: crate::AnonymousMemberKey {
                                        kind: crate::AnonymousMemberKind::Destructor,
                                        name: Arc::from("__drop"),
                                    },
                                };
                                record_depth(
                                    &mut instance_depth,
                                    &drop_glue,
                                    child_depth(current_depth, &drop_glue),
                                );
                                pending.insert(drop_glue);
                            }
                        }
                    }
                    let references = transaction.references();
                    for reference in references.0.iter() {
                        if let crate::body_query::BodyReference::Callable(callable) = reference
                            && matches!(
                                callable,
                                crate::FunctionInstanceKey::Definition(_)
                                    | crate::FunctionInstanceKey::Specialization { .. }
                                    | crate::FunctionInstanceKey::AnonymousMember { .. }
                            )
                            && callable_has_any_body(callable)
                        {
                            // Publish each referenced callable at this body's
                            // depth, +1 when the callable is itself a
                            // specialization. This is the sole edge through
                            // which `Specialization` instances enter the
                            // worklist, so it governs the chain-depth budget.
                            record_depth(
                                &mut instance_depth,
                                callable,
                                child_depth(current_depth, callable),
                            );
                            pending.insert(callable.clone());
                        }
                        if let crate::body_query::BodyReference::Type(ty) = reference {
                            enqueue_owned_destructor_closure(
                                ty,
                                prepared_definitions.definitions(),
                                query_declarations.as_deref().expect(
                                    "body traversal follows a successful declaration projection",
                                ),
                                &body_produced_anonymous,
                                &query_anonymous_nominals,
                                &mut pending,
                            );
                        }
                    }
                    if matches!(
                        transaction,
                        crate::body_query::BodyTransaction::Success { .. }
                    ) && callable_has_query_body(&instance)
                    {
                        let body = match self.queries.revisioned.canonical_body_projection(
                            runtime_revision,
                            key.clone(),
                            cancellation.clone(),
                        ) {
                            Ok(body) => body,
                            Err(rue_query::QueryAbort::Canceled) if !cancellation.is_canceled() => {
                                body_query_errors.insert(
                                    instance.clone(),
                                    crate::CompileErrors::from(crate::CompileError::without_span(
                                        rue_error::ErrorKind::InternalError(format!(
                                            "reached body {instance:?} canonical-body projection did not publish a terminal"
                                        )),
                                    )),
                                );
                                break;
                            }
                            Err(abort) => return Err(SemanticRequestControl::Abort(abort)),
                        };
                        let rue_query::QueryOutcome::Success(body) = body.outcome() else {
                            unreachable!("CanonicalBody publishes typed values")
                        };
                        match body {
                            crate::body_query::CanonicalBody::Ordinary { owner, body } => {
                                let Some(record) =
                                    prepared_definitions.definitions().definition_by_key(owner)
                                else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an ordinary body whose owner has no issued definition record"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                let Some(body_span) = record.body_span() else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an ordinary body whose owner definition record carries no body span"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                queried_ordinary.push(
                                    crate::canonical_semantic::PreparedDurableBodyCandidate {
                                        owner: owner.clone(),
                                        body_span,
                                        body: body.clone(),
                                    },
                                );
                            }
                            crate::body_query::CanonicalBody::Anonymous { identity, body } => {
                                let crate::FunctionInstanceKey::AnonymousMember { owner, member } =
                                    identity
                                else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an anonymous body whose identity is not an anonymous member"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                let crate::TypeInstanceKey::Nominal(
                                    crate::NominalInstanceKey::Anonymous(owner_identity),
                                ) = owner.as_ref()
                                else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an anonymous body whose owner is not an anonymous nominal type"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                let source_key = match &owner_identity.producer {
                                    crate::StableProducerId::Function(function) => {
                                        crate::revisioned_query_database::function_definition_key(
                                            function,
                                        )
                                    }
                                    crate::StableProducerId::Definition(definition) => {
                                        Some(definition)
                                    }
                                };
                                let Some(source_key) = source_key else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an anonymous body whose producing function has no definition key"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                let Some(source) = prepared_definitions
                                    .definitions()
                                    .definition_by_key(source_key)
                                else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an anonymous body whose source definition has no issued definition record"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                let source_span = source.declaration_span();
                                let occurrence = owner_identity.anchor.segments().last();
                                let body_span = rir.rir().iter().find_map(|(_, instruction)| {
                                    let rue_rir::InstData::AnonStructType {
                                        methods, anchor, ..
                                    } = &instruction.data
                                    else {
                                        return None;
                                    };
                                    if anchor.segments().last() != occurrence
                                        || instruction.span.file_id != source_span.file_id
                                        || instruction.span.start < source_span.start
                                        || instruction.span.end > source_span.end
                                    {
                                        return None;
                                    }
                                    rir.rir().anon_struct_methods(methods).iter().find_map(
                                        |method_ref| {
                                            let method_instruction = rir.rir().get(*method_ref);
                                            let rue_rir::InstData::FnDecl { name, .. } =
                                                &method_instruction.data
                                            else {
                                                return None;
                                            };
                                            (rir.semantic_symbols().interner().resolve(name)
                                                == member.name.as_ref())
                                            .then_some(method_instruction.span)
                                        },
                                    )
                                });
                                let Some(body_span) = body_span else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published an anonymous body whose method span was not found in the owning RIR anon-struct"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                queried_anonymous.push(
                                crate::canonical_semantic::PreparedDurableAnonymousBodyCandidate {
                                    identity: identity.clone(),
                                    body_span,
                                    body: body.clone(),
                                },
                            );
                            }
                            crate::body_query::CanonicalBody::Specialization {
                                identity,
                                body,
                                ..
                            } => {
                                let Some(record) = prepared_definitions
                                    .definitions()
                                    .definition_by_key(&identity.base)
                                else {
                                    body_query_errors.insert(
                                        instance.clone(),
                                        crate::CompileErrors::from(crate::CompileError::without_span(
                                            rue_error::ErrorKind::InternalError(format!(
                                                "reached body {instance:?} published a specialization body whose base has no issued definition record"
                                            )),
                                        )),
                                    );
                                    break;
                                };
                                queried_specialized.push(
                                crate::canonical_semantic::PreparedDurableSpecializedBodyCandidate {
                                    instance: instance.clone(),
                                    identity: identity.clone(),
                                    body_span: record.declaration_span(),
                                    body: body.clone(),
                                },
                            );
                            }
                        }
                    }
                }
                if representative_changed {
                    queried_body_work.closure_restarts += 1;
                    continue 'closure;
                }
                durable_body_candidates = queried_ordinary;
                durable_specialized_body_candidates = queried_specialized;
                durable_anonymous_body_candidates = queried_anonymous;
                // Completion snapshots for the published closure. `visited` is
                // the distinct reached-body set; `instance_depth` holds every
                // recorded specialization chain depth.
                queried_body_work.closure_bodies_visited = visited.len();
                queried_body_work.max_specialization_depth =
                    instance_depth.values().copied().max().unwrap_or(0);
                // One wide completion event per successful semantic request,
                // not per body.
                tracing::info!(
                    bodies_attempted = queried_body_work.bodies_attempted,
                    bodies_visited = queried_body_work.closure_bodies_visited,
                    closure_restarts = queried_body_work.closure_restarts,
                    deferred_producer_retries = queried_body_work.deferred_producer_retries,
                    max_specialization_depth = queried_body_work.max_specialization_depth,
                    "body closure complete"
                );
                break 'closure;
            }
        }
        let query_anonymous_nominals = {
            let mut all = query_anonymous_nominals
                .iter()
                .cloned()
                .map(|nominal| (nominal.identity.clone(), nominal))
                .collect::<BTreeMap<_, _>>();
            all.extend(body_produced_anonymous);
            crate::revisioned_query_database::materialize_contextual_anonymous_aliases(&mut all);
            Arc::from(all.into_values().collect::<Vec<_>>())
        };
        // Declaration payloads have one production authority: the revisioned
        // semantic query nucleus. The old durable declaration cache remains a
        // test-oracle fixture only; it must never select or revive a second
        // declaration resolver in a real request.
        let query_declarations_for_cache = query_declarations.clone();
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
                &rir,
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
                previous_cfg_cache.clone(),
                durable_body_work,
            )?;
            output.install_body_references(body_query_reference_cache);
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
        let published_declaration_cache = result
            .is_ok()
            .then_some(query_declarations_for_cache)
            .flatten()
            .map(|semantics| DurableDeclarationCache { semantics });
        let mut published_cfg_cache = None;
        if let Ok(output) = &result {
            published_cfg_cache = Some(output.durable_cfgs().clone());
            debug_assert_eq!(output.input(), &input);
            debug_assert_eq!(semantic_work.binding.bind_invocations, 1);
            debug_assert_eq!(semantic_work.manifest.build_invocations, 1);
            debug_assert!(!semantic_work.stable_ids_requested);
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
        let rir_key = RirQueryKey {
            source: rir.source_revision().clone(),
        };
        let rir_dependency = self
            .queries
            .rir
            .handle(&rir_key)
            .expect("semantic publication retains its RIR terminal")
            .observed();
        let dependencies = [
            rir_dependency,
            self.queries
                .import_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .target_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .preview_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .optimization_inputs
                .selected(&self.queries.graph)
                .unwrap(),
        ];
        guard.observe(dependencies);
        guard.attach_diagnostics(diagnostics.clone());
        #[cfg(test)]
        if result.is_ok() {
            self.last_successful_cfg_cache = published_cfg_cache.clone();
            self.durable_declaration_cache = published_declaration_cache.clone();
        }
        let handle = self
            .queries
            .semantic
            .publish_selected_attempt(
                &mut self.queries.graph,
                SemanticCacheEntry {
                    key,
                    result: result.clone(),
                    rir: result.is_ok().then(|| rir.clone()),
                    diagnostics,
                    durable_declaration_cache: result
                        .is_ok()
                        .then_some(published_declaration_cache)
                        .flatten(),
                    successful_cfg_cache: result.is_ok().then_some(published_cfg_cache).flatten(),
                    oracle_injected: false,
                },
                dependencies,
                guard.structural(),
                *execution,
            )
            .unwrap_or_else(query_control_error);
        self.diagnostics.select(handle.as_view());
        guard.bind(handle.as_view());
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
        self.metrics
            .publish_definition(self.queries.definitions.len());
        self.metrics.synchronize();
        result
    }

    fn stable_definitions_attempt(
        &mut self,
        options: &CompileOptions,
        attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        origin: &mut Option<AttemptId>,
        attempt_record: &mut Option<DefinitionQueryRecord>,
    ) -> Result<Arc<BoundDefinitionSet>, CompileErrors> {
        self.require_successful_import_diagnostics()?;
        let imports = self.accepted_semantic_import_graph()?;
        self.queries.publish_import_graph(imports.clone());
        self.queries.publish_request_inputs(options);
        let snapshot = self
            .published_snapshot
            .clone()
            .expect("definition query retains its exact source snapshot");
        let input =
            SemanticInputDescriptor::new(&snapshot, options.target, &options.preview_features);
        let key = DefinitionQueryKey {
            input: input.clone(),
            imports: imports.clone(),
        };
        if let Some((entry, handle)) = self
            .queries
            .definitions
            .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
            .unwrap_or_else(query_control_error)
        {
            *execution = QueryAttemptExecution::Reused;
            *origin = Some(handle.origin_attempt_id());
            guard.bind(handle.as_view());
            return entry.output.result;
        }
        let rir = match self.canonical_rir() {
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
                let dependency = self
                    .queries
                    .rir
                    .selected_handle(&self.queries.graph)
                    .expect("definition rejection observes a deterministic RIR failure")
                    .observed();
                let handle = self
                    .queries
                    .definitions
                    .publish_selected_attempt(
                        &mut self.queries.graph,
                        DefinitionCacheEntry {
                            key: key.clone(),
                            output: DefinitionQueryOutput {
                                provenance: key,
                                result: Err(errors.clone()),
                            },
                        },
                        [dependency],
                        guard.structural(),
                        *execution,
                    )
                    .unwrap_or_else(query_control_error);
                guard.bind(handle.as_view());
                self.refresh_retention_metrics();
                return Err(errors);
            }
        };
        let merged = self
            .queries
            .rir
            .selected_record(&self.queries.graph)
            .and_then(|entry| entry.merged.clone())
            .expect("successful RIR terminal retains its exact merge input");

        let semantic_binding_key = SemanticBindingLookupKey {
            input: input.clone(),
            imports: imports.clone(),
        };
        let cached_semantic = self
            .queries
            .semantic
            .get_secondary_with_handle(&semantic_binding_key)
            .and_then(|(entry, handle)| {
                handle
                    .is_valid(&mut self.queries.graph)
                    .then(|| entry.result.clone())
            })
            .and_then(Result::ok);
        let semantic = match cached_semantic {
            Some(semantic) => semantic,
            None => match self.canonical_semantic(options) {
                Ok(semantic) => semantic,
                Err(errors) => {
                    let dependency = self
                        .queries
                        .semantic
                        .selected_handle(&self.queries.graph)
                        .expect("definition rejection observes a deterministic semantic failure")
                        .observed();
                    let record = DefinitionQueryRecord {
                        input: input.clone(),
                        binding: DeclarationBindingWork::default(),
                        manifest: SemanticBindingManifestWork::default(),
                        issuance: BoundDefinitionWork::default(),
                        failed: true,
                    };
                    guard.accrue(QueryStructuralWork::Definition(Box::new(record.clone())));
                    *attempt_record = Some(record);
                    let handle = self
                        .queries
                        .definitions
                        .publish_selected_attempt(
                            &mut self.queries.graph,
                            DefinitionCacheEntry {
                                key: key.clone(),
                                output: DefinitionQueryOutput {
                                    provenance: key,
                                    result: Err(errors.clone()),
                                },
                            },
                            [dependency],
                            guard.structural(),
                            *execution,
                        )
                        .unwrap_or_else(query_control_error);
                    guard.bind(handle.as_view());
                    self.refresh_retention_metrics();
                    return Err(errors);
                }
            },
        };
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let computation = compute_stable_definitions(&merged, options, &imports, &semantic);
        let result = computation.output.result.clone();
        let record = DefinitionQueryRecord {
            input,
            binding: computation.binding,
            manifest: computation.manifest,
            issuance: computation.issuance,
            failed: result.is_err(),
        };
        guard.accrue(QueryStructuralWork::Definition(Box::new(record.clone())));
        *attempt_record = Some(record);
        let rir_key = RirQueryKey {
            source: rir.source_revision().clone(),
        };
        let rir_dependency = self
            .queries
            .rir
            .handle(&rir_key)
            .expect("definition publication retains its RIR terminal")
            .observed();
        let semantic_dependency = self
            .queries
            .semantic
            .get_secondary_with_handle(&semantic_binding_key)
            .map(|(_, handle)| handle.observed())
            .expect("definition publication retains a successful semantic terminal");
        let dependencies = [
            rir_dependency,
            semantic_dependency,
            self.queries
                .import_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .target_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .preview_inputs
                .selected(&self.queries.graph)
                .unwrap(),
        ];
        let handle = self
            .queries
            .definitions
            .publish_selected_attempt(
                &mut self.queries.graph,
                DefinitionCacheEntry {
                    key,
                    output: computation.output,
                },
                dependencies,
                guard.structural(),
                *execution,
            )
            .unwrap_or_else(query_control_error);
        guard.bind(handle.as_view());
        self.refresh_retention_metrics();
        result
    }

    /// Materialize the stable semantic dependency manifest.
    ///
    /// The manifest contains the supported call, destructor, declaration-type,
    /// type-call-head, and named-constant edge families. Per-surface completeness
    /// flags and stable blockers make incomplete capture fail closed. The query
    /// shares the import and stable-definition inputs and performs no additional
    /// RIR traversal.
    pub(crate) fn semantic_dependency_inputs(
        &mut self,
        options: &CompileOptions,
        std_dir: Option<&str>,
    ) -> Result<Arc<SemanticDependencyInputManifest>, CompileErrors> {
        let mut guard = self.metrics.begin::<DependencyManifestQuery>();
        let attempt_id = guard.id;
        let mut execution = QueryAttemptExecution::Rejected;
        let mut origin = None;
        let mut attempt_work = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.semantic_dependency_inputs_attempt(
                options,
                std_dir,
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
        if let Err(errors) = &result
            && let Some(key) = self.queries.manifests.computing_key()
        {
            let mut dependencies = Vec::new();
            if let Some(handle) = self.queries.semantic.selected_handle(&self.queries.graph) {
                dependencies.push(handle.observed());
            }
            if let Some(handle) = self
                .queries
                .definitions
                .selected_handle(&self.queries.graph)
            {
                dependencies.push(handle.observed());
            }
            let handle = self
                .queries
                .manifests
                .publish_selected_attempt(
                    &mut self.queries.graph,
                    DependencyManifestCacheEntry {
                        key,
                        result: Err(errors.clone()),
                    },
                    dependencies,
                    guard.structural(),
                    execution,
                )
                .unwrap_or_else(query_control_error);
            guard.bind(handle.as_view());
            self.refresh_retention_metrics();
        }
        let structural = attempt_work
            .map(Box::new)
            .map(QueryStructuralWork::DependencyManifest)
            .unwrap_or(QueryStructuralWork::None);
        guard.finish(execution, origin, &result, structural);
        self.metrics.synchronize();
        result
    }

    pub fn unstable_dependency_baseline(
        &mut self,
        options: &CompileOptions,
        std_dir: Option<&str>,
    ) -> Result<Arc<crate::unstable::DependencyBaseline>, CompileErrors> {
        self.semantic_dependency_inputs(options, std_dir)
            .map(|manifest| {
                crate::unstable::DependencyBaseline::new(manifest, self.identity.clone())
            })
            .map(Arc::new)
    }

    fn semantic_dependency_inputs_attempt(
        &mut self,
        options: &CompileOptions,
        std_dir: Option<&str>,
        attempt_id: AttemptId,
        guard: &mut QueryComputationGuard,
        execution: &mut QueryAttemptExecution,
        origin: &mut Option<AttemptId>,
        attempt_work: &mut Option<SemanticDependencyManifestWork>,
    ) -> Result<Arc<SemanticDependencyInputManifest>, CompileErrors> {
        let imports = self.import_graph(std_dir)?;
        let snapshot = self
            .published_snapshot
            .as_ref()
            .expect("stable definitions retain a published source snapshot")
            .clone();
        let input =
            SemanticInputDescriptor::new(&snapshot, options.target, &options.preview_features);
        let key = DependencyManifestQueryKey {
            input: input.clone(),
            imports: imports.graph().clone(),
        };
        if let Some((cached, handle)) = self
            .queries
            .manifests
            .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
            .unwrap_or_else(query_control_error)
        {
            *execution = QueryAttemptExecution::Reused;
            *origin = Some(handle.origin_attempt_id());
            guard.bind(handle.as_view());
            return cached.result;
        }
        *execution = QueryAttemptExecution::Computed;
        guard.started();
        let semantic = self.canonical_semantic(options);
        let definitions = self.stable_definitions(options);
        let definition_universe_state = match &definitions {
            Ok(_) => SemanticDefinitionUniverseState::Complete,
            Err(errors) => SemanticDefinitionUniverseState::Incomplete(
                SemanticDefinitionUniverseIncompleteReason::StableDefinitionsFailed(
                    NonEmptyDefinitionFailures::from_errors(errors),
                ),
            ),
        };
        let definition_records = definitions
            .as_ref()
            .map(|definitions| definitions.definitions())
            .unwrap_or(&[]);
        let mut keys = definition_records
            .iter()
            .map(|record| record.stable_key().clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        let mut partial_work = SemanticDependencyManifestWork {
            import_records_visited: imports.graph().records().len(),
            ..SemanticDependencyManifestWork::default()
        };
        let mut definition_fingerprints = Vec::with_capacity(definition_records.len());
        for record in definition_records {
            partial_work.definition_records_visited += 1;
            guard.accrue(QueryStructuralWork::DependencyManifest(Box::new(
                partial_work,
            )));
            definition_fingerprints.push(stable_definition_input_fingerprint(&snapshot, record)?);
        }
        let (
            mut free_function_dependencies,
            mut named_method_dependencies,
            mut named_destructor_dependencies,
            mut declaration_type_dependencies,
            mut declaration_type_call_head_dependencies,
            mut builtin_type_call_head_inputs,
            mut named_const_dependencies,
            mut implicit_named_destructor_dependencies,
            free_function_events_translated,
            specialization_origins_validated,
            named_method_events_translated,
            named_destructor_events_translated,
            declaration_type_events_translated,
            declaration_type_call_head_events_translated,
            builtin_type_call_head_inputs_translated,
            named_const_events_translated,
            implicit_named_destructor_events_translated,
            mut free_function_caller_dependencies_complete,
            named_method_dependencies_complete,
            generic_named_method_dependencies_complete,
            named_destructor_dependencies_complete,
            declaration_type_dependencies_complete,
            declaration_type_call_head_dependencies_complete,
            supported_type_call_heads_complete,
            named_value_const_dependencies_complete,
            implicit_named_destructor_dependencies_complete,
        ) = match (&semantic, &definitions) {
            (Ok(semantic), Ok(definitions)) => {
                if definitions.source_revision() != &input.sources {
                    return Err(invalid_dependency_manifest(
                        "semantic dependency translation used a foreign definition revision",
                    ));
                }
                if semantic.body_owner_issuer().source_revision() != &input.sources {
                    return Err(invalid_dependency_manifest(
                        "semantic dependency translation used a stale body-owner issuer revision",
                    ));
                }
                let mut edges = Vec::new();
                for origin in semantic.specialized_free_function_origins() {
                    stable_free_function_endpoint(
                        definitions,
                        origin.base_file,
                        &origin.base_name,
                    )?;
                }
                for event in semantic.ordinary_free_function_dependencies() {
                    let provenance = stable_free_function_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_name,
                    )?;
                    edges.push(StableFreeFunctionDependency {
                        caller: stable_token_endpoint(semantic, event.caller_token, &provenance)?,
                        callee: stable_free_function_endpoint(
                            definitions,
                            event.callee_file,
                            &event.callee_name,
                        )?,
                    });
                }
                for event in semantic.specialized_free_function_dependencies() {
                    edges.push(StableFreeFunctionDependency {
                        caller: stable_free_function_endpoint(
                            definitions,
                            event.base_file,
                            &event.base_name,
                        )?,
                        callee: stable_free_function_endpoint(
                            definitions,
                            event.callee_file,
                            &event.callee_name,
                        )?,
                    });
                }
                let mut method_edges = Vec::new();
                for event in semantic.named_method_dependencies() {
                    let provenance = stable_named_method_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_owner_name,
                        &event.caller_method_name,
                    )?;
                    let caller = stable_token_endpoint(semantic, event.caller_token, &provenance)?;
                    let target = match &event.target {
                        rue_air::NamedMethodDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedMethodDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedMethodDependencyTargetEvent::NamedMethod {
                            file,
                            owner_name,
                            method_name,
                        } => StableNamedMethodDependencyTarget::NamedMethod(
                            stable_named_method_endpoint(
                                definitions,
                                *file,
                                owner_name,
                                method_name,
                            )?,
                        ),
                    };
                    method_edges.push(StableNamedMethodDependency { caller, target });
                }
                let mut destructor_edges = Vec::new();
                for event in semantic.named_destructor_dependencies() {
                    let provenance = stable_named_destructor_endpoint(
                        definitions,
                        event.caller_file,
                        &event.caller_owner_name,
                    )?;
                    let caller = stable_token_endpoint(semantic, event.caller_token, &provenance)?;
                    let target = match &event.target {
                        rue_air::NamedMethodDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedMethodDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedMethodDependencyTargetEvent::NamedMethod {
                            file,
                            owner_name,
                            method_name,
                        } => StableNamedMethodDependencyTarget::NamedMethod(
                            stable_named_method_endpoint(
                                definitions,
                                *file,
                                owner_name,
                                method_name,
                            )?,
                        ),
                    };
                    destructor_edges.push(StableNamedDestructorDependency { caller, target });
                }
                let mut type_edges = Vec::new();
                for event in semantic.declaration_type_dependencies() {
                    let provenance = stable_declaration_source_endpoint(definitions, event)?;
                    type_edges.push(StableDeclarationTypeDependency {
                        source: match event.source_token {
                            Some(token) => stable_token_endpoint(semantic, token, &provenance)?,
                            None => provenance,
                        },
                        target: stable_named_type_endpoint(definitions, event)?,
                        kind: event.dependency_kind,
                    });
                }
                let mut type_call_head_edges = Vec::new();
                for event in semantic.declaration_type_call_head_dependencies() {
                    let provenance = stable_declaration_type_source_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        event.source_owner_name.as_deref(),
                        event.source_kind,
                    )?;
                    type_call_head_edges.push(StableDeclarationTypeCallHeadDependency {
                        source: match event.source_token {
                            Some(token) => stable_token_endpoint(semantic, token, &provenance)?,
                            None => provenance,
                        },
                        callable: stable_free_function_endpoint(
                            definitions,
                            event.callable_file,
                            &event.callable_name,
                        )?,
                        kind: event.dependency_kind,
                    });
                }
                let mut builtin_head_inputs = Vec::new();
                for event in semantic.declaration_builtin_type_call_head_dependencies() {
                    let provenance = stable_declaration_type_source_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        event.source_owner_name.as_deref(),
                        event.source_kind,
                    )?;
                    builtin_head_inputs.push(StableBuiltinTypeCallHeadInput {
                        source: match event.source_token {
                            Some(token) => stable_token_endpoint(semantic, token, &provenance)?,
                            None => provenance,
                        },
                        builtin: event.builtin,
                        kind: event.dependency_kind,
                    });
                }
                let mut const_edges = Vec::new();
                for event in semantic.named_const_dependencies() {
                    let source = stable_top_level_endpoint(
                        definitions,
                        event.source_file,
                        &event.source_name,
                        StableDefinitionNamespace::Value,
                        StableDefinitionKind::ValueConst,
                    )?;
                    let target = match &event.target {
                        rue_air::NamedConstDependencyTargetEvent::ValueConst { file, name } => {
                            StableNamedConstDependencyTarget::ValueConst(stable_top_level_endpoint(
                                definitions,
                                *file,
                                name,
                                StableDefinitionNamespace::Value,
                                StableDefinitionKind::ValueConst,
                            )?)
                        }
                        rue_air::NamedConstDependencyTargetEvent::FreeFunction { file, name } => {
                            StableNamedConstDependencyTarget::FreeFunction(
                                stable_free_function_endpoint(definitions, *file, name)?,
                            )
                        }
                        rue_air::NamedConstDependencyTargetEvent::NamedType {
                            file,
                            name,
                            kind,
                        } => {
                            let kind = match kind {
                                rue_air::DeclarationTypeDependencyTargetKind::Struct => {
                                    StableDefinitionKind::Struct
                                }
                                rue_air::DeclarationTypeDependencyTargetKind::Enum => {
                                    StableDefinitionKind::Enum
                                }
                                rue_air::DeclarationTypeDependencyTargetKind::ValueConst => {
                                    StableDefinitionKind::ValueConst
                                }
                            };
                            let namespace = if matches!(kind, StableDefinitionKind::ValueConst) {
                                StableDefinitionNamespace::Value
                            } else {
                                StableDefinitionNamespace::Type
                            };
                            StableNamedConstDependencyTarget::NamedType(stable_top_level_endpoint(
                                definitions,
                                *file,
                                name,
                                namespace,
                                kind,
                            )?)
                        }
                        rue_air::NamedConstDependencyTargetEvent::ModuleBinding { file, name } => {
                            StableNamedConstDependencyTarget::ModuleBinding(
                                stable_top_level_endpoint(
                                    definitions,
                                    *file,
                                    name,
                                    StableDefinitionNamespace::Value,
                                    StableDefinitionKind::ModuleBinding,
                                )?,
                            )
                        }
                    };
                    const_edges.push(StableNamedConstDependency { source, target });
                }
                let mut implicit_destructor_edges = Vec::new();
                for event in semantic.implicit_named_destructor_dependencies() {
                    if matches!(
                        event.source,
                        rue_air::ImplicitDropDependencySourceEvent::Specialization { .. }
                    ) {
                        continue;
                    }
                    implicit_destructor_edges.push(StableImplicitNamedDestructorDependency {
                        source: stable_implicit_drop_source_endpoint(
                            semantic,
                            definitions,
                            &event.source,
                        )?,
                        target: stable_named_destructor_endpoint(
                            definitions,
                            event.target_file,
                            &event.target_owner_name,
                        )?,
                    });
                }
                (
                    edges,
                    method_edges,
                    destructor_edges,
                    type_edges,
                    type_call_head_edges,
                    builtin_head_inputs,
                    const_edges,
                    implicit_destructor_edges,
                    semantic.ordinary_free_function_dependencies().len()
                        + semantic.specialized_free_function_dependencies().len(),
                    semantic.specialized_free_function_origins().len(),
                    semantic.named_method_dependencies().len(),
                    semantic.named_destructor_dependencies().len(),
                    semantic.declaration_type_dependencies().len(),
                    semantic.declaration_type_call_head_dependencies().len(),
                    semantic
                        .declaration_builtin_type_call_head_dependencies()
                        .len(),
                    semantic.named_const_dependencies().len(),
                    semantic.implicit_named_destructor_dependencies().len(),
                    semantic.ordinary_free_function_dependencies_complete()
                        && semantic.specialized_free_function_dependencies_complete(),
                    semantic.non_generic_named_method_dependencies_complete(),
                    semantic.generic_named_method_dependencies_complete(),
                    semantic.named_destructor_dependencies_complete(),
                    semantic.declaration_type_dependencies_complete(),
                    semantic.declaration_type_call_head_dependencies_complete(),
                    semantic.supported_type_call_heads_complete(),
                    semantic.named_value_const_dependencies_complete(),
                    semantic.implicit_named_destructor_dependencies_complete(),
                )
            }
            _ => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            ),
        };
        if let Ok(semantic) = &semantic {
            let mut query_edges = Vec::new();
            for function in semantic.functions() {
                let Some(caller) = crate::revisioned_query_database::function_definition_key(
                    &function.semantic_identity,
                ) else {
                    continue;
                };
                let references = semantic
                    .body_references(&function.semantic_identity)
                    .ok_or_else(|| {
                        invalid_dependency_manifest(
                            "canonical function is missing its query-owned body references",
                        )
                    })?;
                for reference in references.0.iter() {
                    let crate::body_query::BodyReference::Callable(callable) = reference else {
                        continue;
                    };
                    let Some(callee) =
                        crate::revisioned_query_database::function_definition_key(callable)
                    else {
                        continue;
                    };
                    query_edges.push(StableFreeFunctionDependency {
                        caller: caller.clone(),
                        callee: callee.clone(),
                    });
                }
            }
            // RUE-1027: body call edges have one production authority. The
            // query-owned references returned by each reached body transaction
            // replace the whole-program observer projection used by the
            // compatibility oracle above.
            free_function_dependencies = query_edges;
            free_function_caller_dependencies_complete = true;
        }
        let (mut analyzed_body_owners, anonymous_body_owners) = match (&semantic, &definitions) {
            (Ok(semantic), Ok(definitions)) => {
                let mut owners = Vec::new();
                let mut anonymous = 0usize;
                for event in semantic.analyzed_body_owners() {
                    let owner = match stable_body_owner_endpoint(semantic, definitions, event)? {
                        Some(owner) => Some(owner),
                        None => {
                            anonymous += 1;
                            None
                        }
                    };
                    if let Some(owner) = owner {
                        owners.push(owner);
                    }
                }
                (owners, anonymous)
            }
            _ => (Vec::new(), 0),
        };
        analyzed_body_owners.sort();
        analyzed_body_owners.dedup();
        let mut body_named_dependencies = Vec::new();
        if let (Ok(semantic), Ok(definitions)) = (&semantic, &definitions) {
            for event in semantic.body_named_dependencies() {
                let Some((source, _)) =
                    stable_body_owner_endpoint(semantic, definitions, &event.source)?
                else {
                    continue;
                };
                let target = match &event.target {
                    rue_air::NamedConstDependencyTargetEvent::ValueConst { file, name } => {
                        stable_top_level_endpoint(
                            definitions,
                            *file,
                            name,
                            StableDefinitionNamespace::Value,
                            StableDefinitionKind::ValueConst,
                        )?
                    }
                    rue_air::NamedConstDependencyTargetEvent::ModuleBinding { file, name } => {
                        stable_top_level_endpoint(
                            definitions,
                            *file,
                            name,
                            StableDefinitionNamespace::Value,
                            StableDefinitionKind::ModuleBinding,
                        )?
                    }
                    // Body observers currently emit only value/module choices.
                    // Keep all other variants fail-closed if that contract changes.
                    _ => {
                        return Err(invalid_dependency_manifest(
                            "unsupported body-local named dependency target",
                        ));
                    }
                };
                body_named_dependencies.push((source, target));
            }
        }
        body_named_dependencies.sort();
        body_named_dependencies.dedup();
        free_function_dependencies.sort();
        free_function_dependencies.dedup();
        named_method_dependencies.sort();
        named_method_dependencies.dedup();
        named_destructor_dependencies.sort();
        named_destructor_dependencies.dedup();
        declaration_type_dependencies.sort();
        declaration_type_dependencies.dedup();
        declaration_type_call_head_dependencies.sort();
        declaration_type_call_head_dependencies.dedup();
        builtin_type_call_head_inputs.sort();
        builtin_type_call_head_inputs.dedup();
        named_const_dependencies.sort();
        named_const_dependencies.dedup();
        implicit_named_destructor_dependencies.sort();
        implicit_named_destructor_dependencies.dedup();
        // A per-body record cannot authorize reuse when an observer-backed
        // dependency surface for this semantic execution is incomplete. The
        // current completeness evidence is whole-graph rather than per-owner,
        // so conservatively retain its ownerless blockers on every record.
        let mut whole_graph_body_blockers = BTreeSet::new();
        let mut block_body_surface =
            |complete: bool,
             surface: SemanticDependencySurface,
             reason: SemanticDependencyIncompleteReason| {
                if !complete {
                    whole_graph_body_blockers.insert(SemanticDependencyBlocker {
                        owner: None,
                        surface,
                        reason,
                    });
                }
            };
        block_body_surface(
            free_function_caller_dependencies_complete,
            SemanticDependencySurface::FreeFunctionCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block_body_surface(
            named_method_dependencies_complete,
            SemanticDependencySurface::NonGenericNamedMethodCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block_body_surface(
            generic_named_method_dependencies_complete,
            SemanticDependencySurface::GenericNamedMethodCall,
            SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
        );
        block_body_surface(
            named_destructor_dependencies_complete,
            SemanticDependencySurface::NamedDestructorCall,
            SemanticDependencyIncompleteReason::DestructorEndpointUnavailable,
        );
        block_body_surface(
            implicit_named_destructor_dependencies_complete,
            SemanticDependencySurface::ImplicitNamedDestructor,
            SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable,
        );
        block_body_surface(
            declaration_type_dependencies_complete,
            SemanticDependencySurface::DeclarationType,
            SemanticDependencyIncompleteReason::ResolvedTypeIdentityUnavailable,
        );
        block_body_surface(
            declaration_type_call_head_dependencies_complete,
            SemanticDependencySurface::DeclarationTypeCallHead,
            SemanticDependencyIncompleteReason::TypeCallHeadIdentityUnavailable,
        );
        block_body_surface(
            supported_type_call_heads_complete,
            SemanticDependencySurface::SupportedTypeCallHead,
            SemanticDependencyIncompleteReason::UnsupportedDynamicTypeCallHead,
        );
        block_body_surface(
            named_value_const_dependencies_complete,
            SemanticDependencySurface::NamedValueConst,
            SemanticDependencyIncompleteReason::ConstEndpointUnavailable,
        );
        let mut body_dependencies = Vec::new();
        for (owner, generic) in &analyzed_body_owners {
            let fingerprint = definition_fingerprints
                .iter()
                .find(|fingerprint| &fingerprint.key == owner)
                .cloned()
                .ok_or_else(|| {
                    invalid_dependency_manifest(
                        "analyzed body owner is absent from definition fingerprints",
                    )
                })?;
            let mut direct_dependencies = Vec::new();
            if let Some(payload) = semantic.as_ref().ok().and_then(|semantic| {
                semantic
                    .durable_ordinary_body_payloads()
                    .iter()
                    .find(|payload| &payload.owner == owner)
            }) {
                // Imported bodies intentionally skip source analysis and its
                // declaration-type observers. The durable AIR is already
                // joined to the same authoritative definition universe, so
                // retain every named type/callable it embeds as direct input.
                direct_dependencies.extend(payload.referenced_definition_keys());
            }
            direct_dependencies.extend(
                free_function_dependencies
                    .iter()
                    .filter(|edge| &edge.caller == owner)
                    .map(|edge| edge.callee.clone()),
            );
            for edge in named_method_dependencies
                .iter()
                .filter(|edge| &edge.caller == owner)
            {
                direct_dependencies.push(match &edge.target {
                    StableNamedMethodDependencyTarget::FreeFunction(target)
                    | StableNamedMethodDependencyTarget::NamedMethod(target) => target.clone(),
                });
            }
            for edge in named_destructor_dependencies
                .iter()
                .filter(|edge| &edge.caller == owner)
            {
                direct_dependencies.push(match &edge.target {
                    StableNamedMethodDependencyTarget::FreeFunction(target)
                    | StableNamedMethodDependencyTarget::NamedMethod(target) => target.clone(),
                });
            }
            direct_dependencies.extend(
                implicit_named_destructor_dependencies
                    .iter()
                    .filter(|edge| &edge.source == owner)
                    .map(|edge| edge.target.clone()),
            );
            direct_dependencies.extend(
                declaration_type_dependencies
                    .iter()
                    .filter(|edge| &edge.source == owner)
                    .map(|edge| edge.target.clone()),
            );
            direct_dependencies.extend(
                declaration_type_call_head_dependencies
                    .iter()
                    .filter(|edge| &edge.source == owner)
                    .map(|edge| edge.callable.clone()),
            );
            direct_dependencies.extend(
                body_named_dependencies
                    .iter()
                    .filter(|(source, _)| source == owner)
                    .map(|(_, target)| target.clone()),
            );
            direct_dependencies.sort();
            direct_dependencies.dedup();
            let direct_dependency_inputs = direct_dependencies
                .into_iter()
                .map(|dependency| {
                    definition_fingerprints
                        .iter()
                        .find(|fingerprint| fingerprint.key == dependency)
                        .cloned()
                        .ok_or_else(|| {
                            invalid_dependency_manifest(
                                "body dependency is absent from definition fingerprints",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let builtin_inputs = builtin_type_call_head_inputs
                .iter()
                .filter(|input| &input.source == owner)
                .cloned()
                .collect::<Vec<_>>();
            let mut blockers = whole_graph_body_blockers
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            if *generic {
                blockers.push(SemanticDependencyBlocker {
                    owner: Some(owner.clone()),
                    surface: SemanticDependencySurface::GenericNamedMethodCall,
                    reason:
                        SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
                });
            }
            blockers.sort();
            blockers.dedup();
            body_dependencies.push(StableBodyDependencyInputRecord {
                owner: owner.clone(),
                fingerprint,
                target: input.target,
                preview_features: input.preview_features.clone(),
                direct_dependency_inputs: direct_dependency_inputs.into(),
                builtin_type_call_heads: builtin_inputs.into(),
                blockers: blockers.into(),
            });
        }
        body_dependencies.sort_by(|left, right| left.owner.cmp(&right.owner));
        let mut body_dependency_blockers = body_dependencies
            .iter()
            .flat_map(|record| record.blockers.iter().cloned())
            .collect::<Vec<_>>();
        if anonymous_body_owners != 0 {
            body_dependency_blockers.push(SemanticDependencyBlocker {
                owner: None,
                surface: SemanticDependencySurface::BodyOwner,
                reason: SemanticDependencyIncompleteReason::AnonymousBodyOwnerUnavailable,
            });
        }
        body_dependency_blockers.sort();
        body_dependency_blockers.dedup();
        let mut durable_body_work = crate::DurableBodyWork::default();
        let durable_declarations = self
            .queries
            .semantic
            .last_good_record()
            .and_then(|entry| entry.durable_declaration_cache.as_ref())
            .map(|cache| cache.semantics.clone());
        #[cfg(test)]
        let durable_declarations = self
            .durable_declaration_cache
            .as_ref()
            .map(|cache| cache.semantics.clone())
            .or(durable_declarations);
        let durable_ordinary_bodies = match &semantic {
            Ok(semantic) => match crate::finalize_durable_ordinary_bodies(
                semantic.durable_ordinary_body_payloads(),
                &body_dependencies,
                &mut durable_body_work,
            ) {
                Ok(candidates) if candidates.is_empty() => candidates,
                Ok(candidates) => match durable_declarations {
                    None => {
                        durable_body_work.atomic_discards += 1;
                        Arc::from([])
                    }
                    Some(declarations) => {
                        match crate::import_durable_declaration_semantics(&declarations) {
                            Ok(epoch) => {
                                let mut installed_instructions = 0;
                                let mut installed_places = 0;
                                let mut installed_strings = 0;
                                let mut failed = false;
                                for candidate in candidates.iter() {
                                    let dto = match candidate
                                        .project_semantic_body(&mut durable_body_work)
                                    {
                                        Ok(dto) => dto,
                                        Err(reason) => {
                                            durable_body_work.last_projection_failure =
                                                Some(reason);
                                            durable_body_work.atomic_discards += 1;
                                            failed = true;
                                            break;
                                        }
                                    };
                                    let owner_records = definition_records
                                        .iter()
                                        .filter(|record| record.stable_key() == candidate.owner())
                                        .collect::<Vec<_>>();
                                    let [owner_record] = owner_records.as_slice() else {
                                        durable_body_work.record_import_failure(
                                            rue_air::SemanticBodyImportFailureKind::StructuralValidation,
                                            1,
                                        );
                                        durable_body_work.atomic_discards += 1;
                                        failed = true;
                                        break;
                                    };
                                    let Some(body_span) = owner_record.body_span() else {
                                        durable_body_work.record_import_failure(
                                            rue_air::SemanticBodyImportFailureKind::StructuralValidation,
                                            1,
                                        );
                                        durable_body_work.atomic_discards += 1;
                                        failed = true;
                                        break;
                                    };
                                    durable_body_work.import_attempts += 1;
                                    match epoch.import_body(&dto, body_span) {
                                        Ok(imported) => {
                                            durable_body_work.import_successes += 1;
                                            installed_instructions += imported.air.len();
                                            installed_places += imported.air.places().len();
                                            installed_strings += imported.strings.len();
                                        }
                                        Err(reason) => {
                                            durable_body_work
                                                .record_import_failure(reason.kind(), 1);
                                            durable_body_work.atomic_discards += 1;
                                            failed = true;
                                            break;
                                        }
                                    }
                                }
                                if failed {
                                    durable_body_work.installed_instructions +=
                                        installed_instructions;
                                    durable_body_work.installed_places += installed_places;
                                    durable_body_work.installed_strings += installed_strings;
                                    Arc::from([])
                                } else {
                                    durable_body_work.installed_instructions +=
                                        installed_instructions;
                                    durable_body_work.installed_places += installed_places;
                                    durable_body_work.installed_strings += installed_strings;
                                    candidates
                                }
                            }
                            Err(reason) => {
                                durable_body_work.record_import_failure(
                                    rue_air::SemanticBodyImportFailureKind::Semantic(reason),
                                    1,
                                );
                                durable_body_work.atomic_discards += 1;
                                Arc::from([])
                            }
                        }
                    }
                },
                Err(_) => Arc::from([]),
            },
            Err(_) => Arc::from([]),
        };
        let work = SemanticDependencyManifestWork {
            definition_records_visited: partial_work.definition_records_visited,
            import_records_visited: partial_work.import_records_visited,
            free_function_events_translated,
            specialization_origins_validated,
            named_method_events_translated,
            named_destructor_events_translated,
            declaration_type_events_translated,
            declaration_type_call_head_events_translated,
            builtin_type_call_head_inputs_translated,
            named_const_events_translated,
            implicit_named_destructor_events_translated,
            body_owner_events_translated: analyzed_body_owners.len() + anonymous_body_owners,
            body_named_events_translated: body_named_dependencies.len(),
            body_dependency_records_built: body_dependencies.len(),
            durable_bodies: durable_body_work,
            extra_rir_instructions_visited: 0,
        };
        *attempt_work = Some(work);
        guard.accrue(QueryStructuralWork::DependencyManifest(Box::new(work)));
        let module_imports = imports
            .graph()
            .records()
            .iter()
            .map(|record| match record.resolution() {
                CanonicalImportResolution::Resolved(target) => {
                    StableModuleImportDependency::Resolved {
                        importer: record.importer().clone(),
                        normalized_specifier: Arc::from(record.normalized_specifier()),
                        target: target.clone(),
                    }
                }
                CanonicalImportResolution::Missing => StableModuleImportDependency::Missing {
                    importer: record.importer().clone(),
                    normalized_specifier: Arc::from(record.normalized_specifier()),
                },
                CanonicalImportResolution::Ambiguous {
                    file_module,
                    directory_module,
                } => StableModuleImportDependency::Ambiguous {
                    importer: record.importer().clone(),
                    normalized_specifier: Arc::from(record.normalized_specifier()),
                    file_module: file_module.clone(),
                    directory_module: directory_module.clone(),
                },
            })
            .collect::<Vec<_>>();
        let mut dependency_blockers = whole_graph_body_blockers;
        let mut block = |complete: bool,
                         surface: SemanticDependencySurface,
                         reason: SemanticDependencyIncompleteReason| {
            if !complete {
                dependency_blockers.insert(SemanticDependencyBlocker {
                    owner: None,
                    surface,
                    reason,
                });
            }
        };
        block(
            free_function_caller_dependencies_complete,
            SemanticDependencySurface::FreeFunctionCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block(
            named_method_dependencies_complete,
            SemanticDependencySurface::NonGenericNamedMethodCall,
            SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        );
        block(
            generic_named_method_dependencies_complete,
            SemanticDependencySurface::GenericNamedMethodCall,
            SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
        );
        block(
            named_destructor_dependencies_complete,
            SemanticDependencySurface::NamedDestructorCall,
            SemanticDependencyIncompleteReason::DestructorEndpointUnavailable,
        );
        block(
            implicit_named_destructor_dependencies_complete,
            SemanticDependencySurface::ImplicitNamedDestructor,
            SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable,
        );
        block(
            declaration_type_dependencies_complete,
            SemanticDependencySurface::DeclarationType,
            SemanticDependencyIncompleteReason::ResolvedTypeIdentityUnavailable,
        );
        block(
            declaration_type_call_head_dependencies_complete,
            SemanticDependencySurface::DeclarationTypeCallHead,
            SemanticDependencyIncompleteReason::TypeCallHeadIdentityUnavailable,
        );
        block(
            supported_type_call_heads_complete,
            SemanticDependencySurface::SupportedTypeCallHead,
            SemanticDependencyIncompleteReason::UnsupportedDynamicTypeCallHead,
        );
        block(
            named_value_const_dependencies_complete,
            SemanticDependencySurface::NamedValueConst,
            SemanticDependencyIncompleteReason::ConstEndpointUnavailable,
        );
        let manifest = Arc::new(SemanticDependencyInputManifest {
            input: input.clone(),
            imports: imports.graph().clone(),
            definitions: keys.into(),
            definition_fingerprints: definition_fingerprints.into(),
            module_imports: module_imports.into(),
            free_function_dependencies: free_function_dependencies.into(),
            named_method_dependencies: named_method_dependencies.into(),
            named_destructor_dependencies: named_destructor_dependencies.into(),
            implicit_named_destructor_dependencies: implicit_named_destructor_dependencies.into(),
            declaration_type_dependencies: declaration_type_dependencies.into(),
            declaration_type_call_head_dependencies: declaration_type_call_head_dependencies.into(),
            builtin_type_call_head_inputs: builtin_type_call_head_inputs.into(),
            named_const_dependencies: named_const_dependencies.into(),
            body_dependencies: body_dependencies.into(),
            durable_ordinary_bodies,
            durable_body_candidate_state: DurableBodyCandidateState::from_blockers(
                body_dependency_blockers,
            ),
            dependency_graph_state: SemanticDependencyGraphState::from_blockers(
                dependency_blockers.into_iter().collect(),
            ),
            definition_universe_state,
            work,
        });
        let semantic_key = SemanticQueryKey {
            input: CodegenInputDescriptor {
                semantic: input.clone(),
                opt_level: options.opt_level.into(),
            },
            imports: imports.graph().clone(),
        };
        let definition_key = DefinitionQueryKey {
            input: input.clone(),
            imports: imports.graph().clone(),
        };
        let mut dependencies = vec![
            self.queries
                .source_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .import_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .target_inputs
                .selected(&self.queries.graph)
                .unwrap(),
            self.queries
                .preview_inputs
                .selected(&self.queries.graph)
                .unwrap(),
        ];
        if let Some((_, semantic_handle)) = self.queries.semantic.get_with_handle(&semantic_key) {
            dependencies.push(semantic_handle.observed());
        } else if let Some((_, merge_handle)) =
            self.queries.merge.get_secondary_with_handle(&input.sources)
        {
            // Semantic requests rejected by merge/RIR do not publish a
            // semantic terminal. Retain the actual failing upstream terminal
            // read by this incomplete manifest instead.
            dependencies.push(merge_handle.observed());
        }
        if let Some(definition_handle) = self.queries.definitions.handle(&definition_key) {
            dependencies.push(definition_handle.observed());
        }
        let handle = self
            .queries
            .manifests
            .publish_selected_attempt(
                &mut self.queries.graph,
                DependencyManifestCacheEntry {
                    key,
                    result: Ok(manifest.clone()),
                },
                dependencies,
                guard.structural(),
                *execution,
            )
            .unwrap_or_else(query_control_error);
        guard.bind(handle.as_view());
        self.refresh_retention_metrics();
        Ok(manifest)
    }

    /// Compare two immutable semantic manifests without lowering or scanning RIR.
    ///
    /// Supported production manifests with complete dependency capture can produce
    /// an incremental invalidation plan. Unsupported dependency surfaces, incomplete
    /// capture, and global semantic-input changes fail closed to full invalidation.
    pub(crate) fn semantic_invalidation_plan(
        &mut self,
        previous: &Arc<SemanticDependencyInputManifest>,
        current: &Arc<SemanticDependencyInputManifest>,
    ) -> Arc<SemanticInvalidationPlan> {
        let mut guard = self.metrics.begin::<InvalidationPlanQuery>();
        let attempt_id = guard.id;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let key = InvalidationPlanQueryKey {
                previous: previous.clone(),
                current: current.clone(),
            };
            if let Some((entry, handle)) = self
                .queries
                .invalidation_plans
                .request_selected(&mut self.queries.graph, key.clone(), attempt_id.0)
                .expect("invalidation planning cannot recursively request itself")
            {
                guard.bind(handle.as_view());
                return (
                    entry.plan.clone(),
                    QueryAttemptExecution::Reused,
                    Some(handle.origin_attempt_id()),
                    QueryStructuralWork::None,
                );
            }
            guard.started();
            let plan = Arc::new(plan_semantic_invalidation(previous, current));
            let structural = QueryStructuralWork::Invalidation(plan.work());
            guard.accrue(structural.clone());
            let mut dependencies = Vec::new();
            for manifest in [previous, current] {
                let manifest_key = DependencyManifestQueryKey {
                    input: manifest.input.clone(),
                    imports: manifest.imports.clone(),
                };
                if let Some(handle) = self.queries.manifests.handle(&manifest_key) {
                    dependencies.push(handle.observed());
                }
            }
            let handle = self
                .queries
                .invalidation_plans
                .publish_selected_attempt(
                    &mut self.queries.graph,
                    InvalidationPlanCacheEntry {
                        key,
                        plan: plan.clone(),
                    },
                    dependencies,
                    guard.structural(),
                    QueryAttemptExecution::Computed,
                )
                .unwrap_or_else(query_control_error);
            guard.bind(handle.as_view());
            self.refresh_retention_metrics();
            (plan, QueryAttemptExecution::Computed, None, structural)
        }));
        let (plan, execution, origin, structural) = match result {
            Ok(result) => result,
            Err(payload) => self.resume_canceled_query(&mut guard, payload),
        };
        guard.finish(execution, origin, &Ok::<(), ()>(()), structural);
        self.metrics.synchronize();
        plan
    }

    pub fn unstable_invalidation_metrics(
        &mut self,
        previous: &crate::unstable::DependencyBaseline,
        current: &crate::unstable::DependencyBaseline,
    ) -> Result<crate::unstable::InvalidationMetrics, CompileErrors> {
        if !previous.belongs_to(&self.identity) || !current.belongs_to(&self.identity) {
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::InvalidCompilerInput(
                    "dependency baselines belong to a different compiler session".into(),
                ),
            )));
        }
        let plan = self.semantic_invalidation_plan(&previous.inner, &current.inner);
        Ok(crate::unstable::InvalidationMetrics::from_plan(&plan))
    }
}

fn plan_semantic_invalidation(
    previous: &SemanticDependencyInputManifest,
    current: &SemanticDependencyInputManifest,
) -> SemanticInvalidationPlan {
    let previous_fingerprints = previous
        .definition_fingerprints
        .iter()
        .map(|entry| (entry.key.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let current_fingerprints = current
        .definition_fingerprints
        .iter()
        .map(|entry| (entry.key.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut work = SemanticInvalidationWork::default();
    let mut added = BTreeSet::new();
    let mut removed = BTreeSet::new();
    let mut changed = BTreeSet::new();
    for (key, fingerprint) in &current_fingerprints {
        match previous_fingerprints.get(key) {
            None => {
                added.insert(key.clone());
            }
            Some(previous) => {
                work.definition_fingerprints_compared += 1;
                if *previous != *fingerprint {
                    changed.insert(key.clone());
                }
            }
        }
    }
    for key in previous_fingerprints.keys() {
        if !current_fingerprints.contains_key(key) {
            removed.insert(key.clone());
        }
    }

    let mut reasons = BTreeSet::new();
    if previous.input.sources.root() != current.input.sources.root() {
        reasons.insert(SemanticFullInvalidationReason::RootChanged);
    }
    if previous.module_imports != current.module_imports {
        reasons.insert(SemanticFullInvalidationReason::ModuleImportsChanged);
    }
    if previous.input.target != current.input.target {
        reasons.insert(SemanticFullInvalidationReason::TargetChanged);
    }
    if previous.input.preview_features != current.input.preview_features {
        reasons.insert(SemanticFullInvalidationReason::PreviewFeaturesChanged);
    }
    for state in [
        &previous.definition_universe_state,
        &current.definition_universe_state,
    ] {
        match state {
            SemanticDefinitionUniverseState::Complete => {}
            SemanticDefinitionUniverseState::Incomplete(reason) => match reason {
                SemanticDefinitionUniverseIncompleteReason::StableDefinitionsFailed(failures) => {
                    assert!(!failures.failures.is_empty());
                    reasons.insert(SemanticFullInvalidationReason::IncompleteDefinitionUniverse);
                }
            },
        }
    }
    let mut dependency_blockers = BTreeSet::new();
    for graph in [
        &previous.dependency_graph_state,
        &current.dependency_graph_state,
    ] {
        graph.fold_planning_blockers(&mut dependency_blockers);
    }
    if !dependency_blockers.is_empty() {
        reasons.insert(SemanticFullInvalidationReason::IncompleteDependencyGraph(
            dependency_blockers.into_iter().collect::<Vec<_>>().into(),
        ));
    }

    let mut invalidated = BTreeSet::new();
    let mut reusable = BTreeSet::new();
    let scope = if reasons.is_empty() {
        invalidated.extend(added.iter().cloned());
        invalidated.extend(removed.iter().cloned());
        invalidated.extend(changed.iter().cloned());
        let mut reverse = BTreeMap::<StableDefinitionKey, BTreeSet<StableDefinitionKey>>::new();
        collect_reverse_dependencies(previous, &mut reverse, &mut work);
        collect_reverse_dependencies(current, &mut reverse, &mut work);
        let mut queue = invalidated.iter().cloned().collect::<VecDeque<_>>();
        while let Some(key) = queue.pop_front() {
            work.reverse_closure_nodes_visited += 1;
            if let Some(dependents) = reverse.get(&key) {
                for dependent in dependents {
                    if invalidated.insert(dependent.clone()) {
                        queue.push_back(dependent.clone());
                    }
                }
            }
        }
        reusable.extend(
            current_fingerprints
                .keys()
                .filter(|key| !invalidated.contains(*key))
                .cloned(),
        );
        SemanticInvalidationScope::Incremental
    } else {
        SemanticInvalidationScope::Full {
            reasons: reasons.into_iter().collect::<Vec<_>>().into(),
        }
    };
    SemanticInvalidationPlan {
        scope,
        added: added.into_iter().collect::<Vec<_>>().into(),
        removed: removed.into_iter().collect::<Vec<_>>().into(),
        changed: changed.into_iter().collect::<Vec<_>>().into(),
        invalidated: invalidated.into_iter().collect::<Vec<_>>().into(),
        reusable: reusable.into_iter().collect::<Vec<_>>().into(),
        work,
    }
}

fn collect_reverse_dependencies(
    manifest: &SemanticDependencyInputManifest,
    reverse: &mut BTreeMap<StableDefinitionKey, BTreeSet<StableDefinitionKey>>,
    work: &mut SemanticInvalidationWork,
) {
    let mut add = |source: &StableDefinitionKey, target: &StableDefinitionKey| {
        work.dependency_edges_visited += 1;
        reverse
            .entry(target.clone())
            .or_default()
            .insert(source.clone());
    };
    for edge in manifest.free_function_dependencies.iter() {
        add(&edge.caller, &edge.callee);
    }
    for edge in manifest.named_method_dependencies.iter() {
        let target = match &edge.target {
            StableNamedMethodDependencyTarget::FreeFunction(key)
            | StableNamedMethodDependencyTarget::NamedMethod(key) => key,
        };
        add(&edge.caller, target);
    }
    for edge in manifest.named_destructor_dependencies.iter() {
        let target = match &edge.target {
            StableNamedMethodDependencyTarget::FreeFunction(key)
            | StableNamedMethodDependencyTarget::NamedMethod(key) => key,
        };
        add(&edge.caller, target);
    }
    for edge in manifest.implicit_named_destructor_dependencies.iter() {
        add(&edge.source, &edge.target);
    }
    for edge in manifest.declaration_type_dependencies.iter() {
        add(&edge.source, &edge.target);
    }
    for edge in manifest.declaration_type_call_head_dependencies.iter() {
        add(&edge.source, &edge.callable);
    }
    for edge in manifest.named_const_dependencies.iter() {
        let target = match &edge.target {
            StableNamedConstDependencyTarget::ValueConst(key)
            | StableNamedConstDependencyTarget::FreeFunction(key)
            | StableNamedConstDependencyTarget::NamedType(key)
            | StableNamedConstDependencyTarget::ModuleBinding(key) => key,
        };
        add(&edge.source, target);
    }
}

const DEFINITION_FINGERPRINT_SCHEMA_V2: u16 = 2;
const DEFINITION_DECLARATION_DOMAIN_V2: &[u8] = b"rue.definition.declaration\0v2\0sha256\0";
const DEFINITION_SIGNATURE_DOMAIN_V2: &[u8] = b"rue.definition.signature\0v2\0sha256\0";
const DEFINITION_BODY_DOMAIN_V2: &[u8] = b"rue.definition.body-or-initializer\0v2\0sha256\0";

fn declaration_shell_failure_diagnostics(
    program: &crate::canonical_merge::CanonicalMergedAst,
    failure: &crate::declaration_candidate::DeclarationShellFailure,
) -> CompileErrors {
    use crate::declaration_candidate::DeclarationShellFailure as F;
    let key = match failure {
        F::Absent(key) | F::Ambiguous(key) | F::ParserCapabilityMismatch(key) => Some(key),
        F::OccurrencesUnavailable(_) => None,
    };
    let span = key.and_then(|key| {
        program
            .modules()
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
    program: &crate::canonical_merge::CanonicalMergedAst,
    declaration: Option<&crate::declaration_candidate::DeclarationCandidateKey>,
    failure: &crate::semantic_query_nucleus::SemanticNucleusFailure,
) -> CompileErrors {
    use crate::semantic_query_nucleus::SemanticNucleusFailure as F;
    if let (Some(declaration), F::DiagnosticAtParameter { kind, ordinal }) = (declaration, failure)
        && let Some(module) = program
            .modules()
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
        && let Some(span) = program
            .modules()
            .iter()
            .find(|module| module.module_id() == &declaration.module)
            .and_then(|module| module.definitions().declaration_locator(declaration))
            .map(|locator| locator.declaration_span)
    {
        return CompileErrors::from(CompileError::new(kind.clone(), span));
    }
    if let F::OwnershipGate { kind, gate } = failure {
        let primary_span = declaration.and_then(|key| {
            program
                .modules()
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
            && let Some(span) = program
                .modules()
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
        && let Some(module) = program
            .modules()
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
        program
            .modules()
            .iter()
            .find(|module| module.module_id() == &key.module)
            .and_then(|module| module.definitions().declaration_locator(key))
            .map(|locator| locator.declaration_span)
    });
    let (kind, help) = match failure {
        F::Diagnostic(kind) => (kind.clone(), None),
        F::DiagnosticAtParameter { kind, .. } => (kind.clone(), None),
        F::DiagnosticAtDeclaration { kind, .. } => (kind.clone(), None),
        F::OwnershipGate { kind, .. } => (kind.clone(), None),
        F::DiagnosticWithHelp { kind, help } => (kind.clone(), Some(help.clone())),
        F::Cycle(nodes) => (
            ErrorKind::ConstInitializerCycle {
                cycle: nodes
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            },
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
        ),
        F::Resolution(message) => (
            ErrorKind::ComptimeEvaluationFailed {
                reason: message.to_string(),
            },
            None,
        ),
        F::Shell(message) | F::Syntax(message) => (
            ErrorKind::InternalError(format!("semantic query invariant failed: {message}")),
            None,
        ),
    };
    let error = match span {
        Some(span) => CompileError::new(kind, span),
        None => CompileError::without_span(kind),
    };
    CompileErrors::from(match help {
        Some(help) => error.with_help(help.to_string()),
        None => error,
    })
}

pub(crate) fn stable_definition_input_fingerprint(
    snapshot: &SourceSnapshot,
    record: &crate::BoundDefinitionRecord,
) -> Result<StableDefinitionInputFingerprint, CompileErrors> {
    stable_definition_input_fingerprint_parts(
        snapshot,
        record.stable_key(),
        record.visibility(),
        record.input_partition(),
    )
}

fn stable_definition_input_fingerprint_parts(
    snapshot: &SourceSnapshot,
    key: &StableDefinitionKey,
    visibility: Option<rue_parser::ast::Visibility>,
    partition: crate::bound_definitions::BoundDefinitionInputPartition,
) -> Result<StableDefinitionInputFingerprint, CompileErrors> {
    let source_fragment = |span: Span| -> Result<&str, CompileErrors> {
        let source = snapshot.source_text(span.file_id).ok_or_else(|| {
            invalid_dependency_manifest("definition fingerprint span references an absent source")
        })?;
        let start = usize::try_from(span.start).map_err(|_| {
            invalid_dependency_manifest(
                "definition fingerprint span start cannot address this host",
            )
        })?;
        let end = usize::try_from(span.end).map_err(|_| {
            invalid_dependency_manifest("definition fingerprint span end cannot address this host")
        })?;
        source.get(start..end).ok_or_else(|| {
            invalid_dependency_manifest(
                "definition fingerprint span is reversed, out of bounds, or not on UTF-8 boundaries",
            )
        })
    };

    let mut declaration = FramedDefinitionHasher::new(DEFINITION_DECLARATION_DOMAIN_V2);
    hash_stable_definition_key(&mut declaration, key);
    declaration.frame(&[match visibility {
        None => 0,
        Some(rue_parser::ast::Visibility::Private) => 1,
        Some(rue_parser::ast::Visibility::Public) => 2,
    }]);
    let (signature_spans, payload_span, precision) = match partition {
        crate::bound_definitions::BoundDefinitionInputPartition::Body { signature, body } => (
            vec![signature],
            Some(body),
            StableDefinitionFingerprintPrecision::SignatureAndBody,
        ),
        crate::bound_definitions::BoundDefinitionInputPartition::Initializer {
            signature,
            initializer,
        } => (
            vec![signature],
            Some(initializer),
            StableDefinitionFingerprintPrecision::SignatureAndInitializer,
        ),
        crate::bound_definitions::BoundDefinitionInputPartition::ExactSignature(spans) => (
            spans.to_vec(),
            None,
            StableDefinitionFingerprintPrecision::ExactSignature,
        ),
    };
    let mut signature = FramedDefinitionHasher::new(DEFINITION_SIGNATURE_DOMAIN_V2);
    for span in signature_spans {
        signature.frame(source_fragment(span)?.as_bytes());
    }
    let body_or_initializer = payload_span
        .map(|span| {
            let mut payload = FramedDefinitionHasher::new(DEFINITION_BODY_DOMAIN_V2);
            payload.frame(source_fragment(span)?.as_bytes());
            Ok::<_, CompileErrors>(payload.finish())
        })
        .transpose()?;
    Ok(StableDefinitionInputFingerprint {
        schema_version: DEFINITION_FINGERPRINT_SCHEMA_V2,
        key: key.clone(),
        declaration: declaration.finish(),
        signature: signature.finish(),
        body_or_initializer,
        precision,
    })
}

struct FramedDefinitionHasher(Sha256);

impl FramedDefinitionHasher {
    fn new(domain: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        Self(hasher)
    }

    fn frame(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    fn finish(self) -> StableDefinitionFingerprint {
        StableDefinitionFingerprint(self.0.finalize().into())
    }
}

fn hash_stable_definition_key(hasher: &mut FramedDefinitionHasher, key: &StableDefinitionKey) {
    hasher.frame(key.module().as_str().as_bytes());
    hasher.frame(&[stable_namespace_tag(key.namespace())]);
    hasher.frame(&[stable_kind_tag(key.kind())]);
    hasher.frame(key.name().as_bytes());
    match key.owner() {
        None => hasher.frame(&[]),
        Some(owner) => {
            hasher.frame(&[1]);
            hasher.frame(owner.module().as_str().as_bytes());
            hasher.frame(&[stable_kind_tag(owner.kind())]);
            hasher.frame(owner.name().as_bytes());
        }
    }
}

fn stable_namespace_tag(namespace: StableDefinitionNamespace) -> u8 {
    match namespace {
        StableDefinitionNamespace::Value => 0,
        StableDefinitionNamespace::Type => 1,
        StableDefinitionNamespace::Destructor => 2,
        StableDefinitionNamespace::Method => 3,
    }
}

fn stable_kind_tag(kind: StableDefinitionKind) -> u8 {
    match kind {
        StableDefinitionKind::Function => 0,
        StableDefinitionKind::Struct => 1,
        StableDefinitionKind::Enum => 2,
        StableDefinitionKind::ValueConst => 3,
        StableDefinitionKind::ModuleBinding => 4,
        StableDefinitionKind::Destructor => 5,
        StableDefinitionKind::Method => 6,
        StableDefinitionKind::AssociatedFunction => 7,
    }
}

fn stable_free_function_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            record.declaration_span().file_id.index() == file
                && record.stable_key().name() == name
                && record.stable_key().namespace() == StableDefinitionNamespace::Value
                && record.stable_key().kind() == StableDefinitionKind::Function
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "free-function dependency endpoint ({file}, '{name}') did not join exactly one bound function",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_body_owner_endpoint(
    semantic: &CanonicalSemanticOutput,
    definitions: &BoundDefinitionSet,
    event: &rue_air::AnalyzedBodyOwnerEvent,
) -> Result<Option<(StableDefinitionKey, bool)>, CompileErrors> {
    let (token, provenance, generic) = match event {
        rue_air::AnalyzedBodyOwnerEvent::FreeFunction { token, file, name } => (
            *token,
            stable_free_function_endpoint(definitions, *file, name)?,
            false,
        ),
        rue_air::AnalyzedBodyOwnerEvent::NamedMethod {
            token,
            file,
            owner_name,
            method_name,
            generic,
        } => (
            *token,
            stable_named_method_endpoint(definitions, *file, owner_name, method_name)?,
            *generic,
        ),
        rue_air::AnalyzedBodyOwnerEvent::NamedDestructor {
            token,
            file,
            owner_name,
        } => (
            *token,
            stable_named_destructor_endpoint(definitions, *file, owner_name)?,
            false,
        ),
        rue_air::AnalyzedBodyOwnerEvent::Anonymous => return Ok(None),
    };
    let authoritative = semantic
        .body_owner_issuer()
        .key_for_body_token(token)
        .map_err(CompileErrors::from)?;
    if authoritative != &provenance {
        return Err(invalid_dependency_manifest(
            "body owner token does not match its checked source provenance",
        ));
    }
    Ok(Some((authoritative.clone(), generic)))
}

fn stable_token_endpoint(
    semantic: &CanonicalSemanticOutput,
    token: rue_air::BodyOwnerToken,
    provenance: &StableDefinitionKey,
) -> Result<StableDefinitionKey, CompileErrors> {
    let authoritative = semantic
        .body_owner_issuer()
        .key_for_body_token(token)
        .map_err(CompileErrors::from)?;
    if authoritative != provenance {
        return Err(invalid_dependency_manifest(
            "body-local observation token does not match its checked source provenance",
        ));
    }
    Ok(authoritative.clone())
}

fn stable_implicit_drop_source_endpoint(
    semantic: &CanonicalSemanticOutput,
    definitions: &BoundDefinitionSet,
    source: &rue_air::ImplicitDropDependencySourceEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    match source {
        rue_air::ImplicitDropDependencySourceEvent::Anonymous => Err(invalid_dependency_manifest(
            "anonymous drop-dependency source has no stable endpoint",
        )),
        rue_air::ImplicitDropDependencySourceEvent::Specialization { .. } => {
            Err(invalid_dependency_manifest(
                "specialized drop-dependency source requires specialization identity",
            ))
        }
        rue_air::ImplicitDropDependencySourceEvent::FreeFunction { token, file, name } => {
            let provenance = stable_free_function_endpoint(definitions, *file, name)?;
            stable_token_endpoint(semantic, *token, &provenance)
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedMethod {
            token,
            file,
            owner_name,
            method_name,
        } => {
            let provenance =
                stable_named_method_endpoint(definitions, *file, owner_name, method_name)?;
            stable_token_endpoint(semantic, *token, &provenance)
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedDestructor {
            token,
            file,
            owner_name,
        } => {
            let provenance = stable_named_destructor_endpoint(definitions, *file, owner_name)?;
            stable_token_endpoint(semantic, *token, &provenance)
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedStruct { file, name } => {
            stable_top_level_endpoint(
                definitions,
                *file,
                name,
                StableDefinitionNamespace::Type,
                StableDefinitionKind::Struct,
            )
        }
        rue_air::ImplicitDropDependencySourceEvent::NamedEnum { file, name } => {
            stable_top_level_endpoint(
                definitions,
                *file,
                name,
                StableDefinitionNamespace::Type,
                StableDefinitionKind::Enum,
            )
        }
    }
}

fn stable_named_method_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    owner_name: &str,
    method_name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            let key = record.stable_key();
            record.declaration_span().file_id.index() == file
                && key.name() == method_name
                && key.namespace() == StableDefinitionNamespace::Method
                && matches!(
                    key.kind(),
                    StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction
                )
                && key.owner().is_some_and(|owner| owner.name() == owner_name)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "named-method dependency endpoint ({file}, '{owner_name}', '{method_name}') did not join exactly one bound method",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_named_destructor_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    owner_name: &str,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            let key = record.stable_key();
            record.declaration_span().file_id.index() == file
                && key.name() == owner_name
                && key.namespace() == StableDefinitionNamespace::Destructor
                && key.kind() == StableDefinitionKind::Destructor
                && key.owner().is_some_and(|owner| owner.name() == owner_name)
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(&format!(
            "named-destructor dependency endpoint ({file}, '{owner_name}') did not join exactly one bound destructor",
        )));
    };
    Ok(record.stable_key().clone())
}

fn stable_declaration_source_endpoint(
    definitions: &BoundDefinitionSet,
    event: &rue_air::DeclarationTypeDependencyEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    stable_declaration_type_source_endpoint(
        definitions,
        event.source_file,
        &event.source_name,
        event.source_owner_name.as_deref(),
        event.source_kind,
    )
}

fn stable_declaration_type_source_endpoint(
    definitions: &BoundDefinitionSet,
    source: u32,
    source_name: &str,
    source_owner_name: Option<&str>,
    source_kind: rue_air::DeclarationTypeDependencySourceKind,
) -> Result<StableDefinitionKey, CompileErrors> {
    use rue_air::DeclarationTypeDependencySourceKind as K;
    match source_kind {
        K::Function => stable_free_function_endpoint(definitions, source, source_name),
        K::Method | K::AssociatedFunction => stable_named_method_endpoint(
            definitions,
            source,
            source_owner_name.unwrap_or(""),
            source_name,
        ),
        K::Destructor => stable_named_destructor_endpoint(
            definitions,
            source,
            source_owner_name.unwrap_or(source_name),
        ),
        K::Struct => stable_top_level_endpoint(
            definitions,
            source,
            source_name,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Struct,
        ),
        K::Enum => stable_top_level_endpoint(
            definitions,
            source,
            source_name,
            StableDefinitionNamespace::Type,
            StableDefinitionKind::Enum,
        ),
        K::ValueConst => stable_top_level_endpoint(
            definitions,
            source,
            source_name,
            StableDefinitionNamespace::Value,
            StableDefinitionKind::ValueConst,
        ),
    }
}

fn stable_named_type_endpoint(
    definitions: &BoundDefinitionSet,
    event: &rue_air::DeclarationTypeDependencyEvent,
) -> Result<StableDefinitionKey, CompileErrors> {
    let kind = match event.target_kind {
        rue_air::DeclarationTypeDependencyTargetKind::Struct => StableDefinitionKind::Struct,
        rue_air::DeclarationTypeDependencyTargetKind::Enum => StableDefinitionKind::Enum,
        rue_air::DeclarationTypeDependencyTargetKind::ValueConst => {
            return stable_top_level_endpoint(
                definitions,
                event.target_file,
                &event.target_name,
                StableDefinitionNamespace::Value,
                StableDefinitionKind::ValueConst,
            );
        }
    };
    stable_top_level_endpoint(
        definitions,
        event.target_file,
        &event.target_name,
        StableDefinitionNamespace::Type,
        kind,
    )
}

fn stable_top_level_endpoint(
    definitions: &BoundDefinitionSet,
    file: u32,
    name: &str,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
) -> Result<StableDefinitionKey, CompileErrors> {
    let matches = definitions
        .definitions()
        .iter()
        .filter(|record| {
            record.declaration_span().file_id.index() == file
                && record.stable_key().name() == name
                && record.stable_key().namespace() == namespace
                && record.stable_key().kind() == kind
                && record.stable_key().owner().is_none()
        })
        .collect::<Vec<_>>();
    let [record] = matches.as_slice() else {
        return Err(invalid_dependency_manifest(
            "declaration-type dependency endpoint did not join exactly one stable definition",
        ));
    };
    Ok(record.stable_key().clone())
}

fn invalid_dependency_manifest(reason: &str) -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        reason.to_owned(),
    )))
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
    accepted_reads: &[crate::AcceptedReadManifestEntry],
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
    accepted_reads: &[crate::AcceptedReadManifestEntry],
    carried_ledger: &crate::ImportObservationLedger,
) -> bool {
    if previous.status != ImportDiscoveryRevisionStatus::Open || previous.context != *context {
        return false;
    }
    let Some(program) = previous.program.as_deref() else {
        return false;
    };
    if program.root() != snapshot.source_revision().root()
        || !program.modules().iter().all(|module| {
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
        .all(|entry| accepted_reads.iter().any(|candidate| candidate == entry))
    {
        return false;
    }
    previous
        .ledger
        .iter()
        .all(|observation| carried_ledger.get(observation.request()) == Some(observation))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    };

    use rue_span::FileId;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        CanonicalSemanticFailurePhase, LinkerMode, ModuleId, OptLevel, PreviewFeature,
        PreviewFeatures, SourceMetadata, SourceSnapshot, Target,
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
    fn legacy_import_authority_is_one_explicit_compatibility_boundary() {
        let discovery = include_str!("import_discovery.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap();
        let session = include_str!("session.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap();
        let gate = "RUE-1033 DELETION/REPLACEMENT GATE: retire the entire legacy supported";
        assert_eq!(discovery.matches(gate).count(), 1);
        assert_eq!(session.matches(gate).count(), 0);

        // ADR-0061 keeps this legacy host-driven surface stable for now. Every
        // listed item can participate in bypassing canonical
        // begin/frontier/publish freshness or speculation authority, so the
        // inventory must change atomically when RUE-1033 replaces it.
        for (surface, declaration) in [
            (
                "ImportObservationStatus construction",
                "pub enum ImportObservationStatus {",
            ),
            (
                "AcceptedImportSource::new",
                "pub fn new(\n        requested_path: impl Into<Arc<str>>",
            ),
            (
                "ImportObservation::absent",
                "pub fn absent(request: ImportDiscoveryRequest)",
            ),
            (
                "ImportObservation::accepted",
                "pub fn accepted(\n        request: ImportDiscoveryRequest",
            ),
            (
                "ImportObservation::failure",
                "pub fn failure(\n        request: ImportDiscoveryRequest",
            ),
            (
                "ImportObservationLedger::default",
                "derive(Debug, Clone, Default, PartialEq, Eq, Hash)",
            ),
            (
                "ImportObservationLedger::record",
                "pub fn record(&mut self, observation: ImportObservation)",
            ),
            (
                "ImportDiscoveryPlan::groups",
                "pub fn groups(&self) -> &[Arc<[ImportDiscoveryRequest]>]",
            ),
        ] {
            assert_eq!(
                discovery.matches(declaration).count(),
                1,
                "RUE-1033 discovery bypass inventory drifted at {surface}"
            );
        }
        for (surface, declaration) in [
            (
                "CompilerSession::import_discovery_plan",
                "pub fn import_discovery_plan(",
            ),
            (
                "CompilerSession::stage_import_discovery",
                "pub fn stage_import_discovery(",
            ),
            (
                "CompilerSession::close_import_discovery",
                "pub fn close_import_discovery(",
            ),
        ] {
            assert_eq!(
                session.matches(declaration).count(),
                1,
                "RUE-1033 session bypass inventory drifted at {surface}"
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
    fn caught_session_cancellation_is_immediately_observable_and_commits_no_cache() {
        let mut session = CompilerSession::new();
        session.update(&base()).into_result().unwrap();
        session.cancel_merge_before_commit = true;
        let canceled = session.merge();
        assert!(canceled.is_err());
        assert_eq!(session.queries.merge.len(), 0);
        assert_eq!(session.work().merge.calls, 1);
        assert_eq!(session.work().merge.executions, 1);
        let attempt = session
            .metrics
            .attempts()
            .into_iter()
            .find(|attempt| attempt.family == "merge")
            .unwrap();
        assert_eq!(
            attempt.attempt.outcome(),
            crate::typed_query_store::AttemptOutcomeKind::Aborted(AbortedQueryReason::Canceled)
        );
        assert_eq!(attempt.attempt.execution(), QueryAttemptExecution::Computed);
        // Parse is runtime-owned; the legacy Merge adapter observes only the
        // exact source leaf rather than recreating a peer parse graph node.
        assert_eq!(attempt.attempt.dependencies().len(), 1);
        assert!(attempt.attempt.diagnostics().is_none());
        let QueryStructuralWork::Merge(work) = attempt.attempt.work() else {
            panic!("canceled merge must retain exact prefix work");
        };
        assert_eq!(work.modules_visited, 2);
        assert_eq!(work.items_visited, 2);

        session.merge().unwrap();
        assert_eq!(session.queries.merge.len(), 1);
        assert_eq!(session.work().merge.calls, 2);
        assert_eq!(session.work().merge.executions, 2);
    }

    #[test]
    fn semantic_cancellation_is_control_flow_before_work_and_after_dependencies() {
        let source = base();
        let options = CompileOptions::default();

        let mut prework = CompilerSession::new();
        prework.update(&source).into_result().unwrap();
        let canceled = rue_query::CancellationToken::new();
        canceled.cancel();
        assert!(matches!(
            prework.canonical_semantic_with_cancellation(&options, canceled),
            Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled
            ))
        ));
        assert_eq!(prework.queries.semantic.len(), 0);
        prework.canonical_semantic(&options).unwrap();

        let mut after_dependency = CompilerSession::new();
        after_dependency.update(&source).into_result().unwrap();
        after_dependency.cancel_semantic_after_dependency = true;
        assert!(matches!(
            after_dependency.canonical_semantic_with_cancellation(
                &options,
                rue_query::CancellationToken::new(),
            ),
            Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled
            ))
        ));
        assert_eq!(after_dependency.queries.semantic.len(), 0);
        assert_eq!(after_dependency.queries.rir.len(), 1);
        after_dependency.canonical_semantic(&options).unwrap();
    }

    #[test]
    fn semantic_cancellation_preserves_completed_dependencies_and_last_good() {
        let mut session = CompilerSession::new();
        session.update(&base()).into_result().unwrap();
        let default = CompileOptions::default();
        session.canonical_semantic(&default).unwrap();
        let diagnostics = session.latest_diagnostics().unwrap().clone();
        let last_good = session.last_good_semantic_diagnostics().unwrap().clone();
        let cfgs = session.last_successful_cfg_cache.clone().unwrap();
        let edited = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        session.update(&edited).into_result().unwrap();
        let merge_terminals = session.queries.merge.len();
        let rir_terminals = session.queries.rir.len();
        let rir_attempts = session
            .metrics
            .attempts()
            .into_iter()
            .filter(|attempt| attempt.family == "rir")
            .count();
        assert_eq!(session.queries.semantic.len(), 1);

        let mut variant = default.clone();
        variant.opt_level = OptLevel::O1;
        session.cancel_semantic_before_publication = true;
        let cancellation = rue_query::CancellationToken::new();
        assert!(matches!(
            session.canonical_semantic_with_cancellation(&variant, cancellation),
            Err(SemanticRequestControl::Abort(
                rue_query::QueryAbort::Canceled
            ))
        ));
        assert_eq!(session.queries.semantic.len(), 1);
        assert!(!Arc::ptr_eq(
            session.latest_diagnostics().unwrap(),
            &diagnostics
        ));
        assert!(matches!(
            session.latest_diagnostics().unwrap().identity(),
            FrontendDiagnosticIdentity::Rir(_)
        ));
        assert!(session.queries.merge.len() > merge_terminals);
        assert!(session.queries.rir.len() > rir_terminals);
        assert!(
            session
                .metrics
                .attempts()
                .into_iter()
                .filter(|attempt| attempt.family == "rir")
                .count()
                > rir_attempts
        );
        assert!(Arc::ptr_eq(
            session.last_good_semantic_diagnostics().unwrap(),
            &last_good
        ));
        assert!(Arc::ptr_eq(
            session.last_successful_cfg_cache.as_ref().unwrap(),
            &cfgs
        ));
        assert_eq!(session.work().semantic.calls, 2);
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries, 1);
        let canceled = session
            .metrics
            .attempts()
            .into_iter()
            .rev()
            .find(|attempt| attempt.family == "semantic")
            .unwrap();
        assert_eq!(
            canceled.attempt.outcome(),
            crate::typed_query_store::AttemptOutcomeKind::Aborted(AbortedQueryReason::Canceled)
        );
        assert!(matches!(canceled.attempt.work(), QueryStructuralWork::None));

        let recomputed = session.canonical_semantic(&variant).unwrap();
        assert_eq!(session.queries.semantic.len(), 2);
        assert!(Arc::ptr_eq(
            &session.canonical_semantic(&variant).unwrap(),
            &recomputed
        ));
        assert_eq!(session.work().semantic.executions, 3);
        assert_eq!(session.work().semantic.reuses, 1);
    }

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

    fn publish_with_test_imports(
        session: &mut CompilerSession,
        source: &SourceSnapshot,
    ) -> ParsedModulesWork {
        let update = session.update(source);
        let work = update.work();
        let program = update.into_owner_result().unwrap();
        if !program.import_directives().is_empty() {
            let graph = crate::test_support::test_fixture_import_graph(&program).unwrap();
            session.adopt_test_import_graph(graph);
        }
        work
    }

    fn legacy_air_declaration_shell_oracle(
        merged: &CanonicalMergedProgram,
        rir: &CanonicalRirOutput,
        preview_features: PreviewFeatures,
    ) -> Result<Vec<rue_air::SemanticDeclarationShell>, CompileErrors> {
        let shells = rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            preview_features,
        )
        .predeclare_declaration_shells_for_test()?
        .declaration_shells()
        .cloned()
        .collect::<Vec<_>>();
        let modules = merged
            .ast()
            .modules()
            .iter()
            .map(|module| (module.file_id(), module.module_id().clone()))
            .collect::<HashMap<_, _>>();
        let mut shells = shells
            .into_iter()
            .map(|mut shell| {
                let module = &modules[&shell.declaration_span.file_id];
                shell.identity.module_path = Arc::from(module.as_str());
                shell.identity.is_trusted_standard_library = module.is_trusted_standard_library();
                shell.signature_fingerprint = [0; 32];
                shell
            })
            .collect::<Vec<_>>();
        sort_shell_comparison_algebra(&mut shells);
        Ok(shells)
    }

    fn sort_shell_comparison_algebra(shells: &mut [rue_air::SemanticDeclarationShell]) {
        let mut spans_by_file = HashMap::<FileId, Vec<rue_span::Span>>::new();
        for shell in shells.iter() {
            spans_by_file
                .entry(shell.declaration_span.file_id)
                .or_default()
                .push(shell.declaration_span);
        }
        let mut normalized_source_order = HashMap::new();
        for spans in spans_by_file.values_mut() {
            spans.sort_by_key(|span| (span.start, span.end));
            spans.dedup();
            for (source_order, span) in spans.iter().enumerate() {
                normalized_source_order.insert(*span, source_order as u32);
            }
        }
        for shell in shells.iter_mut() {
            shell.source_order = normalized_source_order[&shell.declaration_span];
        }
        shells.sort_by(|left, right| {
            let category = |shell: &rue_air::SemanticDeclarationShell| {
                u8::from(shell.identity.namespace != rue_air::StableDefinitionNamespace::Type)
            };
            category(left)
                .cmp(&category(right))
                .then(left.identity.cmp(&right.identity))
        });
    }

    fn independently_expected_signature_fingerprints(
        merged: &CanonicalMergedProgram,
    ) -> HashMap<rue_span::Span, [u8; 32]> {
        fn prefix(declaration: rue_span::Span, payload: rue_span::Span) -> rue_span::Span {
            rue_span::Span::with_file(declaration.file_id, declaration.start, payload.start)
        }
        fn fingerprint(
            module: &crate::parsed_modules::ParsedModule,
            spans: &[rue_span::Span],
        ) -> [u8; 32] {
            let mut hasher = Sha256::new();
            for span in spans {
                for token in module.tokens().iter().filter(|token| {
                    token.span.start >= span.start
                        && token.span.end <= span.end
                        && !matches!(token.kind, rue_lexer::TokenKind::Eof)
                }) {
                    let value = match token.kind {
                        rue_lexer::TokenKind::Ident(symbol) => {
                            format!("ident:{}", module.resolve_raw_symbol(symbol))
                        }
                        rue_lexer::TokenKind::String(symbol) => {
                            format!("string:{}", module.resolve_raw_symbol(symbol))
                        }
                        rue_lexer::TokenKind::Int(value) => format!("int:{value}"),
                        kind => kind.name().to_owned(),
                    };
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value.as_bytes());
                }
                hasher.update(0_u64.to_le_bytes());
            }
            hasher.finalize().into()
        }

        let mut expected = HashMap::new();
        for module in merged.ast().modules() {
            for item in &module.ast().items {
                match item {
                    rue_parser::Item::Function(value) => {
                        expected.insert(
                            value.span,
                            fingerprint(module, &[prefix(value.span, value.body.span())]),
                        );
                    }
                    rue_parser::Item::Struct(value) => {
                        let mut spans = Vec::with_capacity(value.methods.len() + 1);
                        let mut cursor = value.span.start;
                        for method in &value.methods {
                            let body = method.body.span();
                            spans.push(rue_span::Span::with_file(
                                value.span.file_id,
                                cursor,
                                body.start,
                            ));
                            cursor = body.end;
                            expected.insert(
                                method.span,
                                fingerprint(module, &[prefix(method.span, body)]),
                            );
                        }
                        spans.push(rue_span::Span::with_file(
                            value.span.file_id,
                            cursor,
                            value.span.end,
                        ));
                        expected.insert(value.span, fingerprint(module, &spans));
                    }
                    rue_parser::Item::Enum(value) => {
                        expected.insert(value.span, fingerprint(module, &[value.span]));
                    }
                    rue_parser::Item::Const(value) => {
                        expected.insert(
                            value.span,
                            fingerprint(module, &[prefix(value.span, value.init.span())]),
                        );
                    }
                    rue_parser::Item::DropFn(value) => {
                        expected.insert(
                            value.span,
                            fingerprint(module, &[prefix(value.span, value.body.span())]),
                        );
                    }
                    rue_parser::Item::Extern(value) => {
                        for function in &value.fns {
                            expected.insert(function.span, fingerprint(module, &[function.span]));
                        }
                    }
                    rue_parser::Item::Error(_) => {}
                }
            }
        }
        expected
    }

    fn function_modules(count: usize, edited: Option<usize>) -> SourceSnapshot {
        let owned = (0..count)
            .map(|index| {
                let id = u32::try_from(index + 1).unwrap();
                let logical = if index == 0 {
                    "main.rue".to_owned()
                } else {
                    format!("m{index}.rue")
                };
                let physical = format!("/p/{logical}");
                let body = if index == 0 {
                    format!(
                        "fn main() -> i32 {{ {} }}",
                        usize::from(edited == Some(index))
                    )
                } else {
                    format!(
                        "fn f{index}() -> i32 {{ {} }}",
                        if edited == Some(index) {
                            index + 1
                        } else {
                            index
                        }
                    )
                };
                (id, physical, logical, body)
            })
            .collect::<Vec<_>>();
        let borrowed = owned
            .iter()
            .map(|(id, physical, logical, body)| {
                (*id, physical.as_str(), logical.as_str(), body.as_str())
            })
            .collect::<Vec<_>>();
        snapshot(&borrowed, 1)
    }

    #[test]
    fn leaf_body_edit_reuses_128_durable_declarations_and_skips_ordinary_resolution() {
        let options = CompileOptions::default();
        let first = function_modules(128, None);
        // Edit the reachable entry body while retaining all 128 declarations;
        // this proves reuse does not accidentally pass by changing dead code.
        let second = function_modules(128, Some(0));
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        let cold = session.canonical_semantic(&options).unwrap();
        assert_eq!(cold.work().binding.bind_invocations, 1);
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(
            cold.work()
                .declaration_reuse
                .durable_cache_population_exports,
            0
        );
        assert_eq!(cold.work().manifest.rir_instructions_visited, 256);
        session.update(&second).into_result().unwrap();
        let reused = session.canonical_semantic(&options).unwrap();

        assert_eq!(reused.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(reused.work().binding.bind_invocations, 1);
        assert_eq!(reused.work().declaration_reuse.semantic_epochs_started, 1);
        assert_eq!(reused.work().declaration_reuse.declaration_indexes_built, 1);
        assert_eq!(
            reused.work().declaration_reuse.shell_predeclaration_epochs,
            1
        );
        assert_eq!(reused.work().declaration_reuse.fallback_epochs_started, 0);
        assert_eq!(reused.work().binding.durable_payloads_installed, 128);
        assert_eq!(reused.work().declaration_reuse.durable_records_reused, 128);
        assert_eq!(
            reused
                .work()
                .declaration_reuse
                .ordinary_declaration_resolutions_skipped,
            1
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let ordinary = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            ordinary.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(reused.work().body_analysis, ordinary.work().body_analysis);
        assert_eq!(
            format!("{:?}", reused.functions()),
            format!("{:?}", ordinary.functions())
        );
        assert_eq!(reused.strings(), ordinary.strings());
        assert_eq!(
            format!("{:?}", reused.warnings()),
            format!("{:?}", ordinary.warnings())
        );
    }

    #[test]
    fn module_bindings_populate_the_durable_baseline_and_reuse_across_relocation() {
        let original = snapshot(
            &[
                (
                    71,
                    "/old/main.rue",
                    "main.rue",
                    "const lib = @import(\"lib.rue\"); fn main() -> i32 { lib.value() + 1 }",
                ),
                (
                    72,
                    "/old/lib.rue",
                    "lib.rue",
                    "pub fn value() -> i32 { 40 }",
                ),
            ],
            71,
        );
        let relocated_edit = snapshot(
            &[
                (4, "/new/lib.rue", "lib.rue", "pub fn value() -> i32 { 40 }"),
                (
                    9,
                    "/new/main.rue",
                    "main.rue",
                    "const lib = @import(\"lib.rue\"); fn main() -> i32 { lib.value() + 2 }",
                ),
            ],
            9,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &original);
        let cold = session.canonical_semantic(&options).unwrap();
        assert_eq!(cold.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(
            cold.work()
                .declaration_reuse
                .durable_cache_population_exports,
            0
        );
        let module = session
            .durable_declaration_cache
            .as_ref()
            .unwrap()
            .semantics
            .iter()
            .find(|record| record.key.kind() == StableDefinitionKind::ModuleBinding)
            .unwrap();
        assert!(matches!(
            &module.payload,
            crate::DurableDeclarationPayload::ModuleBinding { target }
                if target.as_str() == "lib.rue"
        ));

        publish_with_test_imports(&mut session, &relocated_edit);
        let reused = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            reused.work().binding.declaration_resolution_invocations,
            0,
            "{:#?}",
            reused.work()
        );
        assert_eq!(reused.work().binding.durable_payloads_installed, 3);
        assert_eq!(reused.work().declaration_reuse.durable_records_reused, 3);
        assert_eq!(
            reused
                .work()
                .declaration_reuse
                .ordinary_declaration_resolutions_skipped,
            1
        );
        assert_eq!(reused.work().declaration_reuse.fallbacks, 0);

        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &relocated_edit);
        let ordinary = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &reused, &ordinary);
        assert_diagnostic_parity(&session, &fresh);
    }

    #[test]
    fn unresolved_durable_module_target_falls_back_without_installing() {
        let source = |body: i32| {
            snapshot(
                &[
                    (
                        1,
                        "/p/main.rue",
                        "main.rue",
                        &format!(
                            "const lib = @import(\"lib.rue\"); fn main() -> i32 {{ lib.value() + {body} }}"
                        ),
                    ),
                    (2, "/p/lib.rue", "lib.rue", "pub fn value() -> i32 { 40 }"),
                ],
                1,
            )
        };
        let first = source(1);
        let edited = source(2);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &first);
        session.canonical_semantic(&options).unwrap();
        let cache = session.durable_declaration_cache.as_mut().unwrap();
        let mut records = cache.semantics.to_vec();
        let module = records
            .iter_mut()
            .find(|record| record.key.kind() == StableDefinitionKind::ModuleBinding)
            .unwrap();
        module.payload = crate::DurableDeclarationPayload::ModuleBinding {
            target: crate::ModuleId::from_logical_path("missing.rue").unwrap(),
        };
        cache.semantics = records.into();

        publish_with_test_imports(&mut session, &edited);
        let fallback = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            fallback.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(fallback.work().binding.durable_install_invocations, 1);
        assert!(fallback.work().binding.durable_payloads_installed > 0);
        assert_eq!(fallback.work().declaration_reuse.fallbacks, 0);
        assert_eq!(fallback.work().declaration_reuse.fallback_epochs_started, 0);

        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &edited);
        let ordinary = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &fallback, &ordinary);
        assert_diagnostic_parity(&session, &fresh);
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
        assert!(warm.work().cfg.cfg_import_failures >= 1);
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
        assert!(warm.work().cfg.cfg_import_failures >= 1);
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
    fn specialized_cfg_reuse_is_stable_and_malformed_import_falls_back_atomically() {
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
        let third = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn choose(comptime n: i32) -> i32 { n }\nfn b() -> i32 { 4 }\nfn main() -> i32 { choose(40) + b() }",
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
        let cache = Arc::make_mut(session.last_successful_cfg_cache.as_mut().unwrap());
        let specialization = cache
            .iter_mut()
            .find(|candidate| {
                matches!(
                    candidate.input.function,
                    crate::FunctionInstanceKey::Specialization { .. }
                )
            })
            .unwrap();
        specialization.semantic_schema_version.implementation_epoch += 1;
        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        assert_eq!(warm.work().cfg.cfg_import_failures, 2);
        assert_eq!(warm.work().cfg.cfg_schema_version_rejections, 1);
        assert_eq!(warm.work().cfg.cfg_fallbacks, 2);
        assert_eq!(warm.work().cfg.cfg_reuses, 1);
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 2);
        assert_eq!(warm.work().cfg.optimization_attempts, 2);
        assert_eq!(warm.work().cfg.optimized_level_attempts, 0);
        session.update(&third).into_result().unwrap();
        let repaired = session.canonical_semantic(&options).unwrap();
        assert_eq!(repaired.work().cfg.cfg_reuses, 2);
        assert_eq!(repaired.work().cfg.cfg_builds_attempted, 1);
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
        assert!(cross_target.work().cfg.cfg_import_failures >= 2);
        session.update(&renamed).into_result().unwrap();
        let changed = session.canonical_semantic(&other_options).unwrap();
        assert_eq!(changed.work().cfg.cfg_reuses, 1);
        assert_eq!(changed.work().cfg.cfg_builds_attempted, 2);
        assert_eq!(changed.work().cfg.cfg_import_failures, 1);
        let mut fresh = CompilerSession::new();
        fresh.update(&renamed).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&other_options).unwrap();
        assert_eq!(
            format!("{:?}", changed.functions()),
            format!("{:?}", fresh.functions())
        );
    }

    #[test]
    fn incomplete_optimized_cfg_projection_is_rejected_at_export() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn choose(value: bool) -> i32 { if value { 1 } else { 2 } }\nfn main() -> i32 { choose(true) }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let output = session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O0,
                ..CompileOptions::default()
            })
            .unwrap();
        assert_eq!(output.work().cfg.cfg_export_attempts, 2);
        assert!(output.work().cfg.cfg_export_rejections >= 1);
        assert_eq!(
            output.work().cfg.cfg_export_attempts,
            output.work().cfg.cfg_export_successes + output.work().cfg.cfg_export_rejections
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
        assert_eq!(
            format!("{:?}", actual.analyzed_body_owners()),
            format!("{:?}", fresh.analyzed_body_owners())
        );
        assert_eq!(
            format!("{:?}", actual.ordinary_free_function_dependencies()),
            format!("{:?}", fresh.ordinary_free_function_dependencies())
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
    fn generic_callable_signature_round_trips_through_durable_declaration_reuse() {
        let source = |value| {
            snapshot(
                &[(
                    1,
                    "/p/main.rue",
                    "main.rue",
                    &format!(
                        "fn id(comptime T: type, value: T) -> T {{ value }} fn main() -> i32 {{ {value} }}"
                    ),
                )],
                1,
            )
        };
        let first = source(1);
        let edited = source(2);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&edited).into_result().unwrap();
        let reused = session.canonical_semantic(&options).unwrap();
        assert_eq!(reused.work().binding.declaration_resolution_invocations, 0);
        assert_eq!(reused.work().binding.durable_payloads_installed, 2);
        assert_eq!(reused.work().declaration_reuse.durable_records_reused, 2);
        assert_eq!(reused.work().declaration_reuse.fallback_epochs_started, 0);

        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let ordinary = fresh.canonical_semantic(&options).unwrap();
        assert_semantic_artifact_parity(&session, &reused, &ordinary);
        assert_diagnostic_parity(&session, &fresh);
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
        assert_eq!(reused.work().declaration_reuse.fallback_epochs_started, 0);

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
        assert_eq!(actual.work().declaration_reuse.fallback_epochs_started, 0);
        assert_eq!(actual.work().declaration_reuse.semantic_epochs_started, 1);

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
        assert_eq!(ordinary.work().declaration_reuse.fallback_epochs_started, 0);
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
        assert_eq!(ordinary.work().declaration_reuse.fallback_epochs_started, 0);
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
    fn repeated_queries_and_noop_update_retain_pointer_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompilerSession>();
        assert_send_sync::<CanonicalMergedProgram>();
        assert_send_sync::<CanonicalRirOutput>();

        let source = base();
        let mut session = CompilerSession::new();
        let first_program = session.update(&source).into_owner_result().unwrap();
        let first_merge = session.merge().unwrap();
        let second_merge = session.merge().unwrap();
        let first_rir = session.canonical_rir().unwrap();
        let second_rir = session.canonical_rir().unwrap();
        assert!(Arc::ptr_eq(&first_merge, &second_merge));
        assert!(Arc::ptr_eq(&first_rir, &second_rir));

        let noop = session.update(&source);
        assert!(!noop.downstream_invalidated());
        let second_program = noop.into_owner_result().unwrap();
        assert!(Arc::ptr_eq(&first_program, &second_program));
        assert!(Arc::ptr_eq(&first_merge, &session.merge().unwrap()));
        assert!(Arc::ptr_eq(&first_rir, &session.canonical_rir().unwrap()));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().downstream_invalidations, 0);

        let published = session.published_owner().unwrap().clone();
        let merged = first_merge.clone();
        let rir = first_rir.clone();
        std::thread::spawn(move || {
            assert_eq!(published.modules().len(), 2);
            assert_eq!(merged.ast().modules().len(), 2);
            assert!(!rir.rir().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn one_edit_among_128_recomputes_downstream_once() {
        let make = |edited: bool| {
            let physical = (0..128)
                .map(|index| (FileId::new(index), format!("/p/m{index}.rue")))
                .collect();
            let logical = (0..128)
                .map(|index| (FileId::new(index), format!("m{index}.rue")))
                .collect();
            let metadata = SourceMetadata::new(FileId::new(0), physical, logical).unwrap();
            SourceSnapshot::new(
                metadata,
                (0..128)
                    .map(|index| {
                        let value = if edited && index == 81 { 2 } else { 1 };
                        (
                            FileId::new(index),
                            Arc::new(format!("fn f{index}() -> i32 {{ {value} }}")),
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut session = CompilerSession::new();
        session.update(&make(false)).into_result().unwrap();
        session.canonical_rir().unwrap();
        let first_shards = session
            .definition_shard_baseline
            .as_ref()
            .unwrap()
            .shards()
            .to_vec();
        let update = session.update(&make(true));
        assert!(update.downstream_invalidated());
        assert_eq!(update.work().modules_reused, 127);
        assert_eq!(update.work().modules_reparsed, 1);
        session.canonical_rir().unwrap();
        let second_shards = session.definition_shard_baseline.as_ref().unwrap().shards();
        assert!(
            first_shards
                .iter()
                .zip(second_shards)
                .all(|(first, second)| Arc::ptr_eq(first, second))
        );
        assert_eq!(session.work().last_merge.definition_shards_indexed, 128);
        assert_eq!(session.work().last_merge.definition_shards_reused, 128);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);
        session.canonical_rir().unwrap();
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 2);
        assert_eq!(session.work().downstream_invalidations, 1);
    }

    #[test]
    fn definition_shards_fail_closed_on_surface_identity_changes() {
        let initial = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            ],
            1,
        );
        let body = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn a() -> i32 { 2 }"),
            ],
            1,
        );
        let renamed_definition = snapshot(
            &[
                (1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/p/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let relocated = snapshot(
            &[
                (1, "/m/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/m/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let reassigned = snapshot(
            &[
                (11, "/m/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (12, "/m/a.rue", "a.rue", "fn b() -> i32 { 2 }"),
            ],
            11,
        );
        let mut session = CompilerSession::new();
        session.update(&initial).into_result().unwrap();
        session.merge().unwrap();

        session.update(&body).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 2);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);

        session.update(&renamed_definition).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 1);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 1);

        session.update(&relocated).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 2);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 0);

        session.update(&reassigned).into_result().unwrap();
        session.merge().unwrap();
        assert_eq!(session.work().last_merge.definition_shards_reused, 0);
        assert_eq!(session.work().last_merge.definition_shards_rebuilt, 2);
    }

    #[test]
    fn syntax_failure_preserves_published_revision_and_cached_queries() {
        let source = base();
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let mut session = CompilerSession::new();
        let program = session.update(&source).into_owner_result().unwrap();
        let merged = session.merge().unwrap();
        let rir = session.canonical_rir().unwrap();
        let failed = session.update(&broken);
        assert!(failed.result().is_err());
        assert!(!failed.downstream_invalidated());
        assert!(Arc::ptr_eq(session.published_owner().unwrap(), &program));
        assert!(Arc::ptr_eq(&session.merge().unwrap(), &merged));
        assert!(Arc::ptr_eq(&session.canonical_rir().unwrap(), &rir));
    }

    #[test]
    fn duplicate_merge_error_is_memoized_and_recovery_invalidates_it() {
        let duplicate = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn same() {} fn same() {} fn main() {}",
            )],
            1,
        );
        let fixed = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&duplicate).into_result().unwrap();
        let first = session.merge().unwrap_err();
        let second = session.merge().unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert!(session.canonical_rir().is_err());
        assert!(
            session
                .canonical_semantic(&CompileOptions::default())
                .is_err()
        );
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().semantic.executions, 0);
        let merge_attempts = session
            .metrics
            .attempts()
            .into_iter()
            .filter(|attempt| attempt.family == "merge")
            .collect::<Vec<_>>();
        // The failed RIR terminal observes the failed merge stamp, so the
        // semantic rejection reuses that deterministic failure without a
        // fourth merge request.
        assert_eq!(merge_attempts.len(), 3);
        assert_eq!(
            merge_attempts[0].attempt.execution(),
            QueryAttemptExecution::Computed
        );
        assert_eq!(
            merge_attempts[0].attempt.outcome(),
            crate::typed_query_store::AttemptOutcomeKind::Failure
        );
        let QueryStructuralWork::Merge(failed_work) = merge_attempts[0].attempt.work() else {
            panic!("failed merge must retain typed structural work");
        };
        assert_eq!(failed_work.modules_visited, 1);
        assert_eq!(failed_work.items_visited, 3);
        assert_eq!(
            merge_attempts[1].attempt.execution(),
            QueryAttemptExecution::Reused
        );
        assert_eq!(
            merge_attempts[1].attempt.origin_id(),
            merge_attempts[0].attempt.id()
        );
        assert_eq!(merge_attempts[1].attempt.work(), &QueryStructuralWork::None);
        assert!(merge_attempts[1..].iter().all(|attempt| {
            attempt.attempt.execution() == QueryAttemptExecution::Reused
                && attempt.attempt.outcome()
                    == crate::typed_query_store::AttemptOutcomeKind::Failure
                && attempt.attempt.origin_id() == merge_attempts[0].attempt.id()
                && attempt.attempt.work() == &QueryStructuralWork::None
        }));

        let update = session.update(&fixed);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        assert!(session.canonical_rir().is_ok());
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 1);
    }

    #[test]
    fn failed_merge_attempt_identity_includes_presentation_order() {
        let forward = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn same() {} fn same() {} fn main() {}",
                ),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            1,
        );
        let reversed = snapshot(
            &[
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn same() {} fn same() {} fn main() {}",
                ),
            ],
            1,
        );
        assert_eq!(forward.source_revision(), reversed.source_revision());
        let mut session = CompilerSession::new();
        session
            .update_for_presentation(&forward)
            .into_result()
            .unwrap();
        session.merge().unwrap_err();
        session
            .update_for_presentation(&reversed)
            .into_result()
            .unwrap();
        session.merge().unwrap_err();

        let identities = session
            .queries
            .merge
            .attempt_history()
            .filter(|attempt| attempt.execution == QueryAttemptExecution::Computed)
            .map(|attempt| {
                (
                    attempt.key.source.revision.clone(),
                    attempt.key.presentation.clone().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].0, identities[1].0);
        assert_ne!(identities[0].1, identities[1].1);
        assert_eq!(session.work().merge.executions, 2);
    }

    #[test]
    fn switching_update_modes_is_a_distinct_diagnostic_identity() {
        // RUE-775: canonical `update()` and `update_for_presentation()` over
        // one byte-identical snapshot must not reuse each other's merge
        // diagnostics — the presentation provenance is part of the attempt
        // key — while returning to an already-computed mode reuses it.
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    "fn same() {} fn same() {} fn main() {}",
                ),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.merge().unwrap_err();
        session
            .update_for_presentation(&source)
            .into_result()
            .unwrap();
        session.merge().unwrap_err();
        assert_eq!(session.work().merge.executions, 2);

        let identities = session
            .queries
            .merge
            .attempt_history()
            .filter(|attempt| attempt.execution == QueryAttemptExecution::Computed)
            .map(|attempt| {
                (
                    attempt.key.source.revision.clone(),
                    attempt.key.presentation.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].0, identities[1].0);
        assert!(identities[0].1.is_none(), "canonical attempt has no order");
        assert!(identities[1].1.is_some(), "presentation attempt has order");

        // Returning to each already-attempted mode reuses its own result
        // instead of recomputing or crossing modes.
        session.update(&source).into_result().unwrap();
        session.merge().unwrap_err();
        session
            .update_for_presentation(&source)
            .into_result()
            .unwrap();
        session.merge().unwrap_err();
        assert_eq!(session.work().merge.executions, 2);
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
    fn source_table_reorder_reprojects_parse_record_without_recomputing_modules() {
        let first = snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
                (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let reordered = snapshot(
            &[
                (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
                (1, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            ],
            1,
        );
        assert_eq!(first.source_revision(), reordered.source_revision());
        assert_eq!(first.metadata(), reordered.metadata());

        let mut session = CompilerSession::new();
        let initial_update = session.update(&first);
        let initial_diagnostics = initial_update.diagnostics().clone();
        initial_update.into_result().unwrap();
        let initial_program = session.published.as_ref().unwrap().clone();
        let initial_merge = session.merge().unwrap();
        let initial_rir = session.rir().unwrap();

        let update = session.update(&reordered);
        assert_eq!(update.work().modules_reparsed, 0);
        assert_eq!(update.work().modules_reused, 2);
        assert!(!update.downstream_invalidated());
        update.result().as_ref().unwrap();
        let current_program = session.published.as_ref().unwrap();
        assert!(Arc::ptr_eq(&initial_program, current_program));
        assert!(!Arc::ptr_eq(&initial_diagnostics, update.diagnostics()));
        assert_eq!(
            update
                .diagnostics()
                .source()
                .files()
                .map(|source| source.file_id)
                .collect::<Vec<_>>(),
            [FileId::new(2), FileId::new(1)]
        );
        assert_eq!(
            current_program
                .modules()
                .iter()
                .map(|module| (module.module_id().as_str(), module.file_id()))
                .collect::<Vec<_>>(),
            [("a.rue", FileId::new(1)), ("b.rue", FileId::new(2))]
        );
        assert!(Arc::ptr_eq(&initial_merge, &session.merge().unwrap()));
        let current_rir = session.canonical_rir().unwrap();
        assert!(Arc::ptr_eq(initial_rir.owner(), &current_rir));
        assert!(initial_rir.owner().structurally_eq(&current_rir));
        assert!(
            current_rir
                .rir()
                .iter()
                .all(|(_, instruction)| { matches!(instruction.span.file_id.index(), 1 | 2) })
        );
    }

    #[test]
    fn semantic_queries_reuse_by_codegen_identity_and_ignore_linker() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CanonicalSemanticOutput>();

        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        let first = session.canonical_semantic(&options).unwrap();
        let second = session.canonical_semantic(&options).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let linker_only = CompileOptions {
            linker: LinkerMode::System("unused-linker".to_string()),
            ..options.clone()
        };
        assert!(Arc::ptr_eq(
            &first,
            &session.canonical_semantic(&linker_only).unwrap()
        ));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 2);
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(first.work().binding.bind_invocations, 1);
        assert_eq!(first.work().manifest.build_invocations, 1);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.functions().is_empty()))
            .join()
            .unwrap();
    }

    #[test]
    fn semantic_option_variants_create_deterministic_distinct_entries() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let default = CompileOptions::default();
        session.canonical_semantic(&default).unwrap();
        session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..default.clone()
            })
            .unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .canonical_semantic(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .canonical_semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        let work = session.work();
        assert_eq!(work.semantic.executions, 4);
        assert_eq!(work.semantic_entries, 4);
        assert_eq!(work.semantic_records.len(), 4);
        assert!(work.semantic_records.iter().all(|record| {
            !record.failed
                && record.work.binding.bind_invocations == 1
                && record.work.manifest.build_invocations == 1
        }));
        for (index, left) in work.semantic_records.iter().enumerate() {
            assert!(
                work.semantic_records[index + 1..]
                    .iter()
                    .all(|right| left.input != right.input)
            );
        }
    }

    #[test]
    fn semantic_reuse_origin_is_the_exact_typed_terminal_across_multiple_keys() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let a = CompileOptions::default();
        let b = CompileOptions {
            opt_level: OptLevel::O1,
            ..a.clone()
        };
        session.canonical_semantic(&a).unwrap();
        session.canonical_semantic(&b).unwrap();
        session.canonical_semantic(&a).unwrap();
        let attempts = session
            .metrics
            .attempts()
            .into_iter()
            .filter(|attempt| attempt.family == "semantic")
            .collect::<Vec<_>>();
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts[0].attempt.execution(),
            QueryAttemptExecution::Computed
        );
        assert_eq!(
            attempts[1].attempt.execution(),
            QueryAttemptExecution::Computed
        );
        assert_eq!(
            attempts[2].attempt.execution(),
            QueryAttemptExecution::Reused
        );
        assert_eq!(attempts[2].attempt.origin_id(), attempts[0].attempt.id());
        assert_ne!(attempts[2].attempt.origin_id(), attempts[1].attempt.id());
    }

    #[test]
    fn retained_terminal_origin_survives_attempt_ledger_rollover() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let options = CompileOptions::default();
        session.canonical_semantic(&options).unwrap();
        let origin = session
            .metrics
            .attempts()
            .into_iter()
            .find(|attempt| attempt.family == "semantic")
            .unwrap()
            .attempt
            .id();
        for _ in 0..(QUERY_ATTEMPT_RETENTION_LIMIT + 44) {
            session.import_graph(None).unwrap();
        }
        session.canonical_semantic(&options).unwrap();
        let attempts = session.metrics.attempts();
        assert!(
            attempts
                .iter()
                .any(|attempt| attempt.attempt.id() == origin)
        );
        let reused = attempts
            .iter()
            .rev()
            .find(|attempt| attempt.family == "semantic")
            .unwrap();
        assert_eq!(reused.attempt.execution(), QueryAttemptExecution::Reused);
        assert_eq!(reused.attempt.origin_id(), origin);
    }

    #[test]
    fn dependency_manifest_retention_is_bounded_and_recomputes_fifo() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let variants = retention_variants();
        for options in &variants {
            session.semantic_dependency_inputs(options, None).unwrap();
        }
        assert_eq!(
            session.queries.manifests.len(),
            QUERY_TERMINAL_RETENTION_LIMIT
        );
        assert_eq!(
            session.work().dependency_manifests.executions,
            variants.len()
        );
        session
            .semantic_dependency_inputs(&variants[0], None)
            .unwrap();
        assert_eq!(
            session.work().dependency_manifests.executions,
            variants.len() + 1
        );
        session
            .semantic_dependency_inputs(&variants[0], None)
            .unwrap();
        assert_eq!(session.work().dependency_manifests.reuses, 1);
        assert!(session.metrics.attempts().len() <= QUERY_ATTEMPT_RETENTION_LIMIT);
    }

    #[test]
    fn semantic_store_eviction_recomputes_then_reuses_with_exact_metrics() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let variants = retention_variants();

        for options in &variants {
            session.canonical_semantic(options).unwrap();
        }
        assert_eq!(session.work().semantic.calls, variants.len());
        assert_eq!(session.work().semantic.executions, variants.len());
        assert_eq!(session.work().semantic.reuses, 0);
        assert_eq!(
            session.work().retention.semantic_query_entries,
            QUERY_TERMINAL_RETENTION_LIMIT
        );
        assert_eq!(session.work().retention.semantic_query_evictions, 1);

        session.canonical_semantic(&variants[0]).unwrap();
        assert_eq!(session.work().semantic.calls, variants.len() + 1);
        assert_eq!(session.work().semantic.executions, variants.len() + 1);
        assert_eq!(session.work().semantic.reuses, 0);
        assert_eq!(session.work().retention.semantic_query_evictions, 2);

        session.canonical_semantic(&variants[0]).unwrap();
        assert_eq!(session.work().semantic.calls, variants.len() + 2);
        assert_eq!(session.work().semantic.executions, variants.len() + 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().semantic_records.len(), variants.len() + 1);
        assert_eq!(session.work().retention.semantic_query_evictions, 2);
    }

    #[test]
    fn semantic_cache_invalidates_on_edit_but_survives_failed_parse() {
        let source = base();
        let edited = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let broken = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let first = session.canonical_semantic(&options).unwrap();
        assert!(session.update(&broken).result().is_err());
        assert!(Arc::ptr_eq(
            &first,
            &session.canonical_semantic(&options).unwrap()
        ));
        let update = session.update(&edited);
        assert!(update.downstream_invalidated());
        update.into_result().unwrap();
        let second = session.canonical_semantic(&options).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
    }

    #[test]
    fn dependency_edges_preserve_noops_and_restore_exact_retained_terminals() {
        let source = base();
        let edited = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let first = session.canonical_semantic(&options).unwrap();
        let original_key = session
            .queries
            .semantic
            .records()
            .next()
            .unwrap()
            .key
            .clone();
        let original_handle = session.queries.semantic.handle(&original_key).unwrap();

        let noop = session.update(&source);
        assert!(!noop.downstream_invalidated());
        assert!(Arc::ptr_eq(
            &first,
            &session.canonical_semantic(&options).unwrap()
        ));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);

        let changed = session.update(&edited);
        assert!(changed.downstream_invalidated());
        changed.into_result().unwrap();
        assert!(!original_handle.is_valid(&mut session.queries.graph));
        assert!(original_handle.invalidation_cause_count(&session.queries.graph) > 0);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
        let second = session.canonical_semantic(&options).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));

        session.update(&source).into_result().unwrap();
        assert!(original_handle.is_valid(&mut session.queries.graph));
        assert_eq!(
            original_handle.invalidation_cause_count(&session.queries.graph),
            0
        );
        assert!(Arc::ptr_eq(
            &first,
            &session.canonical_semantic(&options).unwrap()
        ));
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic.reuses, 2);
        assert_eq!(session.work().merge.executions, 2);
        assert_eq!(session.work().rir.executions, 2);
    }

    #[test]
    fn semantic_errors_are_memoized_and_recovery_reexecutes() {
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            1,
        );
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&invalid).into_result().unwrap();
        let first = session.canonical_semantic(&options).unwrap_err();
        let second = session.canonical_semantic(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().semantic.calls, 2);
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        let semantic_attempts = session
            .metrics
            .attempts()
            .into_iter()
            .filter(|attempt| attempt.family == "semantic")
            .collect::<Vec<_>>();
        assert_eq!(semantic_attempts.len(), 2);
        assert!(semantic_attempts.iter().all(|attempt| {
            attempt.attempt.outcome() == crate::typed_query_store::AttemptOutcomeKind::Failure
        }));
        assert_eq!(
            semantic_attempts[1].attempt.origin_id(),
            semantic_attempts[0].attempt.id()
        );
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);
        let retained_failed_work = session.work().semantic_records[0].work;

        session.update(&valid).into_result().unwrap();
        assert!(session.canonical_semantic(&options).is_ok());
        assert_eq!(session.work().semantic.calls, 3);
        assert_eq!(session.work().semantic.executions, 2);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().semantic_entries, 2);
        assert_eq!(session.work().semantic_entries_invalidated, 1);
        assert_eq!(
            retained_failed_work.cfg.cfg_builds_attempted, 0,
            "a failed body terminal must stop before CFG construction"
        );
    }

    #[test]
    fn deterministic_body_failure_is_terminal_and_recovers_without_replacing_last_good() {
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        let baseline = session.canonical_semantic(&options).unwrap();
        let key = body_query_key(&mut session, &options, "main");

        session.update(&invalid).into_result().unwrap();
        let first = session.canonical_semantic(&options).unwrap_err();
        let second = session.canonical_semantic(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        let record = session.work().semantic_records.last().unwrap();
        assert_eq!(
            record.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::BodyAnalysis
        );
        assert_eq!(record.work.cfg.cfg_builds_attempted, 0);
        let (first_stamp, first_kind, first_transaction) =
            retained_body_transaction(&session, &key);
        assert_eq!(first_kind, rue_query::QueryTerminalKind::Failure);
        assert!(matches!(
            first_transaction,
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        ));
        let (reused_stamp, reused_kind, reused_transaction) =
            retained_body_transaction(&session, &key);
        assert_eq!(first_stamp, reused_stamp);
        assert_eq!(reused_kind, rue_query::QueryTerminalKind::Failure);
        assert!(matches!(
            reused_transaction,
            crate::body_query::BodyTransaction::DeterministicFailure { .. }
        ));

        session.update(&valid).into_result().unwrap();
        let recovered = session.canonical_semantic(&options).unwrap();
        assert!(
            Arc::ptr_eq(&recovered, &baseline),
            "restoring the exact source leaf must reinstate its retained terminal"
        );
        let (success_stamp, success_kind, success_transaction) =
            retained_body_transaction(&session, &key);
        assert_ne!(first_stamp, success_stamp);
        assert_eq!(success_kind, rue_query::QueryTerminalKind::Success);
        assert!(matches!(
            success_transaction,
            crate::body_query::BodyTransaction::Success { .. }
        ));
    }

    #[test]
    fn specialization_failure_work_is_retained() {
        let invalid = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn runaway(comptime n: i32) -> i32 { runaway(n + 1) }\nfn main() -> i32 { runaway(0) }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&invalid).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::ComptimeEvaluationFailed { reason })
                if reason.contains("maximum nesting depth")
        ));
        let record = session.work().semantic_records.last().unwrap();
        assert_eq!(
            record.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::BodyAnalysis
        );
        // The retired whole-program specialization driver did not run. The
        // query coordinator owns the bounded frontier and reports overflow
        // atomically instead of publishing a partial closure.
        assert_eq!(record.work.body_analysis.specialization_rounds, 0);
    }

    #[test]
    fn reused_specializations_consume_the_persistent_round_budget() {
        let source = |requested: i32| {
            let program = format!(
                "fn chain(comptime n: i32) -> i32 {{\n\
                     if n == 0 {{ 0 }} else {{ chain(n - 1) }}\n\
                 }}\n\
                 fn main() -> i32 {{ chain({requested}) }}"
            );
            snapshot(&[(1, "/p/main.rue", "main.rue", program.as_str())], 1)
        };
        let baseline = source(63);
        let overflowing = source(64);
        let mut session = CompilerSession::new();
        session.update(&baseline).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        session.update(&overflowing).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::ComptimeEvaluationFailed { reason })
                if reason.contains("maximum nesting depth")
        ));
        let failure = session.work().semantic_records.last().unwrap();
        assert_eq!(
            failure.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::BodyAnalysis
        );
        session.update(&baseline).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O2,
                ..CompileOptions::default()
            })
            .unwrap();

        session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O3,
                ..CompileOptions::default()
            })
            .unwrap();
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
    fn declaration_failure_retains_completed_declaration_work() {
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let changed_source = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&changed_source).into_result().unwrap();
        crate::canonical_semantic::with_test_declaration_failure_injection(|| {
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap_err();
        });

        let record = session.work().semantic_records.last().unwrap();
        assert_eq!(
            record.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::Declaration
        );
        assert_eq!(record.work.declaration_index.build_invocations, 1);
        assert_eq!(record.work.binding.bind_invocations, 1);
        assert_eq!(record.work.manifest.build_invocations, 1);
    }

    #[test]
    fn declaration_resolution_failure_retains_exact_attempt_work() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "struct Recursive { next: Recursive } fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();

        let record = session.work().semantic_records.last().unwrap();
        assert_eq!(
            record.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::Declaration
        );
        assert_eq!(record.work.declaration_index.build_invocations, 1);
        assert_eq!(record.work.binding.bind_invocations, 0);
        assert_eq!(record.work.binding.namespace_setup_invocations, 0);
        assert_eq!(record.work.binding.declaration_resolution_invocations, 0);
        assert_eq!(record.work.binding.declaration_resolution_failures, 0);
        assert_eq!(
            record.work.binding.body_readiness_finalization_invocations,
            0
        );
        assert_eq!(record.work.manifest.build_invocations, 0);
        assert_eq!(record.work.body_analysis.bodies_attempted, 0);
    }

    #[test]
    fn body_traversal_coordinator_counters_populate_from_a_multi_body_compile() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 7 }\n\
                 fn other() -> i32 { helper() }\n\
                 fn main() -> i32 { helper() + other() }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        let record = session.work().semantic_records.last().unwrap();
        let body = &record.work.body_analysis;
        // `main`, `helper`, and `other` are each reached and analyzed once, so
        // the coordinator computes at least three body transactions and
        // publishes a closure containing all three.
        assert!(
            body.bodies_attempted >= 3,
            "expected at least three attempted bodies, got {}",
            body.bodies_attempted
        );
        assert!(
            body.closure_bodies_visited >= 3,
            "expected at least three closure bodies, got {}",
            body.closure_bodies_visited
        );
        // This program has no anonymous producers and no specializations, so
        // the traversal completes in one pass at depth zero.
        assert_eq!(body.closure_restarts, 0);
        assert_eq!(body.deferred_producer_retries, 0);
        assert_eq!(body.max_specialization_depth, 0);
    }

    #[test]
    fn authoritative_key_mismatch_retains_failed_token_validation_work() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "fn helper() -> i32 { 0 } fn main() -> i32 { helper() }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        crate::canonical_semantic::with_test_authoritative_key_mismatch(|| {
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap_err();
        });

        let record = session.work().semantic_records.last().unwrap();
        assert_eq!(
            record.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::Declaration
        );
        assert_eq!(record.work.body_owner_tokens.provisional_slots, 2);
        assert_eq!(record.work.body_owner_tokens.authoritative_slots, 1);
        assert_eq!(record.work.body_owner_tokens.slots_validated, 1);
        assert_eq!(record.work.body_owner_tokens.tokens_installed, 0);
        assert_eq!(record.work.body_owner_tokens.validation_failures, 1);
    }

    #[test]
    fn body_query_stage_failure_surfaces_as_diagnostic_not_panic() {
        let source = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        // A deterministic per-body stage failure must reach the caller as a real
        // compiler diagnostic rather than a disguised abort that panics the
        // uncanceled session.
        let errors =
            crate::canonical_semantic::with_test_body_query_stage_failure_injection(|| {
                session
                    .canonical_semantic(&CompileOptions::default())
                    .unwrap_err()
            });
        let rendered = errors.to_string();
        assert!(
            rendered.contains("injected_body_query_stage"),
            "expected the failing stage name in diagnostics, got: {rendered}"
        );
        assert!(
            rendered.contains("body instance"),
            "expected the failing body instance in diagnostics, got: {rendered}"
        );
        let record = session.work().semantic_records.last().unwrap();
        assert_eq!(
            record.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::BodyAnalysis
        );
    }

    #[test]
    fn cfg_failure_retains_work_without_replacing_the_last_good_baseline() {
        let valid = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let changed = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        let baseline = session.canonical_semantic(&options).unwrap();

        session.update(&changed).into_result().unwrap();
        crate::canonical_semantic::with_test_cfg_failure_injection(|| {
            session.canonical_semantic(&options).unwrap_err();
        });
        let failed = session.work().semantic_records.last().unwrap();
        assert_eq!(
            failed.failure.unwrap().phase,
            CanonicalSemanticFailurePhase::CfgConstruction
        );
        assert_eq!(failed.work.cfg.functions_considered, 1);
        assert_eq!(failed.work.cfg.cfg_builds_attempted, 1);
        assert_eq!(failed.work.cfg.cfg_builds_failed, 1);
        assert_eq!(failed.work.cfg.optimization_attempts, 0);

        session.update(&valid).into_result().unwrap();
        let recovered = session.canonical_semantic(&options).unwrap();
        assert!(
            Arc::ptr_eq(&recovered, &baseline),
            "restoring the exact source leaf must reinstate its retained terminal"
        );
    }

    #[test]
    fn token_preparation_error_recovery_publishes_only_failure_diagnostics() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const value: i32 = 1; const value: i32 = 2; fn main() {}",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        assert!(
            errors
                .iter()
                .all(|error| error.kind.code().to_string() != "E1400"),
            "unexpected internal diagnostic: {errors:?}"
        );
        assert_eq!(session.work().semantic_entries, 1);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);
        let diagnostics = session.latest_diagnostics().unwrap();
        assert!(!diagnostics.is_success());
        assert!(diagnostics.warnings().is_empty());
    }

    #[test]
    fn stable_definitions_are_lazy_reused_semantic_projections() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoundDefinitionSet>();

        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let ordinary_options = CompileOptions::default();
        let ordinary = session.canonical_semantic(&ordinary_options).unwrap();
        assert_eq!(session.work().definitions.executions, 0);
        assert_eq!(session.work().definition_entries, 0);

        let id_options = CompileOptions {
            linker: LinkerMode::System("ignored".to_string()),
            opt_level: OptLevel::O1,
            ..ordinary_options.clone()
        };
        let first = session.stable_definitions(&id_options).unwrap();
        let second = session
            .stable_definitions(&CompileOptions {
                linker: LinkerMode::Internal,
                opt_level: OptLevel::O3,
                ..ordinary_options
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(session.work().definitions.reuses, 1);
        assert_eq!(session.work().definition_entries, 1);
        let record = &session.work().definition_records[0];
        assert_eq!(ordinary.work().binding.bind_invocations, 1);
        assert_eq!(record.binding.bind_invocations, 1);
        assert_eq!(record.manifest.build_invocations, 1);
        assert_eq!(first.manifest_work().build_invocations, 1);
        assert!(record.issuance.ids_issued > 0);
        assert!(!record.failed);

        let published = first.clone();
        std::thread::spawn(move || assert!(!published.definitions().is_empty()))
            .join()
            .unwrap();
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
    fn query_owned_declaration_shells_match_independent_air_and_token_oracles() {
        let source = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                r#"
pub struct Box {
    fn get(mut self, comptime T: type) -> i32 { 0 }
    fn inspect(borrow self) -> i32 { 1 }
    fn make() -> Box { Box {} }
}
enum Choice { A, B(i32) }
const selected = 1;
const alias = selected;
drop fn Box(self) {}
unchecked fn main(value: i32) -> i32 { value }
extern "C" { fn getpid() -> i32; }
"#,
            )],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let imports = session.accepted_semantic_import_graph().unwrap();
        let rir = session.canonical_rir().unwrap();
        let merged = session
            .queries
            .rir
            .selected_record(&session.queries.graph)
            .and_then(|entry| entry.merged.clone())
            .unwrap();
        let revision = session.queries.revisioned.current_parse_revision().unwrap();
        let query_shells = session
            .queries
            .revisioned
            .projected_declaration_shells(
                revision,
                merged.ast(),
                rue_query::CancellationToken::new(),
            )
            .unwrap();

        let legacy =
            legacy_air_declaration_shell_oracle(&merged, &rir, options.preview_features.clone())
                .unwrap();
        let expected_fingerprints = independently_expected_signature_fingerprints(&merged);
        for shell in &query_shells {
            assert_eq!(
                shell.signature_fingerprint, expected_fingerprints[&shell.declaration_span],
                "query fingerprint differs from independent AST/token partition"
            );
        }
        let mut comparable_query_shells = query_shells.clone();
        for shell in &mut comparable_query_shells {
            shell.signature_fingerprint = [0; 32];
        }
        sort_shell_comparison_algebra(&mut comparable_query_shells);
        assert_eq!(legacy, comparable_query_shells);

        let mut foreign_locator = query_shells;
        let first = &mut foreign_locator[0];
        first.declaration_span.start += 1;
        let locator_error = crate::bound_definitions::configure_canonical_sema(
            &merged,
            &rir,
            options.preview_features,
            options.target,
            &imports,
        )
        .unwrap()
        .predeclare_imported_declaration_shells(&foreign_locator)
        .err()
        .unwrap();
        assert!(matches!(
            locator_error.first().map(|error| &error.kind),
            Some(ErrorKind::InternalError(message))
                if message.contains("current RIR locator")
        ));
    }

    #[test]
    fn declaration_shell_oracles_cover_import_alias_trusted_and_multimodule_inputs() {
        let root = FileId::new(1);
        let standard = FileId::new(2);
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root,
            HashMap::from([
                (root, "/project/main.rue".to_owned()),
                (standard, "/project/std/lib.rue".to_owned()),
            ]),
            HashMap::from([
                (root, "main.rue".to_owned()),
                (standard, "\0rue-std/lib.rue".to_owned()),
            ]),
            HashSet::from([standard]),
        )
        .unwrap();
        let source = SourceSnapshot::new(
            metadata,
            vec![
                (
                    root,
                    Arc::new(
                        r#"
pub const direct = @import("std/lib.rue");
pub const alias = direct;
fn main() -> i32 { 0 }
"#
                        .to_owned(),
                    ),
                ),
                (
                    standard,
                    Arc::new("pub struct Library {} pub const value: i32 = 1;".to_owned()),
                ),
            ],
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let rir = session.canonical_rir().unwrap();
        let merged = session
            .queries
            .rir
            .selected_record(&session.queries.graph)
            .and_then(|entry| entry.merged.clone())
            .unwrap();
        let revision = session.queries.revisioned.current_parse_revision().unwrap();
        let query = session
            .queries
            .revisioned
            .projected_declaration_shells(
                revision,
                merged.ast(),
                rue_query::CancellationToken::new(),
            )
            .unwrap();
        let legacy =
            legacy_air_declaration_shell_oracle(&merged, &rir, options.preview_features.clone())
                .unwrap();
        let expected_fingerprints = independently_expected_signature_fingerprints(&merged);
        let mut comparable = query.clone();
        for shell in &mut comparable {
            assert_eq!(
                shell.signature_fingerprint,
                expected_fingerprints[&shell.declaration_span]
            );
            shell.signature_fingerprint = [0; 32];
        }
        sort_shell_comparison_algebra(&mut comparable);
        assert_eq!(legacy, comparable);
        assert!(
            query
                .iter()
                .any(|shell| shell.identity.is_trusted_standard_library)
        );
        session.canonical_semantic(&options).unwrap();
        let definitions = session.stable_definitions(&options).unwrap();
        for name in ["direct", "alias"] {
            let record = definitions
                .definitions()
                .iter()
                .find(|record| {
                    record.stable_key().module().as_str() == "main.rue"
                        && record.stable_key().name() == name
                        && record.stable_key().kind() == StableDefinitionKind::ModuleBinding
                })
                .unwrap_or_else(|| panic!("missing public module-binding definition for {name}"));
            assert_eq!(
                record.visibility(),
                Some(rue_parser::ast::Visibility::Public)
            );
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
            let parsed = crate::parsed_modules::parse_source_snapshot_modules(&source).unwrap();
            let merged = crate::merge_parsed_modules(&parsed).unwrap();
            let rir = crate::lower_canonical_rir(&merged).unwrap();
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
    fn production_query_semantics_match_the_independent_retired_air_oracle() {
        let source = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                r#"
const base: i32 = 40;
const alias: i32 = base + 2;
fn increment(value: i32) -> i32 { value + 1 }
const callable = increment;
fn main() -> i32 {
    callable(alias)
}
"#,
            )],
            1,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let rir = session.canonical_rir().unwrap();
        let merged = session
            .queries
            .rir
            .selected_record(&session.queries.graph)
            .and_then(|entry| entry.merged.clone())
            .unwrap();
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        let production = session
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
            .declarations;
        let definitions = session.stable_definitions(&options).unwrap();

        // This producer is deliberately test-only and starts from a fresh AIR
        // epoch that resolves declarations through the retired source-owned
        // implementation. Production never evaluates this path or both paths.
        let retired_bound = rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            options.preview_features.clone(),
        )
        .bind_declarations_for_test()
        .unwrap();
        let retired = retired_bound
            .with_declaration_semantics(|exports, _| {
                crate::durable_semantics::convert_declaration_semantics(
                    &merged,
                    &definitions,
                    exports,
                )
                .unwrap_or_else(|failure| {
                    panic!(
                        "retired export conversion failed: {failure:?}; exports={exports:?}; definitions={:?}",
                        definitions
                            .definitions()
                            .iter()
                            .map(|record| record.stable_key())
                            .collect::<Vec<_>>()
                    )
                })
            })
            .unwrap();

        assert_eq!(production, retired, "declaration producers diverged");
    }

    #[test]
    fn stable_then_ordinary_reuses_the_validation_semantic_entry() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&options).unwrap();
        let semantic_executions = session.work().semantic.executions;
        let ordinary = session.canonical_semantic(&options).unwrap();

        assert!(!ordinary.functions().is_empty());
        assert_eq!(semantic_executions, 1);
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(
            session.work().definition_records[0]
                .binding
                .bind_invocations,
            1
        );
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
    fn supplied_test_graph_is_consumed_without_resolution_fallback() {
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
        let program = session.update(&source).into_owner_result().unwrap();
        let graph = crate::test_support::test_fixture_import_graph(&program).unwrap();
        session.adopt_test_import_graph(graph);
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
    fn stable_definitions_prefers_a_successful_semantic_variant() {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        let imports = session.accepted_semantic_import_graph().unwrap();
        let successful_key = SemanticQueryKey {
            input: CodegenInputDescriptor {
                semantic: SemanticInputDescriptor::new(
                    &source,
                    options.target,
                    &options.preview_features,
                ),
                opt_level: options.opt_level.into(),
            },
            imports,
        };
        let successful = session.queries.semantic.get(&successful_key).unwrap();
        let mut failed_input = successful.key.input.clone();
        failed_input.opt_level = crate::StableOptLevel::O1;
        let failed_imports = successful.key.imports.clone();
        let failed_errors = CompileErrors::from(CompileError::without_span(
            ErrorKind::InvalidCompilerInput("synthetic prior failed opt variant".to_string()),
        ));
        let failed_source = session.published_snapshot.clone().unwrap();
        let failed_diagnostics = session.publish_diagnostics(
            &failed_source,
            FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(
                &failed_input,
                failed_imports.clone(),
            )),
            Some(&failed_errors),
            &[],
        );
        let failed_key = SemanticQueryKey {
            input: failed_input,
            imports: failed_imports,
        };
        let source_dependency = session
            .queries
            .source_inputs
            .selected(&session.queries.graph)
            .unwrap();
        session.queries.semantic.insert_with_dependencies(
            &mut session.queries.graph,
            SemanticCacheEntry {
                key: failed_key,
                result: Err(failed_errors),
                rir: None,
                diagnostics: failed_diagnostics,
                durable_declaration_cache: None,
                successful_cfg_cache: None,
                oracle_injected: false,
            },
            [source_dependency],
        );

        let definitions = session
            .stable_definitions(&CompileOptions {
                opt_level: OptLevel::O2,
                ..options
            })
            .unwrap();

        assert!(!definitions.definitions().is_empty());
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().definitions.executions, 1);
        let record = &session.work().definition_records[0];
        assert_eq!(record.binding.bind_invocations, 1);
        assert_eq!(record.manifest.build_invocations, 1);
    }

    #[test]
    fn dependency_input_manifest_is_stable_ordered_and_adds_no_rir_scan() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SemanticDependencyInputManifest>();
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let first = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let second = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(first.definitions().windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            first.work().definition_records_visited,
            first.definitions().len()
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
        assert_eq!(session.work().dependency_manifests.executions, 1);
        assert_eq!(session.work().dependency_manifests.reuses, 1);
        assert_eq!(session.work().rir.executions, 1);
        assert_eq!(session.work().semantic.executions, 1);
    }

    #[test]
    fn definition_fingerprints_ignore_relocation_file_ids_and_input_order() {
        let first = snapshot(
            &[
                (7, "/one/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/one/a.rue", "a.rue", "pub fn a() -> i32 { 1 }"),
            ],
            7,
        );
        let relocated = snapshot(
            &[
                (41, "/else/a.rue", "a.rue", "pub fn a() -> i32 { 1 }"),
                (99, "/else/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            99,
        );
        let mut left = CompilerSession::new();
        left.update(&first).into_result().unwrap();
        let left = left
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let mut right = CompilerSession::new();
        right.update(&relocated).into_result().unwrap();
        let right = right
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();

        assert_eq!(
            left.definition_fingerprints(),
            right.definition_fingerprints()
        );
        assert!(
            left.definition_fingerprints()
                .iter()
                .all(|fingerprint| fingerprint.schema_version == DEFINITION_FINGERPRINT_SCHEMA_V2)
        );
    }

    #[test]
    fn definition_fingerprints_partition_function_signature_and_body_changes() {
        fn fingerprints(source: &str) -> StableDefinitionInputFingerprint {
            let source = snapshot(&[(7, "/p/main.rue", "main.rue", source)], 7);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
                .definition_fingerprints()
                .iter()
                .find(|fingerprint| fingerprint.key.name() == "value")
                .expect("value definition fingerprint")
                .clone()
        }

        let original = fingerprints("fn value() -> i32 { 0 } fn main() { value(); }");
        let body_changed = fingerprints("fn value() -> i32 { 1 } fn main() { value(); }");
        assert_eq!(original.key, body_changed.key);
        assert_eq!(original.declaration, body_changed.declaration);
        assert_eq!(original.signature, body_changed.signature);
        assert_ne!(
            original.body_or_initializer,
            body_changed.body_or_initializer
        );
        assert_eq!(
            original.precision,
            StableDefinitionFingerprintPrecision::SignatureAndBody
        );

        let visibility_changed = fingerprints("pub fn value() -> i32 { 0 } fn main() { value(); }");
        assert_eq!(original.key, visibility_changed.key);
        assert_ne!(original.declaration, visibility_changed.declaration);
        assert_ne!(original.signature, visibility_changed.signature);
        assert_eq!(
            original.body_or_initializer,
            visibility_changed.body_or_initializer
        );

        let signature_changed = fingerprints("fn value() -> i64 { 0 } fn main() { value(); }");
        assert_eq!(original.declaration, signature_changed.declaration);
        assert_ne!(original.signature, signature_changed.signature);
        assert_eq!(
            original.body_or_initializer,
            signature_changed.body_or_initializer
        );
    }

    #[test]
    fn definition_fingerprints_partition_all_authoritative_named_payloads() {
        fn fingerprint(
            source: &str,
            name: &str,
            kind: StableDefinitionKind,
        ) -> StableDefinitionInputFingerprint {
            let source = snapshot(&[(7, "/p/main.rue", "main.rue", source)], 7);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let manifest = session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap();
            manifest
                .definition_fingerprints()
                .iter()
                .find(|value| value.key.name() == name && value.key.kind() == kind)
                .unwrap_or_else(|| {
                    panic!(
                        "missing {name:?} {kind:?}; got {:?}",
                        manifest
                            .definition_fingerprints()
                            .iter()
                            .map(|value| (value.key.name(), value.key.kind()))
                            .collect::<Vec<_>>()
                    )
                })
                .clone()
        }
        fn assert_only_payload_changed(
            before: &StableDefinitionInputFingerprint,
            after: &StableDefinitionInputFingerprint,
            precision: StableDefinitionFingerprintPrecision,
        ) {
            assert_eq!(before.key, after.key);
            assert_eq!(before.declaration, after.declaration);
            assert_eq!(before.signature, after.signature);
            assert_ne!(before.body_or_initializer, after.body_or_initializer);
            assert_eq!(before.precision, precision);
            assert_eq!(after.precision, precision);
        }

        let constant = fingerprint(
            "const answer: i32 = 1; fn main() -> i32 { answer }",
            "answer",
            StableDefinitionKind::ValueConst,
        );
        let constant_changed = fingerprint(
            "const answer: i32 = 2; fn main() -> i32 { answer }",
            "answer",
            StableDefinitionKind::ValueConst,
        );
        assert_only_payload_changed(
            &constant,
            &constant_changed,
            StableDefinitionFingerprintPrecision::SignatureAndInitializer,
        );

        let method = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "get",
            StableDefinitionKind::Method,
        );
        let method_changed = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n + 1 } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "get",
            StableDefinitionKind::Method,
        );
        assert_only_payload_changed(
            &method,
            &method_changed,
            StableDefinitionFingerprintPrecision::SignatureAndBody,
        );
        let method_owner = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "S",
            StableDefinitionKind::Struct,
        );
        let method_owner_after_body_edit = fingerprint(
            "struct S { n: i32, fn get(self) -> i32 { self.n + 1 } fn make() -> S { S { n: 1 } } } fn main() -> i32 { S.make().get() }",
            "S",
            StableDefinitionKind::Struct,
        );
        assert_eq!(method_owner, method_owner_after_body_edit);

        let comptime_function = fingerprint(
            "fn id(comptime value: i32) -> i32 { value } fn main() -> i32 { id(1) }",
            "id",
            StableDefinitionKind::Function,
        );
        let runtime_function = fingerprint(
            "fn id(value: i32) -> i32 { value } fn main() -> i32 { id(1) }",
            "id",
            StableDefinitionKind::Function,
        );
        assert_eq!(comptime_function.declaration, runtime_function.declaration);
        assert_ne!(comptime_function.signature, runtime_function.signature);
        assert_eq!(
            comptime_function.body_or_initializer,
            runtime_function.body_or_initializer
        );

        let destructor = fingerprint(
            "struct S { n: i32 } drop fn S(self) {} fn main() -> i32 { let s = S { n: 1 }; 0 }",
            "S",
            StableDefinitionKind::Destructor,
        );
        let destructor_changed = fingerprint(
            "fn cleanup() {} struct S { n: i32 } drop fn S(self) { cleanup(); } fn main() -> i32 { let s = S { n: 1 }; 0 }",
            "S",
            StableDefinitionKind::Destructor,
        );
        assert_only_payload_changed(
            &destructor,
            &destructor_changed,
            StableDefinitionFingerprintPrecision::SignatureAndBody,
        );

        let structure = fingerprint(
            "struct S { n: i32 } fn main() -> i32 { 0 }",
            "S",
            StableDefinitionKind::Struct,
        );
        let structure_changed = fingerprint(
            "struct S { n: i64 } fn main() -> i32 { 0 }",
            "S",
            StableDefinitionKind::Struct,
        );
        assert_eq!(
            structure.precision,
            StableDefinitionFingerprintPrecision::ExactSignature
        );
        assert_ne!(structure.signature, structure_changed.signature);
        assert_eq!(structure.body_or_initializer, None);

        let enumeration = fingerprint(
            "enum E { A(i32), B } fn main() -> i32 { 0 }",
            "E",
            StableDefinitionKind::Enum,
        );
        let enumeration_changed = fingerprint(
            "enum E { A(i64), B } fn main() -> i32 { 0 }",
            "E",
            StableDefinitionKind::Enum,
        );
        assert_eq!(
            enumeration.precision,
            StableDefinitionFingerprintPrecision::ExactSignature
        );
        assert_ne!(enumeration.signature, enumeration_changed.signature);
        assert_eq!(enumeration.body_or_initializer, None);
    }

    fn synthetic_complete_manifest(
        manifest: &SemanticDependencyInputManifest,
    ) -> Arc<SemanticDependencyInputManifest> {
        let mut manifest = manifest.clone();
        manifest.dependency_graph_state = SemanticDependencyGraphState::from_blockers(Vec::new());
        manifest.definition_universe_state = SemanticDefinitionUniverseState::Complete;
        Arc::new(manifest)
    }

    #[test]
    fn dependency_completeness_states_require_evidence_and_preserve_projection() {
        let complete = SemanticDependencyGraphState::from_blockers(Vec::new());
        assert!(complete.is_complete());
        assert!(complete.blockers().is_empty());
        assert!(complete.surface_complete(SemanticDependencySurface::FreeFunctionCall));

        let blocker = SemanticDependencyBlocker {
            owner: None,
            surface: SemanticDependencySurface::FreeFunctionCall,
            reason: SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
        };
        let incomplete =
            SemanticDependencyGraphState::from_blockers(vec![blocker.clone(), blocker.clone()]);
        assert!(!incomplete.is_complete());
        assert_eq!(incomplete.blockers(), &[blocker]);
        assert!(!incomplete.surface_complete(SemanticDependencySurface::FreeFunctionCall));
        assert!(incomplete.surface_complete(SemanticDependencySurface::NamedValueConst));
    }

    #[test]
    fn every_incomplete_dependency_surface_forces_full_invalidation() {
        let source = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let complete = synthetic_complete_manifest(&manifest);

        for (surface, reason) in [
            (
                SemanticDependencySurface::BodyOwner,
                SemanticDependencyIncompleteReason::AnonymousBodyOwnerUnavailable,
            ),
            (
                SemanticDependencySurface::FreeFunctionCall,
                SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
            ),
            (
                SemanticDependencySurface::NonGenericNamedMethodCall,
                SemanticDependencyIncompleteReason::CallerEndpointUnavailable,
            ),
            (
                SemanticDependencySurface::GenericNamedMethodCall,
                SemanticDependencyIncompleteReason::GenericSubstitutionIdentityUnavailable,
            ),
            (
                SemanticDependencySurface::NamedDestructorCall,
                SemanticDependencyIncompleteReason::DestructorEndpointUnavailable,
            ),
            (
                SemanticDependencySurface::ImplicitNamedDestructor,
                SemanticDependencyIncompleteReason::AnonymousDropOwnerUnavailable,
            ),
            (
                SemanticDependencySurface::DeclarationType,
                SemanticDependencyIncompleteReason::ResolvedTypeIdentityUnavailable,
            ),
            (
                SemanticDependencySurface::DeclarationTypeCallHead,
                SemanticDependencyIncompleteReason::TypeCallHeadIdentityUnavailable,
            ),
            (
                SemanticDependencySurface::SupportedTypeCallHead,
                SemanticDependencyIncompleteReason::UnsupportedDynamicTypeCallHead,
            ),
            (
                SemanticDependencySurface::NamedValueConst,
                SemanticDependencyIncompleteReason::ConstEndpointUnavailable,
            ),
        ] {
            let blocker = SemanticDependencyBlocker {
                owner: None,
                surface,
                reason,
            };
            let mut incomplete = (*complete).clone();
            incomplete.dependency_graph_state =
                SemanticDependencyGraphState::from_blockers(vec![blocker.clone()]);
            let plan = plan_semantic_invalidation(&complete, &incomplete);
            assert_eq!(
                plan.scope(),
                &SemanticInvalidationScope::Full {
                    reasons: Arc::from([
                        SemanticFullInvalidationReason::IncompleteDependencyGraph(Arc::from([
                            blocker
                        ]),)
                    ]),
                },
                "surface {surface:?} must fail closed"
            );
        }
    }

    #[test]
    fn incremental_invalidation_closes_transitively_across_module_call_edges() {
        let build = |leaf_value: i32| {
            let main = r#"
                const lib = @import("lib.rue");
                fn main() -> i32 { lib.middle() }
            "#;
            let lib = format!(
                "pub fn leaf() -> i32 {{ {leaf_value} }}\n\
                 pub fn middle() -> i32 {{ leaf() }}\n\
                 pub fn unaffected() -> i32 {{ 7 }}"
            );
            let source = snapshot(
                &[
                    (1, "/p/main.rue", "main.rue", main),
                    (2, "/p/lib.rue", "lib.rue", lib.as_str()),
                ],
                1,
            );
            let mut session = CompilerSession::new();
            publish_with_test_imports(&mut session, &source);
            session
                .canonical_semantic(&CompileOptions::default())
                .unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let previous = build(1);
        let current = build(2);
        let plan = plan_semantic_invalidation(&previous, &current);

        assert_eq!(plan.scope(), &SemanticInvalidationScope::Incremental);
        assert!(plan.added().is_empty());
        assert!(plan.removed().is_empty());
        assert_eq!(
            plan.changed()
                .iter()
                .map(|key| key.name())
                .collect::<Vec<_>>(),
            ["leaf"]
        );
        let mut expected = current
            .definition_fingerprints
            .iter()
            .filter(|entry| matches!(entry.key.name(), "leaf" | "middle" | "main"))
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(plan.invalidated(), expected.as_slice());
        let mut reusable = current
            .definition_fingerprints
            .iter()
            .filter(|entry| !expected.contains(&entry.key))
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        reusable.sort();
        assert_eq!(plan.reusable(), reusable.as_slice());
        assert_eq!(plan.work().reverse_closure_nodes_visited, 3);
    }

    #[test]
    fn planner_ignores_relocation_but_rejects_global_semantic_input_changes() {
        let original = snapshot(
            &[
                (7, "/one/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/one/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
            ],
            7,
        );
        let relocated = snapshot(
            &[
                (41, "/else/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
                (99, "/else/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            99,
        );
        let build = |source: &SourceSnapshot, options: &CompileOptions| {
            let mut session = CompilerSession::new();
            session.update(source).into_result().unwrap();
            session.semantic_dependency_inputs(options, None).unwrap()
        };
        let previous = build(&original, &CompileOptions::default());
        let moved = build(&relocated, &CompileOptions::default());
        let mut planner = CompilerSession::new();
        let plan = planner.semantic_invalidation_plan(&previous, &moved);
        assert_eq!(plan.scope(), &SemanticInvalidationScope::Incremental);
        assert!(plan.invalidated().is_empty());
        assert_eq!(plan.reusable().len(), 2);

        let alternative_target = *Target::all()
            .iter()
            .find(|&&target| target != moved.input().target)
            .expect("at least one supported target differs from the current target");
        assert_ne!(alternative_target, moved.input().target);
        let target = build(
            &relocated,
            &CompileOptions {
                target: alternative_target,
                ..CompileOptions::default()
            },
        );
        assert!(matches!(
            planner.semantic_invalidation_plan(&moved, &target).scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::TargetChanged)
        ));
        let features = build(
            &relocated,
            &CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..CompileOptions::default()
            },
        );
        assert!(matches!(
            planner.semantic_invalidation_plan(&moved, &features).scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::PreviewFeaturesChanged)
        ));

        let mut root_changed = (*moved).clone();
        root_changed.input.sources = SourceRevision::new(
            ModuleId::from_logical_path("a.rue").unwrap(),
            root_changed.input.sources.modules().to_vec(),
        )
        .unwrap();
        let root_changed = Arc::new(root_changed);
        assert!(matches!(
            planner
                .semantic_invalidation_plan(&moved, &root_changed)
                .scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::RootChanged)
        ));

        let mut imports_changed = (*moved).clone();
        imports_changed.module_imports = vec![StableModuleImportDependency::Missing {
            importer: ModuleId::from_logical_path("main.rue").unwrap(),
            normalized_specifier: Arc::from("a.rue"),
        }]
        .into();
        let imports_changed = Arc::new(imports_changed);
        let plan = planner.semantic_invalidation_plan(&moved, &imports_changed);
        assert!(matches!(
            plan.scope(),
            SemanticInvalidationScope::Full { reasons }
                if reasons.contains(&SemanticFullInvalidationReason::ModuleImportsChanged)
        ));
        assert!(plan.reusable().is_empty());
    }

    #[test]
    fn dependency_manifest_keeps_canonical_module_edges_across_physical_relocation() {
        let original = snapshot(
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
        let moved = snapshot(
            &[
                (
                    1,
                    "/p/app/main.rue",
                    "app/main.rue",
                    "fn main() -> i32 { let h = @import(\"helper.rue\"); 0 }",
                ),
                (2, "/else/helper.rue", "app/helper.rue", "fn helper() {}"),
            ],
            1,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &original);
        let resolved = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(resolved.definition_universe_complete());
        assert!(matches!(
            &resolved.module_imports()[0],
            StableModuleImportDependency::Resolved { importer, target, .. }
                if importer.as_str() == "app/main.rue" && target.as_str() == "app/helper.rue"
        ));

        let work = publish_with_test_imports(&mut session, &moved);
        assert_eq!(work.syntax.lexer_invocations, 0);
        let relocated = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(relocated.definition_universe_complete());
        assert!(matches!(
            &relocated.module_imports()[0],
            StableModuleImportDependency::Resolved { importer, target, .. }
                if importer.as_str() == "app/main.rue" && target.as_str() == "app/helper.rue"
        ));
        assert_eq!(relocated.work().import_records_visited, 1);
        assert_eq!(relocated.work().extra_rir_instructions_visited, 0);
        assert_eq!(session.work().dependency_manifests.executions, 2);
    }

    #[test]
    fn dependency_endpoint_translation_fails_closed_for_missing_and_non_functions() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const answer: i32 = 42; fn main() -> i32 { answer }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let definitions = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert!(stable_free_function_endpoint(&definitions, 1, "missing").is_err());
        assert!(stable_free_function_endpoint(&definitions, 1, "answer").is_err());
        assert!(stable_named_method_endpoint(&definitions, 1, "answer", "answer").is_err());

        let rejected = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() -> i32 { true }")],
            1,
        );
        session.update(&rejected).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(!manifest.definition_universe_complete());
        match &manifest.definition_universe_state {
            SemanticDefinitionUniverseState::Complete => {
                panic!("failed definition universe cannot be complete")
            }
            SemanticDefinitionUniverseState::Incomplete(
                SemanticDefinitionUniverseIncompleteReason::StableDefinitionsFailed(failures),
            ) => assert!(!failures.failures.is_empty()),
        }
        assert!(!manifest.free_function_caller_dependencies_complete());
        assert!(manifest.free_function_dependencies().is_empty());
    }

    #[test]
    fn stable_named_method_dependency_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StableNamedMethodDependency>();
        assert_send_sync::<StableNamedMethodDependencyTarget>();
        assert_send_sync::<StableNamedConstDependency>();
        assert_send_sync::<StableNamedConstDependencyTarget>();
        assert_send_sync::<StableBodyDependencyInputRecord>();
    }

    #[test]
    fn implicit_drop_edges_distinguish_body_obligations_from_global_glue() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StableImplicitNamedDestructorDependency>();
        let program = r#"
            struct Leaf { n: i32 }
            drop fn Leaf(self) {}
            struct Wrapper { leaves: [Leaf; 2] }
            fn consume() { let value = Wrapper { leaves: [Leaf { n: 1 }, Leaf { n: 2 }] }; }
            fn main() { consume(); }
        "#;
        let build = |id, physical: &str| {
            let source = snapshot(&[(id, physical, "main.rue", program)], id);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(4, "/one/main.rue");
        let relocated = build(97, "/else/main.rue");
        assert_eq!(
            first.implicit_named_destructor_dependencies(),
            relocated.implicit_named_destructor_dependencies()
        );
        let names = first
            .implicit_named_destructor_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.source.kind(),
                    edge.source.name().to_string(),
                    edge.target.owner().unwrap().name().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&(
            StableDefinitionKind::Function,
            "consume".into(),
            "Leaf".into(),
        )));
        assert!(names.contains(&(StableDefinitionKind::Struct, "Leaf".into(), "Leaf".into(),)));
        assert!(first.implicit_named_destructor_dependencies_complete());
        assert_eq!(
            first.work().implicit_named_destructor_events_translated,
            first.implicit_named_destructor_dependencies().len()
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn anonymous_drop_owner_composes_through_its_query_identity() {
        let source = snapshot(
            &[(
                4,
                "/p/main.rue",
                "main.rue",
                r#"
                    fn Box(comptime T: type) -> type {
                        struct { v: T, drop fn(self) {} }
                    }
                    fn main() { let B = Box(i32); let value = B { v: 1 }; }
                "#,
            )],
            4,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let output = session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..CompileOptions::default()
            })
            .unwrap();
        // Anonymous destructor ownership is represented by its query identity,
        // so `main`, the type producer, and the reached destructor compose.
        assert_eq!(output.functions().len(), 3);
    }

    #[test]
    fn resolved_declaration_type_edges_translate_without_rir_rescan() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                r#"
                struct Leaf { n: i32 }
                struct Holder { leaf: Leaf, fn get(borrow self, value: Leaf) -> Leaf { value } }
                enum Choice { One(Leaf) }
                fn convert(value: Leaf) -> Holder { Holder { leaf: value } }
                drop fn Holder(self) {}
                fn main() -> i32 { 0 }
            "#,
            )],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let names = manifest
            .declaration_type_dependencies()
            .iter()
            .map(|edge| {
                (
                    edge.source.name().to_owned(),
                    edge.target.name().to_owned(),
                    edge.kind,
                )
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&(
            "Holder".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Field
        )));
        assert!(names.contains(&(
            "Choice".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Payload
        )));
        assert!(names.contains(&(
            "convert".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Signature
        )));
        assert!(names.contains(&(
            "get".into(),
            "Leaf".into(),
            rue_air::DeclarationTypeDependencyKind::Signature
        )));
        assert!(names.contains(&(
            "Holder".into(),
            "Holder".into(),
            rue_air::DeclarationTypeDependencyKind::Owner
        )));
        assert!(manifest.declaration_type_dependencies_complete());
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn deferred_nested_nominal_and_alias_types_keep_stable_declaration_edges() {
        let build = |main_id, lib_id, root: &str, reversed: bool| {
            let main = (
                main_id,
                format!("{root}/main.rue"),
                "main.rue",
                r#"const lib = @import("lib.rue");
                   const Alias = lib.Leaf;
                   fn consume(comptime N: i32, values: [Alias; N]) -> i32 { 0 }
                   fn main() -> i32 { 0 }"#,
            );
            let lib = (
                lib_id,
                format!("{root}/lib.rue"),
                "lib.rue",
                "pub struct Leaf { value: i32 }",
            );
            let owned = if reversed {
                vec![lib, main]
            } else {
                vec![main, lib]
            };
            let entries = owned
                .iter()
                .map(|(id, path, module, text)| (*id, path.as_str(), *module, *text))
                .collect::<Vec<_>>();
            let source = snapshot(&entries, main_id);
            let mut session = CompilerSession::new();
            publish_with_test_imports(&mut session, &source);
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(3, 8, "/p", false);
        let moved = build(91, 4, "/elsewhere", true);
        assert_eq!(
            first.declaration_type_dependencies(),
            moved.declaration_type_dependencies()
        );
        let consume_targets = first
            .declaration_type_dependencies()
            .iter()
            .filter(|edge| edge.source.name() == "consume")
            .map(|edge| {
                (
                    edge.target.module().as_str(),
                    edge.target.name(),
                    edge.target.kind(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            consume_targets.contains(&("main.rue", "Alias", StableDefinitionKind::ValueConst)),
            "{consume_targets:#?}"
        );
        assert!(consume_targets.contains(&("lib.rue", "Leaf", StableDefinitionKind::Struct)));
        assert!(first.declaration_type_dependencies_complete());
        assert!(
            !first
                .dependency_blockers()
                .iter()
                .any(|blocker| { blocker.surface() == SemanticDependencySurface::DeclarationType })
        );
        assert_eq!(first.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn module_qualified_type_call_head_uses_exact_callable_endpoint() {
        let source = snapshot(
            &[
                (
                    3,
                    "/p/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       fn consume(comptime T: type, value: lib.Box(T)) -> i32 { 0 }
                       fn main() -> i32 { 0 }"#,
                ),
                (
                    8,
                    "/p/lib.rue",
                    "lib.rue",
                    "pub fn Box(comptime T: type) -> type { struct { value: T } }",
                ),
            ],
            3,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let [edge] = manifest.declaration_type_call_head_dependencies() else {
            panic!("expected one module-qualified type-call head");
        };
        assert_eq!(edge.source.module().as_str(), "main.rue");
        assert_eq!(edge.source.name(), "consume");
        assert_eq!(edge.callable.module().as_str(), "lib.rue");
        assert_eq!(edge.callable.name(), "Box");
        assert_eq!(edge.callable.kind(), StableDefinitionKind::Function);
    }

    #[test]
    fn fixed_string_type_head_is_a_builtin_input_not_a_definition() {
        let source = snapshot(
            &[(
                4,
                "/p/main.rue",
                "main.rue",
                "fn consume(value: Str(8)) -> i32 { 0 } fn main() -> i32 { 0 }",
            )],
            4,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let manifest = session.semantic_dependency_inputs(&options, None).unwrap();
        let [input] = manifest.builtin_type_call_head_inputs() else {
            panic!("expected one fixed-string builtin input");
        };
        assert_eq!(input.source.name(), "consume");
        assert_eq!(
            input.builtin,
            rue_air::BuiltinTypeCallHead::FixedCapacityString
        );
        assert!(
            manifest
                .declaration_type_call_head_dependencies()
                .is_empty()
        );
        assert!(manifest.supported_type_call_heads_complete());
        assert!(manifest.declaration_type_call_head_dependencies_complete());
        assert_eq!(manifest.work().builtin_type_call_head_inputs_translated, 1);
        assert_eq!(manifest.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn named_owner_associated_type_head_is_not_supported_type_syntax() {
        let source = snapshot(
            &[(
                6,
                "/p/main.rue",
                "main.rue",
                r#"struct Factory {
                       fn Make() -> type { struct { value: i32 } }
                   }
                   fn consume(value: Factory.Make()) -> i32 { 0 }
                   fn main() -> i32 { 0 }"#,
            )],
            6,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        assert!(
            session
                .canonical_semantic(&CompileOptions::default())
                .is_err(),
            "dotted type-call heads are module-qualified free functions, not associated functions"
        );
    }

    #[test]
    fn named_const_initializer_edges_are_stable_direct_and_zero_scan() {
        let program = r#"
            struct Point { x: i32 }
            fn inc(comptime n: i32) -> i32 { n + 1 }
            const A: i32 = 1;
            const B: i32 = inc(A);
            const C: i32 = A + B;
            const D: i32 = B + C;
            const P = Point;
            fn main() -> i32 { D }
        "#;
        let build = |file, path| {
            let source = snapshot(&[(file, path, "main.rue", program)], file);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let first = build(2, "/one/main.rue");
        let moved = build(88, "/moved/main.rue");
        assert_eq!(
            first.named_const_dependencies(),
            moved.named_const_dependencies()
        );
        let names = first
            .named_const_dependencies()
            .iter()
            .map(|edge| {
                let target = match &edge.target {
                    StableNamedConstDependencyTarget::ValueConst(key)
                    | StableNamedConstDependencyTarget::FreeFunction(key)
                    | StableNamedConstDependencyTarget::NamedType(key)
                    | StableNamedConstDependencyTarget::ModuleBinding(key) => key.name(),
                };
                (edge.source.name(), target)
            })
            .collect::<Vec<_>>();
        for edge in [
            ("B", "A"),
            ("B", "inc"),
            ("C", "A"),
            ("C", "B"),
            ("D", "B"),
            ("D", "C"),
            ("P", "Point"),
        ] {
            assert!(
                names.contains(&edge),
                "missing direct edge {edge:?}: {names:?}"
            );
        }
        assert!(first.named_value_const_dependencies_complete());
        assert_eq!(first.work().named_const_events_translated, names.len());
        assert_eq!(first.work().extra_rir_instructions_visited, 0);

        let renamed_program = program
            .replace("const A: i32 = 1", "const Z: i32 = 1")
            .replace("inc(A)", "inc(Z)")
            .replace("A + B", "Z + B");
        let renamed_source = snapshot(&[(2, "/one/main.rue", "main.rue", &renamed_program)], 2);
        let mut renamed_session = CompilerSession::new();
        renamed_session
            .update(&renamed_source)
            .into_result()
            .unwrap();
        let renamed = renamed_session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert_ne!(
            first.named_const_dependencies(),
            renamed.named_const_dependencies()
        );
        assert!(renamed.named_const_dependencies().iter().any(|edge| {
            edge.source.name() == "B"
                && matches!(&edge.target, StableNamedConstDependencyTarget::ValueConst(key) if key.name() == "Z")
        }));
    }

    #[test]
    fn cyclic_const_initializers_publish_no_partial_dependency_graph() {
        let source = snapshot(
            &[(
                1,
                "/p/main.rue",
                "main.rue",
                "const A: i32 = B; const B: i32 = A; fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        assert!(
            session
                .canonical_semantic(&CompileOptions::default())
                .is_err()
        );
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        assert!(manifest.named_const_dependencies().is_empty());
        assert!(!manifest.named_value_const_dependencies_complete());
        assert!(!manifest.definition_universe_complete());
    }

    #[test]
    fn qualified_const_edges_keep_module_binding_and_exact_member_identity() {
        let source = snapshot(
            &[
                (
                    3,
                    "/p/main.rue",
                    "main.rue",
                    r#"const lib = @import("lib.rue");
                       const other = @import("other.rue");
                       const X: i32 = lib.BASE;
                       const Y: i32 = other.BASE;
                       const T = lib.Row;
                       fn main() -> i32 { X + Y }"#,
                ),
                (11, "/p/other.rue", "other.rue", "pub const BASE: i32 = 5;"),
                (
                    9,
                    "/p/lib.rue",
                    "lib.rue",
                    "pub const BASE: i32 = 4; pub struct Row { n: i32 }",
                ),
            ],
            3,
        );
        let mut session = CompilerSession::new();
        publish_with_test_imports(&mut session, &source);
        let manifest = session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let tags = manifest
            .named_const_dependencies()
            .iter()
            .map(|edge| {
                let (tag, target) = match &edge.target {
                    StableNamedConstDependencyTarget::ValueConst(key) => ("const", key),
                    StableNamedConstDependencyTarget::NamedType(key) => ("type", key),
                    StableNamedConstDependencyTarget::ModuleBinding(key) => ("module", key),
                    StableNamedConstDependencyTarget::FreeFunction(key) => ("fn", key),
                };
                (
                    edge.source.name(),
                    tag,
                    target.module().as_str(),
                    target.name(),
                )
            })
            .collect::<Vec<_>>();
        for expected in [
            ("X", "module", "main.rue", "lib"),
            ("X", "const", "lib.rue", "BASE"),
            ("T", "module", "main.rue", "lib"),
            ("T", "type", "lib.rue", "Row"),
            ("Y", "module", "main.rue", "other"),
            ("Y", "const", "other.rue", "BASE"),
        ] {
            assert!(tags.contains(&expected), "missing {expected:?}: {tags:?}");
        }
        assert!(
            tags.iter()
                .all(|(source, _, _, _)| *source != "lib" && *source != "other")
        );
    }

    #[test]
    fn const_dependency_capture_work_is_edge_proportional() {
        let build = |extra: usize| {
            let mut program = "const A: i32 = 1; const B: i32 = A;".to_string();
            for i in 0..extra {
                program.push_str(&format!(" const UNUSED_{i}: i32 = {i};"));
            }
            program.push_str(" fn main() -> i32 { B }");
            let source = snapshot(&[(1, "/p/main.rue", "main.rue", &program)], 1);
            let mut session = CompilerSession::new();
            session.update(&source).into_result().unwrap();
            session
                .semantic_dependency_inputs(&CompileOptions::default(), None)
                .unwrap()
        };
        let one = build(1);
        let many = build(128);
        assert_eq!(
            one.named_const_dependencies(),
            many.named_const_dependencies()
        );
        assert_eq!(one.work().named_const_events_translated, 1);
        assert_eq!(many.work().named_const_events_translated, 1);
        assert_eq!(many.work().extra_rir_instructions_visited, 0);
    }

    #[test]
    fn stable_definition_target_and_feature_inputs_are_separate() {
        let source = base();
        let default = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&default).unwrap();
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != default.target)
            .expect("multiple compiler targets");
        session
            .stable_definitions(&CompileOptions {
                target: other_target,
                ..default.clone()
            })
            .unwrap();
        session
            .stable_definitions(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..default
            })
            .unwrap();

        assert_eq!(session.work().definitions.executions, 3);
        assert_eq!(session.work().definition_entries, 3);
        assert_eq!(session.work().definition_records.len(), 3);
        assert!(session.work().definition_records.iter().all(|record| {
            record.binding.bind_invocations == 1
                && record.manifest.build_invocations == 1
                && !record.failed
        }));
    }

    #[test]
    fn definition_store_eviction_recomputes_then_reuses_with_exact_metrics() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let variants = retention_variants();

        for options in &variants {
            session.stable_definitions(options).unwrap();
        }
        assert_eq!(session.work().definitions.executions, variants.len());
        assert_eq!(session.work().definitions.reuses, 0);
        assert_eq!(
            session.work().retention.definition_query_entries,
            QUERY_TERMINAL_RETENTION_LIMIT
        );
        assert_eq!(session.work().retention.definition_query_evictions, 1);

        session.stable_definitions(&variants[0]).unwrap();
        assert_eq!(session.work().definitions.executions, variants.len() + 1);
        assert_eq!(session.work().definitions.reuses, 0);
        assert_eq!(session.work().retention.definition_query_evictions, 2);

        session.stable_definitions(&variants[0]).unwrap();
        assert_eq!(session.work().definitions.executions, variants.len() + 1);
        assert_eq!(session.work().definitions.reuses, 1);
        assert_eq!(session.work().retention.definition_query_evictions, 2);
    }

    fn definition_query_fixture() -> (DefinitionQueryKey, DefinitionQueryOutput) {
        let source = base();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.stable_definitions(&options).unwrap();
        let imports = session.accepted_semantic_import_graph().unwrap();
        let key = DefinitionQueryKey {
            input: SemanticInputDescriptor::new(&source, options.target, &options.preview_features),
            imports: imports.clone(),
        };
        let output = session
            .queries
            .definitions
            .get(&key)
            .expect("definition computation was published")
            .output
            .clone();
        (key, output)
    }

    #[test]
    #[should_panic(expected = "typed query record key does not match")]
    fn definition_store_rejects_foreign_option_provenance() {
        let (original_key, output) = definition_query_fixture();
        let mut foreign_key = original_key;
        foreign_key.input.preview_features =
            StablePreviewFeatures::new(&PreviewFeatures::from([PreviewFeature::TestInfra]));
        let mut store = TypedQueryStore::<DefinitionQuery>::default();
        store.insert(DefinitionCacheEntry {
            key: foreign_key,
            output,
        });
    }

    #[test]
    #[should_panic(expected = "typed query record key does not match")]
    fn definition_store_rejects_foreign_import_provenance() {
        let (original_key, output) = definition_query_fixture();
        let mut foreign_key = original_key;
        foreign_key.imports = CanonicalImportGraph::from_discovery_records(
            foreign_key.imports.root().clone(),
            vec![crate::CanonicalImportRecord::new(
                foreign_key.imports.root().clone(),
                "foreign.rue",
                CanonicalImportResolution::Missing,
            )],
        );
        let mut store = TypedQueryStore::<DefinitionQuery>::default();
        store.insert(DefinitionCacheEntry {
            key: foreign_key,
            output,
        });
    }

    #[test]
    fn definition_keys_ignore_opt_linker_relocation_file_ids_and_order() {
        let original = snapshot(
            &[
                (7, "/old/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
                (2, "/old/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let moved = snapshot(
            &[
                (90, "/new/a.rue", "a.rue", "fn a() {}"),
                (40, "/new/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            40,
        );
        let renamed = snapshot(
            &[
                (90, "/new/lib/a.rue", "lib/a.rue", "fn a() {}"),
                (40, "/new/main.rue", "main.rue", "fn main() -> i32 { 0 }"),
            ],
            40,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        let first = session
            .stable_definitions(&CompileOptions {
                linker: LinkerMode::System("x".to_string()),
                opt_level: OptLevel::O2,
                ..CompileOptions::default()
            })
            .unwrap();
        let keys = |set: &BoundDefinitionSet| {
            set.definitions()
                .iter()
                .map(|record| record.stable_key().clone())
                .collect::<Vec<_>>()
        };
        let first_keys = keys(&first);

        session.update(&moved).into_result().unwrap();
        let second = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert_eq!(keys(&second), first_keys);
        assert_eq!(session.work().definition_entries_invalidated, 1);

        session.update(&renamed).into_result().unwrap();
        let third = session
            .stable_definitions(&CompileOptions::default())
            .unwrap();
        assert_ne!(keys(&third), first_keys);
    }

    #[test]
    fn failed_parse_preserves_ids_while_semantic_rejection_issues_none() {
        let valid = base();
        let syntax_bad = snapshot(
            &[
                (7, "/p/main.rue", "main.rue", "fn main( {"),
                (2, "/p/a.rue", "a.rue", "fn a() {}"),
            ],
            7,
        );
        let semantic_bad = snapshot(
            &[(
                7,
                "/p/main.rue",
                "main.rue",
                "fn main() -> i32 { missing_name }",
            )],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        let ids = session.stable_definitions(&options).unwrap();
        assert!(session.update(&syntax_bad).result().is_err());
        assert!(Arc::ptr_eq(
            &ids,
            &session.stable_definitions(&options).unwrap()
        ));

        session.update(&semantic_bad).into_result().unwrap();
        let first = session.stable_definitions(&options).unwrap_err();
        let second = session.stable_definitions(&options).unwrap_err();
        assert_eq!(format!("{first:?}"), format!("{second:?}"));
        assert_eq!(session.work().definitions.executions, 1);
        // Current failure and the separately retained last-good definition
        // artifact are both bounded typed terminals.
        assert_eq!(session.work().definition_entries, 2);
        assert_eq!(session.work().semantic_records.len(), 1);
        assert!(session.work().semantic_records[0].failed);

        session.update(&valid).into_result().unwrap();
        assert!(session.stable_definitions(&options).is_ok());
        assert_eq!(session.work().definitions.executions, 1);
        assert_eq!(session.work().definitions.reuses, 3);
    }

    #[test]
    fn diagnostic_artifacts_retain_attempt_provenance_and_query_identity() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrontendDiagnosticSnapshot>();

        let valid = base();
        let syntax_bad = snapshot(&[(7, "/attempt/bad.rue", "bad.rue", "fn main( {")], 7);
        let semantic_bad = snapshot(
            &[(
                7,
                "/attempt/semantic.rue",
                "semantic.rue",
                "fn main() -> i32 { missing_name }",
            )],
            7,
        );
        let warning_source = snapshot(
            &[(
                7,
                "/attempt/warning.rue",
                "warning.rue",
                "fn main() -> i32 { let unused = 1; 0 }",
            )],
            7,
        );
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        let published = session.published_owner().unwrap().clone();

        let failed = session.update(&syntax_bad);
        let syntax_diagnostics = failed.diagnostics().clone();
        assert_eq!(
            syntax_diagnostics.source().metadata(),
            syntax_bad.metadata()
        );
        assert_eq!(
            syntax_diagnostics.source_revision(),
            syntax_bad.source_revision()
        );
        assert!(!syntax_diagnostics.errors().is_empty());
        assert!(Arc::ptr_eq(session.published_owner().unwrap(), &published));
        assert!(Arc::ptr_eq(
            session
                .diagnostics_for(&syntax_bad, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &syntax_diagnostics
        ));

        session.update(&semantic_bad).into_result().unwrap();
        let options = CompileOptions::default();
        session.canonical_semantic(&options).unwrap_err();
        let first = session.latest_diagnostics().unwrap().clone();
        let first_fingerprint = first
            .errors()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        session.canonical_semantic(&options).unwrap_err();
        let reused = session.latest_diagnostics().unwrap().clone();
        assert!(Arc::ptr_eq(&first, &reused));
        assert_eq!(
            first_fingerprint,
            reused
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        let FrontendDiagnosticIdentity::Semantic(input) = first.identity() else {
            panic!("semantic diagnostic stage");
        };
        assert_eq!(input.opt_level(), crate::StableOptLevel::O0);

        session.update(&warning_source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let warning = session.latest_diagnostics().unwrap().clone();
        assert!(warning.is_success());
        assert!(!warning.warnings().is_empty());
        session
            .canonical_semantic(&CompileOptions {
                linker: LinkerMode::Internal,
                ..options.clone()
            })
            .unwrap();
        assert!(Arc::ptr_eq(&warning, session.latest_diagnostics().unwrap()));
        session
            .canonical_semantic(&CompileOptions {
                opt_level: OptLevel::O1,
                ..options.clone()
            })
            .unwrap();
        let optimized = session.latest_diagnostics().unwrap().clone();
        assert!(!Arc::ptr_eq(&warning, &optimized));
        session
            .canonical_semantic(&CompileOptions {
                preview_features: PreviewFeatures::from([PreviewFeature::TestInfra]),
                ..options.clone()
            })
            .unwrap();
        let featured = session.latest_diagnostics().unwrap().clone();
        assert!(!Arc::ptr_eq(&warning, &featured));
        let other_target = *Target::all()
            .iter()
            .find(|&&target| target != options.target)
            .unwrap();
        session
            .canonical_semantic(&CompileOptions {
                target: other_target,
                ..options
            })
            .unwrap();
        assert!(!Arc::ptr_eq(
            &warning,
            session.latest_diagnostics().unwrap()
        ));
        let old = syntax_diagnostics.clone();
        std::thread::spawn(move || {
            assert_eq!(old.source().metadata().root_file_id(), FileId::new(7));
            assert!(!old.errors().is_empty());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn merge_diagnostics_are_memoized_pointer_identically() {
        let duplicate = snapshot(
            &[(1, "/p/main.rue", "main.rue", "fn main() {} fn main() {}")],
            1,
        );
        let mut session = CompilerSession::new();
        session.update(&duplicate).into_result().unwrap();
        session.merge().unwrap_err();
        let first = session.latest_diagnostics().unwrap().clone();
        session.merge().unwrap_err();
        let second = session.latest_diagnostics().unwrap().clone();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(matches!(
            first.identity(),
            FrontendDiagnosticIdentity::Merge
        ));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().diagnostic_reuses, 1);
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

        crate::parsed_modules::reset_parse_operation_entries();
        let exact = session.update(&source);

        assert!(Arc::ptr_eq(exact.diagnostics(), &origin));
        assert_eq!(exact.work(), ParsedModulesWork::default());
        assert_eq!(crate::parsed_modules::parse_operation_entries(), 0);
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

        crate::parsed_modules::reset_parse_operation_entries();
        let exact = session.update_for_presentation(&source);

        assert!(Arc::ptr_eq(exact.diagnostics(), &origin));
        assert_eq!(exact.work(), ParsedModulesWork::default());
        assert_eq!(crate::parsed_modules::parse_operation_entries(), 0);
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
    fn merge_cache_reselects_its_origin_after_diagnostic_index_eviction() {
        let source = base();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.merge().unwrap();
        let origin = session.latest_diagnostics().unwrap().clone();

        evict_diagnostic_index(&mut session);
        assert!(
            session
                .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Merge)
                .is_none()
        );
        let publications = session.work().diagnostic_publications;
        let reuses = session.work().diagnostic_reuses;

        session.merge().unwrap();

        assert!(Arc::ptr_eq(session.latest_diagnostics().unwrap(), &origin));
        assert_eq!(session.work().merge.executions, 1);
        assert_eq!(session.work().merge.reuses, 1);
        assert_eq!(session.work().diagnostic_publications, publications);
        assert_eq!(session.work().diagnostic_reuses, reuses + 1);
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&source, &FrontendDiagnosticIdentity::Merge)
                .unwrap(),
            &origin
        ));
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
        assert_eq!(session.work().retention.import_query_entries, 1);
        assert_eq!(session.work().retention.import_query_evictions, 0);
        assert_eq!(session.work().diagnostic_publications, publications);
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&source, &stage)
                .unwrap(),
            &origin
        ));
    }

    #[test]
    fn semantic_cache_reselects_failure_origin_after_diagnostic_index_eviction() {
        let source = snapshot(
            &[(7, "/p/main.rue", "main.rue", "fn main() -> i32 { missing }")],
            7,
        );
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let first_errors = session.canonical_semantic(&options).unwrap_err();
        let origin = session.latest_diagnostics().unwrap().clone();
        let stage = origin.identity().clone();

        evict_diagnostic_index(&mut session);
        assert!(
            session
                .most_recent_diagnostics_for(&source, &stage)
                .is_none()
        );
        let publications = session.work().diagnostic_publications;
        let reuses = session.work().diagnostic_reuses;

        let reused_errors = session.canonical_semantic(&options).unwrap_err();

        assert_eq!(
            first_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            reused_errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        assert!(Arc::ptr_eq(session.latest_diagnostics().unwrap(), &origin));
        assert_eq!(session.work().semantic.executions, 1);
        assert_eq!(session.work().semantic.reuses, 1);
        assert_eq!(session.work().diagnostic_publications, publications);
        assert_eq!(session.work().diagnostic_reuses, reuses + 1);
        assert!(Arc::ptr_eq(
            session
                .most_recent_diagnostics_for(&source, &stage)
                .unwrap(),
            &origin
        ));
    }

    #[test]
    fn semantic_diagnostic_identity_includes_the_accepted_import_graph() {
        let source = snapshot(
            &[
                (
                    1,
                    "/p/main.rue",
                    "main.rue",
                    r#"const selected = @import("choice.rue");
fn main() -> i32 { selected.value() }"#,
                ),
                (2, "/p/a.rue", "a.rue", "pub fn value() -> i32 { 1 }"),
                (3, "/p/b.rue", "b.rue", "pub fn value() -> i32 { 2 }"),
            ],
            1,
        );
        let root = source.module_id(FileId::new(1)).unwrap().clone();
        let a = source.module_id(FileId::new(2)).unwrap().clone();
        let b = source.module_id(FileId::new(3)).unwrap().clone();
        let resolution = ModuleResolutionInputs::new(
            root.clone(),
            [
                (root.clone(), "/p/main.rue"),
                (a.clone(), "/p/a.rue"),
                (b.clone(), "/p/b.rue"),
            ]
            .into_iter()
            .map(|(module, path)| crate::ModuleResolutionInput {
                module,
                physical_path: Arc::from(path),
            })
            .collect(),
        )
        .unwrap();
        let graph = |target| {
            CanonicalImportGraph::from_supplied(
                root.clone(),
                vec![crate::CanonicalImportRecord::new(
                    root.clone(),
                    "choice.rue",
                    CanonicalImportResolution::Resolved(target),
                )],
                &resolution,
            )
            .unwrap()
        };
        let graph_a = graph(a);
        let graph_b = graph(b);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();

        session.adopt_test_import_graph(graph_a.clone());
        let output_a = session.canonical_semantic(&options).unwrap();
        let diagnostics_a = session.latest_diagnostics().unwrap().clone();
        let main = body_query_key(&mut session, &options, "main");
        let main_a = retained_body_transaction(&session, &main).0;
        session.adopt_test_import_graph(graph_b.clone());
        let output_b = session.canonical_semantic(&options).unwrap();
        let diagnostics_b = session.latest_diagnostics().unwrap().clone();
        let main_b = retained_body_transaction(&session, &main).0;

        let mut fresh = CompilerSession::new();
        fresh.update(&source).into_result().unwrap();
        fresh.adopt_test_import_graph(graph_b.clone());
        let fresh_b = fresh.canonical_semantic(&options).unwrap();

        assert!(!Arc::ptr_eq(&diagnostics_a, &diagnostics_b));
        assert_ne!(
            main_a, main_b,
            "accepted import retargeting invalidates the body terminal"
        );
        assert_ne!(
            normalize_session_local_spurs(format!("{:?}", output_a.functions())),
            normalize_session_local_spurs(format!("{:?}", output_b.functions()))
        );
        assert_semantic_artifact_parity(&session, &output_b, &fresh_b);
        let FrontendDiagnosticIdentity::Semantic(input_a) = diagnostics_a.identity() else {
            panic!("semantic diagnostics");
        };
        let FrontendDiagnosticIdentity::Semantic(input_b) = diagnostics_b.identity() else {
            panic!("semantic diagnostics");
        };
        assert_eq!(input_a.program().imports(), &graph_a);
        assert_eq!(input_b.program().imports(), &graph_b);
        assert_eq!(session.work().semantic.executions, 2);
    }

    #[test]
    fn long_failure_recovery_sequence_bounds_diagnostics_and_preserves_last_good() {
        let options = CompileOptions::default();
        let source = |text: &str| snapshot(&[(7, "/p/main.rue", "main.rue", text)], 7);
        let initial = source("fn main() -> i32 { 0 }");
        let mut session = CompilerSession::new();
        session.update(&initial).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        assert_eq!(session.work().retention.diagnostic_entries, 5);
        assert_eq!(session.work().retention.diagnostic_source_attempts, 1);
        assert_eq!(
            session.work().retention.diagnostic_source_bytes,
            initial.files().map(|file| file.source.len()).sum::<usize>()
        );
        let initial_good = session.last_good_semantic_diagnostics().unwrap().clone();
        assert!(initial_good.is_success());

        let first_bad = source("fn main( {");
        let first_update = session.update(&first_bad);
        assert!(first_update.result().is_err());
        let caller_pinned = first_update.diagnostics().clone();
        assert!(Arc::ptr_eq(
            session.last_good_semantic_diagnostics().unwrap(),
            &initial_good
        ));

        let mut maximum_attempt_bytes: usize = initial.files().map(|file| file.source.len()).sum();
        for revision in 1..=32 {
            let syntax_text = format!("// {}\nfn main( {{", "x".repeat(revision));
            maximum_attempt_bytes = maximum_attempt_bytes.max(syntax_text.len());
            let syntax_bad = source(&syntax_text);
            assert!(session.update(&syntax_bad).result().is_err());
            let before_semantic_failure = session.last_good_semantic_diagnostics().unwrap().clone();

            let semantic_text = format!("fn main() -> i32 {{ missing_{revision} }}");
            maximum_attempt_bytes = maximum_attempt_bytes.max(semantic_text.len());
            let semantic_bad = source(&semantic_text);
            session.update(&semantic_bad).into_result().unwrap();
            session.canonical_semantic(&options).unwrap_err();
            assert!(Arc::ptr_eq(
                session.last_good_semantic_diagnostics().unwrap(),
                &before_semantic_failure
            ));
            assert!(!session.latest_diagnostics().unwrap().is_success());

            let valid_text = format!("fn main() -> i32 {{ {revision} }}");
            maximum_attempt_bytes = maximum_attempt_bytes.max(valid_text.len());
            let valid = source(&valid_text);
            session.update(&valid).into_result().unwrap();
            let recovered = session.canonical_semantic(&options).unwrap();
            let recovered_diagnostics = session.latest_diagnostics().unwrap();
            assert!(recovered_diagnostics.is_success());
            assert!(Arc::ptr_eq(
                recovered_diagnostics,
                session.latest_successful_diagnostics().unwrap()
            ));
            assert!(Arc::ptr_eq(
                recovered_diagnostics,
                session.last_good_semantic_diagnostics().unwrap()
            ));
            assert!(Arc::ptr_eq(
                recovered_diagnostics,
                session
                    .diagnostics_for(
                        &valid,
                        &FrontendDiagnosticIdentity::Semantic(semantic_diagnostic_input(
                            recovered.input(),
                            session.accepted_semantic_import_graph().unwrap(),
                        ))
                    )
                    .unwrap()
            ));
        }

        let retention = session.work().retention;
        assert!(retention.diagnostic_entries <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
        assert!(retention.diagnostic_source_attempts <= retention.diagnostic_entries);
        assert!(
            retention.diagnostic_source_bytes
                <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT * maximum_attempt_bytes
        );
        assert!(
            session
                .diagnostics_for(&first_bad, &FrontendDiagnosticIdentity::Syntax)
                .is_none(),
            "unpinned old cache entry should be evicted"
        );
        assert_eq!(caller_pinned.source_revision(), first_bad.source_revision());
        assert!(!caller_pinned.errors().is_empty());

        let final_source = source("fn main() -> i32 { 32 }");
        let mut fresh = CompilerSession::new();
        fresh.update(&final_source).into_result().unwrap();
        let fresh_output = fresh.canonical_semantic(&options).unwrap();
        let retained_output = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", retained_output.functions()),
            format!("{:?}", fresh_output.functions())
        );
        assert_eq!(retained_output.strings(), fresh_output.strings());
    }

    #[test]
    fn invalidation_plan_keys_coalesce_structurally_equal_manifest_values() {
        let source = snapshot(
            &[(7, "/p/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            7,
        );
        let mut builder = CompilerSession::new();
        builder.update(&source).into_result().unwrap();
        let base = builder
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();
        let manifests = (0..=FRONTEND_INVALIDATION_PLAN_RETENTION_LIMIT + 3)
            .map(|_| Arc::new((*base).clone()))
            .collect::<Vec<_>>();
        let mut planner = CompilerSession::new();
        let first = planner.semantic_invalidation_plan(&manifests[0], &manifests[1]);
        let mut last = first.clone();
        for pair in manifests.windows(2).skip(1) {
            last = planner.semantic_invalidation_plan(&pair[0], &pair[1]);
        }

        assert_eq!(planner.work().retention.invalidation_plans, 1);
        assert_eq!(planner.work().retention.dependency_manifests, 2);
        let executions = planner.work().invalidation_plans.executions;
        let recomputed = planner.semantic_invalidation_plan(&manifests[0], &manifests[1]);
        assert!(Arc::ptr_eq(&first, &recomputed));
        assert_eq!(planner.work().invalidation_plans.executions, executions);
        let reused = planner
            .semantic_invalidation_plan(&manifests[manifests.len() - 2], manifests.last().unwrap());
        assert!(Arc::ptr_eq(&last, &reused));
        assert_eq!(planner.work().retention.invalidation_plans, 1);
        assert_eq!(planner.work().retention.dependency_manifests, 2);
    }

    #[test]
    fn durable_ordinary_bodies_reuse_in_the_canonical_worklist_and_match_fresh_output() {
        let source = snapshot(
            &[(
                41,
                "/relocated/main.rue",
                "main.rue",
                "fn helper(x: i32) -> i32 { let message: str = \"same\"; @dbg(message); x + 1 }\n\
                 fn main() -> i32 { let message: str = \"same\"; @dbg(message); helper(41) }",
            )],
            41,
        );
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();

        let optimized_options = CompileOptions {
            opt_level: OptLevel::O1,
            ..CompileOptions::default()
        };
        let reused = session.canonical_semantic(&optimized_options).unwrap();
        assert_eq!(
            reused
                .functions()
                .iter()
                .map(|function| function.local_atoms.len())
                .sum::<usize>(),
            2
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&source).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&optimized_options).unwrap();
        assert_eq!(
            format!("{:?}", reused.functions()),
            format!("{:?}", fresh.functions())
        );
        assert_eq!(reused.strings(), fresh.strings());
        assert_eq!(
            format!("{:?}", reused.warnings()),
            format!("{:?}", fresh.warnings())
        );
        assert_eq!(
            format!("{:?}", reused.ordinary_free_function_dependencies()),
            format!("{:?}", fresh.ordinary_free_function_dependencies())
        );
        assert_eq!(
            format!("{:?}", reused.analyzed_body_owners()),
            format!("{:?}", fresh.analyzed_body_owners())
        );
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
            .find(|function| function.legacy_name == "choose.vtrue")
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
    fn durable_ordinary_body_edit_reuses_unaffected_reachable_body_and_failure_keeps_baseline() {
        let original = snapshot(
            &[(
                51,
                "/p/main.rue",
                "main.rue",
                "fn a() -> i32 { 1 }\nfn b() -> i32 { 2 }\nfn main() -> i32 { a() + b() }",
            )],
            51,
        );
        let edited = snapshot(
            &[(
                51,
                "/p/main.rue",
                "main.rue",
                "fn a() -> i32 { 3 }\nfn b() -> i32 { 2 }\nfn main() -> i32 { a() + b() }",
            )],
            51,
        );
        let mut session = CompilerSession::new();
        session.update(&original).into_result().unwrap();
        session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();

        session.update(&edited).into_result().unwrap();
        let edited_output = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        assert_eq!(edited_output.functions().len(), 3);
        session
            .semantic_dependency_inputs(&CompileOptions::default(), None)
            .unwrap();

        let invalid = snapshot(
            &[(
                51,
                "/p/main.rue",
                "main.rue",
                "fn a() -> i32 { false }\nfn b() -> i32 { 2 }\nfn main() -> i32 { a() + b() }",
            )],
            51,
        );
        session.update(&invalid).into_result().unwrap();
        assert!(
            session
                .canonical_semantic(&CompileOptions::default())
                .is_err()
        );

        session.update(&edited).into_result().unwrap();
        let recovered_options = CompileOptions {
            opt_level: OptLevel::O2,
            ..CompileOptions::default()
        };
        let recovered = session.canonical_semantic(&recovered_options).unwrap();
        let mut fresh = CompilerSession::new();
        fresh.update(&edited).into_result().unwrap();
        let fresh = fresh.canonical_semantic(&recovered_options).unwrap();
        assert_eq!(
            format!("{:?}", recovered.functions()),
            format!("{:?}", fresh.functions())
        );
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
    fn semantic_fault_injection_copies_the_stale_results_exact_rir_owner() {
        let first = SourceSnapshot::single("main.rue", "fn main() -> i32 { 0 }").unwrap();
        let second = SourceSnapshot::single("main.rue", "fn main() -> i32 { 1 }").unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.semantic(&options).unwrap();
        session.update(&second).into_result().unwrap();
        session.semantic(&options).unwrap();

        let records = session
            .queries
            .semantic
            .records()
            .cloned()
            .collect::<Vec<_>>();
        let current = records.last().unwrap();
        let stale = records
            .iter()
            .rev()
            .skip(1)
            .find(|record| record.result.is_ok() && record.key != current.key)
            .unwrap();
        let stale_rir = stale.rir.as_ref().unwrap().clone();
        let stale_semantic = stale.result.as_ref().unwrap().clone();

        assert!(
            session
                .inject_stale_query_for_oracle(crate::unstable::DifferentialOracleFault::Semantic)
        );
        let injected = session.queries.semantic.records().last().unwrap();
        assert!(Arc::ptr_eq(
            injected.result.as_ref().unwrap(),
            &stale_semantic
        ));
        assert!(Arc::ptr_eq(injected.rir.as_ref().unwrap(), &stale_rir));

        let view = session.semantic(&options).unwrap();
        let view = Arc::try_unwrap(view).unwrap();
        let (semantic_owner, rir_owner) = view.into_owners();
        assert!(Arc::ptr_eq(&semantic_owner, &stale_semantic));
        assert!(Arc::ptr_eq(&rir_owner, &stale_rir));
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
            .body_transaction(
                revision,
                key.clone(),
                Arc::new(BTreeMap::new()),
                Arc::from([]),
                false,
                cancellation.clone(),
                |_| panic!("semantic request must have materialized this body terminal"),
            )
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
            .body_transaction(
                revision,
                key.clone(),
                Arc::new(BTreeMap::new()),
                Arc::from([]),
                false,
                rue_query::CancellationToken::new(),
                |_| panic!("semantic request must have materialized this body terminal"),
            )
            .unwrap();
        let rue_query::QueryOutcome::Success(transaction) = terminal.outcome() else {
            unreachable!("BodyTransaction publishes typed values")
        };
        (terminal.stamp(), terminal.kind(), transaction.clone())
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
            .body_transaction(
                revision,
                key.clone(),
                Arc::new(BTreeMap::new()),
                Arc::from([]),
                false,
                rue_query::CancellationToken::new(),
                |_| panic!("semantic request must have materialized this body terminal"),
            )
            .unwrap()
            .dependencies()
            .iter()
            .map(|dependency| format!("{:?}", dependency.node))
            .collect()
    }

    fn projected_anonymous_nominals(
        session: &mut CompilerSession,
        options: &CompileOptions,
    ) -> Arc<[crate::durable_semantics::DurableAnonymousNominal]> {
        session.canonical_rir().unwrap();
        let merged = session
            .queries
            .rir
            .selected_record(&session.queries.graph)
            .and_then(|entry| entry.merged.clone())
            .unwrap();
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
        let make = body_query_key(&mut session, &options, "Make");
        let size = cold
            .functions()
            .iter()
            .find(|function| function.legacy_name.starts_with("size."))
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
    fn nested_type_constructor_producers_publish_a_complete_body_closure() {
        let first = SourceSnapshot::single(
            "main.rue",
            r#"
                fn Option(comptime T: type) -> type { enum { Some(T), None } }
                fn Result(comptime T: type, comptime E: type) -> type {
                    enum { Ok(T), Err(E) }
                }
                fn make() -> Result(Option(i32), i32) {
                    let O = Option(i32);
                    let R = Result(Option(i32), i32);
                    R.Ok(O.Some(42))
                }
                fn main() -> i32 {
                    let O = Option(i32);
                    let R = Result(Option(i32), i32);
                    match make() {
                        R.Ok(o) => match o { O.Some(v) => v, O.None => 0 },
                        R.Err(e) => 0 - e
                    }
                }
            "#,
        )
        .unwrap();
        let second = SourceSnapshot::single(
            "main.rue",
            r#"
                fn Option(comptime T: type) -> type { enum { None, Some(T) } }
                fn Result(comptime T: type, comptime E: type) -> type {
                    enum { Ok(T), Err(E) }
                }
                fn make() -> Result(Option(i32), i32) {
                    let O = Option(i32);
                    let R = Result(Option(i32), i32);
                    R.Ok(O.Some(42))
                }
                fn main() -> i32 {
                    let O = Option(i32);
                    let R = Result(Option(i32), i32);
                    match make() {
                        R.Ok(o) => match o { O.Some(v) => v, O.None => 0 },
                        R.Err(e) => 0 - e
                    }
                }
            "#,
        )
        .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let first_projection = projected_anonymous_nominals(&mut session, &options);
        let (first_option, first_result) = nested_option_result_facts(&first_projection);
        assert_nested_result_owns_option_argument(first_option, first_result, &first_projection);

        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        let warm_projection = projected_anonymous_nominals(&mut session, &options);
        let (warm_option, warm_result) = nested_option_result_facts(&warm_projection);
        assert_nested_result_owns_option_argument(warm_option, warm_result, &warm_projection);
        assert_ne!(
            first_option.shape, warm_option.shape,
            "changing only the type constructor's variant order must invalidate its fact"
        );

        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let expected = fresh.canonical_semantic(&options).unwrap();
        let fresh_projection = projected_anonymous_nominals(&mut fresh, &options);
        assert_eq!(warm_projection, fresh_projection);
        assert_semantic_artifact_parity(&session, &warm, &expected);
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
                crate::body_query::BodyReference::Type(ty) => {
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
                    crate::body_query::BodyReference::Type(_) => false,
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
    fn unreachable_body_is_not_requested_by_production_reachability() {
        let source =
            SourceSnapshot::single("main.rue", "fn dead() -> i32 { 1 } fn main() -> i32 { 42 }")
                .unwrap();
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&source).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        let dead = body_query_key(&mut session, &options, "dead");
        let revision = session
            .queries
            .revisioned
            .current_semantic_revision()
            .unwrap();
        let compute_called = std::cell::Cell::new(false);
        let result = session.queries.revisioned.body_transaction(
            revision,
            dead,
            Arc::new(BTreeMap::new()),
            Arc::from([]),
            false,
            rue_query::CancellationToken::new(),
            |_| {
                compute_called.set(true);
                Err(rue_query::QueryAbort::Canceled)
            },
        );
        assert!(matches!(
            result.unwrap_err(),
            crate::revisioned_query_database::BodyTransactionRequestFailure::Query(
                rue_query::QueryAbort::Canceled
            )
        ));
        assert!(
            compute_called.get(),
            "an existing terminal would bypass the supplied computation"
        );
    }
}
