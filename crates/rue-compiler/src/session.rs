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
    codegen_executions: Vec<(crate::FunctionInstanceKey, rue_query::RequestExecution)>,
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
    /// Explicit durable baseline for the next semantic attempt (RUE-1143).
    ///
    /// `None` on every ordinary compile, in which case a semantic attempt reuses
    /// from the last-good semantic record. No production path sets it.
    ///
    /// It is deliberately NOT `#[cfg(test)]`. The reuse-baseline selection this
    /// feeds is the most correctness-sensitive lookup in the compiler, and it
    /// previously existed twice: production read the last-good record, while a
    /// `cfg(test)` shadow field took precedence under test. Tests therefore
    /// validated a selection order production never executed — underneath the
    /// differential oracle, which is the defense for exactly that class of bug.
    /// Keeping the slot unconditional means both configurations compile one
    /// selection expression; only whether anything ever fills it differs.
    durable_baseline_override: Option<DurableBaselineOverride>,
}

/// An explicit durable baseline supplied in place of the last-good record.
///
/// Tests use this to drive a specific — including deliberately stale, corrupt,
/// or partially-populated — durable cache through the production reuse path,
/// rather than through a parallel branch compiled only under `cfg(test)`.
#[derive(Debug, Default, Clone)]
struct DurableBaselineOverride {
    durable_declaration_cache: Option<DurableDeclarationCache>,
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

    /// Select `source` as the current exact source WITHOUT disappearing the
    /// predecessor leaf (RUE-1112). A strictly-additive successor adoption
    /// leaves the predecessor's immutable leaf live, so every retained
    /// terminal that correctly depends on it stays valid — nothing is
    /// invalidated or re-walked; new publications simply observe the new
    /// leaf. Ordinary updates keep the disappearing [`Self::publish_source`],
    /// whose invalidation is the real contract for replaced sources.
    fn publish_source_additive(&mut self, source: ExactSourceInput) {
        self.source_inputs.publish_retained(&mut self.graph, source);
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ImportPlanQueryKey {
    /// Ordinary content-addressed staging: keyed on the exact inputs, so an
    /// unchanged program recompiled in a fresh session hits the same memoized
    /// terminal it does today.
    Ordinary(Box<OrdinaryImportPlanKey>),
    /// Trusted-toolchain successor staging (RUE-1112): keyed on the published
    /// lineage identity being staged plus the exact successor delta. The
    /// published revision identity is session-unique and immutably bound to its
    /// leaf view, so it stands in for the full predecessor content without
    /// hashing or comparing predecessor entries; the delta names the appended
    /// module revisions exactly. Identities are never fingerprints (ADR-0066):
    /// a revision id is a published immutable identity, not a content digest.
    Successor {
        revision: crate::ImportInputRevision,
        delta: Arc<[crate::ModuleRevision]>,
        policy_version: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OrdinaryImportPlanKey {
    source: ExactSourceInput,
    context: crate::ImportDiscoveryContext,
    policy_version: u32,
    accepted_reads: crate::AcceptedReadManifest,
    carried_ledger: crate::ImportObservationLedger,
}

impl ImportPlanQueryKey {
    /// The exact source revision an Ordinary key pins; a Successor key pins its
    /// sources through the published revision identity instead.
    fn pinned_source_revision(&self) -> Option<&crate::SourceRevision> {
        match self {
            Self::Ordinary(key) => Some(&key.source.revision),
            Self::Successor { .. } => None,
        }
    }
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
        match record.key.pinned_source_revision() {
            Some(revision) => record.diagnostics.source_revision() == revision,
            None => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ImportClosureQueryKey {
    /// Ordinary content-addressed closure (unchanged warm-reuse semantics).
    Ordinary(Box<OrdinaryImportClosureKey>),
    /// Trusted-toolchain successor closure (RUE-1112): keyed on the published
    /// lineage identity being closed plus the exact successor delta (see the
    /// plan key's Successor variant for the identity discipline).
    Successor {
        revision: crate::ImportInputRevision,
        delta: Arc<[crate::ModuleRevision]>,
        policy_version: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OrdinaryImportClosureKey {
    source: ExactSourceInput,
    context: crate::ImportDiscoveryContext,
    policy_version: u32,
    accepted_reads: crate::AcceptedReadManifest,
    plan: crate::ImportDiscoveryPlan,
    ledger: crate::ImportObservationLedger,
}

impl ImportClosureQueryKey {
    fn pinned_source_revision(&self) -> Option<&crate::SourceRevision> {
        match self {
            Self::Ordinary(key) => Some(&key.source.revision),
            Self::Successor { .. } => None,
        }
    }
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
        let mutual =
            record.artifact.snapshot().source_revision() == record.diagnostics.source_revision();
        match record.key.pinned_source_revision() {
            Some(revision) => {
                record.artifact.snapshot().source_revision() == revision
                    && record.diagnostics.source_revision() == revision
                    && match &record.result {
                        Ok(graph) => &graph.input().sources == revision,
                        Err(_) => true,
                    }
            }
            None => {
                mutual
                    && match &record.result {
                        Ok(graph) => {
                            &graph.input().sources == record.artifact.snapshot().source_revision()
                        }
                        Err(_) => true,
                    }
            }
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
    #[cfg(test)]
    pub(crate) fn with_query_concurrency(workers: usize) -> Self {
        let mut session = Self::default();
        session.queries.revisioned =
            crate::revisioned_query_database::RevisionedQueryDatabase::with_query_concurrency(
                workers,
            );
        session
    }
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

    pub(crate) fn published_owner(&self) -> Option<&Arc<ParsedProgram>> {
        self.published.as_ref()
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
        self.queries.revisioned.parse.selected_terminal()
    }

    /// Cumulative dependency-graph invalidation events across the retained
    /// frontend query families (merge, import diagnostics, RIR, semantic,
    /// definitions, dependency manifests). A strictly-additive successor
    /// adoption keeps the predecessor's immutable source leaf live, so it
    /// contributes ZERO here regardless of how many variants are retained;
    /// only a genuine replacement (an ordinary update) invalidates dependents.
    pub(crate) fn frontend_query_invalidations(&self) -> u64 {
        let graph = &self.queries.graph;
        (graph.invalidation_count::<MergeQuery>()
            + graph.invalidation_count::<ImportDiagnosticQuery>()
            + graph.invalidation_count::<RirQuery>()
            + graph.invalidation_count::<SemanticQuery>()
            + graph.invalidation_count::<DefinitionQuery>()
            + graph.invalidation_count::<DependencyManifestQuery>()) as u64
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
                accepted_reads: crate::AcceptedReadManifest::from_entries(Vec::new()),
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
        // From this point the selected typed plan terminal owns any closed
        // failure. Reinstall protocol context only if staging reaches Open.
        self.open_discovery = None;
        let source_revision = snapshot.source_revision().clone();
        // A successor stage keys on {published lineage identity, exact delta}
        // rather than re-hashing the full content; the ordinary path keeps its
        // exact content key and therefore its warm reuse.
        let plan_key = match &successor {
            Some((revision, delta)) => ImportPlanQueryKey::Successor {
                revision: *revision,
                delta: delta.clone(),
                policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            },
            None => ImportPlanQueryKey::Ordinary(Box::new(OrdinaryImportPlanKey {
                source: ExactSourceInput::new(snapshot),
                context: context.clone(),
                policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
                accepted_reads: accepted_reads.clone(),
                carried_ledger: carried_ledger.clone(),
            })),
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
                successor_parse: None,
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
        drop(plan_build_span);
        let _plan_publish_span = tracing::info_span!("import_plan_publish").entered();
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
            successor_parse: staged_successor_parse,
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
        self.close_import_discovery_artifact(ledger, None)
            .map(|artifact| Arc::new(crate::ImportDiscoveryView::new(artifact)))
    }

    #[cfg(test)]
    pub(crate) fn close_import_discovery(
        &mut self,
        ledger: crate::ImportObservationLedger,
    ) -> Result<Arc<ImportDiscoveryRevisionArtifact>, CompileErrors> {
        self.close_import_discovery_artifact(ledger, None)
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
            // RUE-1033 LEGACY EMBEDDER GATE: callers that bypass the canonical
            // begin/frontier/publish protocol retain their historical plan
            // groups until that entire public boundary is removed together.
            None => plan.groups().to_vec(),
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
        let (closure_key, closure_dependencies, publish_closure) =
            self.select_import_closure_query(&open, &plan, &ledger, successor.as_ref());
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
        successor: Option<&(crate::ImportInputRevision, Arc<[crate::ModuleRevision]>)>,
        status: ImportDiscoveryRevisionStatus,
        graph: Option<Arc<CanonicalImportGraphOutput>>,
        errors: &CompileErrors,
    ) -> Arc<ImportDiscoveryRevisionArtifact> {
        debug_assert_ne!(status, ImportDiscoveryRevisionStatus::ClosedValid);
        let (closure_key, closure_dependencies, publish_closure) =
            self.select_import_closure_query(&open, &plan, &ledger, successor);
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

    /// Durable declaration cache retained by the last successful semantic
    /// record.
    #[cfg(test)]
    fn last_good_durable_declaration_cache(&self) -> Option<&DurableDeclarationCache> {
        self.queries
            .semantic
            .last_good_record()
            .and_then(|entry| entry.durable_declaration_cache.as_ref())
    }

    /// Supply an explicit durable baseline for the next semantic attempt,
    /// replacing the last-good record as the reuse source.
    ///
    /// No production path calls this. It is the injection seam that lets a test
    /// drive a chosen — including deliberately stale or corrupt — durable cache
    /// through the production reuse path, instead of a `cfg(test)` branch that
    /// production never compiles (RUE-1143).
    #[cfg(test)]
    fn set_durable_baseline_override(&mut self, baseline: Option<DurableBaselineOverride>) {
        self.durable_baseline_override = baseline;
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
                provenance: presentation.clone(),
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
        let Some(terminal) = self.queries.revisioned.parse.last_good_terminal() else {
            return Err(reject("no predecessor parse artifact is retained"));
        };
        let Ok(predecessor_terminal) = self
            .queries
            .revisioned
            .parse
            .family_handle()
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
        let parse_family = self.queries.revisioned.parse.family_handle();
        let prepared = self.queries.revisioned.parse.prepare(key.clone());
        let attempt = prepared.execute(runtime_revision, attempt_id, |context| {
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
        // A successor record's classification is relative to the retained
        // predecessor its key pins, so the reused branch reuses it verbatim.
        let invalidation = record.invalidation.clone();
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
        successor: Option<&(crate::ImportInputRevision, Arc<[crate::ModuleRevision]>)>,
    ) -> (
        ImportClosureQueryKey,
        Vec<crate::query_graph::ObservedDependency>,
        bool,
    ) {
        // Reconstruct the exact key the stage published its plan terminal under:
        // the successor identity key for a successor close, the content key
        // otherwise.
        let plan_key = match successor {
            Some((revision, delta)) => ImportPlanQueryKey::Successor {
                revision: *revision,
                delta: delta.clone(),
                policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            },
            None => ImportPlanQueryKey::Ordinary(Box::new(OrdinaryImportPlanKey {
                source: ExactSourceInput::new(&open.snapshot),
                context: open.context.clone(),
                policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
                accepted_reads: open.accepted_reads.clone(),
                carried_ledger: open.ledger.clone(),
            })),
        };
        let plan_dependency = self
            .queries
            .import_plans
            .handle(&plan_key)
            .expect("an open discovery revision retains its typed plan terminal")
            .observed();
        let key = match successor {
            Some((revision, delta)) => ImportClosureQueryKey::Successor {
                revision: *revision,
                delta: delta.clone(),
                policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
            },
            None => ImportClosureQueryKey::Ordinary(Box::new(OrdinaryImportClosureKey {
                source: ExactSourceInput::new(&open.snapshot),
                context: open.context.clone(),
                policy_version: crate::IMPORT_DISCOVERY_POLICY_VERSION,
                accepted_reads: open.accepted_reads.clone(),
                plan: plan.clone(),
                ledger: ledger.clone(),
            })),
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
        let prepared = self.queries.revisioned.parse.prepare(key);
        let attempt = prepared.execute(runtime_revision, attempt_id, |context| {
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
        let view = self.queries.revisioned.parse.attempt_view(
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
                self.queries
                    .publish_source_additive(ExactSourceInput::new(snapshot));
                self.metrics
                    .project_dependency_invalidations(&self.queries.graph, downstream_invalidated);
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
            presentation: self
                .batch_diagnostic_order
                .as_ref()
                .map(crate::shared_segments::SharedList::as_arc),
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

    /// Collect the canonical per-function backend terminals for one semantic
    /// result. This is deliberately a deterministic adapter: `CodegenUnit`
    /// owns lowering, allocation, scheduling, emission, and requested
    /// presentation projections; callers only order and project terminals.
    pub(crate) fn codegen_products(
        &mut self,
        semantic: &crate::CanonicalSemanticOutput,
        foreign_symbols: &[String],
        options: &crate::CompileOptions,
        request: rue_codegen::BackendArtifactRequest,
    ) -> Result<Vec<crate::backend::FunctionBackendProduct>, crate::CompileErrors> {
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
        let mappings = Arc::new(crate::backend::foreign_call_symbol_mappings(
            semantic.functions(),
            foreign_symbols,
        ));
        let foreign: Arc<std::collections::BTreeSet<String>> =
            Arc::new(foreign_symbols.iter().cloned().collect());
        let strings: Arc<[String]> = semantic.strings().to_vec().into();
        let interner = self
            .selected_semantic_rir_owner()
            .expect("semantic output retains the exact RIR interner")
            .semantic_symbols()
            .shared_interner();
        let mut products = Vec::with_capacity(semantic.functions().len());
        #[cfg(test)]
        self.codegen_executions.clear();
        for function in semantic.functions() {
            let attempt = self
                .queries
                .revisioned
                .codegen_unit(
                    revision,
                    function.clone(),
                    semantic.type_pool().clone(),
                    strings.clone(),
                    interner.clone(),
                    mappings.clone(),
                    foreign.clone(),
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
            self.codegen_executions
                .push((function.semantic_identity.clone(), attempt.execution()));
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
                    products.push(unit.backend_product());
                }
                crate::codegen_query::CodegenUnitValue::Failure(errors) => {
                    return Err(errors.clone());
                }
            }
        }
        Ok(products)
    }

    #[cfg(test)]
    pub(crate) fn codegen_executions(
        &self,
    ) -> &[(crate::FunctionInstanceKey, rue_query::RequestExecution)] {
        &self.codegen_executions
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
                let rir = self
                    .selected_semantic_rir_owner()
                    .expect("successful semantic query retains its exact RIR terminal");
                SemanticParkOutcome::Ready(Arc::new(crate::SemanticView::new(owner, rir)))
            }
            Err(SemanticRequestControl::Compile(errors)) => SemanticParkOutcome::Errors(errors),
            Err(SemanticRequestControl::Parked(park)) => {
                // Atomically attach this rooted park's exact sorted missing-demand
                // set to the outstanding closed continuation, making it authorizing
                // (RUE-1112). Demand authority lives only here — bound to this
                // closed revision and this park — so a later, non-parking close can
                // never inherit it, and `publish_trusted_toolchain_successor` can
                // require the successor's added set to EQUAL exactly this set.
                if let Some(state) = self.continuation.as_mut() {
                    let mut demands = park.demands().to_vec();
                    demands.sort();
                    demands.dedup();
                    state.attached_demands = Some(Arc::from(demands));
                }
                SemanticParkOutcome::Parked(park)
            }
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
            self.reuse_diagnostics(entry.diagnostics.clone());
            if cancellation.is_canceled() {
                return Err(SemanticRequestControl::Abort(
                    rue_query::QueryAbort::Canceled,
                ));
            }
            return result.map_err(SemanticRequestControl::Compile);
        }
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
                declaration_shell_failure_diagnostics(merged.ast(), &failure),
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
                            merged.ast(),
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
                        semantic_nucleus_failure_diagnostics(merged.ast(), None, failure),
                    ),
                    crate::body_query::BodyClosureFatal::WellKnownOptionResolution {
                        instance,
                        failure,
                    } => (
                        Some(instance.clone()),
                        well_known_option_resolution_diagnostics(merged.ast(), failure),
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
                let transaction = &analysis.transaction;
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
                        let Some(source) = analysis.source_locator.as_ref() else {
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
                demanded_drop_glue,
                demanded_drop_glue_plans,
                durable_body_work,
                &self.queries.revisioned,
                runtime_revision,
                cancellation.clone(),
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
        if let Ok(output) = &result {
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
        // Same single selection order as the CFG baseline above (RUE-1143).
        let durable_declarations = self
            .durable_baseline_override
            .as_ref()
            .and_then(|override_baseline| override_baseline.durable_declaration_cache.as_ref())
            .or_else(|| {
                self.queries
                    .semantic
                    .last_good_record()
                    .and_then(|entry| entry.durable_declaration_cache.as_ref())
            })
            .map(|cache| cache.semantics.clone());
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
    if let (Some(declaration), F::DiagnosticAtProducerRange { kind, start, end }) =
        (declaration, failure)
        && let Some(producer) = program
            .modules()
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
        F::DiagnosticAtProducerRange { kind, .. } => (kind.clone(), None),
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

fn well_known_option_resolution_diagnostics(
    program: &crate::canonical_merge::CanonicalMergedAst,
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
            let mut errors = semantic_nucleus_failure_diagnostics(program, None, failure);
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
                "derive(Debug, Default)]\npub struct ImportObservationLedger",
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
        assert_eq!(
            reused.work().declaration_reuse.declaration_prefixes_built,
            1
        );
        assert_eq!(reused.work().declaration_reuse.declaration_indexes_built, 1);
        assert_eq!(
            reused
                .work()
                .declaration_reuse
                .declaration_prefix_population_runs,
            1
        );
        assert_eq!(
            reused.work().declaration_reuse.declaration_prefix_fallbacks,
            0
        );
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
        let warm_body_work = reused.work().body_analysis;
        let fresh_body_work = ordinary.work().body_analysis;
        assert_eq!(warm_body_work.body_analyses_computed, 1);
        assert_eq!(warm_body_work.body_analyses_reused, 0);
        assert_eq!(warm_body_work.body_analyses_invalidated, 1);
        assert_eq!(fresh_body_work.body_analyses_computed, 1);
        assert_eq!(fresh_body_work.body_analyses_reused, 0);
        assert_eq!(fresh_body_work.body_analyses_invalidated, 0);
        assert_eq!(
            warm_body_work.body_analyses_computed, fresh_body_work.body_analyses_computed,
            "warm and fresh must compute the same number of bodies"
        );
        assert_eq!(
            warm_body_work.body_analyses_reused, fresh_body_work.body_analyses_reused,
            "warm and fresh must report the same reuse count"
        );
        assert_eq!(
            warm_body_work.body_analyses_invalidated,
            fresh_body_work.body_analyses_invalidated + 1,
            "warm has one intentional invalidation delta for its retained predecessor"
        );
        assert_eq!(
            warm_body_work.bodies_attempted, fresh_body_work.bodies_attempted,
            "warm and fresh must attempt the same bodies"
        );
        assert_eq!(
            warm_body_work.bodies_succeeded, fresh_body_work.bodies_succeeded,
            "warm and fresh must succeed for the same bodies"
        );
        assert_eq!(
            warm_body_work.air_instructions_produced, fresh_body_work.air_instructions_produced,
            "warm and fresh must produce the same AIR instruction count"
        );
        assert_eq!(
            warm_body_work.local_strings_produced, fresh_body_work.local_strings_produced,
            "warm and fresh must produce the same local string count"
        );
        assert_eq!(
            warm_body_work.ordinary_body_exports_attempted,
            fresh_body_work.ordinary_body_exports_attempted,
            "warm and fresh must attempt the same ordinary exports"
        );
        assert_eq!(
            warm_body_work.ordinary_body_exports_succeeded,
            fresh_body_work.ordinary_body_exports_succeeded,
            "warm and fresh must succeed for the same ordinary exports"
        );
        assert_eq!(
            warm_body_work.specialized_bodies_attempted,
            fresh_body_work.specialized_bodies_attempted,
            "warm and fresh must attempt the same specialized bodies"
        );
        assert_eq!(
            warm_body_work.specialized_bodies_succeeded,
            fresh_body_work.specialized_bodies_succeeded,
            "warm and fresh must succeed for the same specialized bodies"
        );
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
            .last_good_durable_declaration_cache()
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
        // Perturb a copy of the produced baseline and inject it, so the next
        // attempt resolves it through the same selection order production uses
        // (RUE-1143).
        let mut cache = session
            .last_good_durable_declaration_cache()
            .unwrap()
            .clone();
        let mut records = cache.semantics.to_vec();
        let module = records
            .iter_mut()
            .find(|record| record.key.kind() == StableDefinitionKind::ModuleBinding)
            .unwrap();
        module.payload = crate::DurableDeclarationPayload::ModuleBinding {
            target: crate::ModuleId::from_logical_path("missing.rue").unwrap(),
        };
        cache.semantics = records.into();
        session.set_durable_baseline_override(Some(DurableBaselineOverride {
            durable_declaration_cache: Some(cache),
        }));

        publish_with_test_imports(&mut session, &edited);
        let fallback = session.canonical_semantic(&options).unwrap();
        assert_eq!(
            fallback.work().binding.declaration_resolution_invocations,
            0
        );
        assert_eq!(fallback.work().binding.durable_install_invocations, 1);
        assert!(fallback.work().binding.durable_payloads_installed > 0);
        assert_eq!(fallback.work().declaration_reuse.fallbacks, 0);
        assert_eq!(
            fallback
                .work()
                .declaration_reuse
                .declaration_prefix_fallbacks,
            0
        );

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
    fn cfg_import_failure_rebuilds_current_body_with_exact_fallback_work() {
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
        let warm = crate::canonical_semantic::with_test_cfg_import_failure_injection(|| {
            session.canonical_semantic(&options).unwrap()
        });
        assert_eq!(warm.work().cfg.cfg_reuse_candidates, 1);
        assert_eq!(warm.work().cfg.cfg_reuses, 0);
        assert_eq!(warm.work().cfg.cfg_import_attempts, 1);
        assert_eq!(warm.work().cfg.cfg_import_successes, 0);
        assert_eq!(warm.work().cfg.cfg_import_failures, 1);
        assert_eq!(warm.work().cfg.cfg_fallbacks, 1);
        assert_eq!(warm.work().cfg.cfg_builds_attempted, 1);
        assert_eq!(warm.work().cfg.cfg_builds_succeeded, 1);
        assert_eq!(warm.work().cfg.cfg_builds_failed, 0);
        assert_eq!(warm.work().cfg.optimization_attempts, 1);
        assert_eq!(warm.work().cfg.optimization_completions, 1);
        assert_eq!(warm.work().cfg.optimized_level_attempts, 1);

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
        let warm_rir = session.canonical_rir().unwrap();
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
        let fresh_rir = fresh.canonical_rir().unwrap();
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
        let warm_rir = session.canonical_rir().unwrap();
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
        let fresh_rir = fresh.canonical_rir().unwrap();
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
        assert_eq!(
            reused.work().declaration_reuse.declaration_prefix_fallbacks,
            0
        );

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
        assert_eq!(
            shifted_stamp, first_stamp,
            "a locator-only edit must reuse the semantic body transaction",
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
    fn anonymous_member_diagnostic_uses_its_exact_producer_location() {
        let text = "fn Box(comptime T: type) -> type { struct { value: T, fn bad(self) -> i32 { missing_name } } } fn main() -> i32 { let B = Box(i32); let value = B { value: 1 }; value.bad() }";
        let shifted_text = format!("// leading trivia relocates the producer\n\n{text}");
        let valid_text = text.replace("missing_name", "0");
        let source = snapshot(&[(1, "/p/main.rue", "main.rue", text)], 1);
        let shifted = snapshot(&[(1, "/p/main.rue", "main.rue", shifted_text.as_str())], 1);
        let valid = snapshot(&[(1, "/p/main.rue", "main.rue", valid_text.as_str())], 1);
        let mut session = CompilerSession::new();
        session.update(&valid).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap();
        session.update(&source).into_result().unwrap();
        session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        session.update(&shifted).into_result().unwrap();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        let body_work = session
            .work()
            .semantic_records
            .last()
            .unwrap()
            .work
            .body_analysis;
        assert_eq!(
            body_work.body_analyses_computed, 0,
            "leading trivia must not recompute the anonymous member transaction",
        );
        assert!(
            body_work.body_analyses_reused >= 2,
            "the producer and anonymous member transactions are both reused",
        );
        let error = errors
            .iter()
            .find(|error| {
                error.span().is_some_and(|span| {
                    span.start as usize == shifted_text.find("missing_name").unwrap()
                })
            })
            .unwrap_or_else(|| panic!("no diagnostic at the anonymous member: {errors:?}"));
        let span = error.span().unwrap();
        assert_eq!(span.file_id, crate::FileId::new(1));
        assert_eq!(
            &shifted_text[span.start as usize..span.end as usize],
            "missing_name"
        );
    }

    #[test]
    fn const_produced_anonymous_member_is_scheduled_and_relocates_diagnostics() {
        let invalid_text = "const B: type = struct { value: i32, fn bad(self) -> i32 { missing_name } }; fn main() -> i32 { let value = B { value: 1 }; value.bad() }";
        let valid_text = invalid_text.replace("missing_name", "0");
        let shifted_text = format!("// relocate the const producer\n\n{invalid_text}");
        let valid = snapshot(&[(1, "/p/main.rue", "main.rue", valid_text.as_str())], 1);
        let invalid = snapshot(&[(1, "/p/main.rue", "main.rue", invalid_text)], 1);
        let shifted = snapshot(&[(1, "/p/main.rue", "main.rue", shifted_text.as_str())], 1);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();

        session.update(&valid).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();
        session.update(&invalid).into_result().unwrap();
        session.canonical_semantic(&options).unwrap_err();
        session.update(&shifted).into_result().unwrap();
        let errors = session.canonical_semantic(&options).unwrap_err();
        let span = errors
            .iter()
            .filter_map(|error| error.span())
            .find(|span| span.start as usize == shifted_text.find("missing_name").unwrap())
            .unwrap_or_else(|| panic!("no diagnostic at const-produced member: {errors:?}"));
        assert_eq!(span.file_id, crate::FileId::new(1));
        assert_eq!(
            &shifted_text[span.start as usize..span.end as usize],
            "missing_name",
        );
        let body_work = session
            .work()
            .semantic_records
            .last()
            .unwrap()
            .work
            .body_analysis;
        assert_eq!(body_work.body_analyses_computed, 0);
        assert!(body_work.body_analyses_reused >= 2);
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
        let _injection = session
            .queries
            .revisioned
            .inject_body_transaction_failure_for_test();
        let errors = session
            .canonical_semantic(&CompileOptions::default())
            .unwrap_err();
        let rendered = errors.to_string();
        assert!(
            rendered.contains("injected_body_transaction_failure"),
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
        // Relocation moves only the canonical read provenance behind an
        // unchanged requested path. Module identity, source text, and therefore
        // the published revision are all untouched, so the manifest is reused
        // rather than recomputed — the edge is stable without recomputing it.
        assert_eq!(session.work().dependency_manifests.executions, 1);
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
    fn implicit_drop_edges_distinguish_body_obligations_from_synthesized_glue() {
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
    fn new_specialization_drop_edge_matches_fresh_and_invalidates_on_destructor_edit() {
        let program = |main_body: &str, destructor: Option<i32>| {
            let destructor = destructor
                .map(|value| format!("drop fn Leaf(self) {{ @dbg({value}); }}\n"))
                .unwrap_or_default();
            format!(
                "struct Leaf {{ n: i32 }}\n\
                 {destructor}\
                 fn consume(comptime T: type, value: T) -> i32 {{ 0 }}\n\
                 fn main() -> i32 {{ {main_body} }}"
            )
        };
        let first_text = program("consume(i32, 1)", Some(1));
        let second_text = program("consume(Leaf, Leaf { n: 1 })", Some(1));
        let third_text = program("consume(Leaf, Leaf { n: 1 })", None);
        let first = snapshot(&[(1, "/p/main.rue", "main.rue", &first_text)], 1);
        let second = snapshot(&[(1, "/p/main.rue", "main.rue", &second_text)], 1);
        let third = snapshot(&[(1, "/p/main.rue", "main.rue", &third_text)], 1);
        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        session.update(&first).into_result().unwrap();
        session.canonical_semantic(&options).unwrap();

        session.update(&second).into_result().unwrap();
        let warm = session.canonical_semantic(&options).unwrap();
        let specialization_edges = warm
            .implicit_named_destructor_dependencies()
            .iter()
            .filter(|edge| {
                matches!(
                    edge.source,
                    rue_air::ImplicitDropDependencySourceEvent::Specialization { .. }
                ) && edge.target_owner_name == "Leaf"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            specialization_edges.len(),
            1,
            "the newly demanded specialization must publish its exact Leaf drop edge: {:?}",
            warm.implicit_named_destructor_dependencies()
        );

        let mut fresh = CompilerSession::new();
        fresh.update(&second).into_result().unwrap();
        let fresh_second = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            format!("{:?}", warm.implicit_named_destructor_dependencies()),
            format!(
                "{:?}",
                fresh_second.implicit_named_destructor_dependencies()
            )
        );

        session.update(&third).into_result().unwrap();
        let changed = session.canonical_semantic(&options).unwrap();
        assert!(
            changed.work().body_analysis.specialized_bodies_attempted > 0,
            "editing Leaf's destructor must invalidate consume(Leaf): {:?}",
            changed.work().body_analysis
        );
        let mut fresh = CompilerSession::new();
        fresh.update(&third).into_result().unwrap();
        let fresh_third = fresh.canonical_semantic(&options).unwrap();
        assert_eq!(
            normalize_session_local_spurs(format!("{:?}", changed.functions())),
            normalize_session_local_spurs(format!("{:?}", fresh_third.functions()))
        );
        assert_eq!(
            format!("{:?}", changed.implicit_named_destructor_dependencies()),
            format!("{:?}", fresh_third.implicit_named_destructor_dependencies())
        );
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
        // Candidate precedence retargets the import, not a substituted graph:
        // the importer-relative candidate outranks the project-root one, so
        // adding a sibling `choice.rue` next to the importer moves
        // `@import("choice.rue")` off the root-level module without touching a
        // single spelling in the importer.
        let main = r#"const selected = @import("choice.rue");
fn main() -> i32 { selected.value() }"#;
        let root_only = snapshot(
            &[
                (1, "/p/app/main.rue", "app/main.rue", main),
                (
                    2,
                    "/p/choice.rue",
                    "choice.rue",
                    "pub fn value() -> i32 { 1 }",
                ),
            ],
            1,
        );
        let with_sibling = snapshot(
            &[
                (1, "/p/app/main.rue", "app/main.rue", main),
                (
                    2,
                    "/p/choice.rue",
                    "choice.rue",
                    "pub fn value() -> i32 { 1 }",
                ),
                (
                    3,
                    "/p/app/choice.rue",
                    "app/choice.rue",
                    "pub fn value() -> i32 { 2 }",
                ),
            ],
            1,
        );
        let resolved_target = |graph: &CanonicalImportGraph| match graph.records()[0].resolution() {
            CanonicalImportResolution::Resolved(module) => module.as_str().to_owned(),
            other => panic!("expected a resolved import, got {other:?}"),
        };

        let options = CompileOptions::default();
        let mut session = CompilerSession::new();
        let graph_a = crate::test_support::TestDiscoveryHost::new(&root_only)
            .unwrap()
            .drive(&mut session)
            .unwrap()
            .graph;
        assert_eq!(resolved_target(&graph_a), "choice.rue");
        let output_a = session.canonical_semantic(&options).unwrap();
        let diagnostics_a = session.latest_diagnostics().unwrap().clone();
        let main_key = body_query_key(&mut session, &options, "main");
        let main_a = retained_body_transaction(&session, &main_key).0;

        let graph_b = crate::test_support::TestDiscoveryHost::new(&with_sibling)
            .unwrap()
            .drive(&mut session)
            .unwrap()
            .graph;
        assert_eq!(resolved_target(&graph_b), "app/choice.rue");
        let output_b = session.canonical_semantic(&options).unwrap();
        let diagnostics_b = session.latest_diagnostics().unwrap().clone();
        let main_b = retained_body_transaction(&session, &main_key).0;

        let mut fresh = CompilerSession::new();
        publish_with_test_imports(&mut fresh, &with_sibling);
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
