//! Phase 1 compatibility layer over the canonical revisioned query runtime.
//!
//! This module deliberately preserves the existing compiler family's typed
//! record shape while moving key identity, execution, immutable attempts,
//! dependency recording, and current/last-good publication into `rue-query`.
//! It is a migration boundary, not a peer database. RUE-1033 / ADR-0063 Phase
//! 12 deletes this selected-state-shaped shim after every family calls the
//! runtime directly.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use rue_query::{
    CancellationToken, InputIdentity, QueryAbort, QueryContext, QueryFamily, QueryKey, QueryOutput,
    QueryRequestAttempt, QueryRuntime, QuerySelection, QueryTerminalKind, RequestExecution,
    Revision,
};

type SemanticNucleusFamily = QueryFamily<
    crate::semantic_query_nucleus::SemanticNucleusKey,
    crate::semantic_query_nucleus::SemanticNucleusValue,
>;

use crate::{
    AcceptedReadManifestEntry, CompileError, CompileResult, DefinitionKind, DefinitionNamespace,
    ErrorKind, ImportDemandFrontier, ImportDemandMode, ImportDemandRoots, ImportDiscoveryContext,
    ImportDiscoveryPlan, ImportDiscoveryRequest, ImportInputRevision, ImportObservation,
    ImportObservationLedger, ModuleId, ModuleRevision, SourceSnapshot, Span, StableDefinitionKey,
    SyntaxWork,
};

use crate::canonical_lower::{ModuleRirOutput, lower_module_rir_with_work};
use crate::parsed_modules::{ParsedModule, ParsedModulesWork, ParsedProgram};

use crate::session::{AttemptId, QueryStructuralWork};
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as CompilerAttemptExecution, AttemptOutcomeKind,
    AttemptView, RuntimeObservation,
};
use crate::typed_query_store::{TerminalKind, TypedQueryFamily};

const IMPORT_INPUT_REVISION_RETENTION: usize = 64;
const MODULE_QUERY_MEMO_RETENTION: usize = 4096;
// Body-keyed families must retain the entire reached-body universe of one
// cold compile. Terminal eviction below that size is not merely a cache miss:
// the body-produced-anonymous projection resolves through the retained
// BodyTransaction terminal, so evicting a still-reachable body's terminal
// makes the projection fail (surfaced as a "did not publish a terminal"
// internal diagnostic), and every coordinator restart recomputes each evicted
// body from scratch. examples/caldera reaches more than 10,000 bodies and
// exceeded the module-family cap (RUE-1083). RUE-1028's database-owned
// reachability should replace this fixed cap with exact rooted membership.
const BODY_QUERY_MEMO_RETENTION: usize = 65536;
// Declaration-keyed families scale with the program's declaration universe
// exactly as body-keyed families scale with reached bodies, and the
// body-produced-anonymous fallback resolves through declaration shells and
// semantic-nucleus terminals mid-traversal, so evicting them fails the same
// projection the body retention protects. Module-keyed families stay at the
// module-scaled retention; real programs have orders of magnitude fewer
// modules than declarations.
const DECLARATION_QUERY_MEMO_RETENTION: usize = BODY_QUERY_MEMO_RETENTION;
// A semantic batch commonly requests hundreds of exact declaration shells.
// Keep one large batch reusable after its active pins drop; the runtime still
// bounds global retention deterministically.
const MODULE_INPUT_REVISION_RETENTION: usize = 4096;

#[derive(Debug, Clone)]
pub(crate) struct CompatibilityKey<K> {
    key: K,
}

impl<K: PartialEq> PartialEq for CompatibilityKey<K> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<K: Eq> Eq for CompatibilityKey<K> {}

impl<K: std::hash::Hash> std::hash::Hash for CompatibilityKey<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Agrees with the `PartialEq` above, which compares only `key`.
        self.key.hash(state);
    }
}

impl<K> QueryKey for CompatibilityKey<K>
where
    K: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
{
    fn stable_identity(&self) -> String {
        // Display only. Exact K equality chooses the memo node and the runtime
        // incarnation makes cycle/wait identity collision-safe.
        "selected-key".to_owned()
    }
}

fn record_equal<F: TypedQueryFamily>(left: &F::Record, right: &F::Record) -> bool {
    F::terminal_kind(left) == F::terminal_kind(right)
        && F::outcome_equal(left, right)
        && F::diagnostics_equal(left, right)
}

pub(crate) struct RevisionedFamily<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    runtime: QueryRuntime,
    family: QueryFamily<CompatibilityKey<F::Key>, F::Record>,
    selection: QuerySelection<CompatibilityKey<F::Key>, F::Record>,
}

impl<F> std::fmt::Debug for RevisionedFamily<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevisionedFamily")
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

impl<F> RevisionedFamily<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    pub(crate) fn new(runtime: &QueryRuntime, name: &'static str) -> Self {
        let family = runtime
            .family_with_equality(name, F::MAX_TERMINALS, record_equal::<F>)
            .expect("compiler query families have unique stable names");
        let selection = family.selection();
        Self {
            runtime: runtime.clone(),
            family,
            selection,
        }
    }

    fn key(&mut self, key: F::Key) -> CompatibilityKey<F::Key> {
        CompatibilityKey { key }
    }

    pub(crate) fn prepare(&mut self, key: F::Key) -> PreparedRevisionedQuery<F> {
        PreparedRevisionedQuery {
            runtime: self.runtime.clone(),
            family: self.family.clone(),
            key: self.key(key),
        }
    }

    pub(crate) fn select(&mut self, attempt: &QueryRequestAttempt<F::Record>) {
        if attempt.execution() == RequestExecution::Aborted {
            self.selection.clear_current();
        }
        if let Some(terminal) = attempt.terminal() {
            self.selection
                .publish(terminal)
                .expect("selected terminal belongs to its compiler family");
            // The selection root now protects the terminal. End the attempt's
            // bridge lease at once: this attempt is about to be ledgered
            // (`attempt_view`) and kept for up to 256 completed requests, and a
            // lingering bridge pin would retain the terminal for that whole life.
            // Releasing only after `publish` established the successor keeps
            // protection continuous — the terminal is never left unpinned.
            attempt.release_result_lease();
        }
    }

    #[cfg(test)]
    pub(crate) fn request(
        &mut self,
        revision: Revision,
        key: F::Key,
        compute: impl FnOnce(&rue_query::QueryContext) -> Result<F::Record, QueryAbort>,
    ) -> Arc<QueryRequestAttempt<F::Record>> {
        let key = self.key(key);
        let attempt = Arc::new(self.runtime.request(
            &self.family,
            revision,
            key,
            CancellationToken::new(),
            |context| {
                let record = compute(context)?;
                assert!(
                    F::record_is_consistent(&record),
                    "typed query record key does not match its terminal artifact revision"
                );
                let kind = match F::terminal_kind(&record) {
                    TerminalKind::Success => QueryTerminalKind::Success,
                    TerminalKind::Failure => QueryTerminalKind::Failure,
                };
                Ok(QueryOutput::success(record).with_terminal_kind(kind))
            },
        ));
        self.select(&attempt);
        attempt
    }

    pub(crate) fn attempt_view(
        &mut self,
        id: AttemptId,
        attempt: Arc<QueryRequestAttempt<F::Record>>,
        work: QueryStructuralWork,
    ) -> Arc<dyn AttemptView> {
        let origin = AttemptId(attempt.origin_request_id());
        let runtime_observations = attempt
            .dependencies()
            .iter()
            .cloned()
            .map(RuntimeObservation::Dependency)
            .chain(
                attempt
                    .inputs()
                    .iter()
                    .cloned()
                    .map(RuntimeObservation::Input),
            )
            .collect::<Vec<_>>()
            .into();
        let runtime_work = attempt.work().to_vec().into();
        Arc::new(RuntimeAttemptView::<F> {
            id,
            origin,
            attempt,
            work,
            runtime_observations,
            runtime_work,
        })
    }

    #[cfg(test)]
    pub(crate) fn current_record(&self) -> Option<&F::Record> {
        let terminal = self.selection.current()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => Some(record),
            rue_query::QueryOutcome::Failure(_) => unreachable!("compiler families retain records"),
        }
    }

    pub(crate) fn last_good_record(&self) -> Option<&F::Record> {
        let terminal = self.selection.last_good()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => Some(record),
            rue_query::QueryOutcome::Failure(_) => unreachable!("compiler families retain records"),
        }
    }

    pub(crate) fn retention(&self) -> rue_query::FamilyRetention {
        self.family.retention()
    }

    pub(crate) fn protected_count(&self) -> usize {
        match (self.selection.current(), self.selection.last_good()) {
            (Some(current), Some(last_good)) if Arc::ptr_eq(current, last_good) => 1,
            (Some(_), Some(_)) => 2,
            (Some(_), None) | (None, Some(_)) => 1,
            (None, None) => 0,
        }
    }

    pub(crate) fn origin_attempt_ids(&self) -> impl Iterator<Item = AttemptId> + '_ {
        let mut origins = self
            .family
            .retained_origin_request_ids()
            .into_iter()
            .map(AttemptId)
            .collect::<std::collections::BTreeSet<_>>();
        origins.extend(
            [self.selection.current(), self.selection.last_good()]
                .into_iter()
                .flatten()
                .map(|terminal| AttemptId(terminal.origin_request_id())),
        );
        origins.into_iter()
    }

    pub(crate) fn retained_aborted_len(&self) -> usize {
        // Runtime aborts are owned by the diagnostic/metrics attempt index;
        // this family retains no separate aborted-attempt history.
        0
    }

    fn any_retained_key(&self, predicate: impl FnMut(&F::Key) -> bool) -> bool {
        let mut predicate = predicate;
        self.family.any_retained_key(|key| predicate(&key.key))
    }
}

pub(crate) struct PreparedRevisionedQuery<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    runtime: QueryRuntime,
    family: QueryFamily<CompatibilityKey<F::Key>, F::Record>,
    key: CompatibilityKey<F::Key>,
}

impl<F> PreparedRevisionedQuery<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    pub(crate) fn execute(
        self,
        revision: Revision,
        origin: AttemptId,
        compute: impl FnOnce(&rue_query::QueryContext) -> Result<F::Record, QueryAbort>,
    ) -> Arc<QueryRequestAttempt<F::Record>> {
        Arc::new(self.runtime.request_with_origin(
            &self.family,
            revision,
            self.key,
            CancellationToken::new(),
            Some(origin.0),
            |context| {
                let record = compute(context)?;
                assert!(F::record_is_consistent(&record));
                let kind = match F::terminal_kind(&record) {
                    TerminalKind::Success => QueryTerminalKind::Success,
                    TerminalKind::Failure => QueryTerminalKind::Failure,
                };
                Ok(QueryOutput::success(record).with_terminal_kind(kind))
            },
        ))
    }
}

#[derive(Debug)]
struct RuntimeAttemptView<F: TypedQueryFamily> {
    id: AttemptId,
    origin: AttemptId,
    attempt: Arc<QueryRequestAttempt<F::Record>>,
    work: QueryStructuralWork,
    runtime_observations: Arc<[RuntimeObservation]>,
    runtime_work: Arc<[(Arc<str>, u64)]>,
}

impl<F> AttemptView for RuntimeAttemptView<F>
where
    F: TypedQueryFamily + 'static,
    F::Record: 'static,
{
    fn id(&self) -> AttemptId {
        self.id
    }

    fn execution(&self) -> CompilerAttemptExecution {
        match self.attempt.execution() {
            RequestExecution::Computed => CompilerAttemptExecution::Computed,
            RequestExecution::Reused | RequestExecution::Joined => CompilerAttemptExecution::Reused,
            RequestExecution::Aborted => CompilerAttemptExecution::Rejected,
        }
    }

    fn outcome(&self) -> AttemptOutcomeKind {
        if let Some(terminal) = self.attempt.terminal() {
            return match terminal.kind() {
                QueryTerminalKind::Success => AttemptOutcomeKind::Success,
                QueryTerminalKind::Failure => AttemptOutcomeKind::Failure,
            };
        }
        let reason = match self.attempt.abort() {
            Some(QueryAbort::Cycle(_)) => AbortedQueryReason::DependencyCycle,
            Some(QueryAbort::Canceled) => AbortedQueryReason::Canceled,
            Some(
                QueryAbort::ForeignRuntime
                | QueryAbort::MissingInput(_)
                | QueryAbort::UnpublishedRevision(_),
            )
            | None => AbortedQueryReason::Canceled,
        };
        AttemptOutcomeKind::Aborted(reason)
    }

    fn origin_id(&self) -> AttemptId {
        self.origin
    }

    fn dependencies(&self) -> &[crate::query_graph::ObservedDependency] {
        &[]
    }

    fn runtime_observations(&self) -> &[RuntimeObservation] {
        &self.runtime_observations
    }

    fn runtime_work(&self) -> &[(Arc<str>, u64)] {
        &self.runtime_work
    }

    fn work(&self) -> &QueryStructuralWork {
        if matches!(self.attempt.execution(), RequestExecution::Computed) {
            &self.work
        } else {
            static NONE: QueryStructuralWork = QueryStructuralWork::None;
            &NONE
        }
    }

    fn diagnostics(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        let terminal = self.attempt.terminal()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => F::diagnostics(record),
            rue_query::QueryOutcome::Failure(_) => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct RevisionedQueryDatabase {
    runtime: QueryRuntime,
    next_revision: u64,
    next_source_stamp: u64,
    source_stamps: VecDeque<(super::session::ExactSourceInput, u64)>,
    import_store: Arc<Mutex<ImportInputStore>>,
    module_store: Arc<Mutex<ModuleInputStore>>,
    #[cfg(test)]
    test_import_store: Arc<Mutex<TestImportInputStore>>,
    parse_modules: QueryFamily<ModuleQueryKey, ParseModuleValue>,
    module_indexes: QueryFamily<ModuleQueryKey, ModuleIndexValue>,
    module_declaration_sets: QueryFamily<ModuleQueryKey, ModuleDeclarationSetValue>,
    declaration_occurrence_indexes: QueryFamily<ModuleQueryKey, DeclarationOccurrenceIndexValue>,
    declaration_shells: QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    #[cfg(test)]
    raw_const_syntax: QueryFamily<RawConstSyntaxQueryKey, RawConstSyntaxQueryValue>,
    #[cfg(test)]
    raw_declaration_signatures:
        QueryFamily<RawDeclarationSignatureQueryKey, RawDeclarationSignatureQueryValue>,
    raw_declaration_bodies: QueryFamily<RawDeclarationBodyQueryKey, RawDeclarationBodyQueryValue>,
    body_transactions:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::BodyTransaction>,
    canonical_bodies:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::CanonicalBody>,
    #[cfg_attr(not(test), allow(dead_code))]
    body_references:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::BodyReferences>,
    body_produced_anonymous:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::ProducedAnonymous>,
    module_rirs: QueryFamily<ModuleQueryKey, ModuleRirValue>,
    resolve_imports: QueryFamily<ResolveImportKey, ResolveImportValue>,
    #[cfg(test)]
    declaration_imports: QueryFamily<DeclarationImportQueryKey, DeclarationImportQueryValue>,
    semantic_nucleus: QueryFamily<
        crate::semantic_query_nucleus::SemanticNucleusKey,
        crate::semantic_query_nucleus::SemanticNucleusValue,
    >,
    lookup_names: QueryFamily<LookupNameKey, LookupNameValue>,
    next_import_request: u64,
    current_import_revision: Option<ImportInputRevision>,
    #[cfg(test)]
    current_test_import_revision: Option<Revision>,
    pub(crate) parse: RevisionedFamily<super::session::ParseQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ModuleQueryKey(ModuleId);

impl QueryKey for ModuleQueryKey {
    fn stable_identity(&self) -> String {
        self.0.as_str().to_owned()
    }
}

#[derive(Debug, Clone)]
struct ParseModuleValue {
    result: Result<Arc<ParsedModule>, crate::CompileErrors>,
    work: SyntaxWork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndexEntry {
    pub(crate) namespace: DefinitionNamespace,
    pub(crate) kind: DefinitionKind,
    pub(crate) visibility: Option<rue_parser::ast::Visibility>,
    pub(crate) name: Arc<str>,
    pub(crate) name_span: rue_span::Span,
    pub(crate) declaration_span: rue_span::Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndex {
    pub(crate) revision: ModuleRevision,
    pub(crate) definitions: Arc<[ModuleIndexEntry]>,
    pub(crate) imports: Arc<[crate::ImportDirective]>,
}

/// Current-file-table projection assembled exclusively from ModuleIndex and
/// LookupName terminals. The terminal values remain module-relative and
/// reusable across snapshot renumbering.
#[derive(Debug, Clone)]
pub(crate) struct ProjectedModuleIndex {
    pub(crate) revision: ModuleRevision,
    pub(crate) definitions: Arc<[ModuleIndexEntry]>,
}

#[derive(Debug, Clone)]
struct ModuleIndexValue(Result<Arc<ModuleIndex>, crate::CompileErrors>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleDeclarationSetFact {
    namespace: DefinitionNamespace,
    kind: DefinitionKind,
    visibility: Option<rue_parser::ast::Visibility>,
    name: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModuleDeclarationSetValue {
    Available {
        declarations: Arc<[ModuleDeclarationSetFact]>,
        import_specifiers: Arc<[Arc<str>]>,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeclarationOccurrenceIndex {
    capabilities: BTreeMap<
        crate::declaration_candidate::DeclarationCandidateKey,
        crate::declaration_candidate::DeclarationOccurrenceCapability,
    >,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclarationOccurrenceIndexValue {
    Available(Arc<DeclarationOccurrenceIndex>),
    Failure(crate::declaration_candidate::DeclarationOccurrenceFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DeclarationShellQueryKey(crate::declaration_candidate::DeclarationCandidateKey);

impl QueryKey for DeclarationShellQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclarationShellQueryValue {
    Available(crate::declaration_candidate::DeclarationShellFact),
    Failure(crate::declaration_candidate::DeclarationShellFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RawConstSyntaxQueryKey(crate::declaration_candidate::DeclarationCandidateKey);

impl QueryKey for RawConstSyntaxQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RawConstSyntaxQueryValue {
    Available(crate::declaration_candidate::RawConstSyntax),
    Failure(crate::declaration_candidate::RawConstSyntaxFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RawDeclarationSignatureQueryKey(crate::declaration_candidate::DeclarationCandidateKey);

impl QueryKey for RawDeclarationSignatureQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RawDeclarationSignatureQueryValue {
    Available(crate::declaration_candidate::RawDeclarationSignatureSyntax),
    Failure(crate::declaration_candidate::RawDeclarationSignatureFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RawDeclarationBodyQueryKey(crate::declaration_candidate::DeclarationCandidateKey);

impl QueryKey for RawDeclarationBodyQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RawDeclarationBodyQueryValue {
    Available(crate::declaration_candidate::RawDeclarationBodySyntax),
    Failure(crate::declaration_candidate::RawDeclarationBodyFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeclarationShellBatchFailure {
    Query(QueryAbort),
    Stable(crate::declaration_candidate::DeclarationShellFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticNucleusBatchFailure {
    Query(QueryAbort),
    Stable {
        declaration: Option<crate::declaration_candidate::DeclarationCandidateKey>,
        failure: Box<crate::semantic_query_nucleus::SemanticNucleusFailure>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticNucleusProjection {
    pub(crate) declarations: Arc<[crate::DurableDeclarationSemantic]>,
    pub(crate) anonymous_nominals: Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
    pub(crate) dependencies: Arc<[crate::semantic_query_nucleus::SemanticDeclarationDependency]>,
    pub(crate) c_export_roots: Arc<[crate::StableDefinitionKey]>,
}

#[derive(Debug, Clone)]
struct ModuleRirValue {
    result: Result<Arc<ModuleRirOutput>, crate::CompileErrors>,
    work: crate::CanonicalRirWork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleInputLeaf {
    revision: ModuleRevision,
}

#[derive(Debug)]
struct ModuleInputView {
    revision: Revision,
    snapshot: SourceSnapshot,
}

#[derive(Debug)]
struct ModuleInputStore {
    revisions: VecDeque<Arc<ModuleInputView>>,
    next_stamp: u64,
    stamps: Vec<(ModuleInputLeaf, u64)>,
}

#[cfg(test)]
#[derive(Debug)]
struct TestImportInputView {
    revision: Revision,
    graph: crate::CanonicalImportGraph,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestImportInputStore {
    revisions: VecDeque<Arc<TestImportInputView>>,
    next_stamp: u64,
    stamps: Vec<(crate::CanonicalImportGraph, u64)>,
}

impl Default for ModuleInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            next_stamp: 1,
            stamps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolveImportKey {
    occurrence: crate::ImportOccurrenceKey,
    mode: ImportDemandMode,
}

impl QueryKey for ResolveImportKey {
    fn stable_identity(&self) -> String {
        format!(
            "{}:{:?}:{}..{}:{}",
            self.occurrence.importer(),
            self.mode,
            self.occurrence.source_offset(),
            self.occurrence.source_end(),
            self.occurrence.specifier()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolveImportValue {
    site_found: bool,
    groups: Arc<[Arc<[ImportDiscoveryRequest]>]>,
    requests: Arc<[ImportDiscoveryRequest]>,
    speculative_blocked: bool,
    resolution: Option<crate::CanonicalImportResolution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DeclarationImportQueryKey(crate::declaration_candidate::DeclarationImportSiteKey);

impl QueryKey for DeclarationImportQueryKey {
    fn stable_identity(&self) -> String {
        self.0.stable_identity()
    }
}

impl QueryKey for crate::semantic_query_nucleus::SemanticNucleusKey {
    fn stable_identity(&self) -> String {
        self.stable_identity()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclarationImportQueryValue {
    Available(crate::CanonicalImportResolution),
    Failure(crate::declaration_candidate::DeclarationImportFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LookupNameKey {
    module: ModuleId,
    namespace: DefinitionNamespace,
    name: Arc<str>,
}

impl QueryKey for LookupNameKey {
    fn stable_identity(&self) -> String {
        format!("{}::{:?}::{}", self.module, self.namespace, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupNameFact {
    namespace: DefinitionNamespace,
    kind: DefinitionKind,
    visibility: Option<rue_parser::ast::Visibility>,
    name: Arc<str>,
}

/// Position-free semantic result retained by `LookupName`.
///
/// Current-epoch spans and the module revision stay in `ModuleIndex`; callers
/// that need source locations rejoin these facts with that locator projection.
/// This lets trivia-only edits preserve downstream semantic stamps without
/// ever serving stale positions to diagnostics or presentation consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LookupNameFailure {
    ModuleIndexUnavailable(ModuleId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookupNameValue(Result<Arc<[LookupNameFact]>, LookupNameFailure>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ImportHostOperationKey {
    context: ImportDiscoveryContext,
    requested_path: Arc<str>,
}

impl ImportHostOperationKey {
    fn new(request: &ImportDiscoveryRequest) -> Self {
        Self {
            context: request.context().clone(),
            requested_path: Arc::from(request.requested_path()),
        }
    }
}

#[derive(Debug)]
struct ImportInputView {
    revision: Revision,
    generation: u64,
    context: ImportDiscoveryContext,
    sources: Arc<[ModuleRevision]>,
    accepted_reads: Arc<[AcceptedReadManifestEntry]>,
    ledger: ImportObservationLedger,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AcceptedImportTopologyFact {
    importer: ModuleId,
    exact_specifier: Arc<str>,
    normalized_specifier: Arc<str>,
    outcome: AcceptedImportTopologyOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum AcceptedImportTopologyOutcome {
    Resolved(ModuleId),
    Absent,
    PresentUnreadable,
    DeniedLexical,
    DeniedCanonical,
    InvalidPhysicalType,
    UnstableRead,
    Cancelled,
}

#[derive(Debug)]
struct ImportInputStore {
    revisions: VecDeque<Arc<ImportInputView>>,
    next_stamp: u64,
    context_stamps: Vec<(ImportDiscoveryContext, u64)>,
    provenance_stamps: Vec<(AcceptedReadManifestEntry, u64)>,
    observation_stamps: Vec<(ImportObservation, u64)>,
    topology_stamps: Vec<(Arc<[AcceptedImportTopologyFact]>, u64)>,
}

impl Default for ImportInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            next_stamp: 1,
            context_stamps: Vec::new(),
            provenance_stamps: Vec::new(),
            observation_stamps: Vec::new(),
            topology_stamps: Vec::new(),
        }
    }
}

fn module_source_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new("module-source", Arc::<str>::from(module.as_str()))
}

fn import_context_input() -> InputIdentity {
    InputIdentity::new("import-discovery-context", "current")
}

fn accepted_import_topology_input() -> InputIdentity {
    InputIdentity::new("accepted-import-topology", "current")
}

fn accepted_read_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new(
        "accepted-read-provenance",
        Arc::<str>::from(module.as_str()),
    )
}

fn accepted_import_provenance_input(identity: crate::PhysicalFileIdentity) -> InputIdentity {
    InputIdentity::new(
        "accepted-import-provenance",
        format!("{}:{}", identity.volume(), identity.file()),
    )
}

fn import_observation_input(request: &ImportDiscoveryRequest) -> InputIdentity {
    InputIdentity::new("import-observation", request.runtime_input_key())
}

#[cfg(test)]
fn test_import_graph_input() -> InputIdentity {
    accepted_import_topology_input()
}

fn import_input_error(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

fn exact_value_stamp<T: Clone + Eq>(
    next_stamp: &mut u64,
    values: &mut Vec<(T, u64)>,
    value: &T,
) -> u64 {
    values
        .iter()
        .find_map(|(candidate, stamp)| (candidate == value).then_some(*stamp))
        .unwrap_or_else(|| {
            let stamp = *next_stamp;
            *next_stamp += 1;
            values.push((value.clone(), stamp));
            stamp
        })
}

fn lock_import_store(
    store: &Mutex<ImportInputStore>,
) -> std::sync::MutexGuard<'_, ImportInputStore> {
    store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn pending_occurrence_requests(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> Vec<ImportDiscoveryRequest> {
    let mut pending = Vec::new();
    for group in groups {
        let observations = group
            .iter()
            .map(|request| ledger.get(request))
            .collect::<Vec<_>>();
        if observations
            .iter()
            .any(|observation| observation.is_some_and(|value| value.status().is_failure()))
        {
            break;
        }
        let missing = group
            .iter()
            .zip(&observations)
            .filter_map(|(request, observation)| observation.is_none().then_some(request.clone()))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            pending.extend(missing);
            break;
        }
        if observations
            .iter()
            .any(|observation| observation.is_some_and(|value| value.accepted_source().is_some()))
        {
            break;
        }
    }
    pending
}

fn parse_module_value_equal(left: &ParseModuleValue, right: &ParseModuleValue) -> bool {
    match (&left.result, &right.result) {
        (Ok(left), Ok(right)) => left.revision() == right.revision(),
        (Err(left), Err(right)) => left == right,
        _ => false,
    }
}

fn module_index_value_equal(left: &ModuleIndexValue, right: &ModuleIndexValue) -> bool {
    match (&left.0, &right.0) {
        (Ok(left), Ok(right)) => left == right,
        (Err(left), Err(right)) => left == right,
        _ => false,
    }
}

fn declaration_occurrence_index_value_equal(
    left: &DeclarationOccurrenceIndexValue,
    right: &DeclarationOccurrenceIndexValue,
) -> bool {
    left == right
}

fn module_rir_value_equal(left: &ModuleRirValue, right: &ModuleRirValue) -> bool {
    match (&left.result, &right.result) {
        (Ok(left), Ok(right)) => left.revision() == right.revision(),
        (Err(left), Err(right)) => left == right,
        _ => false,
    }
}

fn module_rir_value_from_lowering(
    result: Result<ModuleRirOutput, (crate::CompileError, crate::CanonicalRirWork)>,
) -> ModuleRirValue {
    match result {
        Ok(output) => {
            let work = output.work();
            ModuleRirValue {
                result: Ok(Arc::new(output)),
                work,
            }
        }
        Err((error, work)) => ModuleRirValue {
            result: Err(crate::CompileErrors::from(error)),
            work,
        },
    }
}

fn project_semantic_shell(
    fact: &crate::declaration_candidate::DeclarationShellFact,
    declaration_span: rue_span::Span,
    source_order: u32,
) -> rue_air::SemanticDeclarationShell {
    use crate::declaration_candidate::{
        DeclarationCandidateCategory as C, DeclarationParameterMode as M,
    };
    use rue_air::{StableDefinitionKind as K, StableDefinitionNamespace as N};

    let (namespace, kind) = match fact.key.category {
        C::Function | C::ExternFunction => (N::Value, K::Function),
        C::Struct => (N::Type, K::Struct),
        C::Enum => (N::Type, K::Enum),
        // This is an epoch-local adapter only. The query fact and key remain
        // `ConstCandidate`; no stable definition ID is issued from this value.
        C::ConstCandidate => (N::Value, K::ValueConst),
        C::Destructor => (N::Destructor, K::Destructor),
        C::Method => (N::Method, K::Method),
        C::AssociatedFunction => (N::Method, K::AssociatedFunction),
    };
    let parameter_names = fact
        .parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>()
        .into();
    let parameter_modes = fact
        .parameters
        .iter()
        .map(|parameter| match parameter.mode {
            M::Value => rue_rir::RirParamMode::Normal,
            M::Borrow => rue_rir::RirParamMode::Borrow,
            M::Inout => rue_rir::RirParamMode::Inout,
        })
        .collect::<Vec<_>>()
        .into();
    let parameter_comptime = fact
        .parameters
        .iter()
        .map(|parameter| parameter.is_comptime)
        .collect::<Vec<_>>()
        .into();
    rue_air::SemanticDeclarationShell {
        identity: rue_air::SemanticDeclarationShellIdentity {
            module_path: Arc::from(fact.key.module.as_str()),
            is_trusted_standard_library: fact.key.module.is_trusted_standard_library(),
            namespace,
            kind,
            name: fact.key.name.clone(),
            owner: fact.key.owner.as_ref().map(|owner| owner.name.clone()),
        },
        declaration_span,
        parameter_names,
        parameter_modes,
        parameter_comptime,
        source_order,
        has_self: fact.receiver.is_some(),
        receiver_mode: fact.receiver.map(|mode| match mode {
            M::Value => rue_rir::RirParamMode::Normal,
            M::Borrow => rue_rir::RirParamMode::Borrow,
            M::Inout => rue_rir::RirParamMode::Inout,
        }),
        receiver_is_mut: fact.receiver_is_mut,
        is_generic: fact.is_generic,
        is_public: fact.is_public,
        is_unchecked: fact.is_unchecked,
        is_extern: fact.is_extern,
        signature_fingerprint: fact.signature_fingerprint,
    }
}

fn module_input_view(
    store: &Mutex<ModuleInputStore>,
    revision: Revision,
) -> Result<Arc<ModuleInputView>, QueryAbort> {
    store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .revisions
        .iter()
        .find(|view| view.revision == revision)
        .cloned()
        .ok_or(QueryAbort::UnpublishedRevision(revision))
}

fn publish_module_inputs(
    store: &Mutex<ModuleInputStore>,
    revision: Revision,
    snapshot: &SourceSnapshot,
) -> Vec<(InputIdentity, u64)> {
    let mut store = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut leaves = Vec::new();
    for source in snapshot.source_revision().modules() {
        let leaf = ModuleInputLeaf {
            revision: source.clone(),
        };
        let ModuleInputStore {
            next_stamp, stamps, ..
        } = &mut *store;
        leaves.push((
            module_source_input(&source.module),
            exact_value_stamp(next_stamp, stamps, &leaf),
        ));
    }
    store.revisions.push_back(Arc::new(ModuleInputView {
        revision,
        snapshot: snapshot.clone(),
    }));
    while store.revisions.len() > MODULE_INPUT_REVISION_RETENTION {
        store.revisions.pop_front();
    }
    let retained = store.revisions.iter().cloned().collect::<Vec<_>>();
    store.stamps.retain(|(leaf, _)| {
        retained.iter().any(|view| {
            view.snapshot
                .source_revision()
                .modules()
                .contains(&leaf.revision)
        })
    });
    leaves
}

struct SemanticNucleusTypeProvider<'a> {
    context: &'a QueryContext,
    family: &'a SemanticNucleusFamily,
    shells: &'a QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    names: &'a QueryFamily<LookupNameKey, LookupNameValue>,
    configuration: crate::semantic_query_nucleus::SemanticQueryConfiguration,
    substitutions: BTreeMap<Arc<str>, crate::durable_semantics::DurableType>,
    value_substitutions: BTreeMap<Arc<str>, crate::durable_semantics::DurableConstValue>,
    deferred_value_parameters: BTreeMap<Arc<str>, crate::durable_semantics::DurableType>,
    anonymous_nominals:
        BTreeMap<crate::AnonymousNominalKey, crate::durable_semantics::DurableAnonymousNominal>,
    dependency_source: crate::StableDefinitionKey,
    dependency_kind: rue_air::DeclarationTypeDependencyKind,
    dependencies: BTreeSet<crate::semantic_query_nucleus::SemanticDeclarationDependency>,
    deferred_ownership: BTreeSet<crate::semantic_query_nucleus::DeferredOwnershipGate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinearOwnershipFact {
    DoesNotCarry,
    Carries,
    Deferred,
}

impl LinearOwnershipFact {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Carries, _) | (_, Self::Carries) => Self::Carries,
            (Self::Deferred, _) | (_, Self::Deferred) => Self::Deferred,
            _ => Self::DoesNotCarry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluatedSemanticConst {
    Value(Arc<TypedSemanticConst>),
    Module(ModuleId),
    TargetEnum(TargetEnumValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetEnumValue {
    type_name: &'static str,
    variant: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypedSemanticConst {
    value: crate::durable_semantics::DurableConstValue,
    /// `None` is reserved for an unconstrained integer literal. Every named
    /// value, local derived from one, and completed operation carries its
    /// canonical semantic type; consumers must never reconstruct it from the
    /// value's magnitude.
    ty: Option<crate::durable_semantics::DurableType>,
}

impl TypedSemanticConst {
    fn typed(
        value: crate::durable_semantics::DurableConstValue,
        ty: crate::durable_semantics::DurableType,
    ) -> Arc<Self> {
        Arc::new(Self {
            value,
            ty: Some(ty),
        })
    }

    fn integer_literal(value: i128) -> Arc<Self> {
        Arc::new(Self {
            value: crate::durable_semantics::DurableConstValue::Integer(value),
            ty: None,
        })
    }
}

enum EvaluateSemanticConstError {
    Abort(QueryAbort),
    Failure(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
}

impl EvaluateSemanticConstError {
    fn failure(failure: crate::semantic_query_nucleus::SemanticNucleusFailure) -> Self {
        Self::Failure(Box::new(failure))
    }
}

fn durable_type_diagnostic_name(ty: &crate::durable_semantics::DurableType) -> String {
    use crate::durable_semantics::DurableType as T;

    fn function_name(function: &crate::FunctionInstanceKey) -> Option<&str> {
        match function {
            crate::FunctionInstanceKey::Definition(key) => Some(key.name()),
            crate::FunctionInstanceKey::Specialization { base, .. } => function_name(base),
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_) => None,
        }
    }

    match ty {
        T::I8 => "i8".to_owned(),
        T::I16 => "i16".to_owned(),
        T::I32 => "i32".to_owned(),
        T::I64 => "i64".to_owned(),
        T::U8 => "u8".to_owned(),
        T::U16 => "u16".to_owned(),
        T::U32 => "u32".to_owned(),
        T::U64 => "u64".to_owned(),
        T::Bool => "bool".to_owned(),
        T::Unit => "()".to_owned(),
        T::Never => "!".to_owned(),
        T::ComptimeType => "type".to_owned(),
        T::BuiltinNominal { name, .. } => name.to_string(),
        T::Nominal(key) => key.name().to_owned(),
        T::AnonymousNominal(key) => match &key.producer {
            crate::StableProducerId::Definition(key) => key.name().to_owned(),
            crate::StableProducerId::Function(function) => {
                let name = function_name(function).unwrap_or("anonymous");
                let mut arguments = key
                    .arguments
                    .types
                    .iter()
                    .filter_map(durable_type_from_instance_key)
                    .map(|ty| durable_type_diagnostic_name(&ty))
                    .collect::<Vec<_>>();
                arguments.extend(key.arguments.values.iter().map(|value| match value {
                    crate::CanonicalArgumentValue::Integer(value) => value.to_string(),
                    crate::CanonicalArgumentValue::Bool(value) => value.to_string(),
                    crate::CanonicalArgumentValue::Type(value) => {
                        durable_type_from_instance_key(value).map_or_else(
                            || "type".to_owned(),
                            |ty| durable_type_diagnostic_name(&ty),
                        )
                    }
                    crate::CanonicalArgumentValue::Function(_) => "function".to_owned(),
                    crate::CanonicalArgumentValue::Unit => "()".to_owned(),
                    crate::CanonicalArgumentValue::String(value) => format!("\"{value}\""),
                }));
                if arguments.is_empty() {
                    name.to_owned()
                } else {
                    format!("{name}({})", arguments.join(", "))
                }
            }
        },
        T::Array { element, len } => {
            format!("[{}; {len}]", durable_type_diagnostic_name(element))
        }
        T::Slice { name, .. } => name.to_string(),
        T::PtrConst(pointee) => {
            format!("ptr const {}", durable_type_diagnostic_name(pointee))
        }
        T::PtrMut(pointee) => format!("ptr mut {}", durable_type_diagnostic_name(pointee)),
        T::Module(module) => module.to_string(),
        T::GenericParameter(index) => format!("T{index}"),
    }
}

fn inferred_const_type_name(value: &crate::durable_semantics::DurableConstValue) -> &'static str {
    use crate::durable_semantics::DurableConstValue as V;
    match value {
        V::Integer(value) if i32::try_from(*value).is_ok() => "i32",
        V::Integer(value) if i64::try_from(*value).is_ok() => "i64",
        V::Integer(_) => "u64",
        V::Bool(_) => "bool",
        V::Unit => "()",
        V::String(_) => "str",
        V::Type(_) | V::Function(_) => "type",
    }
}

fn substitute_durable_generics(
    ty: &crate::durable_semantics::DurableType,
    type_arguments: &[crate::durable_semantics::DurableType],
) -> crate::durable_semantics::DurableType {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::GenericParameter(index) => type_arguments
            .get(*index as usize)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        T::Array { element, len } => T::Array {
            element: Box::new(substitute_durable_generics(element, type_arguments)),
            len: *len,
        },
        T::Slice { element, name } => T::Slice {
            element: Box::new(substitute_durable_generics(element, type_arguments)),
            name: name.clone(),
        },
        T::PtrConst(pointee) => T::PtrConst(Box::new(substitute_durable_generics(
            pointee,
            type_arguments,
        ))),
        T::PtrMut(pointee) => T::PtrMut(Box::new(substitute_durable_generics(
            pointee,
            type_arguments,
        ))),
        _ => ty.clone(),
    }
}

fn durable_const_fits_type(
    value: &crate::durable_semantics::DurableConstValue,
    ty: &crate::durable_semantics::DurableType,
) -> bool {
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    match (ty, value) {
        (T::I8, V::Integer(value)) => i8::try_from(*value).is_ok(),
        (T::I16, V::Integer(value)) => i16::try_from(*value).is_ok(),
        (T::I32, V::Integer(value)) => i32::try_from(*value).is_ok(),
        (T::I64, V::Integer(value)) => i64::try_from(*value).is_ok(),
        (T::U8, V::Integer(value)) => u8::try_from(*value).is_ok(),
        (T::U16, V::Integer(value)) => u16::try_from(*value).is_ok(),
        (T::U32, V::Integer(value)) => u32::try_from(*value).is_ok(),
        (T::U64, V::Integer(value)) => u64::try_from(*value).is_ok(),
        (T::Bool, V::Bool(_)) | (T::Unit, V::Unit) => true,
        (T::ComptimeType, V::Type(_)) => true,
        _ => false,
    }
}

fn semantic_nucleus_declaration_name(identity: &str) -> Option<Arc<str>> {
    let candidate = [
        "identity:",
        "signature:",
        "nominal-well-formed:",
        "const:",
        "comptime:",
        "anonymous:",
    ]
    .iter()
    .find_map(|prefix| identity.strip_prefix(prefix))?;
    let (module_len, rest) = candidate.split_once(':')?;
    let module_len = module_len.parse::<usize>().ok()?;
    let rest = rest.get(module_len..)?.strip_prefix(':')?;
    let (_, rest) = rest.split_once(':')?;
    let (name_len, rest) = rest.split_once(':')?;
    let name_len = name_len.parse::<usize>().ok()?;
    Some(Arc::from(rest.get(..name_len)?))
}

fn semantic_nucleus_cycle_names(nodes: &[rue_query::NodeIdentity]) -> Arc<[Arc<str>]> {
    let mut names = nodes
        .iter()
        .filter(|node| node.family() == "compiler.semantic-nucleus")
        .filter_map(|node| semantic_nucleus_declaration_name(node.key()))
        .collect::<Vec<_>>();
    if let Some(first) = names.first().cloned()
        && (names.len() == 1 || names.last() != Some(&first))
    {
        names.push(first);
    }
    names.into()
}

pub(crate) fn function_definition_key(
    function: &crate::FunctionInstanceKey,
) -> Option<&StableDefinitionKey> {
    match function {
        crate::FunctionInstanceKey::Definition(key) => Some(key),
        crate::FunctionInstanceKey::Specialization { base, .. } => function_definition_key(base),
        crate::FunctionInstanceKey::AnonymousMember { .. }
        | crate::FunctionInstanceKey::DropGlue(_) => None,
    }
}

fn body_source_definition_key(
    function: &crate::FunctionInstanceKey,
) -> Option<&StableDefinitionKey> {
    if let crate::FunctionInstanceKey::AnonymousMember { owner, .. } = function {
        let crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(owner)) =
            owner.as_ref()
        else {
            return None;
        };
        return match &owner.producer {
            crate::StableProducerId::Definition(key) => Some(key),
            crate::StableProducerId::Function(function) => function_definition_key(function),
        };
    }
    function_definition_key(function)
}

fn declaration_candidate_for_stable_key(
    key: &StableDefinitionKey,
) -> Option<crate::declaration_candidate::DeclarationCandidateKey> {
    use crate::StableDefinitionKind as K;
    use crate::declaration_candidate::{
        DeclarationCandidateCategory as C, DeclarationCandidateOwner,
    };

    let category = match key.kind() {
        K::Function => C::Function,
        K::Struct => C::Struct,
        K::Enum => C::Enum,
        K::ValueConst | K::ModuleBinding => C::ConstCandidate,
        K::Method => C::Method,
        K::AssociatedFunction => C::AssociatedFunction,
        K::Destructor => C::Destructor,
    };
    let owner = match key.owner() {
        Some(owner) => Some(DeclarationCandidateOwner {
            category: match owner.kind() {
                K::Struct => C::Struct,
                K::Enum => C::Enum,
                _ => return None,
            },
            name: Arc::from(owner.name()),
        }),
        None => None,
    };
    if key.kind().requires_owner() && owner.is_none() {
        return None;
    }
    Some(crate::declaration_candidate::DeclarationCandidateKey {
        module: key.module().clone(),
        category,
        name: Arc::from(key.name()),
        owner,
        duplicate_discriminator: 0,
    })
}

fn anonymous_nominal_query_key(
    identity: &crate::AnonymousNominalKey,
    configuration: &crate::semantic_query_nucleus::SemanticQueryConfiguration,
) -> Option<crate::semantic_query_nucleus::AnonymousNominalQueryKey> {
    let producer = match &identity.producer {
        crate::StableProducerId::Definition(key) => key,
        crate::StableProducerId::Function(function) => function_definition_key(function)?,
    };
    Some(crate::semantic_query_nucleus::AnonymousNominalQueryKey {
        producer: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate_for_stable_key(producer)?,
            configuration: configuration.clone(),
        },
        identity: identity.clone(),
    })
}

#[derive(Debug)]
pub(crate) enum BodyTransactionRequestFailure {
    Query(QueryAbort),
    DeferredAnonymousProducers(Arc<[crate::FunctionInstanceKey]>),
    /// An anonymous producer this body depends on committed an internal-error
    /// (E9000-class) failure — the anchor-transport invariant violation
    /// (RUE-1089). The dependent body cannot be built and must fail closed; the
    /// failure is never retried and never rescued by RIR recomputation.
    ProducerFailed(crate::semantic_query_nucleus::SemanticNucleusFailure),
}

/// Whether a committed semantic-nucleus failure is an internal-error
/// (E9000-class) diagnostic. The anonymous-anchor transport invariant violation
/// (RUE-1089) surfaces exactly as `Diagnostic(InternalError(_))`. Such a
/// committed failure is a corrupt-input fact and must fail closed, never be
/// downgraded to a retryable abort or rescued by structural recomputation.
pub(crate) fn semantic_nucleus_failure_is_internal_error(
    failure: &crate::semantic_query_nucleus::SemanticNucleusFailure,
) -> bool {
    use crate::semantic_query_nucleus::SemanticNucleusFailure as F;
    let kind = match failure {
        F::Diagnostic(kind)
        | F::DiagnosticAtParameter { kind, .. }
        | F::DiagnosticAtDeclaration { kind, .. }
        | F::OwnershipGate { kind, .. }
        | F::DiagnosticWithHelp { kind, .. } => kind,
        F::Shell(_)
        | F::Syntax(_)
        | F::Resolution(_)
        | F::SignatureReentry { .. }
        | F::Cycle(_) => {
            return false;
        }
    };
    matches!(kind, rue_error::ErrorKind::InternalError(_))
}

pub(crate) fn collect_instance_anonymous_nominals(
    function: &crate::FunctionInstanceKey,
) -> BTreeSet<crate::AnonymousNominalKey> {
    fn arguments(
        arguments: &crate::CanonicalArguments,
        output: &mut BTreeSet<crate::AnonymousNominalKey>,
    ) {
        for ty in arguments.types.iter() {
            instance_type(ty, output);
        }
        for value in arguments.values.iter() {
            match value {
                crate::CanonicalArgumentValue::Type(ty) => instance_type(ty, output),
                crate::CanonicalArgumentValue::Function(function) => {
                    instance_function(function, output);
                }
                _ => {}
            }
        }
    }

    fn anonymous(
        identity: &crate::AnonymousNominalKey,
        output: &mut BTreeSet<crate::AnonymousNominalKey>,
    ) {
        if !output.insert(identity.clone()) {
            return;
        }
        if let crate::StableProducerId::Function(function) = &identity.producer {
            instance_function(function, output);
        }
        arguments(&identity.arguments, output);
    }

    fn instance_type(
        ty: &crate::TypeInstanceKey,
        output: &mut BTreeSet<crate::AnonymousNominalKey>,
    ) {
        match ty {
            crate::TypeInstanceKey::Nominal(crate::NominalInstanceKey::Anonymous(identity)) => {
                anonymous(identity, output);
            }
            crate::TypeInstanceKey::Array { element, .. }
            | crate::TypeInstanceKey::Slice { element, .. }
            | crate::TypeInstanceKey::PtrConst(element)
            | crate::TypeInstanceKey::PtrMut(element) => instance_type(element, output),
            _ => {}
        }
    }

    fn instance_function(
        function: &crate::FunctionInstanceKey,
        output: &mut BTreeSet<crate::AnonymousNominalKey>,
    ) {
        match function {
            crate::FunctionInstanceKey::Definition(_) => {}
            crate::FunctionInstanceKey::Specialization {
                base,
                arguments: values,
            } => {
                instance_function(base, output);
                arguments(values, output);
            }
            crate::FunctionInstanceKey::AnonymousMember { owner, .. }
            | crate::FunctionInstanceKey::DropGlue(owner) => instance_type(owner, output),
        }
    }

    let mut output = BTreeSet::new();
    instance_function(function, &mut output);
    output
}

pub(crate) fn durable_type_from_instance_key(
    value: &crate::TypeInstanceKey,
) -> Option<crate::durable_semantics::DurableType> {
    use crate::TypeInstanceKey as T;
    use crate::durable_semantics::DurableType as D;
    Some(match value {
        T::I8 => D::I8,
        T::I16 => D::I16,
        T::I32 => D::I32,
        T::I64 => D::I64,
        T::U8 => D::U8,
        T::U16 => D::U16,
        T::U32 => D::U32,
        T::U64 => D::U64,
        T::Bool => D::Bool,
        T::Unit => D::Unit,
        T::Never => D::Never,
        T::ComptimeType => D::ComptimeType,
        T::BuiltinNominal { kind, name } => D::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
        },
        T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => D::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
        },
        T::Nominal(crate::NominalInstanceKey::Named(key)) => D::Nominal(key.clone()),
        T::Nominal(crate::NominalInstanceKey::Anonymous(key)) => D::AnonymousNominal(key.clone()),
        T::Array { element, len } => D::Array {
            element: Box::new(durable_type_from_instance_key(element)?),
            len: *len,
        },
        T::Slice { element, name } => D::Slice {
            element: Box::new(durable_type_from_instance_key(element)?),
            name: name.clone(),
        },
        T::PtrConst(value) => D::PtrConst(Box::new(durable_type_from_instance_key(value)?)),
        T::PtrMut(value) => D::PtrMut(Box::new(durable_type_from_instance_key(value)?)),
        T::Module(value) => D::Module(value.clone()),
        T::GenericParameter(index) => D::GenericParameter(*index),
    })
}

pub(crate) fn durable_value_from_argument(
    value: &crate::CanonicalArgumentValue,
) -> Option<crate::durable_semantics::DurableConstValue> {
    use crate::CanonicalArgumentValue as V;
    use crate::durable_semantics::DurableConstValue as D;
    Some(match value {
        V::Integer(value) => D::Integer(*value),
        V::Bool(value) => D::Bool(*value),
        V::Type(value) => D::Type(durable_type_from_instance_key(value)?),
        V::Function(value) => {
            let crate::FunctionInstanceKey::Definition(key) = value.as_ref() else {
                return None;
            };
            D::Function(key.clone())
        }
        V::Unit => D::Unit,
        V::String(value) => D::String(value.clone()),
    })
}

fn comptime_call_for_anonymous_function(
    producer: &crate::semantic_query_nucleus::DeclarationSemanticQueryKey,
    function: &crate::FunctionInstanceKey,
    shell: &crate::declaration_candidate::DeclarationShellFact,
    signature: &crate::semantic_query_nucleus::ResolvedDeclarationSignature,
) -> Option<crate::semantic_query_nucleus::ComptimeCallQueryKey> {
    let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
        parameters,
        result: crate::durable_semantics::DurableType::ComptimeType,
        is_extern: false,
        ..
    } = &signature.signature
    else {
        return None;
    };
    let expected = crate::semantic_query_nucleus::direct_identity(shell)?.key;
    let arguments = match function {
        crate::FunctionInstanceKey::Definition(definition) if *definition == expected => {
            crate::CanonicalArguments::default()
        }
        crate::FunctionInstanceKey::Specialization { base, arguments }
            if matches!(
                base.as_ref(),
                crate::FunctionInstanceKey::Definition(definition) if *definition == expected
            ) =>
        {
            arguments.clone()
        }
        _ => return None,
    };
    if shell.parameters.len() != parameters.len()
        || shell
            .parameters
            .iter()
            .any(|parameter| !parameter.is_comptime)
    {
        return None;
    }
    let mut type_arguments = arguments.types.iter();
    let mut value_arguments = arguments.values.iter();
    let mut types = Vec::new();
    let mut values = Vec::new();
    for (header, parameter) in shell.parameters.iter().zip(parameters.iter()) {
        if parameter.ty == crate::durable_semantics::DurableType::ComptimeType
            && let Some(value) = type_arguments.next()
        {
            types.push((header.name.clone(), durable_type_from_instance_key(value)?));
        } else {
            values.push((
                header.name.clone(),
                durable_value_from_argument(value_arguments.next()?)?,
            ));
        }
    }
    if type_arguments.next().is_some() || value_arguments.next().is_some() {
        return None;
    }
    Some(crate::semantic_query_nucleus::ComptimeCallQueryKey {
        declaration: producer.clone(),
        type_arguments: types.into(),
        value_arguments: values.into(),
    })
}

fn collect_anonymous_nominal_type_dependencies(
    ty: &crate::durable_semantics::DurableType,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::AnonymousNominal(identity) => {
            output.insert(identity.clone());
        }
        T::Array { element, .. }
        | T::Slice { element, .. }
        | T::PtrConst(element)
        | T::PtrMut(element) => collect_anonymous_nominal_type_dependencies(element, output),
        _ => {}
    }
}

fn collect_anonymous_nominal_value_dependencies(
    value: &crate::durable_semantics::DurableConstValue,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    if let crate::durable_semantics::DurableConstValue::Type(ty) = value {
        collect_anonymous_nominal_type_dependencies(ty, output);
    }
}

fn collect_durable_anonymous_nominal_dependencies(
    nominal: &crate::durable_semantics::DurableAnonymousNominal,
    output: &mut BTreeSet<crate::AnonymousNominalKey>,
) {
    use crate::durable_semantics::{
        DurableAnonymousMethodType as M, DurableAnonymousNominalShape as S,
    };
    for (_, ty) in nominal.type_captures.iter() {
        collect_anonymous_nominal_type_dependencies(ty, output);
    }
    for (_, value) in nominal.value_captures.iter() {
        collect_anonymous_nominal_value_dependencies(value, output);
    }
    match &nominal.shape {
        S::Struct { fields, methods } => {
            for (_, ty) in fields.iter() {
                collect_anonymous_nominal_type_dependencies(ty, output);
            }
            for method in methods.iter() {
                for (ty, _, _) in method.parameters.iter() {
                    if let M::Concrete(ty) = ty {
                        collect_anonymous_nominal_type_dependencies(ty, output);
                    }
                }
                if let M::Concrete(ty) = &method.result {
                    collect_anonymous_nominal_type_dependencies(ty, output);
                }
            }
        }
        S::Enum { variants } => {
            for (_, fields) in variants.iter() {
                for ty in fields.iter() {
                    collect_anonymous_nominal_type_dependencies(ty, output);
                }
            }
        }
    }
}

struct SemanticConstEvaluator<'a, 'provider> {
    provider: &'provider mut SemanticNucleusTypeProvider<'a>,
    imports: &'a QueryFamily<DeclarationImportQueryKey, DeclarationImportQueryValue>,
    declaration: &'a crate::semantic_query_nucleus::DeclarationSemanticQueryKey,
    source: &'a str,
    interner: &'a crate::ThreadedRodeo,
    import_sites: &'a [crate::ImportDirective],
    locals: BTreeMap<Arc<str>, EvaluatedSemanticConst>,
    producer: crate::StableProducerId,
    canonical_arguments: crate::CanonicalArguments,
    /// Anonymous type literals transported from the frontend, in this fragment's
    /// synthetic-source coordinate space, each carrying the exact `AstGen`
    /// anchor. The single anchor authority: `eval_type_literal` looks each
    /// reparsed literal up here and fails closed on any locator/kind/anchor
    /// disagreement (RUE-1089).
    anonymous_sites: &'a [crate::semantic_query_nucleus::TransportedAnonymousSite],
    next_call: u32,
    expected_type: Option<crate::durable_semantics::DurableType>,
}

impl SemanticConstEvaluator<'_, '_> {
    fn failure<T>(message: impl Into<Arc<str>>) -> Result<T, EvaluateSemanticConstError> {
        Err(Self::failure_value(message))
    }

    fn failure_value(message: impl Into<Arc<str>>) -> EvaluateSemanticConstError {
        EvaluateSemanticConstError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(message.into()),
        )
    }

    fn comptime_failure_value(reason: impl Into<String>) -> EvaluateSemanticConstError {
        EvaluateSemanticConstError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::ComptimeEvaluationFailed {
                    reason: reason.into(),
                },
            ),
        )
    }

    fn provider_error(
        error: rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    ) -> EvaluateSemanticConstError {
        match error {
            rue_air::SemanticProviderError::Abort(abort) => Self::abort(abort),
            rue_air::SemanticProviderError::Failure(failure) => Self::domain_failure(failure),
        }
    }

    fn abort(abort: QueryAbort) -> EvaluateSemanticConstError {
        EvaluateSemanticConstError::Abort(abort)
    }

    fn domain_failure(
        failure: crate::semantic_query_nucleus::SemanticNucleusFailure,
    ) -> EvaluateSemanticConstError {
        EvaluateSemanticConstError::failure(failure)
    }

    fn symbol(&self, symbol: &lasso::Spur) -> Arc<str> {
        Arc::from(self.interner.resolve(symbol))
    }

    fn value(
        &self,
        value: EvaluatedSemanticConst,
    ) -> Result<TypedSemanticConst, EvaluateSemanticConstError> {
        match value {
            EvaluatedSemanticConst::Value(value) => Ok(Arc::unwrap_or_clone(value)),
            EvaluatedSemanticConst::Module(_) => {
                Self::failure("module used where a value is required")
            }
            EvaluatedSemanticConst::TargetEnum(_) => {
                Self::failure("target descriptor used where a durable const value is required")
            }
        }
    }

    fn target_intrinsic(
        &self,
        intrinsic: &rue_parser::ast::IntrinsicCallExpr,
        type_name: &'static str,
        variant: &'static str,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        if !intrinsic.args.is_empty() {
            return Err(Self::domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::IntrinsicWrongArgCount {
                        name: self.symbol(&intrinsic.name.name).to_string(),
                        expected: 0,
                        found: intrinsic.args.len(),
                    },
                ),
            ));
        }
        Ok(EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name,
            variant,
        }))
    }

    fn target_enum_variant(
        &self,
        type_name: &str,
        variant: &str,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        let canonical_type = match type_name {
            "Arch" => "Arch",
            "Os" => "Os",
            "DataModel" => "DataModel",
            _ => return Self::failure("unknown target descriptor enum"),
        };
        let valid = match canonical_type {
            "Arch" => matches!(variant, "X86_64" | "Aarch64"),
            "Os" => matches!(variant, "Linux" | "Macos"),
            "DataModel" => matches!(variant, "Ilp32" | "Lp64" | "Llp64"),
            _ => unreachable!(),
        };
        if !valid {
            return Err(Self::domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::UnknownVariant {
                        enum_name: canonical_type.to_owned(),
                        variant_name: variant.to_owned(),
                    },
                ),
            ));
        }
        let variant = match variant {
            "X86_64" => "X86_64",
            "Aarch64" => "Aarch64",
            "Linux" => "Linux",
            "Macos" => "Macos",
            "Ilp32" => "Ilp32",
            "Lp64" => "Lp64",
            "Llp64" => "Llp64",
            _ => unreachable!(),
        };
        Ok(EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: canonical_type,
            variant,
        }))
    }

    fn bool_value(
        &mut self,
        expression: &rue_parser::ast::Expr,
    ) -> Result<bool, EvaluateSemanticConstError> {
        let evaluated = self.eval(expression)?;
        match self.value(evaluated)?.value {
            crate::durable_semantics::DurableConstValue::Bool(value) => Ok(value),
            _ => Self::failure("comptime condition is not boolean"),
        }
    }

    fn int_value(
        &mut self,
        expression: &rue_parser::ast::Expr,
    ) -> Result<(i128, Option<crate::durable_semantics::DurableType>), EvaluateSemanticConstError>
    {
        let evaluated = self.eval(expression)?;
        let typed = self.value(evaluated)?;
        match typed.value {
            crate::durable_semantics::DurableConstValue::Integer(value) => Ok((value, typed.ty)),
            _ => Self::failure("comptime arithmetic operand is not an integer"),
        }
    }

    fn integer_type(
        &self,
        left: Option<crate::durable_semantics::DurableType>,
        right: Option<crate::durable_semantics::DurableType>,
    ) -> Result<crate::durable_semantics::DurableType, EvaluateSemanticConstError> {
        use crate::durable_semantics::DurableType as T;
        let fallback = self
            .expected_type
            .clone()
            .filter(|ty| {
                matches!(
                    ty,
                    T::I8 | T::I16 | T::I32 | T::I64 | T::U8 | T::U16 | T::U32 | T::U64
                )
            })
            .unwrap_or(T::I32);
        match (left, right) {
            (Some(left), Some(right)) if left != right => Err(EvaluateSemanticConstError::failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::TypeMismatch {
                        expected: durable_type_diagnostic_name(&left),
                        found: durable_type_diagnostic_name(&right),
                    },
                ),
            )),
            (Some(ty), _) | (_, Some(ty)) => Ok(ty),
            (None, None) => Ok(fallback),
        }
    }

    fn require_integer_fits(
        ty: &crate::durable_semantics::DurableType,
        value: i128,
    ) -> Result<(), EvaluateSemanticConstError> {
        let value = crate::durable_semantics::DurableConstValue::Integer(value);
        if durable_const_fits_type(&value, ty) {
            Ok(())
        } else {
            let value = match value {
                crate::durable_semantics::DurableConstValue::Integer(value) => value,
                _ => unreachable!(),
            };
            let type_name = durable_type_diagnostic_name(ty);
            Err(Self::comptime_failure_value(format!(
                "integer overflow evaluating constant at type {type_name}: value {value} is out of range for type {type_name}; {value} does not fit in {type_name} (this operation would panic at runtime)",
            )))
        }
    }

    fn eval_binary(
        &mut self,
        expression: &rue_parser::ast::BinaryExpr,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        use crate::durable_semantics::DurableConstValue as V;
        use rue_parser::ast::BinaryOp as O;
        if expression.op == O::And {
            return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Bool(self.bool_value(&expression.left)? && self.bool_value(&expression.right)?),
                crate::durable_semantics::DurableType::Bool,
            )));
        }
        if expression.op == O::Or {
            return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Bool(self.bool_value(&expression.left)? || self.bool_value(&expression.right)?),
                crate::durable_semantics::DurableType::Bool,
            )));
        }
        let left = self.eval(&expression.left)?;
        let right = self.eval(&expression.right)?;
        if let (
            EvaluatedSemanticConst::TargetEnum(left),
            EvaluatedSemanticConst::TargetEnum(right),
        ) = (&left, &right)
        {
            let value = match expression.op {
                O::Eq => left == right,
                O::Ne => left != right,
                _ => return Self::failure("target descriptors support only equality comparisons"),
            };
            return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Bool(value),
                crate::durable_semantics::DurableType::Bool,
            )));
        }
        if matches!(left, EvaluatedSemanticConst::TargetEnum(_))
            || matches!(right, EvaluatedSemanticConst::TargetEnum(_))
        {
            return Self::failure("target descriptor comparison requires matching enum variants");
        }
        let left = self.value(left)?;
        let right = self.value(right)?;
        if let (V::Bool(left), V::Bool(right)) = (&left.value, &right.value) {
            let value = match expression.op {
                O::Eq => *left == *right,
                O::Ne => *left != *right,
                _ => return Self::failure("boolean values support only equality comparisons"),
            };
            return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Bool(value),
                crate::durable_semantics::DurableType::Bool,
            )));
        }
        let (V::Integer(left), left_ty) = (left.value, left.ty) else {
            return Self::failure("comptime arithmetic operand is not an integer");
        };
        let (V::Integer(right), right_ty) = (right.value, right.ty) else {
            return Self::failure("comptime arithmetic operand is not an integer");
        };
        let operand_ty = self.integer_type(left_ty, right_ty)?;
        Self::require_integer_fits(&operand_ty, left)?;
        Self::require_integer_fits(&operand_ty, right)?;
        let value = match expression.op {
            O::Add => V::Integer(left.checked_add(right).ok_or_else(|| {
                Self::comptime_failure_value("integer overflow evaluating addition")
            })?),
            O::Sub => V::Integer(left.checked_sub(right).ok_or_else(|| {
                Self::comptime_failure_value("integer overflow evaluating subtraction")
            })?),
            O::Mul => V::Integer(left.checked_mul(right).ok_or_else(|| {
                Self::comptime_failure_value("integer overflow evaluating multiplication")
            })?),
            O::Div if right == 0 => {
                return Err(Self::comptime_failure_value(
                    "division by zero (this operation would panic at runtime)",
                ));
            }
            O::Mod if right == 0 => {
                return Err(Self::comptime_failure_value(
                    "remainder by zero (this operation would panic at runtime)",
                ));
            }
            O::Div => V::Integer(left.checked_div(right).ok_or_else(|| {
                Self::comptime_failure_value("integer overflow evaluating division")
            })?),
            O::Mod => V::Integer(left.checked_rem(right).ok_or_else(|| {
                Self::comptime_failure_value("integer overflow evaluating remainder")
            })?),
            O::Eq => V::Bool(left == right),
            O::Ne => V::Bool(left != right),
            O::Lt => V::Bool(left < right),
            O::Gt => V::Bool(left > right),
            O::Le => V::Bool(left <= right),
            O::Ge => V::Bool(left >= right),
            O::BitAnd => V::Integer(left & right),
            O::BitOr => V::Integer(left | right),
            O::BitXor => V::Integer(left ^ right),
            O::Shl => V::Integer(left.wrapping_shl((right as u32) & 127)),
            O::Shr => V::Integer(left.wrapping_shr((right as u32) & 127)),
            O::And | O::Or => unreachable!(),
        };
        let ty = if matches!(value, V::Bool(_)) {
            crate::durable_semantics::DurableType::Bool
        } else {
            let V::Integer(result) = value else {
                unreachable!()
            };
            Self::require_integer_fits(&operand_ty, result)?;
            return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Integer(result),
                operand_ty,
            )));
        };
        Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
            value, ty,
        )))
    }

    fn eval_block(
        &mut self,
        block: &rue_parser::ast::BlockExpr,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        let saved = self.locals.clone();
        let saved_types = self.provider.substitutions.clone();
        let saved_values = self.provider.value_substitutions.clone();
        for statement in &block.statements {
            match statement {
                rue_parser::ast::Statement::Let(binding) => {
                    let mut value = self.eval(&binding.init)?;
                    if let Some(annotation) = &binding.ty {
                        let syntax = self
                            .source
                            .get(annotation.span().start as usize..annotation.span().end as usize)
                            .ok_or_else(|| {
                                Self::failure_value("local type annotation has an invalid span")
                            })?;
                        let expected = rue_air::resolve_semantic_type_syntax(
                            self.provider,
                            &self.declaration.declaration.module,
                            syntax,
                        )
                        .map_err(|error| match error {
                            rue_air::SemanticResolutionError::ProviderAbort(abort) => {
                                EvaluateSemanticConstError::Abort(abort)
                            }
                            rue_air::SemanticResolutionError::ProviderFailure(failure) => {
                                EvaluateSemanticConstError::failure(failure)
                            }
                            other => Self::failure_value(format!("{other:?}")),
                        })?;
                        if matches!(
                            expected,
                            crate::durable_semantics::DurableType::Slice { .. }
                        ) {
                            return Err(EvaluateSemanticConstError::failure(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                    rue_error::ErrorKind::SliceEscapesScope,
                                ),
                            ));
                        }
                        let typed = self.value(value)?;
                        if let Some(found) = &typed.ty
                            && found != &expected
                        {
                            return Err(EvaluateSemanticConstError::failure(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                    rue_error::ErrorKind::TypeMismatch {
                                        expected: durable_type_diagnostic_name(&expected),
                                        found: durable_type_diagnostic_name(found),
                                    },
                                ),
                            ));
                        }
                        if !durable_const_fits_type(&typed.value, &expected) {
                            let kind = match typed.value {
                                crate::durable_semantics::DurableConstValue::Integer(value)
                                    if value >= 0 =>
                                {
                                    rue_error::ErrorKind::LiteralOutOfRange {
                                        value: value as u64,
                                        ty: durable_type_diagnostic_name(&expected),
                                    }
                                }
                                _ => rue_error::ErrorKind::TypeMismatch {
                                    expected: durable_type_diagnostic_name(&expected),
                                    found: inferred_const_type_name(&typed.value).to_owned(),
                                },
                            };
                            return Err(EvaluateSemanticConstError::failure(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                                    kind,
                                ),
                            ));
                        }
                        value = EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                            typed.value,
                            expected,
                        ));
                    }
                    if let rue_parser::ast::LetPattern::Ident(name) = &binding.pattern {
                        let name = self.symbol(&name.name);
                        match &value {
                            EvaluatedSemanticConst::Value(value)
                                if matches!(
                                    value.value,
                                    crate::durable_semantics::DurableConstValue::Type(_)
                                ) =>
                            {
                                let crate::durable_semantics::DurableConstValue::Type(ty) =
                                    &value.value
                                else {
                                    unreachable!()
                                };
                                self.provider.substitutions.insert(name.clone(), ty.clone());
                                self.provider.value_substitutions.remove(&name);
                            }
                            EvaluatedSemanticConst::Value(value) => {
                                self.provider
                                    .value_substitutions
                                    .insert(name.clone(), value.value.clone());
                                self.provider.substitutions.remove(&name);
                            }
                            EvaluatedSemanticConst::Module(_)
                            | EvaluatedSemanticConst::TargetEnum(_) => {
                                self.provider.substitutions.remove(&name);
                                self.provider.value_substitutions.remove(&name);
                            }
                        }
                        self.locals.insert(name, value);
                    }
                }
                rue_parser::ast::Statement::Expr(expression) => {
                    self.eval(expression)?;
                }
                rue_parser::ast::Statement::Assign(_) => {
                    return Self::failure(
                        "assignment is not supported in declaration-time comptime",
                    );
                }
            }
        }
        let value = self.eval(&block.expr);
        self.locals = saved;
        self.provider.substitutions = saved_types;
        self.provider.value_substitutions = saved_values;
        value
    }

    fn eval_import(
        &self,
        intrinsic: &rue_parser::ast::IntrinsicCallExpr,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        let index = self
            .import_sites
            .binary_search_by_key(&intrinsic.span.start, |site| site.source_offset())
            .map_err(|_| {
                Self::failure_value(
                    "exact const import is absent from its parser-authored site index",
                )
            })?;
        let site = &self.import_sites[index];
        let key = crate::declaration_candidate::DeclarationImportSiteKey {
            declaration: self.declaration.declaration.clone(),
            occurrence: index as u32,
            specifier: Arc::from(site.specifier()),
        };
        let terminal = self
            .provider
            .context
            .query_registered(self.imports, DeclarationImportQueryKey(key.clone()))
            .map_err(EvaluateSemanticConstError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationImport publishes typed values")
        };
        match value {
            DeclarationImportQueryValue::Available(crate::CanonicalImportResolution::Resolved(
                module,
            )) => Ok(EvaluatedSemanticConst::Module(module.clone())),
            DeclarationImportQueryValue::Available(crate::CanonicalImportResolution::Missing) => {
                Self::failure(format!("cannot find module `{}`", site.specifier()))
            }
            DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Ambiguous { .. },
            ) => Self::failure(format!("ambiguous module `{}`", site.specifier())),
            DeclarationImportQueryValue::Failure(
                crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(_),
            ) => Err(EvaluateSemanticConstError::Abort(QueryAbort::MissingInput(
                InputIdentity::new(
                    "declaration-import-resolution",
                    format!(
                        "{}:{}:{}",
                        key.declaration.stable_identity(),
                        key.occurrence,
                        key.specifier
                    ),
                ),
            ))),
            DeclarationImportQueryValue::Failure(failure) => Self::failure(format!("{failure:?}")),
        }
    }

    fn eval_identifier(
        &mut self,
        name: Arc<str>,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        if let Some(value) = self.locals.get(&name) {
            return Ok(value.clone());
        }
        if let Some(candidate) = self
            .provider
            .candidate(
                &self.declaration.declaration.module,
                &name,
                DefinitionKind::Const,
            )
            .map_err(Self::provider_error)?
        {
            let resolution = self
                .provider
                .const_resolution(candidate)
                .map_err(Self::provider_error)?;
            let key = match &resolution {
                crate::semantic_query_nucleus::ConstResolutionProjection::Value { key, .. }
                | crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                    key,
                    ..
                } => key.clone(),
            };
            self.provider.dependencies.insert(
                crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: self.provider.dependency_source.clone(),
                    kind: rue_air::DeclarationTypeDependencyKind::Body,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        key,
                    ),
                },
            );
            return Ok(match resolution {
                crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    value,
                    ty,
                    ..
                } => EvaluatedSemanticConst::Value(TypedSemanticConst::typed(*value, ty)),
                crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                    target,
                    ..
                } => EvaluatedSemanticConst::Module(target),
            });
        }
        if let Some(candidate) = self
            .provider
            .candidate(
                &self.declaration.declaration.module,
                &name,
                DefinitionKind::Function,
            )
            .map_err(Self::provider_error)?
        {
            let identity = self
                .provider
                .identity(candidate)
                .map_err(Self::provider_error)?;
            self.provider.dependencies.insert(
                crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: self.provider.dependency_source.clone(),
                    kind: rue_air::DeclarationTypeDependencyKind::Body,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        identity.key.clone(),
                    ),
                },
            );
            return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                crate::durable_semantics::DurableConstValue::Function(identity.key),
                crate::durable_semantics::DurableType::ComptimeType,
            )));
        }
        for kind in [DefinitionKind::Struct, DefinitionKind::Enum] {
            if let Some(candidate) = self
                .provider
                .candidate(&self.declaration.declaration.module, &name, kind)
                .map_err(Self::provider_error)?
            {
                let identity = self
                    .provider
                    .identity(candidate)
                    .map_err(Self::provider_error)?;
                self.provider.dependencies.insert(
                    crate::semantic_query_nucleus::SemanticDeclarationDependency {
                        source: self.provider.dependency_source.clone(),
                        kind: rue_air::DeclarationTypeDependencyKind::Body,
                        target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                            identity.key.clone(),
                        ),
                    },
                );
                return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                    crate::durable_semantics::DurableConstValue::Type(
                        crate::durable_semantics::DurableType::Nominal(identity.key),
                    ),
                    crate::durable_semantics::DurableType::ComptimeType,
                )));
            }
        }
        Self::failure(format!("undefined constant `{name}`"))
    }

    fn eval_call(
        &mut self,
        call: &rue_parser::ast::CallExpr,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        let module = self.declaration.declaration.module.clone();
        let name = self.symbol(&call.name.name);
        self.eval_named_call(&module, name, &call.args)
    }

    fn eval_named_call(
        &mut self,
        module: &ModuleId,
        name: Arc<str>,
        arguments: &[rue_parser::ast::CallArg],
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        use crate::semantic_query_nucleus::{
            ComptimeCallQueryKey, ComptimeCallResultProjection, SemanticNucleusKey,
            SemanticNucleusValue,
        };
        let call_ordinal = self.next_call;
        self.next_call += 1;
        let Some(candidate) = self
            .provider
            .candidate(module, &name, DefinitionKind::Function)
            .map_err(Self::provider_error)?
        else {
            return Self::failure(format!("undefined comptime function `{name}`"));
        };
        let identity = self
            .provider
            .identity(candidate.clone())
            .map_err(Self::provider_error)?;
        self.provider.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.provider.dependency_source.clone(),
                kind: rue_air::DeclarationTypeDependencyKind::Body,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                        identity.key,
                    ),
            },
        );
        let signature = self
            .provider
            .signature(candidate.clone())
            .map_err(Self::provider_error)?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            result,
            ..
        } = signature
        else {
            return Self::failure(format!("`{name}` is not callable"));
        };
        let shell = self
            .provider
            .context
            .query_registered(
                self.provider.shells,
                DeclarationShellQueryKey(candidate.clone()),
            )
            .map_err(EvaluateSemanticConstError::Abort)?;
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Self::failure("comptime call shell became unavailable");
        };
        if shell.parameters.len() != arguments.len() || parameters.len() != arguments.len() {
            return Self::failure(format!("comptime call `{name}` has the wrong arity"));
        }
        for (parameter, argument) in parameters.iter().zip(arguments) {
            use crate::durable_semantics::DurableParameterMode as ParameterMode;
            use rue_parser::ast::ArgMode;
            let failure = match (parameter.mode, argument.mode) {
                (ParameterMode::Value, ArgMode::Normal)
                | (ParameterMode::Borrow, ArgMode::Borrow)
                | (ParameterMode::Inout, ArgMode::Inout) => None,
                (ParameterMode::Inout, _) => Some(rue_error::ErrorKind::InoutKeywordMissing),
                (ParameterMode::Borrow, _) => Some(rue_error::ErrorKind::BorrowKeywordMissing),
                (ParameterMode::Value, ArgMode::Borrow) => {
                    Some(rue_error::ErrorKind::UnexpectedCallArgumentMode { mode: "borrow" })
                }
                (ParameterMode::Value, ArgMode::Inout) => {
                    Some(rue_error::ErrorKind::UnexpectedCallArgumentMode { mode: "inout" })
                }
            };
            if let Some(kind) = failure {
                return Err(EvaluateSemanticConstError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind),
                ));
            }
        }
        let all_parameters_comptime =
            !parameters.is_empty() && parameters.iter().all(|parameter| parameter.is_comptime);
        let is_type_function = result == crate::durable_semantics::DurableType::ComptimeType;
        let eligible = if is_type_function {
            parameters.is_empty() || all_parameters_comptime
        } else {
            all_parameters_comptime
        };
        if !eligible {
            return Err(EvaluateSemanticConstError::failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported {
                        expr_kind: format!("call to `{name}`"),
                    },
                ),
            ));
        }
        let mut type_arguments = Vec::new();
        let mut value_arguments = Vec::new();
        for ((header, parameter), argument) in shell
            .parameters
            .iter()
            .zip(parameters.iter())
            .zip(arguments.iter())
        {
            if parameter.ty == crate::durable_semantics::DurableType::ComptimeType {
                if matches!(
                    argument.expr,
                    rue_parser::ast::Expr::Int(_)
                        | rue_parser::ast::Expr::Bool(_)
                        | rue_parser::ast::Expr::String(_)
                ) {
                    return Self::failure(format!(
                        "argument for comptime parameter `{}` must be a type",
                        header.name
                    ));
                }
                let syntax = self
                    .source
                    .get(argument.expr.span().start as usize..argument.expr.span().end as usize)
                    .ok_or_else(|| {
                        Self::failure_value("comptime type argument has an invalid span")
                    })?;
                let ty = rue_air::resolve_semantic_type_syntax(
                    self.provider,
                    &self.declaration.declaration.module,
                    syntax,
                )
                .map_err(|error| match error {
                    rue_air::SemanticResolutionError::ProviderAbort(abort) => {
                        EvaluateSemanticConstError::Abort(abort)
                    }
                    rue_air::SemanticResolutionError::ProviderFailure(failure) => {
                        EvaluateSemanticConstError::failure(failure)
                    }
                    other => Self::failure_value(format!("{other:?}")),
                })?;
                type_arguments.push((header.name.clone(), ty));
            } else {
                let evaluated = self.eval(&argument.expr)?;
                let typed = self.value(evaluated)?;
                let value = typed.value;
                let concrete_type_arguments = type_arguments
                    .iter()
                    .map(|(_, ty)| ty.clone())
                    .collect::<Vec<_>>();
                let expected = substitute_durable_generics(&parameter.ty, &concrete_type_arguments);
                if let Some(found) = typed.ty
                    && found != expected
                {
                    return Err(EvaluateSemanticConstError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::TypeMismatch {
                                expected: durable_type_diagnostic_name(&expected),
                                found: durable_type_diagnostic_name(&found),
                            },
                        ),
                    ));
                }
                if !durable_const_fits_type(&value, &expected) {
                    if matches!(
                        &value,
                        crate::durable_semantics::DurableConstValue::Function(_)
                    ) {
                        return Self::failure(
                            "a callable alias cannot be passed as a comptime value argument",
                        );
                    }
                    if matches!(
                        &value,
                        crate::durable_semantics::DurableConstValue::Integer(_)
                    ) && matches!(
                        &expected,
                        crate::durable_semantics::DurableType::I8
                            | crate::durable_semantics::DurableType::I16
                            | crate::durable_semantics::DurableType::I32
                            | crate::durable_semantics::DurableType::I64
                            | crate::durable_semantics::DurableType::U8
                            | crate::durable_semantics::DurableType::U16
                            | crate::durable_semantics::DurableType::U32
                            | crate::durable_semantics::DurableType::U64
                    ) {
                        return Self::failure(format!(
                            "value {} is outside the range of type {}",
                            match &value {
                                crate::durable_semantics::DurableConstValue::Integer(value) =>
                                    value,
                                _ => unreachable!(),
                            },
                            durable_type_diagnostic_name(&expected),
                        ));
                    }
                    return Err(EvaluateSemanticConstError::failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::TypeMismatch {
                                expected: durable_type_diagnostic_name(&expected),
                                found: inferred_const_type_name(&value).to_owned(),
                            },
                        ),
                    ));
                }
                value_arguments.push((header.name.clone(), value));
            }
        }
        let query = SemanticNucleusKey::ComptimeCall(ComptimeCallQueryKey {
            declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: candidate,
                configuration: self.declaration.configuration.clone(),
            },
            type_arguments: type_arguments.into(),
            value_arguments: value_arguments.into(),
        });
        match self.provider.query(query).map_err(Self::provider_error)? {
            SemanticNucleusValue::ComptimeCall(value) => {
                self.provider.anonymous_nominals.extend(
                    value
                        .anonymous_nominals
                        .iter()
                        .cloned()
                        .map(|value| (value.identity.clone(), value)),
                );
                self.provider
                    .dependencies
                    .extend(value.dependencies.iter().cloned());
                self.provider.deferred_ownership.extend(
                    value.deferred_ownership.iter().cloned().map(|mut gate| {
                        if gate.application.is_none() {
                            gate.application = Some(
                                crate::semantic_query_nucleus::DeferredOwnershipApplication {
                                    declaration: self.declaration.declaration.clone(),
                                    call_ordinal,
                                },
                            );
                        }
                        gate
                    }),
                );
                Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                    match value.result {
                        ComptimeCallResultProjection::Type(value) => {
                            crate::durable_semantics::DurableConstValue::Type(value)
                        }
                        ComptimeCallResultProjection::Value(value) => value,
                    },
                    result,
                )))
            }
            SemanticNucleusValue::Failure(failure) => Err(Self::domain_failure(failure)),
            _ => Self::failure("comptime query returned the wrong projection"),
        }
    }

    /// Fail-closed E9000-class internal diagnostic for an anonymous-anchor
    /// transport disagreement. Never a panic and never a public error code; it
    /// is raised before any nominal/member terminal or alias can publish.
    fn anchor_transport_failure<T>(
        &self,
        message: String,
    ) -> Result<T, EvaluateSemanticConstError> {
        Err(EvaluateSemanticConstError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::InternalError(message),
            ),
        ))
    }

    /// The exact frontend anchor for the anonymous literal at `span`, copied from
    /// the transported table. There is no fallback: a missing locator, a
    /// duplicate locator, two frontend sites sharing one anchor under this
    /// producer, or a kind mismatch each fail closed with the producer and the
    /// expected/observed anchors named (RUE-1089).
    fn resolve_anonymous_anchor(
        &self,
        span: rue_span::Span,
        kind: rue_air::AnonymousNominalKind,
    ) -> Result<rue_rir::RirStructuralAnchor, EvaluateSemanticConstError> {
        let expected_kind = match kind {
            rue_air::AnonymousNominalKind::Struct => rue_rir::AnonymousTypeSiteKind::Struct,
            rue_air::AnonymousNominalKind::Enum => rue_rir::AnonymousTypeSiteKind::Enum,
        };
        // Test-only fault injection (RUE-1089 acceptance criterion 7). The mode
        // is selected entirely by a marker embedded in the fragment source, so it
        // affects only the one declaration under test and is race-free under
        // parallel test execution — no global state, no reset. It corrupts the
        // transported table exactly as a real anchor-transport bug would, so the
        // fail-closed path below is exercised without a real divergence.
        #[cfg(test)]
        let injected_sites: Vec<crate::semantic_query_nucleus::TransportedAnonymousSite>;
        #[cfg(test)]
        let sites: &[crate::semantic_query_nucleus::TransportedAnonymousSite] =
            if self.source.contains("__RUE1089_FAULT_MISSING__") {
                &[]
            } else if self.source.contains("__RUE1089_FAULT_DUPLICATE__") {
                injected_sites = self
                    .anonymous_sites
                    .iter()
                    .chain(self.anonymous_sites.iter())
                    .cloned()
                    .collect();
                &injected_sites
            } else if self.source.contains("__RUE1089_FAULT_WRONG_KIND__") {
                injected_sites = self
                    .anonymous_sites
                    .iter()
                    .map(|site| {
                        let mut site = site.clone();
                        site.kind = match site.kind {
                            rue_rir::AnonymousTypeSiteKind::Struct => {
                                rue_rir::AnonymousTypeSiteKind::Enum
                            }
                            rue_rir::AnonymousTypeSiteKind::Enum => {
                                rue_rir::AnonymousTypeSiteKind::Struct
                            }
                        };
                        site
                    })
                    .collect();
                &injected_sites
            } else {
                self.anonymous_sites
            };
        #[cfg(not(test))]
        let sites = self.anonymous_sites;
        // Whole-producer well-formedness: no two frontend sites may share a
        // locator or an anchor. Either is anchor-transport corruption.
        for (index, left) in sites.iter().enumerate() {
            for right in &sites[index + 1..] {
                if (left.span.start, left.span.end) == (right.span.start, right.span.end) {
                    return self.anchor_transport_failure(format!(
                        "anchor transport for producer {:?} carries a duplicate anonymous-type \
                         locator {}..{} (anchors {:?} and {:?})",
                        self.producer, left.span.start, left.span.end, left.anchor, right.anchor,
                    ));
                }
                if left.anchor == right.anchor {
                    return self.anchor_transport_failure(format!(
                        "anchor transport for producer {:?} carries two distinct anonymous sites \
                         with the same anchor {:?}",
                        self.producer, left.anchor,
                    ));
                }
            }
        }
        let mut matching = sites
            .iter()
            .filter(|site| (site.span.start, site.span.end) == (span.start, span.end));
        let Some(site) = matching.next() else {
            return self.anchor_transport_failure(format!(
                "anchor transport for producer {:?} has no anchor for the anonymous type at \
                 {}..{}",
                self.producer, span.start, span.end,
            ));
        };
        if matching.next().is_some() {
            return self.anchor_transport_failure(format!(
                "anchor transport for producer {:?} carries a duplicate locator for the anonymous \
                 type at {}..{}",
                self.producer, span.start, span.end,
            ));
        }
        if site.kind != expected_kind {
            return self.anchor_transport_failure(format!(
                "anchor transport for producer {:?} disagrees on the kind of the anonymous type at \
                 {}..{} (expected {expected_kind:?}, transported {:?}) at anchor {:?}",
                self.producer, span.start, span.end, site.kind, site.anchor,
            ));
        }
        // Test-only divergent-anchor injection (RUE-1089 acceptance criterion 7):
        // publish a WRONG-but-present anchor, reproducing the exact pre-fix
        // hazard where a reached member cannot match its owner terminal. The fix
        // is load-bearing — this must fail closed (loud E9000) downstream, never
        // miscompile.
        #[cfg(test)]
        if self.source.contains("__RUE1089_FAULT_DIVERGE__") {
            let mut segments = site.anchor.segments().to_vec();
            segments.push(rue_rir::RirStructuralPathSegment::AnonymousType(9999));
            return Ok(rue_rir::RirStructuralAnchor::new(segments));
        }
        Ok(site.anchor.clone())
    }

    fn eval_type_literal(
        &mut self,
        type_expr: &rue_parser::ast::TypeExpr,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        use crate::durable_semantics::{
            DurableAnonymousNominal, DurableAnonymousNominalShape, DurableConstValue as V,
            DurableType,
        };
        let resolve = |provider: &mut SemanticNucleusTypeProvider<'_>, syntax: &str| {
            rue_air::resolve_semantic_type_syntax(
                provider,
                &self.declaration.declaration.module,
                syntax,
            )
            .map_err(|error| match error {
                rue_air::SemanticResolutionError::ProviderAbort(abort) => {
                    EvaluateSemanticConstError::Abort(abort)
                }
                rue_air::SemanticResolutionError::ProviderFailure(failure) => {
                    EvaluateSemanticConstError::failure(failure)
                }
                other => Self::failure_value(format!("{other:?}")),
            })
        };
        let fragment = |span: rue_span::Span| {
            self.source
                .get(span.start as usize..span.end as usize)
                .ok_or_else(|| Self::failure_value("type literal span is invalid"))
        };
        let (kind, shape) = match type_expr {
            rue_parser::ast::TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                let fields = fields
                    .iter()
                    .map(|field| {
                        let name = Arc::from(self.interner.resolve(&field.name.name));
                        let syntax = fragment(field.ty.span())?;
                        Ok((name, resolve(self.provider, syntax)?))
                    })
                    .collect::<Result<Vec<_>, EvaluateSemanticConstError>>()?;
                let method_type =
                    |provider: &mut SemanticNucleusTypeProvider<'_>,
                     ty: &rue_parser::ast::TypeExpr| {
                        let syntax = fragment(ty.span())?;
                        Ok(if syntax.trim() == "Self" {
                            crate::durable_semantics::DurableAnonymousMethodType::SelfType
                        } else {
                            crate::durable_semantics::DurableAnonymousMethodType::Concrete(resolve(
                                provider, syntax,
                            )?)
                        })
                    };
                let mode = |mode: rue_parser::ast::ParamMode| match mode {
                    rue_parser::ast::ParamMode::Normal | rue_parser::ast::ParamMode::Comptime => {
                        crate::durable_semantics::DurableParameterMode::Value
                    }
                    rue_parser::ast::ParamMode::Borrow => {
                        crate::durable_semantics::DurableParameterMode::Borrow
                    }
                    rue_parser::ast::ParamMode::Inout => {
                        crate::durable_semantics::DurableParameterMode::Inout
                    }
                };
                let methods = methods
                    .iter()
                    .map(|method| {
                        let parameters = method
                            .params
                            .iter()
                            .map(|parameter| {
                                Ok((
                                    method_type(self.provider, &parameter.ty)?,
                                    mode(parameter.mode),
                                    parameter.mode == rue_parser::ast::ParamMode::Comptime,
                                ))
                            })
                            .collect::<Result<Vec<_>, EvaluateSemanticConstError>>()?;
                        let result = match &method.return_type {
                            Some(ty) => method_type(self.provider, ty)?,
                            None => crate::durable_semantics::DurableAnonymousMethodType::Concrete(
                                DurableType::Unit,
                            ),
                        };
                        Ok(crate::durable_semantics::DurableAnonymousMethodSignature {
                            name: Arc::from(self.interner.resolve(&method.name.name)),
                            has_self: method.receiver.is_some(),
                            self_mode: method.receiver.as_ref().map_or(
                                crate::durable_semantics::DurableParameterMode::Value,
                                |receiver| mode(receiver.mode),
                            ),
                            parameters: parameters.into(),
                            result,
                        })
                    })
                    .collect::<Result<Vec<_>, EvaluateSemanticConstError>>()?;
                (
                    rue_air::AnonymousNominalKind::Struct,
                    DurableAnonymousNominalShape::Struct {
                        fields: fields.into(),
                        methods: methods.into(),
                    },
                )
            }
            rue_parser::ast::TypeExpr::AnonymousEnum { variants, .. } => {
                let variants = variants
                    .iter()
                    .map(|variant| {
                        let name = Arc::from(self.interner.resolve(&variant.name.name));
                        let payload = variant
                            .payload
                            .iter()
                            .map(|ty| {
                                let syntax = fragment(ty.span())?;
                                resolve(self.provider, syntax)
                            })
                            .collect::<Result<Vec<_>, EvaluateSemanticConstError>>()?;
                        Ok((name, Arc::from(payload)))
                    })
                    .collect::<Result<Vec<_>, EvaluateSemanticConstError>>()?;
                (
                    rue_air::AnonymousNominalKind::Enum,
                    DurableAnonymousNominalShape::Enum {
                        variants: variants.into(),
                    },
                )
            }
            _ => {
                let syntax = fragment(type_expr.span())?;
                return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                    V::Type(resolve(self.provider, syntax)?),
                    DurableType::ComptimeType,
                )));
            }
        };
        let anchor = self.resolve_anonymous_anchor(type_expr.span(), kind)?;
        let identity = crate::AnonymousNominalKey {
            kind,
            producer: self.producer.clone(),
            anchor,
            arguments: self.canonical_arguments.clone(),
        };
        self.provider.anonymous_nominals.insert(
            identity.clone(),
            DurableAnonymousNominal {
                identity: identity.clone(),
                shape,
                type_captures: self
                    .provider
                    .substitutions
                    .iter()
                    .map(|(name, ty)| (name.clone(), ty.clone()))
                    .collect::<Vec<_>>()
                    .into(),
                value_captures: self
                    .provider
                    .value_substitutions
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>()
                    .into(),
            },
        );
        Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
            V::Type(DurableType::AnonymousNominal(identity)),
            DurableType::ComptimeType,
        )))
    }

    fn eval(
        &mut self,
        expression: &rue_parser::ast::Expr,
    ) -> Result<EvaluatedSemanticConst, EvaluateSemanticConstError> {
        use crate::durable_semantics::DurableConstValue as V;
        use rue_parser::ast::Expr as E;
        match expression {
            E::Int(value) => Ok(EvaluatedSemanticConst::Value(
                TypedSemanticConst::integer_literal(value.value as i128),
            )),
            E::String(value) => Ok(EvaluatedSemanticConst::Value(Arc::new(
                TypedSemanticConst {
                    value: V::String(self.symbol(&value.value)),
                    ty: None,
                },
            ))),
            E::Bool(value) => Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Bool(value.value),
                crate::durable_semantics::DurableType::Bool,
            ))),
            E::Unit(_) => Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                V::Unit,
                crate::durable_semantics::DurableType::Unit,
            ))),
            E::Ident(name) => self.eval_identifier(self.symbol(&name.name)),
            E::Call(call) => self.eval_call(call),
            E::MethodCall(call) => {
                let EvaluatedSemanticConst::Module(module) = self.eval(&call.receiver)? else {
                    return Self::failure(
                        "method call in declaration-time comptime requires a module receiver",
                    );
                };
                self.eval_named_call(&module, self.symbol(&call.method.name), &call.args)
            }
            E::Binary(binary) => self.eval_binary(binary),
            E::Unary(unary) => match unary.op {
                rue_parser::ast::UnaryOp::Not => {
                    Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        V::Bool(!self.bool_value(&unary.operand)?),
                        crate::durable_semantics::DurableType::Bool,
                    )))
                }
                op => {
                    let (operand, ty) = self.int_value(&unary.operand)?;
                    let ty = self.integer_type(ty, None)?;
                    let result = match op {
                        rue_parser::ast::UnaryOp::Neg => {
                            operand.checked_neg().ok_or_else(|| {
                                Self::comptime_failure_value("integer overflow evaluating negation")
                            })?
                        }
                        rue_parser::ast::UnaryOp::BitNot => !operand,
                        rue_parser::ast::UnaryOp::Not => unreachable!(),
                    };
                    Self::require_integer_fits(&ty, result)?;
                    Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        V::Integer(result),
                        ty,
                    )))
                }
            },
            E::Paren(value) => self.eval(&value.inner),
            E::Block(block) => self.eval_block(block),
            E::If(value) => {
                if self.bool_value(&value.cond)? {
                    self.eval_block(&value.then_block)
                } else if let Some(block) = &value.else_block {
                    self.eval_block(block)
                } else {
                    Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        V::Unit,
                        crate::durable_semantics::DurableType::Unit,
                    )))
                }
            }
            E::Match(value) => {
                let evaluated = self.eval(&value.scrutinee)?;
                for arm in &value.arms {
                    let matches = match (&arm.pattern, &evaluated) {
                        (rue_parser::ast::Pattern::Wildcard(_), _) => true,
                        (
                            rue_parser::ast::Pattern::Int(pattern),
                            EvaluatedSemanticConst::Value(value),
                        ) => {
                            matches!(&value.value, V::Integer(value) if *value == pattern.value as i128)
                        }
                        (
                            rue_parser::ast::Pattern::NegInt(pattern),
                            EvaluatedSemanticConst::Value(value),
                        ) => {
                            matches!(&value.value, V::Integer(value) if *value == -(pattern.value as i128))
                        }
                        (
                            rue_parser::ast::Pattern::Bool(pattern),
                            EvaluatedSemanticConst::Value(value),
                        ) => {
                            matches!(&value.value, V::Bool(value) if *value == pattern.value)
                        }
                        (
                            rue_parser::ast::Pattern::Path(pattern),
                            EvaluatedSemanticConst::TargetEnum(target),
                        ) if pattern.base.is_none() && pattern.bindings.is_empty() => {
                            self.interner.resolve(&pattern.type_name.name) == target.type_name
                                && self.interner.resolve(&pattern.variant.name) == target.variant
                        }
                        _ => false,
                    };
                    if matches {
                        return self.eval(&arm.body);
                    }
                }
                Self::failure("comptime match has no selected arm")
            }
            E::Comptime(value) => self.eval(&value.expr),
            E::Checked(value) => self.eval(&value.expr),
            E::TypeLit(value) => self.eval_type_literal(&value.type_expr),
            E::IntrinsicCall(intrinsic)
                if self.symbol(&intrinsic.name.name).as_ref() == "import" =>
            {
                self.eval_import(intrinsic)
            }
            E::IntrinsicCall(intrinsic)
                if self.symbol(&intrinsic.name.name).as_ref() == "target_arch" =>
            {
                let variant = match self.declaration.configuration.target.arch() {
                    rue_target::Arch::X86_64 => "X86_64",
                    rue_target::Arch::Aarch64 => "Aarch64",
                };
                self.target_intrinsic(intrinsic, "Arch", variant)
            }
            E::IntrinsicCall(intrinsic)
                if self.symbol(&intrinsic.name.name).as_ref() == "target_os" =>
            {
                let variant = match self.declaration.configuration.target.os() {
                    rue_target::Os::Linux => "Linux",
                    rue_target::Os::Macos => "Macos",
                };
                self.target_intrinsic(intrinsic, "Os", variant)
            }
            E::IntrinsicCall(intrinsic)
                if self.symbol(&intrinsic.name.name).as_ref() == "target_data_model" =>
            {
                let variant = match self.declaration.configuration.target.data_model() {
                    rue_target::DataModel::Ilp32 => "Ilp32",
                    rue_target::DataModel::Lp64 => "Lp64",
                    rue_target::DataModel::Llp64 => "Llp64",
                };
                self.target_intrinsic(intrinsic, "DataModel", variant)
            }
            E::IntrinsicCall(intrinsic)
                if matches!(
                    self.symbol(&intrinsic.name.name).as_ref(),
                    "require_droppable" | "require_trivially_droppable"
                ) =>
            {
                let intrinsic_name = self.symbol(&intrinsic.name.name);
                let [rue_parser::ast::IntrinsicArg::Type(ty)] = intrinsic.args.as_slice() else {
                    return Self::failure(format!("@{intrinsic_name} expects one type argument"));
                };
                let evaluated = self.eval_type_literal(ty)?;
                let crate::durable_semantics::DurableConstValue::Type(ty) =
                    self.value(evaluated)?.value
                else {
                    return Self::failure(format!("@{intrinsic_name} argument is not a type"));
                };
                // Ownership is a post-signature well-formedness fact. Publishing
                // the gate instead of inspecting nominal signatures here lets a
                // recursive but indirect type graph finish before the keyed
                // ownership query validates it.
                self.provider.deferred_ownership.insert(
                    crate::semantic_query_nucleus::DeferredOwnershipGate {
                        kind: if intrinsic_name.as_ref() == "require_droppable" {
                            crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable
                        } else {
                            crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireTriviallyDroppable
                        },
                        ty,
                        application: None,
                    },
                );
                Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                    V::Unit,
                    crate::durable_semantics::DurableType::Unit,
                )))
            }
            E::Path(path) if path.base.is_none() => {
                let type_name = self.interner.resolve(&path.type_name.name);
                if matches!(type_name, "Arch" | "Os" | "DataModel") {
                    self.target_enum_variant(type_name, self.interner.resolve(&path.variant.name))
                } else {
                    Self::failure("path expression is not supported in declaration-time comptime")
                }
            }
            E::Field(field) => {
                if let E::Ident(base) = field.base.as_ref() {
                    let type_name = self.interner.resolve(&base.name);
                    if matches!(type_name, "Arch" | "Os" | "DataModel") {
                        return self.target_enum_variant(
                            type_name,
                            self.interner.resolve(&field.field.name),
                        );
                    }
                }
                let EvaluatedSemanticConst::Module(module) = self.eval(&field.base)? else {
                    return Self::failure("member access on a non-module const value");
                };
                let name = self.symbol(&field.field.name);
                if let Some(candidate) = self
                    .provider
                    .candidate(&module, &name, DefinitionKind::Const)
                    .map_err(Self::provider_error)?
                {
                    let resolution = self
                        .provider
                        .const_resolution(candidate)
                        .map_err(Self::provider_error)?;
                    let key = match &resolution {
                        crate::semantic_query_nucleus::ConstResolutionProjection::Value { key, .. }
                        | crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                            key,
                            ..
                        } => key.clone(),
                    };
                    self.provider.dependencies.insert(
                        crate::semantic_query_nucleus::SemanticDeclarationDependency {
                            source: self.provider.dependency_source.clone(),
                            kind: rue_air::DeclarationTypeDependencyKind::Body,
                            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                                key,
                            ),
                        },
                    );
                    return Ok(match resolution {
                        crate::semantic_query_nucleus::ConstResolutionProjection::Value { value, ty, .. } => EvaluatedSemanticConst::Value(TypedSemanticConst::typed(*value, ty)),
                        crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding { target, .. } => EvaluatedSemanticConst::Module(target),
                    });
                }
                for kind in [DefinitionKind::Struct, DefinitionKind::Enum] {
                    if let Some(candidate) = self
                        .provider
                        .candidate(&module, &name, kind)
                        .map_err(Self::provider_error)?
                    {
                        let identity = self
                            .provider
                            .identity(candidate)
                            .map_err(Self::provider_error)?;
                        self.provider.dependencies.insert(
                            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                                source: self.provider.dependency_source.clone(),
                                kind: rue_air::DeclarationTypeDependencyKind::Body,
                                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                                    identity.key.clone(),
                                ),
                            },
                        );
                        return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                            V::Type(crate::durable_semantics::DurableType::Nominal(identity.key)),
                            crate::durable_semantics::DurableType::ComptimeType,
                        )));
                    }
                }
                if let Some(candidate) = self
                    .provider
                    .candidate(&module, &name, DefinitionKind::Function)
                    .map_err(Self::provider_error)?
                {
                    let identity = self
                        .provider
                        .identity(candidate)
                        .map_err(Self::provider_error)?;
                    self.provider.dependencies.insert(
                        crate::semantic_query_nucleus::SemanticDeclarationDependency {
                            source: self.provider.dependency_source.clone(),
                            kind: rue_air::DeclarationTypeDependencyKind::Body,
                            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedValue(
                                identity.key.clone(),
                            ),
                        },
                    );
                    return Ok(EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
                        V::Function(identity.key),
                        crate::durable_semantics::DurableType::ComptimeType,
                    )));
                }
                Err(EvaluateSemanticConstError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        rue_error::ErrorKind::UnknownModuleMember {
                            module_name: module.to_string(),
                            member_name: name.to_string(),
                        },
                    ),
                ))
            }
            E::StructLit(_) | E::ArrayLit(_) => Err(Self::domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported {
                        expr_kind: "aggregate expression".to_owned(),
                    },
                ),
            )),
            E::IntrinsicCall(intrinsic) => Err(Self::domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ConstExprNotSupported {
                        expr_kind: format!(
                            "intrinsic `@{}`",
                            self.interner.resolve(&intrinsic.name.name)
                        ),
                    },
                ),
            )),
            _ => Self::failure("expression is not supported in declaration-time comptime"),
        }
    }
}

impl SemanticNucleusTypeProvider<'_> {
    fn ffi_shape_failure(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        path: &mut Vec<String>,
    ) -> Result<
        Option<(
            rue_air::FfiRejectReason,
            Vec<String>,
            crate::durable_semantics::DurableType,
        )>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::DurableType as T;
        use rue_air::FfiRejectReason as R;
        match ty {
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::PtrConst(_)
            | T::PtrMut(_) => Ok(None),
            T::Array { element, .. } => self.ffi_shape_failure(element, path),
            T::Nominal(key) if key.kind() == crate::StableDefinitionKind::Enum => {
                Ok(Some((R::Enum, path.clone(), ty.clone())))
            }
            T::Nominal(key) if key.kind() == crate::StableDefinitionKind::Struct => {
                let Some(candidate) =
                    self.candidate(key.module(), key.name(), DefinitionKind::Struct)?
                else {
                    return Self::provider_failure(format!(
                        "FFI struct `{}` is unavailable",
                        key.name()
                    ));
                };
                let signature = self.signature(candidate)?;
                let crate::semantic_query_nucleus::DeclarationSignatureProjection::Struct {
                    fields,
                    is_linear,
                    is_repr_c,
                    ..
                } = signature
                else {
                    return Self::provider_failure("FFI nominal has the wrong signature kind");
                };
                if !is_repr_c {
                    return Ok(Some((R::NonReprCAggregate, path.clone(), ty.clone())));
                }
                if fields.is_empty() {
                    return Ok(Some((R::EmptyStruct, path.clone(), ty.clone())));
                }
                if is_linear {
                    return Ok(Some((R::Linear, path.clone(), ty.clone())));
                }
                if self
                    .candidate(key.module(), key.name(), DefinitionKind::Destructor)?
                    .is_some()
                {
                    return Ok(Some((R::HasDestructor, path.clone(), ty.clone())));
                }
                for (name, field) in fields.iter() {
                    path.push(name.to_string());
                    if let Some(failure) = self.ffi_shape_failure(field, path)? {
                        return Ok(Some(failure));
                    }
                    path.pop();
                }
                Ok(None)
            }
            T::AnonymousNominal(_)
            | T::Slice { .. }
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => Ok(Some((R::UnsupportedType, path.clone(), ty.clone()))),
            T::Nominal(_) => Ok(Some((R::UnsupportedType, path.clone(), ty.clone()))),
        }
    }

    fn repr_c_failure_for_fields(
        &mut self,
        fields: &[(Arc<str>, crate::durable_semantics::DurableType)],
        is_linear: bool,
        has_destructor: bool,
    ) -> Result<
        Option<(
            rue_air::FfiRejectReason,
            Vec<String>,
            crate::durable_semantics::DurableType,
        )>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use rue_air::FfiRejectReason as R;
        if fields.is_empty() {
            return Ok(Some((
                R::EmptyStruct,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        if is_linear {
            return Ok(Some((
                R::Linear,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        if has_destructor {
            return Ok(Some((
                R::HasDestructor,
                Vec::new(),
                crate::durable_semantics::DurableType::Unit,
            )));
        }
        let mut path = Vec::new();
        for (name, ty) in fields {
            path.push(name.to_string());
            if let Some(failure) = self.ffi_shape_failure(ty, &mut path)? {
                return Ok(Some(failure));
            }
            path.pop();
        }
        Ok(None)
    }

    fn provider_failure_value(
        message: impl Into<Arc<str>>,
    ) -> rue_air::SemanticProviderError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        rue_air::SemanticProviderError::Failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(message.into()),
        )
    }

    fn provider_failure<T>(
        message: impl Into<Arc<str>>,
    ) -> Result<
        T,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Err(Self::provider_failure_value(message))
    }

    fn provider_domain_failure<T>(
        failure: crate::semantic_query_nucleus::SemanticNucleusFailure,
    ) -> Result<
        T,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Err(rue_air::SemanticProviderError::Failure(failure))
    }

    fn type_carries_linear(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        match self.type_carries_linear_inner(ty, &mut BTreeSet::new())? {
            LinearOwnershipFact::DoesNotCarry => Ok(false),
            LinearOwnershipFact::Carries => Ok(true),
            LinearOwnershipFact::Deferred => Ok(false),
        }
    }

    fn type_carries_linear_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        visiting: &mut BTreeSet<StableDefinitionKey>,
    ) -> Result<
        LinearOwnershipFact,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        match ty {
            T::Array { len: 0, .. } => Ok(LinearOwnershipFact::DoesNotCarry),
            T::Array { element, .. } => self.type_carries_linear_inner(element, visiting),
            T::Nominal(key) => {
                if !visiting.insert(key.clone()) {
                    return Ok(LinearOwnershipFact::DoesNotCarry);
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        visiting.remove(key);
                        return Self::provider_failure(format!(
                            "non-nominal definition `{}` used as a nominal type",
                            key.name()
                        ));
                    }
                };
                let candidate =
                    self.candidate(key.module(), key.name(), kind)?
                        .ok_or_else(|| {
                            Self::provider_failure_value(format!(
                                "nominal definition `{}` is unavailable",
                                key.name()
                            ))
                        })?;
                let signature_query = crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                    self.declaration_query(candidate.clone()),
                );
                let resolved = match self.resolved_signature(candidate) {
                    Ok(signature) => signature,
                    Err(rue_air::SemanticProviderError::Failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::SignatureReentry {
                            signature,
                            ..
                        },
                    )) if signature == *key => {
                        visiting.remove(key);
                        return Ok(LinearOwnershipFact::Deferred);
                    }
                    Err(rue_air::SemanticProviderError::Abort(QueryAbort::Cycle(nodes)))
                        if nodes.iter().any(|node| {
                            node.family() == "compiler.semantic-nucleus"
                                && node.key() == signature_query.stable_identity()
                        }) =>
                    {
                        visiting.remove(key);
                        return Ok(LinearOwnershipFact::Deferred);
                    }
                    Err(error) => {
                        visiting.remove(key);
                        return Err(error);
                    }
                };
                self.anonymous_nominals.extend(
                    resolved
                        .anonymous_nominals
                        .iter()
                        .cloned()
                        .map(|nominal| (nominal.identity.clone(), nominal)),
                );
                let signature = resolved.signature;
                let carries = match signature {
                    P::Struct {
                        fields, is_linear, ..
                    } => {
                        let mut carries = if is_linear {
                            LinearOwnershipFact::Carries
                        } else {
                            LinearOwnershipFact::DoesNotCarry
                        };
                        for (_, field) in fields.iter() {
                            carries =
                                carries.combine(self.type_carries_linear_inner(field, visiting)?);
                        }
                        carries
                    }
                    P::Enum { variants } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                carries = carries
                                    .combine(self.type_carries_linear_inner(field, visiting)?);
                            }
                        }
                        carries
                    }
                    _ => {
                        visiting.remove(key);
                        return Self::provider_failure(format!(
                            "nominal definition `{}` has a non-nominal signature",
                            key.name()
                        ));
                    }
                };
                visiting.remove(key);
                Ok(carries)
            }
            T::AnonymousNominal(key) => {
                let Some(nominal) = self.anonymous_nominals.get(key).cloned() else {
                    return Self::provider_failure(
                        "anonymous nominal is unavailable while checking linearity",
                    );
                };
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, field) in fields.iter() {
                            carries =
                                carries.combine(self.type_carries_linear_inner(field, visiting)?);
                        }
                        Ok(carries)
                    }
                    S::Enum { variants } => {
                        let mut carries = LinearOwnershipFact::DoesNotCarry;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                carries = carries
                                    .combine(self.type_carries_linear_inner(field, visiting)?);
                            }
                        }
                        Ok(carries)
                    }
                }
            }
            T::Slice { .. } | T::PtrConst(_) | T::PtrMut(_) => {
                Ok(LinearOwnershipFact::DoesNotCarry)
            }
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::BuiltinNominal { .. }
            | T::Module(_)
            | T::GenericParameter(_) => Ok(LinearOwnershipFact::DoesNotCarry),
        }
    }

    fn type_has_drop_glue(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.type_has_drop_glue_inner(ty, &mut BTreeSet::new())
    }

    fn type_has_drop_glue_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        visiting: &mut BTreeSet<StableDefinitionKey>,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;
        match ty {
            T::Array { len: 0, .. } => Ok(false),
            T::Array { element, .. } => self.type_has_drop_glue_inner(element, visiting),
            T::Nominal(key) => {
                if !visiting.insert(key.clone()) {
                    return Ok(false);
                }
                if key.kind() == crate::StableDefinitionKind::Struct {
                    let destructors = self
                        .context
                        .query_registered(
                            self.names,
                            LookupNameKey {
                                module: key.module().clone(),
                                namespace: DefinitionNamespace::Destructor,
                                name: Arc::from(key.name()),
                            },
                        )
                        .map_err(rue_air::SemanticProviderError::Abort)?;
                    let rue_query::QueryOutcome::Success(LookupNameValue(destructors)) =
                        destructors.outcome()
                    else {
                        unreachable!("LookupName publishes typed values")
                    };
                    if destructors.as_ref().is_ok_and(|facts| !facts.is_empty()) {
                        visiting.remove(key);
                        return Ok(true);
                    }
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        visiting.remove(key);
                        return Ok(false);
                    }
                };
                let candidate = self
                    .candidate(key.module(), key.name(), kind)?
                    .ok_or_else(|| Self::provider_failure_value("nominal type is unavailable"))?;
                let signature = self.resolved_signature(candidate)?.signature;
                let has_glue = match signature {
                    P::Struct { fields, .. } => {
                        let mut has_glue = false;
                        for (_, field) in fields.iter() {
                            has_glue |= self.type_has_drop_glue_inner(field, visiting)?;
                        }
                        has_glue
                    }
                    P::Enum { variants } => {
                        let mut has_glue = false;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                has_glue |= self.type_has_drop_glue_inner(field, visiting)?;
                            }
                        }
                        has_glue
                    }
                    _ => false,
                };
                visiting.remove(key);
                Ok(has_glue)
            }
            T::AnonymousNominal(key) => {
                let nominal = self.anonymous_nominals.get(key).cloned().ok_or_else(|| {
                    Self::provider_failure_value(
                        "anonymous nominal is unavailable while checking drop glue",
                    )
                })?;
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            if self.type_has_drop_glue_inner(field, visiting)? {
                                return Ok(true);
                            }
                        }
                    }
                    S::Enum { variants } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                if self.type_has_drop_glue_inner(field, visiting)? {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
                Ok(false)
            }
            T::GenericParameter { .. } => Self::provider_failure(
                "generic parameter remained unresolved while checking drop glue",
            ),
            _ => Ok(false),
        }
    }

    fn type_is_copy(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        self.type_is_copy_inner(ty, &mut BTreeSet::new())
    }

    fn type_is_copy_inner(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
        visiting: &mut BTreeSet<StableDefinitionKey>,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        match ty {
            T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::Bool
            | T::Unit
            | T::Never
            | T::ComptimeType
            | T::PtrConst(_)
            | T::PtrMut(_)
            | T::Module(_)
            | T::Slice { .. }
            | T::BuiltinNominal { .. } => Ok(true),
            T::GenericParameter(_) => {
                Self::provider_failure("unsubstituted generic parameter reached Copy validation")
            }
            T::Array { element, .. } => self.type_is_copy_inner(element, visiting),
            T::Nominal(key) => {
                if !visiting.insert(key.clone()) {
                    return Ok(true);
                }
                let kind = match key.kind() {
                    crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                    crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                    _ => {
                        visiting.remove(key);
                        return Self::provider_failure(format!(
                            "non-nominal definition `{}` used as a nominal type",
                            key.name()
                        ));
                    }
                };
                let candidate =
                    self.candidate(key.module(), key.name(), kind)?
                        .ok_or_else(|| {
                            Self::provider_failure_value(format!(
                                "nominal definition `{}` is unavailable",
                                key.name()
                            ))
                        })?;
                let resolved = self.resolved_signature(candidate)?;
                self.anonymous_nominals.extend(
                    resolved
                        .anonymous_nominals
                        .iter()
                        .cloned()
                        .map(|nominal| (nominal.identity.clone(), nominal)),
                );
                let is_copy = match resolved.signature {
                    P::Struct { is_copy, .. } => is_copy,
                    P::Enum { variants } => {
                        let mut is_copy = true;
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                is_copy &= self.type_is_copy_inner(field, visiting)?;
                            }
                        }
                        is_copy
                    }
                    _ => {
                        visiting.remove(key);
                        return Self::provider_failure(format!(
                            "nominal definition `{}` has a non-nominal signature",
                            key.name()
                        ));
                    }
                };
                visiting.remove(key);
                Ok(is_copy)
            }
            T::AnonymousNominal(key) => {
                let nominal = self.anonymous_nominals.get(key).cloned().ok_or_else(|| {
                    Self::provider_failure_value(
                        "anonymous nominal is unavailable while checking Copy",
                    )
                })?;
                match nominal.shape {
                    S::Struct { fields, .. } => {
                        for (_, field) in fields.iter() {
                            if !self.type_is_copy_inner(field, visiting)? {
                                return Ok(false);
                            }
                        }
                    }
                    S::Enum { variants } => {
                        for (_, payload) in variants.iter() {
                            for field in payload.iter() {
                                if !self.type_is_copy_inner(field, visiting)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                Ok(true)
            }
        }
    }

    fn candidate(
        &self,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<crate::declaration_candidate::DeclarationCandidateKey>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let terminal = self
            .context
            .query_registered(
                self.names,
                LookupNameKey {
                    module: module.clone(),
                    namespace: if kind == DefinitionKind::Destructor {
                        DefinitionNamespace::Destructor
                    } else {
                        DefinitionNamespace::ModuleItem
                    },
                    name: Arc::from(name),
                },
            )
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(LookupNameValue(result)) = terminal.outcome() else {
            unreachable!("LookupName publishes typed values")
        };
        let entries = result
            .as_ref()
            .map_err(|failure| Self::provider_failure_value(format!("{failure:?}")))?;
        let mut matching = entries.iter().filter(|entry| entry.kind == kind);
        let Some(entry) = matching.next() else {
            return Ok(None);
        };
        if matching.next().is_some() {
            return Self::provider_failure(format!(
                "ambiguous declaration `{name}` in module {module}"
            ));
        }
        let defining = rue_air::SemanticVisibilityDomain::from_file_path(Some(module.as_str()));
        let accessing = rue_air::SemanticVisibilityDomain::from_file_path(Some(
            self.dependency_source.module().as_str(),
        ));
        let is_public = entry.visibility == Some(rue_parser::ast::Visibility::Public);
        if !defining.is_visible_from(&accessing, is_public) {
            return Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::PrivateMemberAccess {
                        item_kind: format!("{kind:?}").to_lowercase(),
                        name: name.to_owned(),
                    },
                ),
            );
        }
        let categories: &[crate::declaration_candidate::DeclarationCandidateCategory] = match kind {
            DefinitionKind::Function => &[
                crate::declaration_candidate::DeclarationCandidateCategory::Function,
                crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction,
            ],
            DefinitionKind::Struct => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Struct]
            }
            DefinitionKind::Enum => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Enum]
            }
            DefinitionKind::Const => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate]
            }
            DefinitionKind::Destructor => {
                &[crate::declaration_candidate::DeclarationCandidateCategory::Destructor]
            }
        };
        for category in categories {
            let key = crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category: *category,
                name: entry.name.clone(),
                owner: (*category
                    == crate::declaration_candidate::DeclarationCandidateCategory::Destructor)
                    .then(|| crate::declaration_candidate::DeclarationCandidateOwner {
                        category:
                            crate::declaration_candidate::DeclarationCandidateCategory::Struct,
                        name: entry.name.clone(),
                    }),
                duplicate_discriminator: 0,
            };
            let shell = self
                .context
                .query_registered(self.shells, DeclarationShellQueryKey(key.clone()))
                .map_err(rue_air::SemanticProviderError::Abort)?;
            let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
                unreachable!("DeclarationShell publishes typed values")
            };
            if matches!(shell, DeclarationShellQueryValue::Available(_)) {
                return Ok(Some(key));
            }
        }
        Self::provider_failure(format!(
            "name index and declaration-shell index disagree for `{name}`"
        ))
    }

    fn query(
        &self,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> Result<
        crate::semantic_query_nucleus::SemanticNucleusValue,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let terminal = self
            .context
            .query_registered(self.family, key)
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("SemanticNucleus publishes typed values")
        };
        Ok(value.clone())
    }

    fn declaration_query(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
        crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: self.configuration.clone(),
        }
    }

    fn identity(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationIdentityProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::Identity(self.declaration_query(declaration)))? {
            V::Identity(identity) => Ok(identity),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("identity query returned the wrong projection"),
        }
    }

    fn const_resolution(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ConstResolutionProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::ConstResolution(self.declaration_query(declaration)))? {
            V::ConstResolution(value) => Ok(value),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("const query returned the wrong projection"),
        }
    }

    fn signature(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::DeclarationSignatureProjection,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        Ok(self.resolved_signature(declaration)?.signature)
    }

    fn resolved_signature(
        &self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        crate::semantic_query_nucleus::ResolvedDeclarationSignature,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::{SemanticNucleusKey as K, SemanticNucleusValue as V};
        match self.query(K::Signature(self.declaration_query(declaration)))? {
            V::Signature(value) => Ok(value),
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("signature query returned the wrong projection"),
        }
    }

    fn validate_nominal_well_formedness(
        &mut self,
        declaration: crate::declaration_candidate::DeclarationCandidateKey,
    ) -> Result<
        (),
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::durable_semantics::{DurableAnonymousNominalShape as S, DurableType as T};
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as P;

        fn collect_type(
            ty: &T,
            anonymous: &BTreeMap<
                crate::AnonymousNominalKey,
                crate::durable_semantics::DurableAnonymousNominal,
            >,
            neighbors: &mut BTreeSet<StableDefinitionKey>,
        ) {
            let mut pending = vec![ty];
            let mut seen_anonymous = BTreeSet::new();
            while let Some(ty) = pending.pop() {
                match ty {
                    T::Nominal(key) => {
                        neighbors.insert(key.clone());
                    }
                    // Arrays are inline containment edges even at length zero.
                    T::Array { element, .. } => pending.push(element),
                    T::AnonymousNominal(key) if seen_anonymous.insert(key.clone()) => {
                        if let Some(nominal) = anonymous.get(key) {
                            match &nominal.shape {
                                S::Struct { fields, .. } => {
                                    pending.extend(fields.iter().map(|(_, ty)| ty));
                                }
                                S::Enum { variants } => {
                                    pending.extend(
                                        variants.iter().flat_map(|(_, payload)| payload.iter()),
                                    );
                                }
                            }
                        }
                    }
                    // Pointers and slices are indirection and therefore break
                    // the by-value containment graph.
                    T::PtrConst(_) | T::PtrMut(_) | T::Slice { .. } => {}
                    _ => {}
                }
            }
        }

        let root = self.identity(declaration.clone())?.key;
        if declaration.category
            == crate::declaration_candidate::DeclarationCandidateCategory::Struct
            && matches!(
                self.signature(declaration.clone())?,
                P::Struct { is_copy: true, .. }
            )
            && self
                .candidate(
                    &declaration.module,
                    &declaration.name,
                    DefinitionKind::Destructor,
                )?
                .is_some()
        {
            return Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::CopyStructWithDestructor {
                        type_name: declaration.name.to_string(),
                    },
                ),
            );
        }
        let mut colors = BTreeMap::<StableDefinitionKey, u8>::new();
        let mut path = vec![root.clone()];
        let mut frames = Vec::<(StableDefinitionKey, Vec<StableDefinitionKey>, usize)>::new();

        let load = |provider: &mut Self,
                    key: &StableDefinitionKey|
         -> Result<
            Vec<StableDefinitionKey>,
            rue_air::SemanticProviderError<
                QueryAbort,
                crate::semantic_query_nucleus::SemanticNucleusFailure,
            >,
        > {
            let kind = match key.kind() {
                crate::StableDefinitionKind::Struct => DefinitionKind::Struct,
                crate::StableDefinitionKind::Enum => DefinitionKind::Enum,
                _ => return Ok(Vec::new()),
            };
            let Some(candidate) = provider.candidate(key.module(), key.name(), kind)? else {
                return Self::provider_failure(format!(
                    "nominal definition `{}` is unavailable",
                    key.name()
                ));
            };
            let resolved = provider.resolved_signature(candidate)?;
            let anonymous = resolved
                .anonymous_nominals
                .iter()
                .cloned()
                .map(|nominal| (nominal.identity.clone(), nominal))
                .collect::<BTreeMap<_, _>>();
            let mut neighbors = BTreeSet::new();
            match &resolved.signature {
                P::Struct { fields, .. } => {
                    for (_, ty) in fields.iter() {
                        collect_type(ty, &anonymous, &mut neighbors);
                    }
                }
                P::Enum { variants } => {
                    for (_, payload) in variants.iter() {
                        for ty in payload.iter() {
                            collect_type(ty, &anonymous, &mut neighbors);
                        }
                    }
                }
                _ => {
                    return Self::provider_failure(format!(
                        "nominal definition `{}` has a non-nominal signature",
                        key.name()
                    ));
                }
            }
            Ok(neighbors.into_iter().collect())
        };

        colors.insert(root.clone(), 1);
        frames.push((root.clone(), load(self, &root)?, 0));
        while let Some((key, neighbors, next)) = frames.last_mut() {
            if *next == neighbors.len() {
                colors.insert(key.clone(), 2);
                frames.pop();
                path.pop();
                continue;
            }
            let child = neighbors[*next].clone();
            *next += 1;
            match colors.get(&child).copied() {
                Some(1) => {
                    let start = path.iter().position(|key| key == &child).unwrap_or(0);
                    let cycle = path[start..]
                        .iter()
                        .chain(std::iter::once(&child))
                        .map(|key| key.name())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    return Self::provider_domain_failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::RecursiveTypeInfiniteSize {
                                name: child.name().to_owned(),
                                cycle,
                            },
                        ),
                    );
                }
                Some(2) => {}
                _ => {
                    colors.insert(child.clone(), 1);
                    path.push(child.clone());
                    frames.push((child.clone(), load(self, &child)?, 0));
                }
            }
        }
        Ok(())
    }

    fn constructor_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        use crate::semantic_query_nucleus::DeclarationSignatureProjection;
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Function)? else {
            return Ok(None);
        };
        let identity = self.identity(candidate.clone())?;
        let signature = self.signature(candidate.clone())?;
        let DeclarationSignatureProjection::Callable {
            parameters, result, ..
        } = signature
        else {
            return Ok(None);
        };
        let shell = self
            .context
            .query_registered(self.shells, DeclarationShellQueryKey(candidate))
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(shell)) =
            shell.outcome()
        else {
            return Self::provider_failure("constructor shell became unavailable");
        };
        if shell.parameters.len() != parameters.len() {
            return Self::provider_failure("constructor parameter projections disagree");
        }
        let parameters = shell
            .parameters
            .iter()
            .zip(parameters.iter())
            .map(
                |(header, parameter)| rue_air::SemanticTypeConstructorParameter {
                    name: header.name.clone(),
                    is_comptime: parameter.is_comptime,
                    is_type: parameter.is_comptime
                        && parameter.ty == crate::durable_semantics::DurableType::ComptimeType,
                },
            )
            .collect::<Vec<_>>();
        self.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::TypeCallHead(
                    identity.key.clone(),
                ),
            },
        );
        Ok(Some(rue_air::SemanticTypeConstructorHead {
            key: identity.key.clone(),
            site: identity.key,
            parameters: parameters.into(),
            returns_type: result == crate::durable_semantics::DurableType::ComptimeType,
            is_public: identity.is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    fn module_binding_fact(
        &self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Const)? else {
            return Ok(None);
        };
        let resolution = self.const_resolution(candidate)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding { key, target } =
            resolution
        else {
            return Ok(None);
        };
        let shell = self.identity_key_visibility(&key)?;
        Ok(Some(rue_air::SemanticModuleBinding {
            target,
            site: key,
            is_public: shell,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    fn observe_deferred_local_type_references(
        &mut self,
        module: &ModuleId,
        syntax: &str,
    ) -> Result<
        (),
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        for name in syntax
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .filter(|name| !name.is_empty())
        {
            if name.chars().all(|character| character.is_ascii_digit())
                || self.substitutions.contains_key(name)
                || self.value_substitutions.contains_key(name)
                || matches!(
                    name,
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "isize"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "bool"
                        | "type"
                        | "ptr"
                        | "const"
                        | "mut"
                        | "str"
                        | "Str"
                )
            {
                continue;
            }
            if let Some(fact) = self.alias_fact(module, name)? {
                self.dependencies.insert(
                    crate::semantic_query_nucleus::SemanticDeclarationDependency {
                        source: self.dependency_source.clone(),
                        kind: self.dependency_kind,
                        target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                            fact.site,
                        ),
                    },
                );
                <Self as rue_air::SemanticTypeSyntaxProvider<
                    ModuleId,
                    ModuleId,
                    StableDefinitionKey,
                    StableDefinitionKey,
                    Arc<str>,
                    crate::durable_semantics::DurableType,
                    crate::durable_semantics::DurableConstValue,
                >>::observe_materialized_type(self, &fact.value)?;
                continue;
            }
            for candidate_kind in [DefinitionKind::Struct, DefinitionKind::Enum] {
                if let Some(fact) = self.named_fact(module, name, candidate_kind)? {
                    self.dependencies.insert(
                        crate::semantic_query_nucleus::SemanticDeclarationDependency {
                            source: self.dependency_source.clone(),
                            kind: self.dependency_kind,
                            target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                                fact.site,
                            ),
                        },
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    fn identity_key_visibility(
        &self,
        key: &StableDefinitionKey,
    ) -> Result<
        bool,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let category = match key.kind() {
            crate::StableDefinitionKind::Function => {
                crate::declaration_candidate::DeclarationCandidateCategory::Function
            }
            crate::StableDefinitionKind::Struct => {
                crate::declaration_candidate::DeclarationCandidateCategory::Struct
            }
            crate::StableDefinitionKind::Enum => {
                crate::declaration_candidate::DeclarationCandidateCategory::Enum
            }
            crate::StableDefinitionKind::ValueConst
            | crate::StableDefinitionKind::ModuleBinding => {
                crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate
            }
            _ => return Ok(false),
        };
        let candidate = crate::declaration_candidate::DeclarationCandidateKey {
            module: key.module().clone(),
            category,
            name: Arc::from(key.name()),
            owner: None,
            duplicate_discriminator: 0,
        };
        let terminal = self
            .context
            .query_registered(self.shells, DeclarationShellQueryKey(candidate))
            .map_err(rue_air::SemanticProviderError::Abort)?;
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!("DeclarationShell publishes typed values")
        };
        match value {
            DeclarationShellQueryValue::Available(shell) => Ok(shell.is_public),
            DeclarationShellQueryValue::Failure(failure) => {
                Self::provider_failure(format!("{failure:?}"))
            }
        }
    }

    fn named_fact(
        &self,
        module: &ModuleId,
        name: &str,
        kind: DefinitionKind,
    ) -> Result<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, kind)? else {
            return Ok(None);
        };
        let identity = self.identity(candidate)?;
        Ok(Some(rue_air::SemanticTypeFact {
            value: crate::durable_semantics::DurableType::Nominal(identity.key.clone()),
            site: identity.key,
            is_public: identity.is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }

    fn alias_fact(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        rue_air::SemanticProviderError<
            QueryAbort,
            crate::semantic_query_nucleus::SemanticNucleusFailure,
        >,
    > {
        let Some(candidate) = self.candidate(module, name, DefinitionKind::Const)? else {
            return Ok(None);
        };
        let resolution = self.const_resolution(candidate)?;
        let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
            key,
            value,
            anonymous_nominals,
            dependencies,
            ..
        } = resolution
        else {
            return Ok(None);
        };
        let crate::durable_semantics::DurableConstValue::Type(value) = *value else {
            return Ok(None);
        };
        self.anonymous_nominals.extend(
            anonymous_nominals
                .iter()
                .cloned()
                .map(|value| (value.identity.clone(), value)),
        );
        self.dependencies.extend(dependencies.iter().cloned());
        let is_public = self.identity_key_visibility(&key)?;
        Ok(Some(rue_air::SemanticTypeFact {
            value,
            site: key,
            is_public,
            defining_domain: rue_air::SemanticVisibilityDomain::from_file_path(Some(
                module.as_str(),
            )),
            defining_file: Arc::from(module.as_str()),
        }))
    }
}

impl rue_air::SemanticModulePathProvider<ModuleId, ModuleId, StableDefinitionKey>
    for SemanticNucleusTypeProvider<'_>
{
    type Abort = QueryAbort;
    type Failure = crate::semantic_query_nucleus::SemanticNucleusFailure;

    fn root_module_binding(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<Self::Abort, Self::Failure>,
    > {
        self.module_binding_fact(scope, name)
    }

    fn module_binding(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> Result<
        Option<rue_air::SemanticModuleBinding<ModuleId, StableDefinitionKey>>,
        rue_air::SemanticProviderError<Self::Abort, Self::Failure>,
    > {
        self.module_binding_fact(module, name)
    }

    fn module_display_name(&self, module: &ModuleId) -> Arc<str> {
        Arc::from(module.as_str())
    }

    fn accessing_domain(&self, scope: &ModuleId) -> rue_air::SemanticVisibilityDomain {
        rue_air::SemanticVisibilityDomain::from_file_path(Some(scope.as_str()))
    }
}

#[rustfmt::skip]
impl rue_air::SemanticTypeSyntaxProvider<ModuleId, ModuleId, StableDefinitionKey, StableDefinitionKey, Arc<str>, crate::durable_semantics::DurableType, crate::durable_semantics::DurableConstValue> for SemanticNucleusTypeProvider<'_> {
    fn observe_selected_named_type(
        &mut self,
        _name: &str,
        kind: rue_air::SemanticTypeFactKind,
        fact: &rue_air::SemanticTypeFact<
            crate::durable_semantics::DurableType,
            StableDefinitionKey,
        >,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if matches!(
            kind,
            rue_air::SemanticTypeFactKind::Struct
                | rue_air::SemanticTypeFactKind::Enum
                | rue_air::SemanticTypeFactKind::Constant
        ) {
            self.dependencies.insert(
                crate::semantic_query_nucleus::SemanticDeclarationDependency {
                    source: self.dependency_source.clone(),
                    kind: self.dependency_kind,
                    target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                        fact.site.clone(),
                    ),
                },
            );
        }
        Ok(())
    }

    fn observe_materialized_type(
        &mut self,
        ty: &crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        fn collect(
            ty: &crate::durable_semantics::DurableType,
            output: &mut Vec<StableDefinitionKey>,
        ) {
            match ty {
                crate::durable_semantics::DurableType::Nominal(key) => output.push(key.clone()),
                crate::durable_semantics::DurableType::Array { element, .. }
                | crate::durable_semantics::DurableType::Slice { element, .. }
                | crate::durable_semantics::DurableType::PtrConst(element)
                | crate::durable_semantics::DurableType::PtrMut(element) => {
                    collect(element, output)
                }
                _ => {}
            }
        }
        let mut targets = Vec::new();
        collect(ty, &mut targets);
        self.dependencies.extend(targets.into_iter().map(|target| {
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target:
                    crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                        target,
                    ),
            }
        }));
        Ok(())
    }

    fn substituted_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(self.substitutions.get(name).cloned())
    }

    fn primitive_type(
        &mut self,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::durable_semantics::DurableType as T;
        Ok(Some(match name {
            "i8" => T::I8,
            "i16" => T::I16,
            "i32" => T::I32,
            "i64" => T::I64,
            "isize" => T::I64,
            "u8" => T::U8,
            "u16" => T::U16,
            "u32" => T::U32,
            "u64" => T::U64,
            "usize" => T::U64,
            "bool" => T::Bool,
            "()" => T::Unit,
            "!" => T::Never,
            "type" => T::ComptimeType,
            _ => return Ok(None),
        }))
    }

    fn builtin_type(
        &mut self,
        _scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(
            (name == "str").then(|| crate::durable_semantics::DurableType::BuiltinNominal {
                name: Arc::from("str"),
                kind: rue_air::SemanticImportNominalKind::Struct,
            }),
        )
    }

    fn root_struct_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(scope, name, DefinitionKind::Struct)
    }
    fn root_enum_type(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(scope, name, DefinitionKind::Enum)
    }
    fn root_type_alias(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.alias_fact(scope, name)
    }
    fn module_struct_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(module, name, DefinitionKind::Struct)
    }
    fn module_enum_type(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.named_fact(module, name, DefinitionKind::Enum)
    }
    fn module_type_alias(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeFact<crate::durable_semantics::DurableType, StableDefinitionKey>,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.alias_fact(module, name)
    }

    fn resolve_array_length(
        &mut self,
        scope: &ModuleId,
        length: &rue_air::ArrayLen,
    ) -> rue_air::SemanticProviderResult<
        Option<u64>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        match length {
            rue_air::ArrayLen::Literal(value) => Ok(Some(*value)),
            rue_air::ArrayLen::Named(name) => {
                if let Some(crate::durable_semantics::DurableConstValue::Integer(value)) =
                    self.value_substitutions.get(name.as_str())
                {
                    return u64::try_from(*value).map(Some).map_err(|_| {
                        Self::provider_failure_value(format!(
                            "array length `{name}` is negative or too large"
                        ))
                    });
                }
                if let Some(ty) = self.deferred_value_parameters.get(name.as_str()) {
                    if matches!(
                        ty,
                        crate::durable_semantics::DurableType::I8
                            | crate::durable_semantics::DurableType::I16
                            | crate::durable_semantics::DurableType::I32
                            | crate::durable_semantics::DurableType::I64
                            | crate::durable_semantics::DurableType::U8
                            | crate::durable_semantics::DurableType::U16
                            | crate::durable_semantics::DurableType::U32
                            | crate::durable_semantics::DurableType::U64
                    ) {
                        return Ok(None);
                    }
                    return Self::provider_domain_failure(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                            rue_error::ErrorKind::InvalidArrayLength {
                                reason: format!(
                                    "array length expression '{name}' has non-integer type {}",
                                    durable_type_diagnostic_name(ty),
                                ),
                            },
                        ),
                    );
                }
                if let Some((call, arguments)) = rue_air::parse_type_call_syntax(name) {
                    let resolved = rue_air::resolve_semantic_comptime_call(
                        self,
                        scope,
                        &call,
                        &arguments,
                        rue_air::SemanticComptimeCallExpectation::Value,
                    )
                    .map_err(|error| match semantic_type_query_failure(error) {
                        ResolveSemanticSignatureError::Abort(abort) => {
                            rue_air::SemanticProviderError::Abort(abort)
                        }
                        ResolveSemanticSignatureError::Failure(failure) => {
                            rue_air::SemanticProviderError::Failure(*failure)
                        }
                    })?;
                    let rue_air::SemanticComptimeCallResult::Value(
                        crate::durable_semantics::DurableConstValue::Integer(value),
                    ) = resolved.result
                    else {
                        return Self::provider_failure(format!(
                            "array length `{name}` is not an integer"
                        ));
                    };
                    return u64::try_from(value).map(Some).map_err(|_| {
                        Self::provider_failure_value(format!(
                            "array length `{name}` is negative or too large"
                        ))
                    });
                }
                let Some(candidate) = self.candidate(scope, name, DefinitionKind::Const)? else {
                    return Self::provider_failure(format!("unknown array length `{name}`"));
                };
                let resolution = self.const_resolution(candidate)?;
                let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    value,
                    ..
                } = resolution
                else {
                    return Self::provider_failure(format!(
                        "array length `{name}` is not an integer"
                    ));
                };
                let crate::durable_semantics::DurableConstValue::Integer(value) = *value else {
                    return Self::provider_failure(format!(
                        "array length `{name}` is not an integer"
                    ));
                };
                u64::try_from(value).map(Some).map_err(|_| {
                    Self::provider_failure_value(format!(
                        "array length `{name}` is negative or too large"
                    ))
                })
            }
        }
    }

    fn array_type(
        &mut self,
        element: crate::durable_semantics::DurableType,
        length: Option<u64>,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(match length {
            Some(len) => crate::durable_semantics::DurableType::Array {
                element: Box::new(element),
                len,
            },
            None => crate::durable_semantics::DurableType::ComptimeType,
        })
    }
    fn ptr_const_type(
        &mut self,
        pointee: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::PtrConst(Box::new(
            pointee,
        )))
    }
    fn ptr_mut_type(
        &mut self,
        pointee: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::PtrMut(Box::new(
            pointee,
        )))
    }
    fn preflight_slice(
        &mut self,
        _scope: &ModuleId,
        _syntax: &str,
    ) -> rue_air::SemanticProviderResult<
        (),
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if self
            .configuration
            .preview_features
            .names()
            .binary_search_by(|name| name.as_ref().cmp("slices"))
            .is_ok()
        {
            Ok(())
        } else {
            Self::provider_domain_failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::PreviewFeatureRequired {
                        feature: rue_error::PreviewFeature::Slices,
                        what: "the slice type `[T]`".to_owned(),
                    },
                ),
            )
        }
    }
    fn slice_type(
        &mut self,
        _scope: &ModuleId,
        syntax: &str,
        element: crate::durable_semantics::DurableType,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableType,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        Ok(crate::durable_semantics::DurableType::Slice {
            element: Box::new(element),
            name: Arc::from(syntax),
        })
    }
    fn builtin_type_call(
        &mut self,
        _scope: &ModuleId,
        name: &str,
        arguments: &[String],
    ) -> rue_air::SemanticProviderResult<
        Option<crate::durable_semantics::DurableType>,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        if name != "Str" {
            return Ok(None);
        }
        let [capacity] = arguments else {
            return Self::provider_failure("Str expects one capacity argument");
        };
        let capacity = capacity
            .parse::<u64>()
            .map_err(|_| Self::provider_failure_value("Str capacity must be an integer"))?;
        self.dependencies.insert(
            crate::semantic_query_nucleus::SemanticDeclarationDependency {
                source: self.dependency_source.clone(),
                kind: self.dependency_kind,
                target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::BuiltinTypeCallHead(
                    rue_air::BuiltinTypeCallHead::FixedCapacityString,
                ),
            },
        );
        Ok(Some(
            crate::durable_semantics::DurableType::BuiltinNominal {
                name: Arc::from(format!("Str({capacity})")),
                kind: rue_air::SemanticImportNominalKind::Struct,
            },
        ))
    }
    fn root_constructor(
        &mut self,
        scope: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.constructor_fact(scope, name)
    }
    fn module_constructor(
        &mut self,
        module: &ModuleId,
        name: &str,
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticTypeConstructorHead<
                StableDefinitionKey,
                Arc<str>,
                StableDefinitionKey,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        self.constructor_fact(module, name)
    }
    fn resolve_value_argument(
        &mut self,
        scope: &ModuleId,
        _constructor: &str,
        head: &rue_air::SemanticTypeConstructorHead<
            StableDefinitionKey,
            Arc<str>,
            StableDefinitionKey,
        >,
        parameter_index: usize,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
        syntax: &str,
    ) -> rue_air::SemanticProviderResult<
        crate::durable_semantics::DurableConstValue,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::durable_semantics::DurableConstValue as V;
        let syntax = syntax.trim();
        if let Ok(value) = syntax.parse::<i128>() {
            return Ok(V::Integer(value));
        }
        if syntax == "true" || syntax == "false" {
            return Ok(V::Bool(syntax == "true"));
        }
        if let Some((_, value)) = value_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Ok(value.clone());
        }
        if let Some((_, ty)) = type_arguments
            .iter()
            .find(|(name, _)| name.as_ref() == syntax)
        {
            return Ok(V::Type(ty.clone()));
        }
        if let Some(ty) = self.deferred_value_parameters.get(syntax) {
            return match ty {
                crate::durable_semantics::DurableType::I8
                | crate::durable_semantics::DurableType::I16
                | crate::durable_semantics::DurableType::I32
                | crate::durable_semantics::DurableType::I64
                | crate::durable_semantics::DurableType::U8
                | crate::durable_semantics::DurableType::U16
                | crate::durable_semantics::DurableType::U32
                | crate::durable_semantics::DurableType::U64 => Ok(V::Integer(0)),
                crate::durable_semantics::DurableType::Bool => Ok(V::Bool(false)),
                crate::durable_semantics::DurableType::Unit => Ok(V::Unit),
                _ => Self::provider_failure(format!(
                    "comptime parameter `{syntax}` has unsupported declared type {}",
                    durable_type_diagnostic_name(ty),
                )),
            };
        }
        if let Some(value) = self.value_substitutions.get(syntax) {
            return Ok(value.clone());
        }
        if let Some(ty) = self.substitutions.get(syntax) {
            return Ok(V::Type(ty.clone()));
        }
        if let Some(candidate) = self.candidate(scope, syntax, DefinitionKind::Const)? {
            if let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                value, ..
            } = self.const_resolution(candidate)?
            {
                return Ok(*value);
            }
        }
        let parameter = head
            .parameters
            .get(parameter_index)
            .map(|parameter| parameter.name.as_ref())
            .unwrap_or("?");
        Self::provider_failure(format!(
            "argument for comptime parameter `{parameter}` must be a compile-time known value"
        ))
    }
    fn reduce_comptime_call(
        &mut self,
        head: &rue_air::SemanticTypeConstructorHead<
            StableDefinitionKey,
            Arc<str>,
            StableDefinitionKey,
        >,
        type_arguments: &[(Arc<str>, crate::durable_semantics::DurableType)],
        value_arguments: &[(Arc<str>, crate::durable_semantics::DurableConstValue)],
    ) -> rue_air::SemanticProviderResult<
        Option<
            rue_air::SemanticComptimeCallResult<
                crate::durable_semantics::DurableType,
                crate::durable_semantics::DurableConstValue,
            >,
        >,
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
    > {
        use crate::semantic_query_nucleus::{
            ComptimeCallQueryKey, ComptimeCallResultProjection as P, DeclarationSemanticQueryKey,
            SemanticNucleusKey as K, SemanticNucleusValue as V,
        };
        let declaration = crate::declaration_candidate::DeclarationCandidateKey {
            module: head.key.module().clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from(head.key.name()),
            owner: None,
            duplicate_discriminator: 0,
        };
        let signature = self.signature(declaration.clone())?;
        let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
            parameters,
            ..
        } = signature
        else {
            return Self::provider_failure("type constructor has a non-callable signature");
        };
        let concrete_type_arguments = type_arguments
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>();
        for (name, value) in value_arguments {
            let Some((_, parameter)) = head
                .parameters
                .iter()
                .zip(parameters.iter())
                .find(|(header, _)| &header.name == name)
            else {
                return Self::provider_failure("comptime value argument has no parameter");
            };
            let expected = substitute_durable_generics(&parameter.ty, &concrete_type_arguments);
            if !durable_const_fits_type(value, &expected) {
                if matches!(
                    value,
                    crate::durable_semantics::DurableConstValue::Function(_)
                ) {
                    return Self::provider_failure(
                        "a callable alias cannot be passed as a comptime value argument",
                    );
                }
                if let crate::durable_semantics::DurableConstValue::Integer(value) = value
                    && matches!(
                        &expected,
                        crate::durable_semantics::DurableType::I8
                            | crate::durable_semantics::DurableType::I16
                            | crate::durable_semantics::DurableType::I32
                            | crate::durable_semantics::DurableType::I64
                            | crate::durable_semantics::DurableType::U8
                            | crate::durable_semantics::DurableType::U16
                            | crate::durable_semantics::DurableType::U32
                            | crate::durable_semantics::DurableType::U64
                    )
                {
                    return Self::provider_failure(format!(
                        "value {value} is outside the range of type {}",
                        durable_type_diagnostic_name(&expected),
                    ));
                }
                return Self::provider_domain_failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                        rue_error::ErrorKind::TypeMismatch {
                            expected: durable_type_diagnostic_name(&expected),
                            found: inferred_const_type_name(value).to_owned(),
                        },
                    ),
                );
            }
        }
        let query = K::ComptimeCall(ComptimeCallQueryKey {
            declaration: DeclarationSemanticQueryKey {
                declaration,
                configuration: self.configuration.clone(),
            },
            type_arguments: type_arguments.to_vec().into(),
            value_arguments: value_arguments.to_vec().into(),
        });
        match self.query(query)? {
            V::ComptimeCall(value) => {
                self.anonymous_nominals.extend(
                    value
                        .anonymous_nominals
                        .iter()
                        .cloned()
                        .map(|value| (value.identity.clone(), value)),
                );
                self.dependencies.extend(value.dependencies.iter().cloned());
                self.deferred_ownership
                    .extend(value.deferred_ownership.iter().cloned());
                match value.result {
                    P::Type(value) => Ok(Some(rue_air::SemanticComptimeCallResult::Type(value))),
                    P::Value(value) => Ok(Some(rue_air::SemanticComptimeCallResult::Value(value))),
                }
            }
            V::Failure(failure) => Self::provider_domain_failure(failure),
            _ => Self::provider_failure("comptime query returned the wrong projection"),
        }
    }
}

enum ResolveSemanticSignatureError {
    Abort(QueryAbort),
    Failure(Box<crate::semantic_query_nucleus::SemanticNucleusFailure>),
}

impl ResolveSemanticSignatureError {
    fn failure(failure: crate::semantic_query_nucleus::SemanticNucleusFailure) -> Self {
        Self::Failure(Box::new(failure))
    }
}

fn semantic_type_query_failure(
    failure: rue_air::SemanticTypeSyntaxError<
        QueryAbort,
        crate::semantic_query_nucleus::SemanticNucleusFailure,
        StableDefinitionKey,
        Arc<str>,
    >,
) -> ResolveSemanticSignatureError {
    use rue_air::SemanticResolutionError as E;
    use rue_air::SemanticTypeSyntaxFailure as F;
    use rue_error::ErrorKind;
    match failure {
        E::ProviderAbort(abort) => ResolveSemanticSignatureError::Abort(abort),
        E::ProviderFailure(failure) => ResolveSemanticSignatureError::failure(failure),
        E::Semantic(F::UnknownType { syntax }) => ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                ErrorKind::UnknownType(syntax.to_string()),
            ),
        ),
        E::Semantic(F::UnknownModuleMember { module, member, .. }) => {
            ResolveSemanticSignatureError::failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                    ErrorKind::UnknownModuleMember {
                        module_name: module.to_string(),
                        member_name: member.to_string(),
                    },
                ),
            )
        }
        E::Semantic(F::ValueWhereTypeExpected { parameter, .. }) => {
            ResolveSemanticSignatureError::failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                    format!("argument for comptime parameter `{parameter}` must be a type"),
                )),
            )
        }
        E::Semantic(F::InvalidConstructorArity {
            constructor,
            expected,
            found,
            ..
        }) => ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(format!(
                "type constructor `{constructor}` expects {expected} comptime type argument(s), but {found} provided"
            ))),
        ),
        E::Semantic(F::NotTypeConstructor { constructor, .. }) => {
            ResolveSemanticSignatureError::failure(
                crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                    format!("function `{constructor}` is not a type"),
                )),
            )
        }
        E::ComptimeCallTypeArgument { error, .. } => semantic_type_query_failure(*error),
        other => ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("{other:?}"),
                },
            ),
        ),
    }
}

fn resolve_parsed_semantic_signature(
    provider: &mut SemanticNucleusTypeProvider<'_>,
    module: &ModuleId,
    parsed: &crate::semantic_query_nucleus::ParsedSemanticSignature,
) -> Result<
    crate::semantic_query_nucleus::DeclarationSignatureProjection,
    ResolveSemanticSignatureError,
> {
    use crate::durable_semantics::{DurableParameterMode as M, DurableSemanticParameter};
    use crate::semantic_query_nucleus::{
        DeclarationSignatureProjection as Output, ParsedSemanticSignature as Input,
    };

    fn contains_slice(ty: &crate::durable_semantics::DurableType) -> bool {
        use crate::durable_semantics::DurableType as T;
        match ty {
            T::Slice { .. } => true,
            T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                contains_slice(element)
            }
            _ => false,
        }
    }

    let diagnostic = |kind| {
        ResolveSemanticSignatureError::failure(
            crate::semantic_query_nucleus::SemanticNucleusFailure::Diagnostic(kind),
        )
    };

    let resolve = |provider: &mut SemanticNucleusTypeProvider<'_>,
                   syntax: &str,
                   kind: rue_air::DeclarationTypeDependencyKind| {
        provider.dependency_kind = kind;
        let resolved = rue_air::resolve_semantic_type_syntax(provider, module, syntax)
            .map_err(semantic_type_query_failure)?;
        provider
            .observe_deferred_local_type_references(module, syntax)
            .map_err(|error| match error {
                rue_air::SemanticProviderError::Abort(abort) => {
                    ResolveSemanticSignatureError::Abort(abort)
                }
                rue_air::SemanticProviderError::Failure(failure) => {
                    ResolveSemanticSignatureError::failure(failure)
                }
            })?;
        Ok(resolved)
    };
    match parsed {
        Input::Callable {
            parameters,
            result,
            has_self,
            is_unchecked,
            is_extern,
            is_c_export,
        } => {
            let mut generic_index = 0_u32;
            for parameter in parameters.iter() {
                if parameter.is_comptime && parameter.ty.trim() == "type" {
                    provider.substitutions.insert(
                        parameter.name.clone(),
                        crate::durable_semantics::DurableType::GenericParameter(generic_index),
                    );
                    generic_index += 1;
                }
            }
            let parameters = parameters
                .iter()
                .map(|parameter| {
                    let ty = resolve(
                        provider,
                        &parameter.ty,
                        rue_air::DeclarationTypeDependencyKind::Signature,
                    )?;
                    if parameter.is_comptime && parameter.ty.trim() != "type" {
                        provider
                            .deferred_value_parameters
                            .insert(parameter.name.clone(), ty.clone());
                    }
                    Ok(DurableSemanticParameter {
                        ty,
                        mode: match parameter.mode {
                            crate::declaration_candidate::DeclarationParameterMode::Value => {
                                M::Value
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Borrow => {
                                M::Borrow
                            }
                            crate::declaration_candidate::DeclarationParameterMode::Inout => {
                                M::Inout
                            }
                        },
                        is_comptime: parameter.is_comptime,
                    })
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            let result = resolve(
                provider,
                result,
                rue_air::DeclarationTypeDependencyKind::Signature,
            )?;
            if contains_slice(&result) {
                return Err(diagnostic(rue_error::ErrorKind::SliceReturnNotAllowed));
            }
            if (*is_extern || *is_c_export)
                && !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::CFfi)
            {
                return Err(diagnostic(rue_error::ErrorKind::PreviewFeatureRequired {
                    feature: rue_error::PreviewFeature::CFfi,
                    what: if *is_extern {
                        "an `extern \"C\"` foreign declaration".to_owned()
                    } else {
                        "a `pub extern \"C\" fn` export".to_owned()
                    },
                }));
            }
            if *is_extern || *is_c_export {
                let check =
                    |provider: &mut SemanticNucleusTypeProvider<'_>,
                     ty: &crate::durable_semantics::DurableType| {
                        use crate::durable_semantics::DurableType as T;
                        if matches!(ty, T::Array { .. }) {
                            return Err(diagnostic(rue_error::ErrorKind::ExternArrayByValue {
                                ty: durable_type_diagnostic_name(ty),
                            }));
                        }
                        if let T::Nominal(key) = ty
                            && key.kind() == crate::StableDefinitionKind::Struct
                        {
                            let failure = provider.ffi_shape_failure(ty, &mut Vec::new()).map_err(
                                |error| match error {
                                    rue_air::SemanticProviderError::Abort(abort) => {
                                        ResolveSemanticSignatureError::Abort(abort)
                                    }
                                    rue_air::SemanticProviderError::Failure(failure) => {
                                        ResolveSemanticSignatureError::failure(failure)
                                    }
                                },
                            )?;
                            if failure.as_ref().is_some_and(|(reason, _, _)| {
                                *reason == rue_air::FfiRejectReason::NonReprCAggregate
                            }) {
                                return Err(diagnostic(
                                    rue_error::ErrorKind::ExternAggregateNotReprC {
                                        ty: durable_type_diagnostic_name(ty),
                                    },
                                ));
                            }
                            if failure.is_some() {
                                return Err(diagnostic(
                                    rue_error::ErrorKind::ExternSignatureTypeUnsupported {
                                        ty: durable_type_diagnostic_name(ty),
                                    },
                                ));
                            }
                            return Ok(());
                        }
                        if !matches!(
                            ty,
                            T::I8
                                | T::I16
                                | T::I32
                                | T::I64
                                | T::U8
                                | T::U16
                                | T::U32
                                | T::U64
                                | T::Bool
                                | T::PtrConst(_)
                                | T::PtrMut(_)
                        ) {
                            return Err(diagnostic(
                                rue_error::ErrorKind::ExternSignatureTypeUnsupported {
                                    ty: durable_type_diagnostic_name(ty),
                                },
                            ));
                        }
                        Ok(())
                    };
                for parameter in &parameters {
                    check(provider, &parameter.ty)?;
                }
                if result != crate::durable_semantics::DurableType::Unit {
                    check(provider, &result)?;
                }
            }
            if *is_c_export {
                let name = provider.dependency_source.name().to_owned();
                let reject = |reason| {
                    diagnostic(rue_error::ErrorKind::ExportSignatureUnsupported {
                        name: name.clone(),
                        reason,
                    })
                };
                if name == "main" {
                    return Err(reject("an export named `main` collides with the program entry point; give it a different C name".to_owned()));
                }
                if parameters.iter().any(|parameter| parameter.is_comptime) {
                    return Err(reject("a generic function has no single C symbol; export a concrete (non-`comptime`) function".to_owned()));
                }
                if let Some((index, _)) = parameters
                    .iter()
                    .enumerate()
                    .find(|(_, parameter)| parameter.mode != M::Value)
                {
                    return Err(reject(format!(
                        "parameter {} uses a by-reference (`borrow`/`inout`) mode, which does not cross a C boundary; pass a raw pointer instead",
                        index + 1
                    )));
                }
                if let Some(parameter) = parameters.iter().find(|parameter| {
                    matches!(
                        parameter.ty,
                        crate::durable_semantics::DurableType::Nominal(_)
                            | crate::durable_semantics::DurableType::Array { .. }
                    )
                }) {
                    return Err(reject(format!(
                        "aggregate parameter `{}` is not supported by the P4 export thunk (register repacking across the export boundary is future work); pass a pointer instead",
                        durable_type_diagnostic_name(&parameter.ty)
                    )));
                }
                if matches!(
                    result,
                    crate::durable_semantics::DurableType::Nominal(_)
                        | crate::durable_semantics::DurableType::Array { .. }
                ) {
                    return Err(reject(format!(
                        "aggregate return `{}` is not supported by the P4 export thunk",
                        durable_type_diagnostic_name(&result)
                    )));
                }
                if parameters.len() > 6 {
                    return Err(reject(format!(
                        "{} scalar parameters exceed the 6-register argument budget the P4 export thunk supports; reduce the parameter count",
                        parameters.len()
                    )));
                }
            }
            Ok(Output::Callable {
                parameters: parameters.into(),
                result,
                has_self: *has_self,
                is_unchecked: *is_unchecked,
                is_extern: *is_extern,
                is_c_export: *is_c_export,
            })
        }
        Input::Struct {
            fields,
            is_copy,
            is_linear,
            is_repr_c,
        } => {
            if let Some(kind) = rue_air::declaration_validation::linear_copy_struct(
                provider.dependency_source.name(),
                *is_linear,
                *is_copy,
            ) {
                return Err(diagnostic(kind));
            }
            if let Some(kind) = rue_air::declaration_validation::duplicate_field(
                provider.dependency_source.name(),
                fields.iter().map(|(name, _)| name),
            ) {
                return Err(diagnostic(kind));
            }
            let fields = fields
                .iter()
                .map(|(name, syntax)| {
                    Ok((
                        name.clone(),
                        resolve(
                            provider,
                            syntax,
                            rue_air::DeclarationTypeDependencyKind::Field,
                        )?,
                    ))
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            if fields.iter().any(|(_, ty)| contains_slice(ty)) {
                return Err(diagnostic(rue_error::ErrorKind::SliceInAggregateField));
            }
            if fields
                .iter()
                .any(|(_, ty)| *ty == crate::durable_semantics::DurableType::ComptimeType)
            {
                return Err(ResolveSemanticSignatureError::failure(
                    crate::semantic_query_nucleus::SemanticNucleusFailure::Resolution(Arc::from(
                        "type values cannot exist at runtime",
                    )),
                ));
            }
            if *is_copy {
                for (field_name, field_ty) in &fields {
                    if !provider
                        .type_is_copy(field_ty)
                        .map_err(|error| match error {
                            rue_air::SemanticProviderError::Abort(abort) => {
                                ResolveSemanticSignatureError::Abort(abort)
                            }
                            rue_air::SemanticProviderError::Failure(failure) => {
                                ResolveSemanticSignatureError::failure(failure)
                            }
                        })?
                    {
                        return Err(diagnostic(rue_error::ErrorKind::CopyStructNonCopyField(
                            Box::new(rue_error::CopyStructNonCopyFieldError {
                                struct_name: provider.dependency_source.name().to_owned(),
                                field_name: field_name.to_string(),
                                field_type: durable_type_diagnostic_name(field_ty),
                            }),
                        )));
                    }
                }
            }
            if *is_repr_c {
                if !provider
                    .configuration
                    .preview_features
                    .contains(rue_error::PreviewFeature::CFfi)
                {
                    return Err(diagnostic(rue_error::ErrorKind::PreviewFeatureRequired {
                        feature: rue_error::PreviewFeature::CFfi,
                        what: "the `@repr(c)` representation marker".to_owned(),
                    }));
                }
                let has_destructor = provider
                    .candidate(
                        module,
                        provider.dependency_source.name(),
                        DefinitionKind::Destructor,
                    )
                    .map_err(|error| match error {
                        rue_air::SemanticProviderError::Abort(abort) => {
                            ResolveSemanticSignatureError::Abort(abort)
                        }
                        rue_air::SemanticProviderError::Failure(failure) => {
                            ResolveSemanticSignatureError::failure(failure)
                        }
                    })?
                    .is_some();
                if let Some((reason, path, failing)) = provider
                    .repr_c_failure_for_fields(&fields, *is_linear, has_destructor)
                    .map_err(|error| match error {
                        rue_air::SemanticProviderError::Abort(abort) => {
                            ResolveSemanticSignatureError::Abort(abort)
                        }
                        rue_air::SemanticProviderError::Failure(failure) => {
                            ResolveSemanticSignatureError::failure(failure)
                        }
                    })?
                {
                    let field_path = path.join(".");
                    let reason = if field_path.is_empty() {
                        reason.describe().to_owned()
                    } else {
                        format!(
                            "field `{field_path}` of type `{}` — {}",
                            durable_type_diagnostic_name(&failing),
                            reason.describe()
                        )
                    };
                    return Err(diagnostic(rue_error::ErrorKind::ReprCStructIneligible(
                        Box::new(rue_error::ReprCIneligibleError {
                            struct_name: provider.dependency_source.name().to_owned(),
                            field_path,
                            failing_type: durable_type_diagnostic_name(&failing),
                            reason,
                        }),
                    )));
                }
            }
            Ok(Output::Struct {
                fields: fields.into(),
                is_copy: *is_copy,
                is_linear: *is_linear,
                is_repr_c: *is_repr_c,
            })
        }
        Input::Enum { variants } => {
            if let Some(kind) = rue_air::declaration_validation::duplicate_variant(
                provider.dependency_source.name(),
                variants.iter().map(|(name, _)| name),
            ) {
                return Err(diagnostic(kind));
            }
            let variants: Vec<(Arc<str>, Arc<[crate::durable_semantics::DurableType]>)> = variants
                .iter()
                .map(|(name, payload)| {
                    Ok((
                        name.clone(),
                        payload
                            .iter()
                            .map(|syntax| {
                                resolve(
                                    provider,
                                    syntax,
                                    rue_air::DeclarationTypeDependencyKind::Payload,
                                )
                            })
                            .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?
                            .into(),
                    ))
                })
                .collect::<Result<Vec<_>, ResolveSemanticSignatureError>>()?;
            if variants
                .iter()
                .flat_map(|(_, payload)| payload.iter())
                .any(contains_slice)
            {
                return Err(diagnostic(rue_error::ErrorKind::SliceInAggregateField));
            }
            Ok(Output::Enum {
                variants: variants.into(),
            })
        }
        Input::Destructor => Ok(Output::Destructor),
    }
}

impl Default for RevisionedQueryDatabase {
    fn default() -> Self {
        Self::with_declaration_memo_retention(DECLARATION_QUERY_MEMO_RETENTION)
    }
}

impl RevisionedQueryDatabase {
    /// Construct the database with an explicit declaration-keyed memo
    /// retention. Production uses [`DECLARATION_QUERY_MEMO_RETENTION`];
    /// eviction-lifecycle tests pass a small cap so exceeding it stays cheap.
    fn with_declaration_memo_retention(declaration_memo_retention: usize) -> Self {
        let runtime = QueryRuntime::new(1);
        let module_store = Arc::new(Mutex::new(ModuleInputStore::default()));
        #[cfg(test)]
        let test_import_store = Arc::new(Mutex::new(TestImportInputStore {
            next_stamp: 1,
            ..TestImportInputStore::default()
        }));
        let parse_store = module_store.clone();
        let parse_modules = runtime
            .family_with_equality_and_evaluator(
                "compiler.parse-module",
                MODULE_QUERY_MEMO_RETENTION,
                parse_module_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    context.input(module_source_input(&key.0))?;
                    let view = module_input_view(&parse_store, context.revision())?;
                    let (result, work) =
                        crate::parsed_modules::parse_source_snapshot_module(&view.snapshot, &key.0);
                    Ok(QueryOutput::success(ParseModuleValue { result, work }))
                },
            )
            .expect("the ParseModule family has one canonical name");
        let parse_for_index = parse_modules.clone();
        let module_indexes = runtime
            .family_with_equality_and_evaluator(
                "compiler.module-index",
                MODULE_QUERY_MEMO_RETENTION,
                module_index_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    let parsed = context.query_registered(&parse_for_index, key.clone())?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let result = match &parsed.result {
                        Ok(module) => Ok(Arc::new(ModuleIndex {
                            revision: module.revision().clone(),
                            definitions: module
                                .definitions()
                                .candidates()
                                .iter()
                                .map(|candidate| ModuleIndexEntry {
                                    namespace: candidate.namespace(),
                                    kind: candidate.kind(),
                                    visibility: candidate.visibility(),
                                    name: Arc::from(candidate.name()),
                                    name_span: candidate.name_span(),
                                    declaration_span: candidate.declaration_span(),
                                })
                                .collect::<Vec<_>>()
                                .into(),
                            imports: module.imports().to_vec().into(),
                        })),
                        Err(errors) => Err(errors.clone()),
                    };
                    Ok(QueryOutput::success(ModuleIndexValue(result)))
                },
            )
            .expect("the ModuleIndex family has one canonical name");
        let indexes_for_declaration_sets = module_indexes.clone();
        let module_declaration_sets = runtime
            .family_with_equality_and_evaluator(
                "compiler.module-declaration-set",
                MODULE_QUERY_MEMO_RETENTION,
                |left: &ModuleDeclarationSetValue, right: &ModuleDeclarationSetValue| left == right,
                move |context, _, key: &ModuleQueryKey| {
                    let indexed =
                        context.query_registered(&indexes_for_declaration_sets, key.clone())?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let value = match &indexed.0 {
                        Ok(index) => {
                            let mut declarations = index
                                .definitions
                                .iter()
                                .map(|entry| ModuleDeclarationSetFact {
                                    namespace: entry.namespace,
                                    kind: entry.kind,
                                    visibility: entry.visibility,
                                    name: entry.name.clone(),
                                })
                                .collect::<Vec<_>>();
                            declarations.sort_by(|left, right| {
                                let visibility_rank = |visibility| match visibility {
                                    None => 0,
                                    Some(rue_parser::ast::Visibility::Private) => 1,
                                    Some(rue_parser::ast::Visibility::Public) => 2,
                                };
                                (
                                    left.namespace,
                                    left.kind,
                                    visibility_rank(left.visibility),
                                    left.name.as_ref(),
                                )
                                    .cmp(&(
                                        right.namespace,
                                        right.kind,
                                        visibility_rank(right.visibility),
                                        right.name.as_ref(),
                                    ))
                            });
                            let mut import_specifiers = index
                                .imports
                                .iter()
                                .map(|import| Arc::from(import.specifier()))
                                .collect::<Vec<_>>();
                            import_specifiers.sort();
                            ModuleDeclarationSetValue::Available {
                                declarations: declarations.into(),
                                import_specifiers: import_specifiers.into(),
                            }
                        }
                        Err(_) => ModuleDeclarationSetValue::Unavailable,
                    };
                    Ok(QueryOutput::success(value))
                },
            )
            .expect("the ModuleDeclarationSet family has one canonical name");
        let parse_for_declaration_occurrences = parse_modules.clone();
        let parse_for_declaration_shells = parse_modules.clone();
        let declaration_occurrence_indexes = runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-occurrence-index",
                MODULE_QUERY_MEMO_RETENTION,
                declaration_occurrence_index_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    let parsed = context
                        .query_registered(&parse_for_declaration_occurrences, key.clone())?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let value = match &parsed.result {
                        Ok(module) => DeclarationOccurrenceIndexValue::Available(Arc::new(
                            DeclarationOccurrenceIndex {
                                capabilities: module
                                    .definitions()
                                    .declaration_capabilities()
                                    .iter()
                                    .cloned()
                                    .map(|capability| (capability.key().clone(), capability))
                                    .collect(),
                            },
                        )),
                        Err(_) => DeclarationOccurrenceIndexValue::Failure(
                            crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                module: key.0.clone(),
                            },
                        ),
                    };
                    let kind = if matches!(value, DeclarationOccurrenceIndexValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationOccurrenceIndex family has one canonical name");
        let occurrences_for_shells = declaration_occurrence_indexes.clone();
        let declaration_shells = runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-shell",
                declaration_memo_retention,
                |left: &DeclarationShellQueryValue, right: &DeclarationShellQueryValue| {
                    left == right
                },
                move |context, _, key: &DeclarationShellQueryKey| {
                    let indexed = context.query_registered(
                        &occurrences_for_shells,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            DeclarationShellQueryValue::Failure(
                                crate::declaration_candidate::DeclarationShellFailure::OccurrencesUnavailable(
                                    failure.clone(),
                                ),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0) {
                                None => DeclarationShellQueryValue::Failure(
                                    crate::declaration_candidate::DeclarationShellFailure::Absent(
                                        key.0.clone(),
                                    ),
                                ),
                                Some(crate::declaration_candidate::DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    DeclarationShellQueryValue::Failure(
                                        crate::declaration_candidate::DeclarationShellFailure::Ambiguous(
                                            key.0.clone(),
                                        ),
                                    )
                                }
                                Some(crate::declaration_candidate::DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let parsed = context.query_registered(
                                        &parse_for_declaration_shells,
                                        ModuleQueryKey(key.0.module.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                                        unreachable!("ParseModule publishes typed values")
                                    };
                                    match &parsed.result {
                                        Ok(module) => match module
                                            .definitions()
                                            .evaluate_declaration_shell(&key.0)
                                        {
                                            Ok(fact) => DeclarationShellQueryValue::Available(fact),
                                            Err(failure) => DeclarationShellQueryValue::Failure(failure),
                                        },
                                        Err(_) => DeclarationShellQueryValue::Failure(
                                            crate::declaration_candidate::DeclarationShellFailure::OccurrencesUnavailable(
                                                crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                    module: key.0.module.clone(),
                                                },
                                            ),
                                        ),
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, DeclarationShellQueryValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationShell family has one canonical name");
        let occurrences_for_raw_const = declaration_occurrence_indexes.clone();
        let shells_for_raw_const = declaration_shells.clone();
        let parse_for_raw_const = parse_modules.clone();
        let raw_const_syntax = runtime
            .family_with_equality_and_evaluator(
                "compiler.raw-const-syntax",
                declaration_memo_retention,
                |left: &RawConstSyntaxQueryValue, right: &RawConstSyntaxQueryValue| left == right,
                move |context, _, key: &RawConstSyntaxQueryKey| {
                    use crate::declaration_candidate::{
                        DeclarationCandidateCategory, DeclarationOccurrenceCapability,
                        DeclarationShellFailure, RawConstSyntaxFailure,
                    };

                    let indexed = context.query_registered(
                        &occurrences_for_raw_const,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            RawConstSyntaxQueryValue::Failure(
                                RawConstSyntaxFailure::OccurrencesUnavailable(failure.clone()),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0) {
                                None => RawConstSyntaxQueryValue::Failure(
                                    RawConstSyntaxFailure::Absent(key.0.clone()),
                                ),
                                Some(DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    RawConstSyntaxQueryValue::Failure(
                                    RawConstSyntaxFailure::Ambiguous(key.0.clone()),
                                    )
                                }
                                Some(DeclarationOccurrenceCapability::Exact {
                                    duplicate_multiplicity: 0,
                                    ..
                                }) => RawConstSyntaxQueryValue::Failure(
                                    RawConstSyntaxFailure::ParserCapabilityMismatch(key.0.clone()),
                                ),
                                Some(DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let shell = context.query_registered(
                                        &shells_for_raw_const,
                                        DeclarationShellQueryKey(key.0.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(shell) = shell.outcome()
                                    else {
                                        unreachable!("DeclarationShell publishes typed values")
                                    };
                                    match shell {
                                        DeclarationShellQueryValue::Failure(failure) => {
                                            let failure = match failure {
                                                DeclarationShellFailure::OccurrencesUnavailable(
                                                    failure,
                                                ) => RawConstSyntaxFailure::OccurrencesUnavailable(
                                                    failure.clone(),
                                                ),
                                                DeclarationShellFailure::Absent(key) => {
                                                    RawConstSyntaxFailure::Absent(key.clone())
                                                }
                                                DeclarationShellFailure::Ambiguous(key) => {
                                                    RawConstSyntaxFailure::Ambiguous(key.clone())
                                                }
                                                DeclarationShellFailure::ParserCapabilityMismatch(
                                                    key,
                                                ) => RawConstSyntaxFailure::ParserCapabilityMismatch(
                                                    key.clone(),
                                                ),
                                            };
                                            RawConstSyntaxQueryValue::Failure(failure)
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key.category
                                                != DeclarationCandidateCategory::ConstCandidate =>
                                        {
                                            RawConstSyntaxQueryValue::Failure(
                                                RawConstSyntaxFailure::CategoryMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key != key.0 =>
                                        {
                                            RawConstSyntaxQueryValue::Failure(
                                                RawConstSyntaxFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(_) => {
                                            let parsed = context.query_registered(
                                                &parse_for_raw_const,
                                                ModuleQueryKey(key.0.module.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(parsed) =
                                                parsed.outcome()
                                            else {
                                                unreachable!("ParseModule publishes typed values")
                                            };
                                            match &parsed.result {
                                                Err(_) => RawConstSyntaxQueryValue::Failure(
                                                    RawConstSyntaxFailure::OccurrencesUnavailable(
                                                        crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                            module: key.0.module.clone(),
                                                        },
                                                    ),
                                                ),
                                                Ok(module) => module
                                                    .evaluate_raw_const_syntax(&key.0)
                                                    .map_or_else(
                                                        || RawConstSyntaxQueryValue::Failure(
                                                            RawConstSyntaxFailure::ParserCapabilityMismatch(
                                                                key.0.clone(),
                                                            ),
                                                        ),
                                                        RawConstSyntaxQueryValue::Available,
                                                    ),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, RawConstSyntaxQueryValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the RawConstSyntax family has one canonical name");
        let occurrences_for_raw_signature = declaration_occurrence_indexes.clone();
        let shells_for_raw_signature = declaration_shells.clone();
        let parse_for_raw_signature = parse_modules.clone();
        let raw_declaration_signatures = runtime
            .family_with_equality_and_evaluator(
                "compiler.raw-declaration-signature",
                declaration_memo_retention,
                |left: &RawDeclarationSignatureQueryValue,
                 right: &RawDeclarationSignatureQueryValue| { left == right },
                move |context, _, key: &RawDeclarationSignatureQueryKey| {
                    use crate::declaration_candidate::{
                        DeclarationCandidateCategory, DeclarationOccurrenceCapability,
                        DeclarationShellFailure, RawDeclarationSignatureFailure,
                    };

                    let indexed = context.query_registered(
                        &occurrences_for_raw_signature,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            RawDeclarationSignatureQueryValue::Failure(
                                RawDeclarationSignatureFailure::OccurrencesUnavailable(
                                    failure.clone(),
                                ),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0) {
                                None => RawDeclarationSignatureQueryValue::Failure(
                                    RawDeclarationSignatureFailure::Absent(key.0.clone()),
                                ),
                                Some(DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    RawDeclarationSignatureQueryValue::Failure(
                                        RawDeclarationSignatureFailure::Ambiguous(key.0.clone()),
                                    )
                                }
                                Some(DeclarationOccurrenceCapability::Exact {
                                    duplicate_multiplicity: 0,
                                    ..
                                }) => RawDeclarationSignatureQueryValue::Failure(
                                    RawDeclarationSignatureFailure::ParserCapabilityMismatch(
                                        key.0.clone(),
                                    ),
                                ),
                                Some(DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let shell = context.query_registered(
                                        &shells_for_raw_signature,
                                        DeclarationShellQueryKey(key.0.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(shell) = shell.outcome()
                                    else {
                                        unreachable!("DeclarationShell publishes typed values")
                                    };
                                    match shell {
                                        DeclarationShellQueryValue::Failure(failure) => {
                                            let failure = match failure {
                                                DeclarationShellFailure::OccurrencesUnavailable(
                                                    failure,
                                                ) => RawDeclarationSignatureFailure::OccurrencesUnavailable(
                                                    failure.clone(),
                                                ),
                                                DeclarationShellFailure::Absent(key) => {
                                                    RawDeclarationSignatureFailure::Absent(
                                                        key.clone(),
                                                    )
                                                }
                                                DeclarationShellFailure::Ambiguous(key) => {
                                                    RawDeclarationSignatureFailure::Ambiguous(
                                                        key.clone(),
                                                    )
                                                }
                                                DeclarationShellFailure::ParserCapabilityMismatch(
                                                    key,
                                                ) => RawDeclarationSignatureFailure::ParserCapabilityMismatch(
                                                    key.clone(),
                                                ),
                                            };
                                            RawDeclarationSignatureQueryValue::Failure(failure)
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key.category
                                                == DeclarationCandidateCategory::ConstCandidate =>
                                        {
                                            RawDeclarationSignatureQueryValue::Failure(
                                                RawDeclarationSignatureFailure::CategoryMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key != key.0 =>
                                        {
                                            RawDeclarationSignatureQueryValue::Failure(
                                                RawDeclarationSignatureFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(_) => {
                                            let parsed = context.query_registered(
                                                &parse_for_raw_signature,
                                                ModuleQueryKey(key.0.module.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(parsed) =
                                                parsed.outcome()
                                            else {
                                                unreachable!("ParseModule publishes typed values")
                                            };
                                            match &parsed.result {
                                                Err(_) => RawDeclarationSignatureQueryValue::Failure(
                                                    RawDeclarationSignatureFailure::OccurrencesUnavailable(
                                                        crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                            module: key.0.module.clone(),
                                                        },
                                                    ),
                                                ),
                                                Ok(module) => module
                                                    .evaluate_raw_declaration_signature(&key.0)
                                                    .map_or_else(
                                                        || RawDeclarationSignatureQueryValue::Failure(
                                                            RawDeclarationSignatureFailure::ParserCapabilityMismatch(
                                                                key.0.clone(),
                                                            ),
                                                        ),
                                                        RawDeclarationSignatureQueryValue::Available,
                                                    ),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(
                        value,
                        RawDeclarationSignatureQueryValue::Available(_)
                    ) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the RawDeclarationSignature family has one canonical name");
        let occurrences_for_raw_body = declaration_occurrence_indexes.clone();
        let shells_for_raw_body = declaration_shells.clone();
        let parse_for_raw_body = parse_modules.clone();
        let raw_declaration_bodies = runtime
            .family_with_equality_and_evaluator(
                "compiler.raw-declaration-body",
                declaration_memo_retention,
                |left: &RawDeclarationBodyQueryValue,
                 right: &RawDeclarationBodyQueryValue| { left == right },
                move |context, _, key: &RawDeclarationBodyQueryKey| {
                    use crate::declaration_candidate::{
                        DeclarationCandidateCategory, DeclarationOccurrenceCapability,
                        DeclarationShellFailure, RawDeclarationBodyFailure,
                    };

                    let indexed = context.query_registered(
                        &occurrences_for_raw_body,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            RawDeclarationBodyQueryValue::Failure(
                                RawDeclarationBodyFailure::OccurrencesUnavailable(failure.clone()),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0) {
                                None => RawDeclarationBodyQueryValue::Failure(
                                    RawDeclarationBodyFailure::Absent(key.0.clone()),
                                ),
                                Some(DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    RawDeclarationBodyQueryValue::Failure(
                                        RawDeclarationBodyFailure::Ambiguous(key.0.clone()),
                                    )
                                }
                                Some(DeclarationOccurrenceCapability::Exact {
                                    duplicate_multiplicity: 0,
                                    ..
                                }) => RawDeclarationBodyQueryValue::Failure(
                                    RawDeclarationBodyFailure::ParserCapabilityMismatch(
                                        key.0.clone(),
                                    ),
                                ),
                                Some(DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let shell = context.query_registered(
                                        &shells_for_raw_body,
                                        DeclarationShellQueryKey(key.0.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(shell) = shell.outcome()
                                    else {
                                        unreachable!("DeclarationShell publishes typed values")
                                    };
                                    match shell {
                                        DeclarationShellQueryValue::Failure(failure) => {
                                            let failure = match failure {
                                                DeclarationShellFailure::OccurrencesUnavailable(
                                                    failure,
                                                ) => RawDeclarationBodyFailure::OccurrencesUnavailable(
                                                    failure.clone(),
                                                ),
                                                DeclarationShellFailure::Absent(key) => {
                                                    RawDeclarationBodyFailure::Absent(key.clone())
                                                }
                                                DeclarationShellFailure::Ambiguous(key) => {
                                                    RawDeclarationBodyFailure::Ambiguous(key.clone())
                                                }
                                                DeclarationShellFailure::ParserCapabilityMismatch(
                                                    key,
                                                ) => RawDeclarationBodyFailure::ParserCapabilityMismatch(
                                                    key.clone(),
                                                ),
                                            };
                                            RawDeclarationBodyQueryValue::Failure(failure)
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if !matches!(
                                                fact.key.category,
                                                DeclarationCandidateCategory::Function
                                                    | DeclarationCandidateCategory::Method
                                                    | DeclarationCandidateCategory::AssociatedFunction
                                                    | DeclarationCandidateCategory::Destructor
                                            ) =>
                                        {
                                            RawDeclarationBodyQueryValue::Failure(
                                                RawDeclarationBodyFailure::CategoryMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key != key.0 =>
                                        {
                                            RawDeclarationBodyQueryValue::Failure(
                                                RawDeclarationBodyFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(_) => {
                                            let parsed = context.query_registered(
                                                &parse_for_raw_body,
                                                ModuleQueryKey(key.0.module.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(parsed) =
                                                parsed.outcome()
                                            else {
                                                unreachable!("ParseModule publishes typed values")
                                            };
                                            match &parsed.result {
                                                Err(_) => RawDeclarationBodyQueryValue::Failure(
                                                    RawDeclarationBodyFailure::OccurrencesUnavailable(
                                                        crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                            module: key.0.module.clone(),
                                                        },
                                                    ),
                                                ),
                                                Ok(module) => module
                                                    .evaluate_raw_declaration_body(&key.0)
                                                    .map_or_else(
                                                        || RawDeclarationBodyQueryValue::Failure(
                                                            RawDeclarationBodyFailure::ParserCapabilityMismatch(
                                                                key.0.clone(),
                                                            ),
                                                        ),
                                                        RawDeclarationBodyQueryValue::Available,
                                                    ),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, RawDeclarationBodyQueryValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the RawDeclarationBody family has one canonical name");
        let index_for_lookup = module_indexes.clone();
        let lookup_names = runtime
            .family_with_equality_and_evaluator(
                "compiler.lookup-name",
                declaration_memo_retention,
                |left: &LookupNameValue, right: &LookupNameValue| left == right,
                move |context, _, key: &LookupNameKey| {
                    let indexed = context
                        .query_registered(&index_for_lookup, ModuleQueryKey(key.module.clone()))?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let result = match &indexed.0 {
                        Ok(index) => Ok(index
                            .definitions
                            .iter()
                            .filter(|entry| {
                                entry.namespace == key.namespace && entry.name == key.name
                            })
                            .map(|entry| LookupNameFact {
                                namespace: entry.namespace,
                                kind: entry.kind,
                                visibility: entry.visibility,
                                name: entry.name.clone(),
                            })
                            .collect::<Vec<_>>()
                            .into()),
                        Err(_) => Err(LookupNameFailure::ModuleIndexUnavailable(
                            key.module.clone(),
                        )),
                    };
                    Ok(QueryOutput::success(LookupNameValue(result)))
                },
            )
            .expect("the LookupName family has one canonical name");
        let parse_for_rir = parse_modules.clone();
        let index_for_rir = module_indexes.clone();
        let module_rirs = runtime
            .family_with_equality_and_evaluator(
                "compiler.module-rir",
                MODULE_QUERY_MEMO_RETENTION,
                module_rir_value_equal,
                move |context, _, key: &ModuleQueryKey| {
                    let parsed = context.query_registered(&parse_for_rir, key.clone())?;
                    let indexed = context.query_registered(&index_for_rir, key.clone())?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let value = match (&parsed.result, &indexed.0) {
                        (Ok(module), Ok(_)) => module_rir_value_from_lowering(
                            lower_module_rir_with_work(module.clone()),
                        ),
                        (Err(errors), _) | (_, Err(errors)) => ModuleRirValue {
                            result: Err(errors.clone()),
                            work: crate::CanonicalRirWork::default(),
                        },
                    };
                    Ok(QueryOutput::success(value))
                },
            )
            .expect("the ModuleRir family has one canonical name");
        let import_store = Arc::new(Mutex::new(ImportInputStore::default()));
        let evaluator_store = import_store.clone();
        let parse_for_import = parse_modules.clone();
        let resolve_imports = runtime
            .family_with_evaluator(
                "compiler.resolve-import",
                MODULE_QUERY_MEMO_RETENTION,
                move |context, _, key: &ResolveImportKey| {
                    let view = {
                        let store = lock_import_store(&evaluator_store);
                        store
                            .revisions
                            .iter()
                            .find(|view| view.revision == context.revision())
                            .cloned()
                    }
                    .ok_or_else(|| QueryAbort::UnpublishedRevision(context.revision()))?;
                    context.input(import_context_input())?;
                    let parsed = context.query_registered(
                        &parse_for_import,
                        ModuleQueryKey(key.occurrence.importer().clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let site = parsed.result.as_ref().ok().and_then(|module| {
                        module.imports().iter().find(|site| {
                            site.importer() == key.occurrence.importer()
                                && site.source_offset() == key.occurrence.source_offset()
                                && site.source_end() == key.occurrence.source_end()
                                && site.specifier() == key.occurrence.specifier()
                        })
                    });
                    let Some(site) = site else {
                        return Ok(QueryOutput::success(ResolveImportValue {
                            site_found: false,
                            groups: Arc::from([]),
                            requests: Arc::from([]),
                            speculative_blocked: false,
                            resolution: None,
                        }));
                    };
                    context.input(accepted_read_input(key.occurrence.importer()))?;
                    let importer = view
                        .accepted_reads
                        .iter()
                        .find(|entry| entry.module() == key.occurrence.importer())
                        .expect("indexed importer retains accepted-read provenance");
                    let occurrence = crate::ImportOccurrenceKey::from_directive(site);
                    let groups = crate::import_discovery::discovery_groups_for_occurrence(
                        &view.context,
                        &occurrence,
                        importer.requested_path(),
                    )
                    .expect("accepted import provenance and captured context are canonical");
                    for request in groups.iter().flat_map(|group| group.iter()) {
                        let present = context
                            .optional_input(import_observation_input(request))
                            .is_some();
                        assert_eq!(present, view.ledger.get(request).is_some());
                    }
                    let pending = pending_occurrence_requests(&groups, &view.ledger);
                    let speculative_blocked =
                        key.mode == ImportDemandMode::Speculative && !pending.is_empty();
                    let resolution = if pending.is_empty()
                        && !crate::import_discovery::exact_import_has_failures(
                            &groups,
                            &view.ledger,
                        ) {
                        enum ProvenanceLookupFailure {
                            Query(QueryAbort),
                            Invalid,
                        }

                        if crate::import_discovery::validate_exact_import_occurrence(
                            &groups,
                            &view.ledger,
                        )
                        .is_err()
                        {
                            None
                        } else {
                            let winner = crate::import_discovery::exact_import_winner(
                                groups.iter(),
                                &view.ledger,
                            );
                            match crate::import_discovery::resolve_exact_import_winner(
                                winner,
                                |source| {
                                    context
                                        .input(accepted_import_provenance_input(
                                            source.metadata_identity(),
                                        ))
                                        .map_err(ProvenanceLookupFailure::Query)?;
                                    crate::import_discovery::accepted_import_module(
                                        source,
                                        &view.accepted_reads,
                                    )
                                    .map_err(|_| ProvenanceLookupFailure::Invalid)
                                },
                            ) {
                                Ok(resolution) => Some(resolution),
                                Err(ProvenanceLookupFailure::Query(abort)) => return Err(abort),
                                Err(ProvenanceLookupFailure::Invalid) => None,
                            }
                        }
                    } else {
                        None
                    };
                    Ok(QueryOutput::success(ResolveImportValue {
                        site_found: true,
                        groups: groups.into(),
                        requests: if speculative_blocked {
                            Arc::from([])
                        } else {
                            pending.into()
                        },
                        speculative_blocked,
                        resolution,
                    }))
                },
            )
            .expect("the ResolveImport family has one canonical name");
        let occurrences_for_declaration_import = declaration_occurrence_indexes.clone();
        let shells_for_declaration_import = declaration_shells.clone();
        let parse_for_declaration_import = parse_modules.clone();
        let resolve_for_declaration_import = resolve_imports.clone();
        #[cfg(test)]
        let test_imports_for_declaration_import = test_import_store.clone();
        let declaration_imports = runtime
            .family_with_equality_and_evaluator(
                "compiler.declaration-import",
                declaration_memo_retention,
                |left: &DeclarationImportQueryValue, right: &DeclarationImportQueryValue| {
                    left == right
                },
                move |context, _, key: &DeclarationImportQueryKey| {
                    use crate::declaration_candidate::{
                        DeclarationCandidateCategory, DeclarationImportFailure,
                        DeclarationOccurrenceCapability, DeclarationShellFailure,
                    };
                    use crate::parsed_modules::ParsedDeclarationImportFailure;

                    let indexed = context.query_registered(
                        &occurrences_for_declaration_import,
                        ModuleQueryKey(key.0.declaration.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("DeclarationOccurrenceIndex publishes typed values")
                    };
                    let value = match indexed {
                        DeclarationOccurrenceIndexValue::Failure(failure) => {
                            DeclarationImportQueryValue::Failure(
                                DeclarationImportFailure::OccurrencesUnavailable(failure.clone()),
                            )
                        }
                        DeclarationOccurrenceIndexValue::Available(index) => {
                            match index.capabilities.get(&key.0.declaration) {
                                None => DeclarationImportQueryValue::Failure(
                                    DeclarationImportFailure::AbsentDeclaration(key.0.clone()),
                                ),
                                Some(DeclarationOccurrenceCapability::Ambiguous { .. }) => {
                                    DeclarationImportQueryValue::Failure(
                                        DeclarationImportFailure::AmbiguousDeclaration(
                                            key.0.clone(),
                                        ),
                                    )
                                }
                                Some(DeclarationOccurrenceCapability::Exact {
                                    duplicate_multiplicity: 0,
                                    ..
                                }) => DeclarationImportQueryValue::Failure(
                                    DeclarationImportFailure::ParserCapabilityMismatch(
                                        key.0.clone(),
                                    ),
                                ),
                                Some(DeclarationOccurrenceCapability::Exact { .. }) => {
                                    let shell = context.query_registered(
                                        &shells_for_declaration_import,
                                        DeclarationShellQueryKey(key.0.declaration.clone()),
                                    )?;
                                    let rue_query::QueryOutcome::Success(shell) = shell.outcome()
                                    else {
                                        unreachable!("DeclarationShell publishes typed values")
                                    };
                                    match shell {
                                        DeclarationShellQueryValue::Failure(failure) => {
                                            let failure = match failure {
                                                DeclarationShellFailure::OccurrencesUnavailable(
                                                    failure,
                                                ) => DeclarationImportFailure::OccurrencesUnavailable(
                                                    failure.clone(),
                                                ),
                                                DeclarationShellFailure::Absent(_) => {
                                                    DeclarationImportFailure::AbsentDeclaration(
                                                        key.0.clone(),
                                                    )
                                                }
                                                DeclarationShellFailure::Ambiguous(_) => {
                                                    DeclarationImportFailure::AmbiguousDeclaration(
                                                        key.0.clone(),
                                                    )
                                                }
                                                DeclarationShellFailure::ParserCapabilityMismatch(
                                                    _,
                                                ) => DeclarationImportFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            };
                                            DeclarationImportQueryValue::Failure(failure)
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if !matches!(
                                                fact.key.category,
                                                DeclarationCandidateCategory::ConstCandidate
                                                    | DeclarationCandidateCategory::Function
                                                    | DeclarationCandidateCategory::Method
                                                    | DeclarationCandidateCategory::AssociatedFunction
                                                    | DeclarationCandidateCategory::Destructor
                                            ) =>
                                        {
                                            DeclarationImportQueryValue::Failure(
                                                DeclarationImportFailure::CategoryMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(fact)
                                            if fact.key != key.0.declaration =>
                                        {
                                            DeclarationImportQueryValue::Failure(
                                                DeclarationImportFailure::ParserCapabilityMismatch(
                                                    key.0.clone(),
                                                ),
                                            )
                                        }
                                        DeclarationShellQueryValue::Available(_) => {
                                            let parsed = context.query_registered(
                                                &parse_for_declaration_import,
                                                ModuleQueryKey(key.0.declaration.module.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(parsed) =
                                                parsed.outcome()
                                            else {
                                                unreachable!("ParseModule publishes typed values")
                                            };
                                            match &parsed.result {
                                                Err(_) => DeclarationImportQueryValue::Failure(
                                                    DeclarationImportFailure::OccurrencesUnavailable(
                                                        crate::declaration_candidate::DeclarationOccurrenceFailure::ParseRejected {
                                                            module: key.0.declaration.module.clone(),
                                                        },
                                                    ),
                                                ),
                                                Ok(module) => match module.declaration_import(&key.0)
                                                {
                                                    Err(ParsedDeclarationImportFailure::SiteOutOfRange {
                                                        available,
                                                    }) => DeclarationImportQueryValue::Failure(
                                                        DeclarationImportFailure::SiteOutOfRange {
                                                            key: key.0.clone(),
                                                            available,
                                                        },
                                                    ),
                                                    Err(ParsedDeclarationImportFailure::SpecifierMismatch {
                                                        actual,
                                                    }) => DeclarationImportQueryValue::Failure(
                                                        DeclarationImportFailure::SpecifierMismatch {
                                                            key: key.0.clone(),
                                                            actual,
                                                        },
                                                    ),
                                                    Err(ParsedDeclarationImportFailure::CapabilityMismatch) => {
                                                        DeclarationImportQueryValue::Failure(
                                                            DeclarationImportFailure::ParserCapabilityMismatch(
                                                                key.0.clone(),
                                                            ),
                                                        )
                                                    }
                                                    Ok(site) => {
                                                        #[cfg(test)]
                                                        {
                                                            let view = test_imports_for_declaration_import
                                                                .lock()
                                                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                                                .revisions
                                                                .iter()
                                                                .find(|view| view.revision == context.revision())
                                                                .cloned();
                                                            if let Some(view) = view {
                                                                context.input(test_import_graph_input())?;
                                                                let normalized = rue_air::normalize_module_path(
                                                                    site.specifier(),
                                                                );
                                                                let value = view
                                                                    .graph
                                                                    .records()
                                                                    .iter()
                                                                    .find(|record| {
                                                                        record.importer() == site.importer()
                                                                            && record.normalized_specifier()
                                                                                == normalized
                                                                    })
                                                                    .map(|record| {
                                                                        DeclarationImportQueryValue::Available(
                                                                            record.resolution().clone(),
                                                                        )
                                                                    })
                                                                    .unwrap_or_else(|| {
                                                                        DeclarationImportQueryValue::Failure(
                                                                            DeclarationImportFailure::ResolutionUnavailable(
                                                                                key.0.clone(),
                                                                            ),
                                                                        )
                                                                });
                                                                let kind = if matches!(
                                                                    value,
                                                                    DeclarationImportQueryValue::Available(_)
                                                                ) {
                                                                    QueryTerminalKind::Success
                                                                } else {
                                                                    QueryTerminalKind::Failure
                                                                };
                                                                return Ok(QueryOutput::success(value)
                                                                    .with_terminal_kind(kind));
                                                            }
                                                        }
                                                        let resolved = context.query_registered(
                                                            &resolve_for_declaration_import,
                                                            ResolveImportKey {
                                                                occurrence: crate::ImportOccurrenceKey::from_directive(&site),
                                                                mode: ImportDemandMode::Rooted,
                                                            },
                                                        )?;
                                                        let rue_query::QueryOutcome::Success(resolved) =
                                                            resolved.outcome()
                                                        else {
                                                            unreachable!("ResolveImport publishes typed values")
                                                        };
                                                        if !resolved.site_found {
                                                            DeclarationImportQueryValue::Failure(
                                                                DeclarationImportFailure::ParserCapabilityMismatch(
                                                                    key.0.clone(),
                                                                ),
                                                            )
                                                        } else if let Some(resolution) =
                                                            &resolved.resolution
                                                        {
                                                            DeclarationImportQueryValue::Available(
                                                                resolution.clone(),
                                                            )
                                                        } else {
                                                            DeclarationImportQueryValue::Failure(
                                                                DeclarationImportFailure::ResolutionUnavailable(
                                                                    key.0.clone(),
                                                                ),
                                                            )
                                                        }
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, DeclarationImportQueryValue::Available(_)) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the DeclarationImport family has one canonical name");
        let shells_for_semantic_nucleus = declaration_shells.clone();
        let shells_for_produced_anonymous = declaration_shells.clone();
        let signatures_for_semantic_nucleus = raw_declaration_signatures.clone();
        let consts_for_semantic_nucleus = raw_const_syntax.clone();
        let bodies_for_semantic_nucleus = raw_declaration_bodies.clone();
        let names_for_semantic_nucleus = lookup_names.clone();
        let imports_for_semantic_nucleus = declaration_imports.clone();
        // Body transactions are supplied per request, but their successful
        // producer-owned anonymous projection has one registered evaluator so
        // SemanticNucleus can observe it without re-running body semantics.
        let body_transactions = runtime
            .family_with_equality(
                "compiler.body-transaction",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::transaction_equal,
            )
            .expect("the BodyTransaction family has one canonical name");
        let transactions_for_produced_anonymous = body_transactions.clone();
        let semantic_nucleus_for_produced_anonymous =
            Arc::new(std::sync::OnceLock::<SemanticNucleusFamily>::new());
        let semantic_nucleus_for_produced_anonymous_evaluator =
            semantic_nucleus_for_produced_anonymous.clone();
        let body_produced_anonymous = runtime
            .family_with_equality_and_evaluator(
                "compiler.body-produced-anonymous",
                BODY_QUERY_MEMO_RETENTION,
                crate::body_query::produced_anonymous_equal,
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    match context.query(&transactions_for_produced_anonymous, key.clone(), |_| {
                        Err(QueryAbort::Canceled)
                    }) {
                        Ok(transaction) => {
                            let rue_query::QueryOutcome::Success(
                                crate::body_query::BodyTransaction::Success {
                                    produced_anonymous_nominals,
                                    ..
                                },
                            ) = transaction.outcome()
                            else {
                                return Err(QueryAbort::Canceled);
                            };
                            return Ok(QueryOutput::success(
                                crate::body_query::ProducedAnonymous::Produced(
                                    produced_anonymous_nominals.clone(),
                                ),
                            ));
                        }
                        Err(QueryAbort::Canceled) => {}
                        Err(abort) => return Err(abort),
                    }

                    // A declaration signature can name the result of a
                    // compile-time type constructor before body reachability
                    // has supplied that constructor's body transaction. Keep
                    // the fact producer-owned by publishing the constructor's
                    // exact semantic projection through this family; the
                    // AnonymousNominal consumer still has one canonical body-
                    // produced dependency path.
                    let Some(definition) = function_definition_key(&key.instance).cloned() else {
                        return Err(QueryAbort::Canceled);
                    };
                    let Some(declaration) = declaration_candidate_for_stable_key(&definition)
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: declaration.clone(),
                        configuration: key.configuration.clone(),
                    };
                    let shell = context.query_registered(
                        &shells_for_produced_anonymous,
                        DeclarationShellQueryKey(declaration),
                    )?;
                    let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(
                        shell,
                    )) = shell.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let semantic_nucleus = semantic_nucleus_for_produced_anonymous_evaluator
                        .get()
                        .expect("SemanticNucleus is installed before requests begin");
                    let signature = context.query_registered(
                        semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::Signature(
                            producer.clone(),
                        ),
                    )?;
                    let rue_query::QueryOutcome::Success(
                        crate::semantic_query_nucleus::SemanticNucleusValue::Signature(signature),
                    ) = signature.outcome()
                    else {
                        return Err(QueryAbort::Canceled);
                    };
                    let Some(call) = comptime_call_for_anonymous_function(
                        &producer,
                        &key.instance,
                        shell,
                        signature,
                    ) else {
                        return Err(QueryAbort::Canceled);
                    };
                    let projected = context.query_registered(
                        semantic_nucleus,
                        crate::semantic_query_nucleus::SemanticNucleusKey::ComptimeCall(call),
                    )?;
                    let projected = match projected.outcome() {
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::ComptimeCall(
                                projected,
                            ),
                        ) => projected,
                        // A COMMITTED internal-error (E9000-class) failure — the
                        // anchored-fragment anchor-transport invariant violation
                        // (missing/duplicate/kind-mismatch, RUE-1089) — is a
                        // corrupt-input fact about a raw fragment terminal that
                        // already exists. It must never collapse into a
                        // retryable `Canceled` abort that a consuming body treats
                        // as "producer unavailable" and rescues by recomputing
                        // the identity from RIR. Carry it so every consumer fails
                        // closed; the transported anchor stays the sole identity
                        // authority.
                        rue_query::QueryOutcome::Success(
                            crate::semantic_query_nucleus::SemanticNucleusValue::Failure(failure),
                        ) if semantic_nucleus_failure_is_internal_error(failure) => {
                            return Ok(QueryOutput::success(
                                crate::body_query::ProducedAnonymous::ProducerFailed(
                                    failure.clone(),
                                ),
                            ));
                        }
                        // Any other non-`ComptimeCall` outcome (an ordinary
                        // domain failure, or a genuinely-unavailable producer)
                        // stays a retryable `Canceled` abort, unchanged.
                        _ => return Err(QueryAbort::Canceled),
                    };
                    let owner = crate::StableProducerId::Function(Box::new(key.instance.clone()));
                    let owned = projected
                        .anonymous_nominals
                        .iter()
                        .filter(|nominal| nominal.identity.producer == owner)
                        .cloned()
                        .collect::<Vec<_>>();
                    if owned.is_empty() {
                        return Err(QueryAbort::Canceled);
                    }
                    Ok(QueryOutput::success(
                        crate::body_query::ProducedAnonymous::Produced(
                            crate::body_query::BodyProducedAnonymousNominals(owned.into()),
                        ),
                    ))
                },
            )
            .expect("the BodyProducedAnonymous family has one canonical name");
        let produced_anonymous_for_semantic_nucleus = body_produced_anonymous.clone();
        let semantic_nucleus = runtime
            .family_with_equality_and_evaluator(
                "compiler.semantic-nucleus",
                declaration_memo_retention,
                |left: &crate::semantic_query_nucleus::SemanticNucleusValue,
                 right: &crate::semantic_query_nucleus::SemanticNucleusValue| left == right,
                move |context, family, key: &crate::semantic_query_nucleus::SemanticNucleusKey| {
                    use crate::semantic_query_nucleus::{
                        SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
                        SemanticNucleusValue as Value,
                    };

                    let shell = context.query_registered(
                        &shells_for_semantic_nucleus,
                        DeclarationShellQueryKey(key.declaration().clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(shell) = shell.outcome() else {
                        unreachable!("DeclarationShell publishes typed values")
                    };
                    let shell = match shell {
                        DeclarationShellQueryValue::Available(shell) => shell,
                        DeclarationShellQueryValue::Failure(failure) => {
                            let value = Value::Failure(Failure::Shell(Arc::from(format!(
                                "{failure:?}"
                            ))));
                            return Ok(QueryOutput::success(value)
                                .with_terminal_kind(QueryTerminalKind::Failure));
                        }
                    };
                    let value = match key {
                        #[cfg(test)]
                        Key::EngineCycleProbe(_) => {
                            let _ = context.query_registered(family, key.clone())?;
                            unreachable!("engine cycle probe must abort before publication")
                        }
                        Key::Identity(query) => {
                            if query.declaration.category
                                == crate::declaration_candidate::DeclarationCandidateCategory::Destructor
                            {
                                let checked = context.query_registered(
                                    family,
                                    Key::Signature(query.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(checked) = checked.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match checked {
                                    Value::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(
                                            failure.clone(),
                                        ))
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    Value::Signature(_) => {}
                                    _ => {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            "destructor validity returned the wrong projection",
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                }
                            }
                            if let Some(identity) =
                                crate::semantic_query_nucleus::direct_identity(shell)
                            {
                                Value::Identity(identity)
                            } else {
                                let resolved = context.query_registered(
                                    family,
                                    Key::ConstResolution(query.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(resolved) =
                                    resolved.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match resolved {
                                    Value::ConstResolution(
                                        crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                            key,
                                            ..
                                        }
                                        | crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                                            key,
                                            ..
                                        },
                                    ) => Value::Identity(
                                        crate::semantic_query_nucleus::DeclarationIdentityProjection {
                                            key: key.clone(),
                                            is_public: shell.is_public,
                                        },
                                    ),
                                    Value::Failure(failure) => Value::Failure(failure.clone()),
                                    _ => Value::Failure(Failure::Resolution(Arc::from(
                                        "const identity dependency returned the wrong projection",
                                    ))),
                                }
                            }
                        }
                        Key::Signature(query) => {
                            if query.declaration.category
                                == crate::declaration_candidate::DeclarationCandidateCategory::Destructor
                            {
                                let named_types = context.query_registered(
                                    &names_for_semantic_nucleus,
                                    LookupNameKey {
                                        module: query.declaration.module.clone(),
                                        namespace: DefinitionNamespace::ModuleItem,
                                        name: query.declaration.name.clone(),
                                    },
                                )?;
                                let rue_query::QueryOutcome::Success(LookupNameValue(named_types)) =
                                    named_types.outcome()
                                else {
                                    unreachable!("LookupName publishes typed values")
                                };
                                let named_types = match named_types {
                                    Ok(named_types) => named_types,
                                    Err(failure) => {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            format!("{failure:?}"),
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                };
                                if !named_types
                                    .iter()
                                    .any(|fact| fact.kind == DefinitionKind::Struct)
                                {
                                    let value = Value::Failure(Failure::Diagnostic(
                                        rue_air::declaration_validation::destructor_unknown_type(
                                            &query.declaration.name,
                                        ),
                                    ));
                                    return Ok(QueryOutput::success(value)
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                }

                                let destructors = context.query_registered(
                                    &names_for_semantic_nucleus,
                                    LookupNameKey {
                                        module: query.declaration.module.clone(),
                                        namespace: DefinitionNamespace::Destructor,
                                        name: query.declaration.name.clone(),
                                    },
                                )?;
                                let rue_query::QueryOutcome::Success(LookupNameValue(destructors)) =
                                    destructors.outcome()
                                else {
                                    unreachable!("LookupName publishes typed values")
                                };
                                let destructors = match destructors {
                                    Ok(destructors) => destructors,
                                    Err(failure) => {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            format!("{failure:?}"),
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                };
                                if destructors
                                    .iter()
                                    .filter(|fact| fact.kind == DefinitionKind::Destructor)
                                    .nth(1)
                                    .is_some()
                                {
                                    let mut duplicate = query.declaration.clone();
                                    duplicate.duplicate_discriminator = 1;
                                    let value = Value::Failure(
                                        Failure::DiagnosticAtDeclaration {
                                            kind: rue_air::declaration_validation::duplicate_destructor(
                                                &query.declaration.name,
                                            ),
                                            declaration: duplicate,
                                        },
                                    );
                                    return Ok(QueryOutput::success(value)
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                }
                            }
                            let raw = context.query_registered(
                                &signatures_for_semantic_nucleus,
                                RawDeclarationSignatureQueryKey(query.declaration.clone()),
                            )?;
                            let rue_query::QueryOutcome::Success(raw) = raw.outcome() else {
                                unreachable!("RawDeclarationSignature publishes typed values")
                            };
                            match raw {
                                RawDeclarationSignatureQueryValue::Failure(failure) => {
                                    Value::Failure(Failure::Syntax(Arc::from(format!(
                                        "{failure:?}"
                                    ))))
                                }
                                RawDeclarationSignatureQueryValue::Available(raw) => {
                                    match crate::semantic_query_nucleus::parse_semantic_signature(
                                        &query.declaration,
                                        raw,
                                    ) {
                                        Ok(parsed) => {
                                            if let crate::semantic_query_nucleus::ParsedSemanticSignature::Callable {
                                                parameters,
                                                ..
                                            } = &parsed
                                                && let Some((kind, ordinal)) = rue_air::declaration_validation::duplicate_parameter_with_ordinal(
                                                    parameters.iter().map(|parameter| &parameter.name),
                                                )
                                            {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::DiagnosticAtParameter {
                                                        kind,
                                                        ordinal: ordinal as u32,
                                                    },
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            if matches!(
                                                query.declaration.category,
                                                crate::declaration_candidate::DeclarationCandidateCategory::Function
                                                    | crate::declaration_candidate::DeclarationCandidateCategory::ExternFunction
                                            ) && let Some(kind) = rue_air::declaration_validation::reserved_function_name(
                                                &query.declaration.name,
                                            ) {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Diagnostic(kind),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            let mut substitutions = BTreeMap::new();
                                            if let Some(owner) = &query.declaration.owner {
                                                let owner_candidate = crate::declaration_candidate::DeclarationCandidateKey {
                                                    module: query.declaration.module.clone(),
                                                    category: owner.category,
                                                    name: owner.name.clone(),
                                                    owner: None,
                                                    duplicate_discriminator: 0,
                                                };
                                                let owner = context.query_registered(
                                                    family,
                                                    Key::Identity(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                                                        declaration: owner_candidate,
                                                        configuration: query.configuration.clone(),
                                                    }),
                                                )?;
                                                let rue_query::QueryOutcome::Success(owner) = owner.outcome() else {
                                                    unreachable!("SemanticNucleus publishes typed values")
                                                };
                                                if let Value::Identity(owner) = owner {
                                                    substitutions.insert(
                                                        Arc::from("Self"),
                                                        crate::durable_semantics::DurableType::Nominal(owner.key.clone()),
                                                    );
                                                }
                                            }
                                            let dependency_source = crate::semantic_query_nucleus::direct_identity(shell)
                                                .expect("signature shell has a direct identity")
                                                .key;
                                            let mut provider = SemanticNucleusTypeProvider {
                                                context,
                                                family,
                                                shells: &shells_for_semantic_nucleus,
                                                names: &names_for_semantic_nucleus,
                                                configuration: query.configuration.clone(),
                                                substitutions,
                                                value_substitutions: BTreeMap::new(),
                                                deferred_value_parameters: BTreeMap::new(),
                                                anonymous_nominals: BTreeMap::new(),
                                                dependency_source,
                                                dependency_kind: rue_air::DeclarationTypeDependencyKind::Signature,
                                                dependencies: BTreeSet::new(),
                                                deferred_ownership: BTreeSet::new(),
                                            };
                                            match resolve_parsed_semantic_signature(
                                                &mut provider,
                                                &query.declaration.module,
                                                &parsed,
                                            ) {
                                                Ok(signature) => Value::Signature(
                                                    crate::semantic_query_nucleus::ResolvedDeclarationSignature {
                                                        signature,
                                                        anonymous_nominals: provider
                                                            .anonymous_nominals
                                                            .values()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        dependencies: provider
                                                            .dependencies
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        deferred_ownership: provider
                                                            .deferred_ownership
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                    },
                                                ),
                                                Err(ResolveSemanticSignatureError::Abort(
                                                    QueryAbort::Cycle(nodes),
                                                )) => Value::Failure(Failure::SignatureReentry {
                                                    signature: provider.dependency_source.clone(),
                                                    cycle: semantic_nucleus_cycle_names(&nodes),
                                                }),
                                                Err(ResolveSemanticSignatureError::Abort(abort)) => {
                                                    return Err(abort)
                                                }
                                                Err(ResolveSemanticSignatureError::Failure(failure)) => {
                                                    Value::Failure(*failure)
                                                }
                                            }
                                        }
                                        Err(failure) => Value::Failure(Failure::Syntax(failure)),
                                    }
                                }
                            }
                        }
                        Key::NominalWellFormedness(query) => {
                            let identity = crate::semantic_query_nucleus::direct_identity(shell)
                                .expect("nominal well-formedness has a direct identity");
                            let mut provider = SemanticNucleusTypeProvider {
                                context,
                                family,
                                shells: &shells_for_semantic_nucleus,
                                names: &names_for_semantic_nucleus,
                                configuration: query.configuration.clone(),
                                substitutions: BTreeMap::new(),
                                value_substitutions: BTreeMap::new(),
                                deferred_value_parameters: BTreeMap::new(),
                                anonymous_nominals: BTreeMap::new(),
                                dependency_source: identity.key.clone(),
                                dependency_kind:
                                    rue_air::DeclarationTypeDependencyKind::Signature,
                                dependencies: BTreeSet::new(),
                                deferred_ownership: BTreeSet::new(),
                            };
                            match provider
                                .validate_nominal_well_formedness(query.declaration.clone())
                            {
                                Ok(()) => Value::NominalWellFormedness,
                                Err(rue_air::SemanticProviderError::Failure(failure)) => {
                                    Value::Failure(failure)
                                }
                                Err(rue_air::SemanticProviderError::Abort(abort)) => {
                                    return Err(abort);
                                }
                            }
                        }
                        Key::DeferredOwnership(query) => {
                            let (dependency_source, anonymous_nominals) = if query
                                .producer
                                .declaration
                                .category
                                == crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate
                            {
                                let resolution = context.query_registered(
                                    family,
                                    Key::ConstResolution(query.producer.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(resolution) =
                                    resolution.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match resolution {
                                    Value::ConstResolution(
                                        crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                            key,
                                            anonymous_nominals,
                                            ..
                                        },
                                    ) => (key.clone(), anonymous_nominals.clone()),
                                    Value::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(
                                            failure.clone(),
                                        ))
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    _ => unreachable!(
                                        "const deferred ownership producer returned the wrong projection"
                                    ),
                                }
                            } else {
                                let signature = context.query_registered(
                                    family,
                                    Key::Signature(query.producer.clone()),
                                )?;
                                let rue_query::QueryOutcome::Success(signature) =
                                    signature.outcome()
                                else {
                                    unreachable!("SemanticNucleus publishes typed values")
                                };
                                match signature {
                                    Value::Signature(signature) => (
                                        crate::semantic_query_nucleus::direct_identity(shell)
                                            .expect(
                                                "deferred ownership producer has a direct identity",
                                            )
                                            .key,
                                        signature.anonymous_nominals.clone(),
                                    ),
                                    Value::Failure(failure) => {
                                        return Ok(QueryOutput::success(Value::Failure(
                                            failure.clone(),
                                        ))
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                    }
                                    _ => unreachable!(
                                        "signature deferred ownership producer returned the wrong projection"
                                    ),
                                }
                            };
                            let mut provider = SemanticNucleusTypeProvider {
                                context,
                                family,
                                shells: &shells_for_semantic_nucleus,
                                names: &names_for_semantic_nucleus,
                                configuration: query.producer.configuration.clone(),
                                substitutions: BTreeMap::new(),
                                value_substitutions: BTreeMap::new(),
                                deferred_value_parameters: BTreeMap::new(),
                                anonymous_nominals: anonymous_nominals
                                    .iter()
                                    .cloned()
                                    .map(|nominal| (nominal.identity.clone(), nominal))
                                    .collect(),
                                dependency_source,
                                dependency_kind:
                                    rue_air::DeclarationTypeDependencyKind::Signature,
                                dependencies: BTreeSet::new(),
                                deferred_ownership: BTreeSet::new(),
                            };
                            let result = match query.gate.kind {
                                crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireDroppable => provider
                                    .type_carries_linear(&query.gate.ty)
                                    .map(|rejected| rejected.then(|| {
                                        rue_error::ErrorKind::ContainerElementIsLinear {
                                            ty: durable_type_diagnostic_name(&query.gate.ty),
                                        }
                                    })),
                                crate::semantic_query_nucleus::DeferredOwnershipGateKind::RequireTriviallyDroppable => provider
                                    .type_has_drop_glue(&query.gate.ty)
                                    .map(|rejected| rejected.then(|| {
                                        rue_error::ErrorKind::ContainerElementNotTriviallyDroppable {
                                            ty: durable_type_diagnostic_name(&query.gate.ty),
                                        }
                                    })),
                            };
                            match result {
                                Ok(Some(kind)) => Value::Failure(Failure::OwnershipGate {
                                    kind,
                                    gate: query.gate.clone(),
                                }),
                                Ok(None) => Value::DeferredOwnership,
                                Err(rue_air::SemanticProviderError::Failure(failure)) => {
                                    Value::Failure(failure)
                                }
                                Err(rue_air::SemanticProviderError::Abort(abort)) => {
                                    return Err(abort)
                                }
                            }
                        }
                        Key::ConstResolution(query) => {
                            let named = context.query_registered(
                                &names_for_semantic_nucleus,
                                LookupNameKey {
                                    module: query.declaration.module.clone(),
                                    namespace: DefinitionNamespace::ModuleItem,
                                    name: query.declaration.name.clone(),
                                },
                            )?;
                            let rue_query::QueryOutcome::Success(LookupNameValue(named)) =
                                named.outcome()
                            else {
                                unreachable!("LookupName publishes typed values")
                            };
                            let named = match named {
                                Ok(named) => named,
                                Err(failure) => {
                                    let value = Value::Failure(Failure::Resolution(Arc::from(
                                        format!("{failure:?}"),
                                    )));
                                    return Ok(QueryOutput::success(value)
                                        .with_terminal_kind(QueryTerminalKind::Failure));
                                }
                            };
                            let const_count = named
                                .iter()
                                .filter(|fact| fact.kind == DefinitionKind::Const)
                                .count();
                            if const_count > 1 {
                                let value = Value::Failure(Failure::Diagnostic(
                                    rue_air::declaration_validation::duplicate_constant(
                                        &query.declaration.name,
                                    ),
                                ));
                                return Ok(QueryOutput::success(value)
                                    .with_terminal_kind(QueryTerminalKind::Failure));
                            }
                            if let Some(kind) =
                                rue_air::declaration_validation::const_cross_kind_collision(
                                    &query.declaration.name,
                                    const_count == 1,
                                    named.iter().any(|fact| fact.kind != DefinitionKind::Const),
                                )
                            {
                                let value = Value::Failure(Failure::Diagnostic(kind));
                                return Ok(QueryOutput::success(value)
                                    .with_terminal_kind(QueryTerminalKind::Failure));
                            }
                            let raw = context.query_registered(
                                &consts_for_semantic_nucleus,
                                RawConstSyntaxQueryKey(query.declaration.clone()),
                            )?;
                            let rue_query::QueryOutcome::Success(raw) = raw.outcome() else {
                                unreachable!("RawConstSyntax publishes typed values")
                            };
                            match raw {
                                RawConstSyntaxQueryValue::Failure(failure) => {
                                    Value::Failure(Failure::Syntax(Arc::from(format!(
                                        "{failure:?}"
                                    ))))
                                }
                                RawConstSyntaxQueryValue::Available(raw) => {
                                    match crate::semantic_query_nucleus::parse_semantic_const(
                                        &query.declaration,
                                        raw,
                                    ) {
                                        Err(failure) => Value::Failure(Failure::Syntax(failure)),
                                        Ok(parsed) => {
                                            let const_identity = crate::semantic_query_nucleus::classified_const_identity(shell, false);
                                            let mut provider = SemanticNucleusTypeProvider {
                                                context,
                                                family,
                                                shells: &shells_for_semantic_nucleus,
                                                names: &names_for_semantic_nucleus,
                                                configuration: query.configuration.clone(),
                                                substitutions: BTreeMap::new(),
                                                value_substitutions: BTreeMap::new(),
                                                deferred_value_parameters: BTreeMap::new(),
                                                anonymous_nominals: BTreeMap::new(),
                                                dependency_source: const_identity.key.clone(),
                                                dependency_kind: rue_air::DeclarationTypeDependencyKind::DeclaredType,
                                                dependencies: BTreeSet::new(),
                                                deferred_ownership: BTreeSet::new(),
                                            };
                                            let expected_type = raw.declared_type.as_deref().and_then(
                                                |syntax| {
                                                    rue_air::resolve_semantic_type_syntax(
                                                        &mut provider,
                                                        &query.declaration.module,
                                                        syntax,
                                                    )
                                                    .ok()
                                                },
                                            );
                                            if matches!(
                                                expected_type,
                                                Some(crate::durable_semantics::DurableType::Slice { .. })
                                            ) {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Diagnostic(
                                                        rue_error::ErrorKind::SliceEscapesScope,
                                                    ),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            }
                                            let result = {
                                                let mut evaluator = SemanticConstEvaluator {
                                                    provider: &mut provider,
                                                    imports: &imports_for_semantic_nucleus,
                                                    declaration: query,
                                                    source: &parsed.source,
                                                    interner: &parsed.interner,
                                                    import_sites: &parsed.import_sites,
                                                    locals: BTreeMap::new(),
                                                    producer: crate::StableProducerId::Definition(
                                                        const_identity.key.clone(),
                                                    ),
                                                    canonical_arguments:
                                                        crate::CanonicalArguments::default(),
                                                    anonymous_sites: &parsed.anonymous_sites,
                                                    next_call: 0,
                                                    expected_type,
                                                };
                                                evaluator.eval(&parsed.declaration.init)
                                            };
                                            match result {
                                                Ok(EvaluatedSemanticConst::Module(target)) => {
                                                    if parsed.declaration.ty.is_some() {
                                                        Value::Failure(Failure::Resolution(
                                                            Arc::from(
                                                                "module binding cannot have a type annotation",
                                                            ),
                                                        ))
                                                    } else {
                                                        let identity = crate::semantic_query_nucleus::classified_const_identity(shell, true);
                                                        Value::ConstResolution(
                                                            crate::semantic_query_nucleus::ConstResolutionProjection::ModuleBinding {
                                                                key: identity.key,
                                                                target,
                                                            },
                                                        )
                                                    }
                                                }
                                                Ok(EvaluatedSemanticConst::Value(typed)) => {
                                                    let typed = Arc::unwrap_or_clone(typed);
                                                    let value = typed.value;
                                                    let resolved_type = match raw.declared_type.as_deref() {
                                                        Some(type_syntax) => rue_air::resolve_semantic_type_syntax(
                                                            &mut provider,
                                                            &query.declaration.module,
                                                            type_syntax,
                                                        ),
                                                        None if matches!(
                                                            value,
                                                            crate::durable_semantics::DurableConstValue::Type(_)
                                                                | crate::durable_semantics::DurableConstValue::Function(_)
                                                        ) => Ok(crate::durable_semantics::DurableType::ComptimeType),
                                                        None => {
                                                            let inferred = inferred_const_type_name(&value);
                                                            return Ok(QueryOutput::success(Value::Failure(
                                                                Failure::DiagnosticWithHelp {
                                                                    kind: rue_error::ErrorKind::ConstMissingTypeAnnotation {
                                                                        name: query.declaration.name.to_string(),
                                                                    },
                                                                    help: Arc::from(format!(
                                                                        "add a type annotation: `const {}: {} = ...;`",
                                                                        query.declaration.name,
                                                                        inferred,
                                                                    )),
                                                                },
                                                            )).with_terminal_kind(QueryTerminalKind::Failure));
                                                        }
                                                    };
                                                    match resolved_type {
                                                        Err(rue_air::SemanticResolutionError::ProviderAbort(abort)) => return Err(abort),
                                                        Err(error) => match semantic_type_query_failure(error) {
                                                            ResolveSemanticSignatureError::Abort(abort) => return Err(abort),
                                                            ResolveSemanticSignatureError::Failure(failure) => Value::Failure(*failure),
                                                        },
                                                        Ok(ty) => {
                                                            let compatible = typed.ty.as_ref().is_none_or(|found| found == &ty)
                                                                && match (&ty, &value) {
                                                                (crate::durable_semantics::DurableType::I8, crate::durable_semantics::DurableConstValue::Integer(value)) => i8::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::I16, crate::durable_semantics::DurableConstValue::Integer(value)) => i16::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::I32, crate::durable_semantics::DurableConstValue::Integer(value)) => i32::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::I64, crate::durable_semantics::DurableConstValue::Integer(value)) => i64::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U8, crate::durable_semantics::DurableConstValue::Integer(value)) => u8::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U16, crate::durable_semantics::DurableConstValue::Integer(value)) => u16::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U32, crate::durable_semantics::DurableConstValue::Integer(value)) => u32::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::U64, crate::durable_semantics::DurableConstValue::Integer(value)) => u64::try_from(*value).is_ok(),
                                                                (crate::durable_semantics::DurableType::Bool, crate::durable_semantics::DurableConstValue::Bool(_))
                                                                | (crate::durable_semantics::DurableType::Unit, crate::durable_semantics::DurableConstValue::Unit)
                                                                | (crate::durable_semantics::DurableType::ComptimeType, crate::durable_semantics::DurableConstValue::Type(_) | crate::durable_semantics::DurableConstValue::Function(_)) => true,
                                                                (crate::durable_semantics::DurableType::BuiltinNominal { name, .. }, crate::durable_semantics::DurableConstValue::String(_)) if name.as_ref() == "str" => true,
                                                                _ => false,
                                                            };
                                                            if compatible {
                                                                let identity = crate::semantic_query_nucleus::classified_const_identity(shell, false);
                                                                Value::ConstResolution(crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                                                    key: identity.key,
                                                                    ty,
                                                                    value: Box::new(value),
                                                                    anonymous_nominals: provider
                                                                        .anonymous_nominals
                                                                        .values()
                                                                        .cloned()
                                                                        .collect::<Vec<_>>()
                                                                        .into(),
                                                                    dependencies: provider
                                                                        .dependencies
                                                                        .iter()
                                                                        .cloned()
                                                                        .collect::<Vec<_>>()
                                                                        .into(),
                                                                    deferred_ownership: provider
                                                                        .deferred_ownership
                                                                        .iter()
                                                                        .cloned()
                                                                        .collect::<Vec<_>>()
                                                                        .into(),
                                                                })
                                                            } else {
                                                                let kind = match (&ty, &value) {
                                                                    (crate::durable_semantics::DurableType::I8
                                                                    | crate::durable_semantics::DurableType::I16
                                                                    | crate::durable_semantics::DurableType::I32
                                                                    | crate::durable_semantics::DurableType::I64
                                                                    | crate::durable_semantics::DurableType::U8
                                                                    | crate::durable_semantics::DurableType::U16
                                                                    | crate::durable_semantics::DurableType::U32
                                                                    | crate::durable_semantics::DurableType::U64,
                                                                    crate::durable_semantics::DurableConstValue::Integer(value)) if *value >= 0 => {
                                                                        rue_error::ErrorKind::LiteralOutOfRange {
                                                                            value: *value as u64,
                                                                            ty: durable_type_diagnostic_name(&ty),
                                                                        }
                                                                    }
                                                                    (crate::durable_semantics::DurableType::I8
                                                                    | crate::durable_semantics::DurableType::I16
                                                                    | crate::durable_semantics::DurableType::I32
                                                                    | crate::durable_semantics::DurableType::I64
                                                                    | crate::durable_semantics::DurableType::U8
                                                                    | crate::durable_semantics::DurableType::U16
                                                                    | crate::durable_semantics::DurableType::U32
                                                                    | crate::durable_semantics::DurableType::U64,
                                                                    crate::durable_semantics::DurableConstValue::Integer(value)) => {
                                                                        rue_error::ErrorKind::ComptimeEvaluationFailed {
                                                                            reason: format!(
                                                                                "value {value} is out of range for type {}",
                                                                                durable_type_diagnostic_name(&ty),
                                                                            ),
                                                                        }
                                                                    }
                                                                    _ => rue_error::ErrorKind::TypeMismatch {
                                                                        expected: durable_type_diagnostic_name(&ty),
                                                                        found: inferred_const_type_name(&value).to_owned(),
                                                                    },
                                                                };
                                                                Value::Failure(Failure::Diagnostic(kind))
                                                            }
                                                        }
                                                    }
                                                }
                                                Ok(EvaluatedSemanticConst::TargetEnum(_)) => {
                                                    Value::Failure(Failure::Resolution(Arc::from(
                                                        "target descriptor must be reduced by a declaration-time branch",
                                                    )))
                                                }
                                                Err(EvaluateSemanticConstError::Failure(failure)) => Value::Failure(*failure),
                                                Err(EvaluateSemanticConstError::Abort(QueryAbort::Cycle(nodes))) => {
                                                    Value::Failure(Failure::Cycle(
                                                        semantic_nucleus_cycle_names(&nodes),
                                                    ))
                                                }
                                                Err(EvaluateSemanticConstError::Abort(abort)) => return Err(abort),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Key::AnonymousNominal(query) => {
                            let projected: Result<
                                Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
                                Failure,
                            > = match &query.identity.producer {
                                crate::StableProducerId::Definition(key) => {
                                    if declaration_candidate_for_stable_key(key).as_ref()
                                        != Some(&query.producer.declaration)
                                    {
                                        Err(Failure::Resolution(Arc::from(
                                            "anonymous nominal producer identity mismatch",
                                        )))
                                    } else {
                                        let resolved = context.query_registered(
                                            family,
                                            Key::ConstResolution(query.producer.clone()),
                                        )?;
                                        let rue_query::QueryOutcome::Success(resolved) =
                                            resolved.outcome()
                                        else {
                                            unreachable!("SemanticNucleus publishes typed values")
                                        };
                                        match resolved {
                                            Value::ConstResolution(
                                                crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                                                    anonymous_nominals,
                                                    ..
                                                },
                                            ) => Ok(anonymous_nominals.clone()),
                                            Value::Failure(failure) => Err(failure.clone()),
                                            _ => Err(Failure::Resolution(Arc::from(
                                                "anonymous nominal const producer returned the wrong projection",
                                            ))),
                                        }
                                    }
                                }
                                crate::StableProducerId::Function(function) => {
                                    let Some(key) = function_definition_key(function) else {
                                        let value = Value::Failure(Failure::Resolution(Arc::from(
                                            "anonymous nominal has an unsupported function producer",
                                        )));
                                        return Ok(QueryOutput::success(value)
                                            .with_terminal_kind(QueryTerminalKind::Failure));
                                    };
                                    if declaration_candidate_for_stable_key(key).as_ref()
                                        != Some(&query.producer.declaration)
                                    {
                                        Err(Failure::Resolution(Arc::from(
                                            "anonymous nominal producer identity mismatch",
                                        )))
                                    } else {
                                        let producer = context.query_registered(
                                            &produced_anonymous_for_semantic_nucleus,
                                            crate::body_query::BodyQueryKey {
                                                instance: (**function).clone(),
                                                configuration: query
                                                    .producer
                                                    .configuration
                                                    .clone(),
                                            },
                                        )?;
                                        let rue_query::QueryOutcome::Success(producer) =
                                            producer.outcome()
                                        else {
                                            unreachable!(
                                                "BodyProducedAnonymous publishes typed values"
                                            )
                                        };
                                        match producer {
                                            crate::body_query::ProducedAnonymous::Produced(
                                                produced,
                                            ) => Ok(produced.0.clone()),
                                            // The producer committed an
                                            // anchor-transport internal error;
                                            // fail closed rather than rescue the
                                            // identity (RUE-1089).
                                            crate::body_query::ProducedAnonymous::ProducerFailed(
                                                failure,
                                            ) => Err(failure.clone()),
                                        }
                                    }
                                }
                            };
                            match projected {
                                Ok(projected) => {
                                    // Producer-nominal identity is exact: the
                                    // producer publishes this precise anchor
                                    // (transported from the frontend), so an exact
                                    // identity match is the only resolution.
                                    projected
                                        .iter()
                                        .find(|nominal| nominal.identity == query.identity)
                                        .cloned()
                                        .map(Value::AnonymousNominal)
                                        .unwrap_or_else(|| {
                                            Value::Failure(Failure::Resolution(Arc::from(
                                                "anonymous nominal producer did not publish the requested identity",
                                            )))
                                        })
                                }
                                Err(failure) => Value::Failure(failure),
                            }
                        }
                        Key::ComptimeCall(call) => {
                            let raw = context.query_registered(
                                &bodies_for_semantic_nucleus,
                                RawDeclarationBodyQueryKey(
                                    call.declaration.declaration.clone(),
                                ),
                            )?;
                            let rue_query::QueryOutcome::Success(raw) = raw.outcome() else {
                                unreachable!("RawDeclarationBody publishes typed values")
                            };
                            match raw {
                                RawDeclarationBodyQueryValue::Failure(failure) => {
                                    Value::Failure(Failure::Syntax(Arc::from(format!(
                                        "{failure:?}"
                                    ))))
                                }
                                RawDeclarationBodyQueryValue::Available(raw) => {
                                    match crate::semantic_query_nucleus::parse_semantic_body(
                                        &call.declaration.declaration,
                                        raw,
                                    ) {
                                        Err(failure) => Value::Failure(Failure::Syntax(failure)),
                                        Ok(parsed) => {
                                            let signature = context.query_registered(
                                                family,
                                                Key::Signature(call.declaration.clone()),
                                            )?;
                                            let rue_query::QueryOutcome::Success(signature) =
                                                signature.outcome()
                                            else {
                                                unreachable!("SemanticNucleus publishes typed values")
                                            };
                                            let Value::Signature(signature) = signature else {
                                                let Value::Failure(failure) = signature else {
                                                    unreachable!("signature query returned the wrong projection")
                                                };
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    failure.clone(),
                                                ))
                                                .with_terminal_kind(QueryTerminalKind::Failure));
                                            };
                                            let crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                                                parameters: callable_parameters,
                                                result: callable_result,
                                                ..
                                            } = &signature.signature else {
                                                return Ok(QueryOutput::success(Value::Failure(
                                                    Failure::Resolution(Arc::from(
                                                        "comptime call target is not callable",
                                                    )),
                                                )).with_terminal_kind(QueryTerminalKind::Failure));
                                            };
                                            let concrete_type_arguments = call
                                                .type_arguments
                                                .iter()
                                                .map(|(_, ty)| ty.clone())
                                                .collect::<Vec<_>>();
                                            let value_parameter_types = callable_parameters
                                                .iter()
                                                .filter(|parameter| {
                                                    parameter.ty
                                                        != crate::durable_semantics::DurableType::ComptimeType
                                                })
                                                .map(|parameter| {
                                                    substitute_durable_generics(
                                                        &parameter.ty,
                                                        &concrete_type_arguments,
                                                    )
                                                })
                                                .collect::<Vec<_>>();
                                            let expected_type = substitute_durable_generics(
                                                callable_result,
                                                &concrete_type_arguments,
                                            );
                                            let substitutions = call
                                                .type_arguments
                                                .iter()
                                                .cloned()
                                                .collect::<BTreeMap<_, _>>();
                                            let value_substitutions = call
                                                .value_arguments
                                                .iter()
                                                .cloned()
                                                .collect::<BTreeMap<_, _>>();
                                            let producer_key = crate::semantic_query_nucleus::direct_identity(shell)
                                                .expect("comptime call shell is callable")
                                                .key;
                                            let mut anonymous_dependencies = BTreeSet::new();
                                            for (_, ty) in call.type_arguments.iter() {
                                                collect_anonymous_nominal_type_dependencies(
                                                    ty,
                                                    &mut anonymous_dependencies,
                                                );
                                            }
                                            for (_, value) in call.value_arguments.iter() {
                                                collect_anonymous_nominal_value_dependencies(
                                                    value,
                                                    &mut anonymous_dependencies,
                                                );
                                            }
                                            let mut anonymous_nominals = BTreeMap::new();
                                            for identity in anonymous_dependencies {
                                                let Some(dependency) = anonymous_nominal_query_key(
                                                    &identity,
                                                    &call.declaration.configuration,
                                                ) else {
                                                    return Ok(QueryOutput::success(Value::Failure(
                                                        Failure::Resolution(Arc::from(
                                                            "anonymous nominal argument has an unsupported producer",
                                                        )),
                                                    ))
                                                    .with_terminal_kind(QueryTerminalKind::Failure));
                                                };
                                                let dependency = context.query_registered(
                                                    family,
                                                    Key::AnonymousNominal(dependency),
                                                )?;
                                                let rue_query::QueryOutcome::Success(dependency) =
                                                    dependency.outcome()
                                                else {
                                                    unreachable!("SemanticNucleus publishes typed values")
                                                };
                                                match dependency {
                                                    Value::AnonymousNominal(value) => {
                                                        anonymous_nominals.insert(
                                                            value.identity.clone(),
                                                            value.clone(),
                                                        );
                                                    }
                                                    Value::Failure(failure) => {
                                                        return Ok(QueryOutput::success(
                                                            Value::Failure(failure.clone()),
                                                        )
                                                        .with_terminal_kind(
                                                            QueryTerminalKind::Failure,
                                                        ));
                                                    }
                                                    _ => {
                                                        return Ok(QueryOutput::success(
                                                            Value::Failure(Failure::Resolution(
                                                                Arc::from(
                                                                    "anonymous nominal dependency returned the wrong projection",
                                                                ),
                                                            )),
                                                        )
                                                        .with_terminal_kind(
                                                            QueryTerminalKind::Failure,
                                                        ));
                                                    }
                                                }
                                            }
                                            let mut provider = SemanticNucleusTypeProvider {
                                                context,
                                                family,
                                                shells: &shells_for_semantic_nucleus,
                                                names: &names_for_semantic_nucleus,
                                                configuration: call
                                                    .declaration
                                                    .configuration
                                                    .clone(),
                                                substitutions,
                                                value_substitutions,
                                                deferred_value_parameters: BTreeMap::new(),
                                                anonymous_nominals,
                                                dependency_source: producer_key.clone(),
                                                dependency_kind: rue_air::DeclarationTypeDependencyKind::Body,
                                                dependencies: BTreeSet::new(),
                                                deferred_ownership: BTreeSet::new(),
                                            };
                                            let canonical_arguments = crate::CanonicalArguments {
                                                types: call
                                                    .type_arguments
                                                    .iter()
                                                    .map(|(_, value)| {
                                                        crate::semantic_identity::type_instance_from_semantic(value)
                                                    })
                                                    .collect::<Option<Vec<_>>>()
                                                    .expect("durable type arguments have canonical identities")
                                                    .into(),
                                                values: call
                                                    .value_arguments
                                                    .iter()
                                                    .map(|(_, value)| {
                                                        crate::semantic_identity::argument_value_from_semantic(value)
                                                    })
                                                    .collect::<Option<Vec<_>>>()
                                                    .expect("durable value arguments have canonical identities")
                                                    .into(),
                                            };
                                            let producer = crate::StableProducerId::Function(Box::new(
                                                crate::FunctionInstanceKey::Specialization {
                                                    base: Box::new(crate::FunctionInstanceKey::Definition(
                                                        producer_key,
                                                    )),
                                                    arguments: canonical_arguments.clone(),
                                                },
                                            ));
                                            let mut locals = BTreeMap::new();
                                            locals.extend(call.type_arguments.iter().map(
                                                |(name, value)| {
                                                    (
                                                        name.clone(),
                                                        EvaluatedSemanticConst::Value(
                                                            TypedSemanticConst::typed(
                                                                crate::durable_semantics::DurableConstValue::Type(value.clone()),
                                                                crate::durable_semantics::DurableType::ComptimeType,
                                                            ),
                                                        ),
                                                    )
                                                },
                                            ));
                                            locals.extend(call.value_arguments.iter().zip(value_parameter_types.iter()).map(
                                                |((name, value), ty)| {
                                                    (
                                                        name.clone(),
                                                        EvaluatedSemanticConst::Value(
                                                            TypedSemanticConst::typed(value.clone(), ty.clone()),
                                                        ),
                                                    )
                                                },
                                            ));
                                            let result = {
                                                let mut evaluator = SemanticConstEvaluator {
                                                    provider: &mut provider,
                                                    imports: &imports_for_semantic_nucleus,
                                                    declaration: &call.declaration,
                                                    source: &parsed.source,
                                                    interner: &parsed.interner,
                                                    import_sites: &parsed.import_sites,
                                                    locals,
                                                    producer,
                                                    canonical_arguments,
                                                    anonymous_sites: &parsed.anonymous_sites,
                                                    next_call: 0,
                                                    expected_type: Some(expected_type),
                                                };
                                                evaluator.eval(&parsed.expression)
                                            };
                                            match result {
                                                Ok(EvaluatedSemanticConst::Value(value))
                                                    if matches!(value.value, crate::durable_semantics::DurableConstValue::Type(_)) =>
                                                {
                                                    let crate::durable_semantics::DurableConstValue::Type(ty) = &value.value else {
                                                        unreachable!()
                                                    };
                                                    Value::ComptimeCall(
                                                    crate::semantic_query_nucleus::ComptimeCallProjection {
                                                        result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Type(ty.clone()),
                                                        anonymous_nominals: provider
                                                            .anonymous_nominals
                                                            .values()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        dependencies: provider
                                                            .dependencies
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                        deferred_ownership: provider
                                                            .deferred_ownership
                                                            .iter()
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .into(),
                                                    },
                                                )
                                                }
                                                Ok(EvaluatedSemanticConst::Value(value)) => {
                                                    let value = Arc::unwrap_or_clone(value);
                                                    Value::ComptimeCall(
                                                        crate::semantic_query_nucleus::ComptimeCallProjection {
                                                            result: crate::semantic_query_nucleus::ComptimeCallResultProjection::Value(value.value),
                                                            anonymous_nominals: provider
                                                                .anonymous_nominals
                                                                .values()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .into(),
                                                            dependencies: provider
                                                                .dependencies
                                                                .iter()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .into(),
                                                            deferred_ownership: provider
                                                                .deferred_ownership
                                                                .iter()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .into(),
                                                        },
                                                    )
                                                }
                                                Ok(EvaluatedSemanticConst::Module(_)) => {
                                                    Value::Failure(Failure::Resolution(Arc::from(
                                                        "comptime function returned a module",
                                                    )))
                                                }
                                                Ok(EvaluatedSemanticConst::TargetEnum(_)) => {
                                                    Value::Failure(Failure::Resolution(Arc::from(
                                                        "comptime function returned an unreduced target descriptor",
                                                    )))
                                                }
                                                Err(EvaluateSemanticConstError::Failure(failure)) => {
                                                    Value::Failure(*failure)
                                                }
                                                Err(EvaluateSemanticConstError::Abort(
                                                    QueryAbort::Cycle(nodes),
                                                )) => Value::Failure(Failure::Cycle(
                                                    semantic_nucleus_cycle_names(&nodes),
                                                )),
                                                Err(EvaluateSemanticConstError::Abort(abort)) => {
                                                    return Err(abort)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    let kind = if matches!(value, Value::Failure(_)) {
                        QueryTerminalKind::Failure
                    } else {
                        QueryTerminalKind::Success
                    };
                    Ok(QueryOutput::success(value).with_terminal_kind(kind))
                },
            )
            .expect("the SemanticNucleus family has one canonical name");
        assert!(
            semantic_nucleus_for_produced_anonymous
                .set(semantic_nucleus.clone())
                .is_ok(),
            "SemanticNucleus producer projection is installed once"
        );
        Self {
            parse: RevisionedFamily::new(&runtime, "compiler.parse"),
            runtime: runtime.clone(),
            next_revision: 1,
            next_source_stamp: 1,
            source_stamps: VecDeque::new(),
            import_store,
            module_store,
            #[cfg(test)]
            test_import_store,
            parse_modules,
            module_indexes,
            module_declaration_sets,
            declaration_occurrence_indexes,
            declaration_shells,
            #[cfg(test)]
            raw_const_syntax,
            #[cfg(test)]
            raw_declaration_signatures,
            raw_declaration_bodies,
            body_transactions,
            canonical_bodies: runtime
                .family_with_equality(
                    "compiler.canonical-body",
                    BODY_QUERY_MEMO_RETENTION,
                    |left: &crate::body_query::CanonicalBody,
                     right: &crate::body_query::CanonicalBody| left == right,
                )
                .expect("the CanonicalBody family has one canonical name"),
            body_references: runtime
                .family_with_equality(
                    "compiler.body-references",
                    BODY_QUERY_MEMO_RETENTION,
                    |left: &crate::body_query::BodyReferences,
                     right: &crate::body_query::BodyReferences| left == right,
                )
                .expect("the BodyReferences family has one canonical name"),
            body_produced_anonymous,
            module_rirs,
            resolve_imports,
            #[cfg(test)]
            declaration_imports,
            semantic_nucleus,
            lookup_names,
            next_import_request: 0,
            current_import_revision: None,
            #[cfg(test)]
            current_test_import_revision: None,
        }
    }
}

impl RevisionedQueryDatabase {
    pub(crate) const SOURCE_INPUT: &'static str = "selected-source";

    pub(crate) fn current_parse_revision(&self) -> Option<Revision> {
        let terminal = self.parse.selection.current()?;
        let rue_query::QueryOutcome::Success(record) = terminal.outcome() else {
            unreachable!("Parse publishes typed records")
        };
        Some(record.runtime_revision())
    }

    /// Revision pin for semantic work. Import discovery republishes the exact
    /// same module leaves together with its observation leaves, so semantic
    /// queries must run on that successor revision when one exists.
    pub(crate) fn current_semantic_revision(&self) -> Option<Revision> {
        self.current_import_revision
            .map(|revision| Revision::new(revision.revision_id, revision.request_generation))
            .or({
                #[cfg(test)]
                {
                    self.current_test_import_revision
                }
                #[cfg(not(test))]
                {
                    None
                }
            })
            .or_else(|| self.current_parse_revision())
    }

    /// Publish an immutable, revisioned import authority for lower-layer
    /// tests. The fixture graph is an input leaf rather than an out-of-band
    /// evaluator read, so changing it invalidates exactly the declaration
    /// import queries which observed it.
    #[cfg(test)]
    pub(crate) fn adopt_test_import_graph(&mut self, graph: crate::CanonicalImportGraph) {
        let parse_revision = self
            .current_parse_revision()
            .expect("test import authority requires a selected parsed revision");
        self.adopt_test_import_graph_for_revision(parse_revision, graph);
    }

    #[cfg(test)]
    fn adopt_test_import_graph_for_revision(
        &mut self,
        parse_revision: Revision,
        graph: crate::CanonicalImportGraph,
    ) {
        let snapshot = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .iter()
            .find(|view| view.revision == parse_revision)
            .expect("selected parse retains its module input view")
            .snapshot
            .clone();
        let revision = Revision::new(self.next_revision, 1);
        self.next_revision += 1;
        let stamp = {
            let mut store = self
                .test_import_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let TestImportInputStore {
                next_stamp, stamps, ..
            } = &mut *store;
            exact_value_stamp(next_stamp, stamps, &graph)
        };
        let mut leaves = vec![(test_import_graph_input(), stamp)];
        leaves.extend(publish_module_inputs(
            &self.module_store,
            revision,
            &snapshot,
        ));
        self.runtime
            .publish_revision(revision, leaves)
            .expect("test import revisions are immutable and uniquely numbered");
        let mut store = self
            .test_import_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store
            .revisions
            .push_back(Arc::new(TestImportInputView { revision, graph }));
        while store.revisions.len() > IMPORT_INPUT_REVISION_RETENTION {
            store.revisions.pop_front();
        }
        let retained = store.revisions.iter().cloned().collect::<Vec<_>>();
        store
            .stamps
            .retain(|(graph, _)| retained.iter().any(|view| &view.graph == graph));
        self.current_test_import_revision = Some(revision);
    }

    /// Request every query-owned declaration shell for the selected parsed
    /// program and attach only the current revision's diagnostic locators.
    pub(crate) fn projected_declaration_shells(
        &self,
        revision: Revision,
        program: &crate::canonical_merge::CanonicalMergedAst,
        cancellation: CancellationToken,
    ) -> Result<Vec<rue_air::SemanticDeclarationShell>, DeclarationShellBatchFailure> {
        let mut shells = Vec::new();
        let mut index_pins = Vec::new();
        let mut shell_pins = Vec::new();
        for module in program.modules() {
            if cancellation.is_canceled() {
                return Err(DeclarationShellBatchFailure::Query(QueryAbort::Canceled));
            }
            let indexed_attempt = self.runtime.request_registered(
                &self.declaration_occurrence_indexes,
                revision,
                ModuleQueryKey(module.module_id().clone()),
                cancellation.clone(),
            );
            let indexed_terminal = indexed_attempt
                .into_result()
                .map_err(DeclarationShellBatchFailure::Query)?;
            index_pins.push(
                self.declaration_occurrence_indexes
                    .pin_terminal(&indexed_terminal)
                    .expect("occurrence terminal belongs to its family"),
            );
            let rue_query::QueryOutcome::Success(indexed) = indexed_terminal.outcome() else {
                unreachable!("DeclarationOccurrenceIndex publishes typed values")
            };
            let index = match indexed {
                DeclarationOccurrenceIndexValue::Available(index) => index,
                DeclarationOccurrenceIndexValue::Failure(failure) => {
                    return Err(DeclarationShellBatchFailure::Stable(
                        crate::declaration_candidate::DeclarationShellFailure::OccurrencesUnavailable(
                            failure.clone(),
                        ),
                    ));
                }
            };
            for capability in index.capabilities.values() {
                let key = capability.key();
                let crate::declaration_candidate::DeclarationOccurrenceCapability::Exact { .. } =
                    capability
                else {
                    return Err(DeclarationShellBatchFailure::Stable(
                        crate::declaration_candidate::DeclarationShellFailure::Ambiguous(
                            key.clone(),
                        ),
                    ));
                };
                if cancellation.is_canceled() {
                    return Err(DeclarationShellBatchFailure::Query(QueryAbort::Canceled));
                }
                let attempt = self.runtime.request_registered(
                    &self.declaration_shells,
                    revision,
                    DeclarationShellQueryKey(key.clone()),
                    cancellation.clone(),
                );
                let terminal = attempt
                    .into_result()
                    .map_err(DeclarationShellBatchFailure::Query)?;
                shell_pins.push(
                    self.declaration_shells
                        .pin_terminal(&terminal)
                        .expect("shell terminal belongs to its family"),
                );
                let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                    unreachable!("DeclarationShell publishes typed values")
                };
                let fact = match value {
                    DeclarationShellQueryValue::Available(fact) => fact,
                    DeclarationShellQueryValue::Failure(failure) => {
                        return Err(DeclarationShellBatchFailure::Stable(failure.clone()));
                    }
                };
                let Some(locator) = module.definitions().declaration_locator(&fact.key) else {
                    return Err(DeclarationShellBatchFailure::Stable(
                        crate::declaration_candidate::DeclarationShellFailure::ParserCapabilityMismatch(
                            fact.key.clone(),
                        ),
                    ));
                };
                shells.push(project_semantic_shell(
                    fact,
                    locator.declaration_span,
                    locator.source_order,
                ));
            }
        }
        if cancellation.is_canceled() {
            return Err(DeclarationShellBatchFailure::Query(QueryAbort::Canceled));
        }
        Ok(shells)
    }

    /// Materialize the declaration payloads required by the current body
    /// adapter exclusively from the keyed semantic nucleus. This deliberately
    /// thin root projection is transitional until RUE-1027 makes body requests
    /// drive declaration reachability: each declaration remains an
    /// independently stamped query terminal and no whole-program semantic
    /// state is retained here.
    pub(crate) fn body_transaction(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        declaration_candidates: Arc<
            BTreeMap<
                crate::StableDefinitionKey,
                crate::declaration_candidate::DeclarationCandidateKey,
            >,
        >,
        declaration_modules: Arc<[ModuleId]>,
        producer_body_terminal_required: bool,
        cancellation: CancellationToken,
        compute: impl FnOnce(
            Arc<[crate::durable_semantics::DurableAnonymousNominal]>,
        ) -> Result<crate::body_query::BodyTransaction, QueryAbort>,
    ) -> Result<
        Arc<rue_query::QueryTerminal<crate::body_query::BodyTransaction>>,
        BodyTransactionRequestFailure,
    > {
        let definition = body_source_definition_key(&key.instance)
            .cloned()
            .ok_or(BodyTransactionRequestFailure::Query(QueryAbort::Canceled))?;
        let candidate = declaration_candidate_for_stable_key(&definition)
            .ok_or(BodyTransactionRequestFailure::Query(QueryAbort::Canceled))?;
        let deferred_anonymous_producers = std::cell::RefCell::new(BTreeSet::new());
        // Set when a depended-on anonymous producer committed an anchor-transport
        // internal error (RUE-1089). It is carried out of the query closure — which
        // can only signal a bare `QueryAbort` — and mapped to a fatal
        // `ProducerFailed` at the request boundary, so the corrupt producer sinks
        // this body instead of being retried or rescued by RIR recomputation.
        let producer_transport_failure: std::cell::RefCell<
            Option<crate::semantic_query_nucleus::SemanticNucleusFailure>,
        > = std::cell::RefCell::new(None);
        let result = self.runtime.query(
            &self.body_transactions,
            revision,
            key.clone(),
            cancellation,
            |context| {
                let raw = context.query_registered(
                    &self.raw_declaration_bodies,
                    RawDeclarationBodyQueryKey(candidate),
                )?;
                let rue_query::QueryOutcome::Success(RawDeclarationBodyQueryValue::Available(_)) =
                    raw.outcome()
                else {
                    return Err(QueryAbort::Canceled);
                };
                // The one-body resolver consumes declaration-set selection,
                // including negative and qualified lookups that do not become
                // positive BodyReferences. Until it publishes those exact
                // lookup keys as a projection, conservatively observe the
                // already-canonical, position-free declaration-set
                // projections admitted by this body epoch. This is a query
                // dependency only: no source/RIR rescanning or peer name
                // resolver is introduced.
                let _ = context.optional_input(accepted_import_topology_input());
                for module in declaration_modules.iter().cloned() {
                    let _ = context.query_registered(
                        &self.module_declaration_sets,
                        ModuleQueryKey(module),
                    )?;
                }
                let mut selected_anonymous = BTreeMap::new();
                let mut pending_anonymous = collect_instance_anonymous_nominals(&key.instance);
                while let Some(identity) = pending_anonymous.pop_first() {
                    if let crate::StableProducerId::Function(function) = &identity.producer
                        && function.as_ref() != &key.instance
                        && (producer_body_terminal_required
                            || !matches!(
                                key.instance,
                                crate::FunctionInstanceKey::AnonymousMember { .. }
                            ))
                    {
                        let produced = match context.query_registered(
                            &self.body_produced_anonymous,
                            crate::body_query::BodyQueryKey {
                                instance: (**function).clone(),
                                configuration: key.configuration.clone(),
                            },
                        ) {
                            Ok(produced) => produced,
                            Err(QueryAbort::Canceled) => {
                                deferred_anonymous_producers
                                    .borrow_mut()
                                    .insert((**function).clone());
                                return Err(QueryAbort::Canceled);
                            }
                            Err(abort) => return Err(abort),
                        };
                        let rue_query::QueryOutcome::Success(produced) = produced.outcome() else {
                            unreachable!("BodyProducedAnonymous publishes typed values")
                        };
                        let produced = match produced {
                            crate::body_query::ProducedAnonymous::Produced(produced) => produced,
                            crate::body_query::ProducedAnonymous::ProducerFailed(failure) => {
                                *producer_transport_failure.borrow_mut() = Some(failure.clone());
                                return Err(QueryAbort::Canceled);
                            }
                        };
                        selected_anonymous.extend(
                            produced
                                .0
                                .iter()
                                .cloned()
                                .map(|nominal| (nominal.identity.clone(), nominal)),
                        );
                    }
                    if !selected_anonymous.contains_key(&identity) {
                        let query = anonymous_nominal_query_key(&identity, &key.configuration)
                            .ok_or(QueryAbort::Canceled)?;
                        let nominal = context.query_registered(
                            &self.semantic_nucleus,
                            crate::semantic_query_nucleus::SemanticNucleusKey::AnonymousNominal(
                                query,
                            ),
                        )?;
                        let rue_query::QueryOutcome::Success(nominal) = nominal.outcome() else {
                            unreachable!("SemanticNucleus publishes typed values")
                        };
                        let crate::semantic_query_nucleus::SemanticNucleusValue::AnonymousNominal(
                            nominal,
                        ) = nominal
                        else {
                            return Err(QueryAbort::Canceled);
                        };
                        selected_anonymous.insert(nominal.identity.clone(), nominal.clone());
                    }
                    if let Some(nominal) = selected_anonymous.get(&identity) {
                        let mut dependencies = BTreeSet::new();
                        collect_durable_anonymous_nominal_dependencies(
                            nominal,
                            &mut dependencies,
                        );
                        pending_anonymous.extend(
                            dependencies
                                .into_iter()
                                .filter(|dependency| !selected_anonymous.contains_key(dependency)),
                        );
                    }
                }
                let transaction = compute(
                    selected_anonymous
                        .into_values()
                        .collect::<Vec<_>>()
                        .into(),
                )?;
                let mut semantic_dependencies = BTreeSet::from([definition]);
                let mut anonymous_dependencies = BTreeSet::new();
                for reference in transaction.references().0.iter() {
                    match reference {
                        crate::body_query::BodyReference::Callable(function) => {
                            if let Some(definition) = function_definition_key(function) {
                                semantic_dependencies.insert(definition.clone());
                            }
                            anonymous_dependencies
                                .extend(collect_instance_anonymous_nominals(function));
                        }
                        crate::body_query::BodyReference::Definition(definition) => {
                            semantic_dependencies.insert(definition.clone());
                        }
                        crate::body_query::BodyReference::Type(ty) => {
                            if let crate::TypeInstanceKey::Nominal(
                                crate::NominalInstanceKey::Named(definition),
                            ) = ty
                            {
                                semantic_dependencies.insert(definition.clone());
                            }
                            anonymous_dependencies.extend(collect_instance_anonymous_nominals(
                                &crate::FunctionInstanceKey::DropGlue(Box::new(ty.clone())),
                            ));
                        }
                    }
                }
                if let crate::body_query::BodyTransaction::Success {
                    produced_anonymous_nominals,
                    ..
                } = &transaction
                {
                    // A body transaction owns the anonymous facts it creates.
                    // Asking the semantic nucleus for one of those facts would
                    // depend back on this transaction's produced-facts
                    // projection and form a query cycle. References to facts
                    // produced by another body still observe that producer
                    // through the semantic nucleus below.
                    for nominal in produced_anonymous_nominals.0.iter() {
                        if matches!(
                            &nominal.identity.producer,
                            crate::StableProducerId::Function(producer)
                                if producer.as_ref() == &key.instance
                        ) {
                            anonymous_dependencies.remove(&nominal.identity);
                        }
                    }
                }
                for identity in anonymous_dependencies {
                    let query = anonymous_nominal_query_key(&identity, &key.configuration)
                        .ok_or(QueryAbort::Canceled)?;
                    match &identity.producer {
                        crate::StableProducerId::Definition(_) => {
                            let _ = context.query_registered(
                                &self.semantic_nucleus,
                                crate::semantic_query_nucleus::SemanticNucleusKey::AnonymousNominal(
                                    query,
                                ),
                            )?;
                        }
                        crate::StableProducerId::Function(producer) => {
                            let produced = match context.query_registered(
                                &self.body_produced_anonymous,
                                crate::body_query::BodyQueryKey {
                                    instance: (**producer).clone(),
                                    configuration: key.configuration.clone(),
                                },
                            ) {
                                Ok(produced) => produced,
                                Err(QueryAbort::Canceled) => {
                                    deferred_anonymous_producers
                                        .borrow_mut()
                                        .insert((**producer).clone());
                                    return Err(QueryAbort::Canceled);
                                }
                                Err(abort) => return Err(abort),
                            };
                            let rue_query::QueryOutcome::Success(produced) = produced.outcome()
                            else {
                                unreachable!("BodyProducedAnonymous publishes typed values")
                            };
                            let produced = match produced {
                                crate::body_query::ProducedAnonymous::Produced(produced) => produced,
                                crate::body_query::ProducedAnonymous::ProducerFailed(failure) => {
                                    *producer_transport_failure.borrow_mut() = Some(failure.clone());
                                    return Err(QueryAbort::Canceled);
                                }
                            };
                            if !produced.0.iter().any(|nominal| nominal.identity == identity) {
                                return Err(QueryAbort::Canceled);
                            }
                        }
                    }
                }
                // Positive semantic references observe their exact nucleus
                // terminals; declaration-set lookup above separately covers
                // negative and qualified lookup inputs.
                for definition in semantic_dependencies {
                    // Synthetic builtins have stable semantic identities but
                    // no source declaration candidate. Their semantics are
                    // fixed by the compiler build and the target/preview
                    // configuration already carried by the body key.
                    let candidate = declaration_candidates.get(&definition).or_else(|| {
                        matches!(
                            definition.kind(),
                            crate::StableDefinitionKind::ValueConst
                                | crate::StableDefinitionKind::ModuleBinding
                        )
                        .then(|| {
                            declaration_candidates.iter().find_map(|(candidate, value)| {
                                (candidate.module() == definition.module()
                                    && candidate.name() == definition.name()
                                    && candidate.owner() == definition.owner()
                                    && matches!(
                                        candidate.kind(),
                                        crate::StableDefinitionKind::ValueConst
                                            | crate::StableDefinitionKind::ModuleBinding
                                    ))
                                .then_some(value)
                            })
                        })
                        .flatten()
                    });
                    let Some(candidate) = candidate.cloned() else {
                        continue;
                    };
                    let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                        declaration: candidate.clone(),
                        configuration: key.configuration.clone(),
                    };
                    if candidate.category
                        == crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate
                    {
                        let _ = context.query_registered(
                            &self.semantic_nucleus,
                            crate::semantic_query_nucleus::SemanticNucleusKey::ConstResolution(
                                query,
                            ),
                        )?;
                    } else {
                        let _ = context.query_registered(
                            &self.semantic_nucleus,
                            crate::semantic_query_nucleus::SemanticNucleusKey::Identity(
                                query.clone(),
                            ),
                        )?;
                        let _ = context.query_registered(
                            &self.semantic_nucleus,
                            crate::semantic_query_nucleus::SemanticNucleusKey::Signature(query),
                        )?;
                    }
                }
                let kind = if matches!(transaction, crate::body_query::BodyTransaction::Success { .. }) {
                    QueryTerminalKind::Success
                } else {
                    QueryTerminalKind::Failure
                };
                Ok(QueryOutput::success(transaction).with_terminal_kind(kind))
            },
        );
        match result {
            Ok(terminal) => Ok(terminal),
            // A committed anchor-transport internal error takes precedence over a
            // deferral: the producer definitively failed, so this body must fail
            // closed rather than reschedule the producer forever (RUE-1089).
            Err(QueryAbort::Canceled) if producer_transport_failure.borrow().is_some() => {
                Err(BodyTransactionRequestFailure::ProducerFailed(
                    producer_transport_failure
                        .into_inner()
                        .expect("guarded by is_some"),
                ))
            }
            Err(QueryAbort::Canceled) if !deferred_anonymous_producers.borrow().is_empty() => {
                Err(BodyTransactionRequestFailure::DeferredAnonymousProducers(
                    deferred_anonymous_producers
                        .into_inner()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .into(),
                ))
            }
            Err(abort) => Err(BodyTransactionRequestFailure::Query(abort)),
        }
    }

    pub(crate) fn canonical_body_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::CanonicalBody>>, QueryAbort> {
        let transactions = self.body_transactions.clone();
        self.runtime.query(
            &self.canonical_bodies,
            revision,
            key.clone(),
            cancellation,
            move |context| {
                let transaction =
                    context.query(&transactions, key, |_| Err(QueryAbort::Canceled))?;
                let rue_query::QueryOutcome::Success(crate::body_query::BodyTransaction::Success {
                    body,
                    ..
                }) = transaction.outcome()
                else {
                    return Err(QueryAbort::Canceled);
                };
                Ok(QueryOutput::success(body.as_ref().clone()))
            },
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn body_references_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::BodyReferences>>, QueryAbort> {
        let transactions = self.body_transactions.clone();
        self.runtime.query(
            &self.body_references,
            revision,
            key.clone(),
            cancellation,
            move |context| {
                let transaction =
                    context.query(&transactions, key, |_| Err(QueryAbort::Canceled))?;
                let rue_query::QueryOutcome::Success(transaction) = transaction.outcome() else {
                    unreachable!("BodyTransaction publishes typed values")
                };
                Ok(QueryOutput::success(transaction.references().clone()))
            },
        )
    }

    pub(crate) fn body_produced_anonymous_projection(
        &self,
        revision: Revision,
        key: crate::body_query::BodyQueryKey,
        cancellation: CancellationToken,
    ) -> Result<Arc<rue_query::QueryTerminal<crate::body_query::ProducedAnonymous>>, QueryAbort>
    {
        self.runtime
            .request_registered(&self.body_produced_anonymous, revision, key, cancellation)
            .into_result()
    }

    pub(crate) fn projected_declaration_semantics(
        &self,
        revision: Revision,
        program: &crate::canonical_merge::CanonicalMergedAst,
        target: rue_target::Target,
        preview_features: &crate::PreviewFeatures,
        cancellation: CancellationToken,
    ) -> Result<SemanticNucleusProjection, SemanticNucleusBatchFailure> {
        use crate::declaration_candidate::{
            DeclarationCandidateCategory as Category, DeclarationOccurrenceCapability,
        };
        use crate::semantic_query_nucleus::{
            DeclarationSemanticQueryKey as DeclarationQuery, DeclarationSemanticValue,
            SemanticNucleusKey as Key, SemanticNucleusValue as Value, SemanticQueryConfiguration,
        };

        let configuration = SemanticQueryConfiguration {
            target,
            preview_features: crate::StablePreviewFeatures::new(preview_features),
        };
        let mut values = Vec::new();
        let mut anonymous_nominals = BTreeMap::new();
        let mut dependencies = BTreeSet::new();
        let mut c_export_roots = BTreeSet::new();
        for module in program.modules() {
            if cancellation.is_canceled() {
                return Err(SemanticNucleusBatchFailure::Query(QueryAbort::Canceled));
            }
            let indexed = self.runtime.request_registered(
                &self.declaration_occurrence_indexes,
                revision,
                ModuleQueryKey(module.module_id().clone()),
                cancellation.clone(),
            );
            let terminal = indexed
                .into_result()
                .map_err(SemanticNucleusBatchFailure::Query)?;
            let rue_query::QueryOutcome::Success(indexed) = terminal.outcome() else {
                unreachable!("DeclarationOccurrenceIndex publishes typed values")
            };
            let DeclarationOccurrenceIndexValue::Available(index) = indexed else {
                let DeclarationOccurrenceIndexValue::Failure(failure) = indexed else {
                    unreachable!()
                };
                return Err(SemanticNucleusBatchFailure::Stable {
                    declaration: None,
                    failure: Box::new(
                        crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(Arc::from(
                            format!("{failure:?}"),
                        )),
                    ),
                });
            };
            for capability in index.capabilities.values() {
                let DeclarationOccurrenceCapability::Exact { .. } = capability else {
                    return Err(SemanticNucleusBatchFailure::Stable {
                        declaration: Some(capability.key().clone()),
                        failure: Box::new(
                            crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(
                                Arc::from(format!(
                                    "ambiguous declaration `{}`",
                                    capability.key().name
                                )),
                            ),
                        ),
                    });
                };
                let declaration = capability.key().clone();
                let query = DeclarationQuery {
                    declaration: declaration.clone(),
                    configuration: configuration.clone(),
                };
                let request = |key: Key| {
                    let attempt = self.runtime.request_registered(
                        &self.semantic_nucleus,
                        revision,
                        key.clone(),
                        cancellation.clone(),
                    );
                    let terminal = attempt
                        .into_result()
                        .map_err(SemanticNucleusBatchFailure::Query)?;
                    let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                        unreachable!("SemanticNucleus publishes typed values")
                    };
                    match value {
                        Value::Failure(failure) => Err(SemanticNucleusBatchFailure::Stable {
                            declaration: Some(declaration.clone()),
                            failure: Box::new(failure.clone()),
                        }),
                        value => Ok(value.clone()),
                    }
                };
                let semantic = if declaration.category == Category::ConstCandidate {
                    let Value::ConstResolution(resolution) =
                        request(Key::ConstResolution(query.clone()))?
                    else {
                        unreachable!("const query returned the wrong projection")
                    };
                    if let crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                        anonymous_nominals: projected,
                        dependencies: projected_dependencies,
                        deferred_ownership,
                        ..
                    } = &resolution
                    {
                        anonymous_nominals.extend(
                            projected
                                .iter()
                                .cloned()
                                .map(|value| (value.identity.clone(), value)),
                        );
                        dependencies.extend(projected_dependencies.iter().cloned());
                        for gate in deferred_ownership.iter() {
                            let Value::DeferredOwnership = request(Key::DeferredOwnership(
                                crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                                    producer: query.clone(),
                                    gate: gate.clone(),
                                },
                            ))?
                            else {
                                unreachable!(
                                    "deferred ownership query returned the wrong projection"
                                )
                            };
                        }
                    }
                    let shell = self.runtime.request_registered(
                        &self.declaration_shells,
                        revision,
                        DeclarationShellQueryKey(declaration.clone()),
                        cancellation.clone(),
                    );
                    let terminal = shell
                        .into_result()
                        .map_err(SemanticNucleusBatchFailure::Query)?;
                    let rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Available(
                        shell,
                    )) = terminal.outcome()
                    else {
                        return Err(SemanticNucleusBatchFailure::Stable {
                            declaration: Some(declaration.clone()),
                            failure: Box::new(
                                crate::semantic_query_nucleus::SemanticNucleusFailure::Shell(
                                    Arc::from("const declaration shell became unavailable"),
                                ),
                            ),
                        });
                    };
                    DeclarationSemanticValue::from_const(shell.is_public, resolution)
                } else {
                    let Value::Identity(identity) = request(Key::Identity(query.clone()))? else {
                        unreachable!("identity query returned the wrong projection")
                    };
                    if matches!(declaration.category, Category::Struct | Category::Enum) {
                        let Value::NominalWellFormedness =
                            request(Key::NominalWellFormedness(query.clone()))?
                        else {
                            unreachable!("nominal well-formedness returned the wrong projection")
                        };
                    }
                    let Value::Signature(signature) = request(Key::Signature(query.clone()))?
                    else {
                        unreachable!("signature query returned the wrong projection")
                    };
                    for gate in signature.deferred_ownership.iter() {
                        let Value::DeferredOwnership = request(Key::DeferredOwnership(
                            crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                                producer: query.clone(),
                                gate: gate.clone(),
                            },
                        ))?
                        else {
                            unreachable!("deferred ownership query returned the wrong projection")
                        };
                    }
                    anonymous_nominals.extend(
                        signature
                            .anonymous_nominals
                            .iter()
                            .cloned()
                            .map(|value| (value.identity.clone(), value)),
                    );
                    dependencies.extend(signature.dependencies.iter().cloned());
                    let is_c_export = matches!(
                        &signature.signature,
                        crate::semantic_query_nucleus::DeclarationSignatureProjection::Callable {
                            is_c_export: true,
                            ..
                        }
                    );
                    let semantic =
                        DeclarationSemanticValue::from_signature(identity, signature.signature);
                    if is_c_export {
                        c_export_roots.insert(semantic.identity.key.clone());
                    }
                    semantic
                };
                values.push(crate::DurableDeclarationSemantic {
                    key: semantic.identity.key,
                    is_public: semantic.identity.is_public,
                    payload: semantic.payload,
                });
            }
        }
        values.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(SemanticNucleusProjection {
            declarations: values.into(),
            anonymous_nominals: anonymous_nominals.into_values().collect::<Vec<_>>().into(),
            dependencies: dependencies.into_iter().collect::<Vec<_>>().into(),
            c_export_roots: c_export_roots.into_iter().collect::<Vec<_>>().into(),
        })
    }

    pub(crate) fn begin_import_inputs(
        &mut self,
        snapshot: &SourceSnapshot,
        context: ImportDiscoveryContext,
        accepted_reads: Arc<[AcceptedReadManifestEntry]>,
    ) -> CompileResult<ImportInputRevision> {
        self.next_import_request += 1;
        let generation = self.next_import_request;
        self.current_import_revision = None;
        // A new request generation is a fresh filesystem observation epoch.
        // Reuse requires a future explicit watch/read-policy proof token. The
        // API deliberately has no carried-ledger input that could be mistaken
        // for freshness authority.
        self.publish_import_view(
            snapshot,
            context,
            accepted_reads,
            ImportObservationLedger::default(),
            generation,
            0,
        )
    }

    pub(crate) fn import_frontier(
        &mut self,
        revision: ImportInputRevision,
        plan: &ImportDiscoveryPlan,
        mode: ImportDemandMode,
        roots: &ImportDemandRoots,
    ) -> CompileResult<ImportDemandFrontier> {
        if self.current_import_revision != Some(revision) {
            return Err(import_input_error(
                "import demand requested from a non-current immutable revision",
            ));
        }
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let view = {
            let store = lock_import_store(&self.import_store);
            store
                .revisions
                .iter()
                .find(|view| view.revision == runtime_revision)
                .cloned()
        }
        .ok_or_else(|| import_input_error("import input revision is no longer retained"))?;
        if plan.context() != &view.context
            || plan.source_revision().modules() != view.sources.as_ref()
        {
            return Err(import_input_error(
                "import plan does not match its pinned granular input revision",
            ));
        }
        if roots.occurrences().iter().any(|occurrence| {
            !plan
                .groups()
                .iter()
                .any(|group| group[0].occurrence() == occurrence)
        }) {
            return Err(import_input_error(
                "import demand roots contain an occurrence outside the pinned plan",
            ));
        }
        let mut requests = Vec::new();
        let mut fanout = Vec::<Vec<ImportDiscoveryRequest>>::new();
        let mut operation_indices = BTreeMap::<ImportHostOperationKey, usize>::new();
        let mut speculative_blocked = false;
        for occurrence in roots.occurrences() {
            let key = ResolveImportKey {
                occurrence: occurrence.clone(),
                mode,
            };
            let attempt = self.runtime.request_registered(
                &self.resolve_imports,
                runtime_revision,
                key,
                CancellationToken::new(),
            );
            let terminal = attempt.terminal().ok_or_else(|| {
                import_input_error(format!(
                    "ResolveImport query aborted: {:?}",
                    attempt.abort()
                ))
            })?;
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("ResolveImport publishes typed success values")
            };
            if !value.site_found {
                return Err(import_input_error(
                    "import demand occurrence is absent from the current parsed module",
                ));
            }
            speculative_blocked |= value.speculative_blocked;
            for request in value.requests.iter() {
                let operation = ImportHostOperationKey::new(request);
                if let Some(index) = operation_indices.get(&operation).copied() {
                    fanout[index].push(request.clone());
                } else {
                    let index = requests.len();
                    operation_indices.insert(operation, index);
                    requests.push(request.clone());
                    fanout.push(vec![request.clone()]);
                }
            }
        }
        Ok(ImportDemandFrontier {
            revision,
            mode,
            requests: requests.into(),
            fanout: fanout
                .into_iter()
                .map(|requests| Arc::<[ImportDiscoveryRequest]>::from(requests))
                .collect::<Vec<_>>()
                .into(),
            speculative_blocked,
        })
    }

    pub(crate) fn current_import_revision(&self) -> Option<ImportInputRevision> {
        self.current_import_revision
    }

    pub(crate) fn exact_import_groups(
        &self,
        revision: ImportInputRevision,
        roots: &ImportDemandRoots,
    ) -> CompileResult<Vec<Arc<[ImportDiscoveryRequest]>>> {
        if self.current_import_revision != Some(revision) {
            return Err(import_input_error(
                "exact import projection requested from a non-current revision",
            ));
        }
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let mut groups = Vec::new();
        for occurrence in roots.occurrences() {
            let attempt = self.runtime.request_registered(
                &self.resolve_imports,
                runtime_revision,
                ResolveImportKey {
                    occurrence: occurrence.clone(),
                    mode: ImportDemandMode::Rooted,
                },
                CancellationToken::new(),
            );
            let terminal = attempt.terminal().ok_or_else(|| {
                import_input_error(format!(
                    "ResolveImport projection aborted: {:?}",
                    attempt.abort()
                ))
            })?;
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("ResolveImport publishes typed values")
            };
            if !value.site_found {
                return Err(import_input_error(
                    "exact import projection occurrence is absent from the current parsed module",
                ));
            }
            groups.extend(value.groups.iter().cloned());
        }
        groups.sort_by(|left, right| left[0].cmp(&right[0]));
        Ok(groups)
    }

    pub(crate) fn publish_import_batch(
        &mut self,
        frontier: &ImportDemandFrontier,
        snapshot: &SourceSnapshot,
        accepted_reads: Arc<[AcceptedReadManifestEntry]>,
        observations: Vec<ImportObservation>,
    ) -> CompileResult<ImportInputRevision> {
        if frontier.mode != ImportDemandMode::Rooted {
            return Err(import_input_error(
                "speculative import work cannot publish host observations",
            ));
        }
        if self.current_import_revision != Some(frontier.revision) {
            return Err(import_input_error(
                "import batch belongs to a stale immutable revision",
            ));
        }
        if observations.len() != frontier.requests.len()
            || observations
                .iter()
                .zip(frontier.requests.iter())
                .any(|(observation, request)| observation.request() != request)
        {
            return Err(import_input_error(
                "host import results must exactly preserve the compiler-produced batch order",
            ));
        }
        let (context, mut ledger) = {
            let store = lock_import_store(&self.import_store);
            let view = store
                .revisions
                .iter()
                .find(|view| view.revision.id() == frontier.revision.revision_id)
                .ok_or_else(|| import_input_error("import input revision is no longer retained"))?;
            (view.context.clone(), view.ledger.clone())
        };
        for (observation, fanout) in observations.into_iter().zip(frontier.fanout.iter()) {
            for request in fanout.iter().cloned() {
                ledger.record(observation.fanout_to(request)?)?;
            }
        }
        self.publish_import_view(
            snapshot,
            context,
            accepted_reads,
            ledger,
            frontier.revision.request_generation,
            frontier.revision.frontier_round + 1,
        )
    }

    pub(crate) fn import_ledger(
        &self,
        revision: ImportInputRevision,
    ) -> CompileResult<ImportObservationLedger> {
        let store = lock_import_store(&self.import_store);
        store
            .revisions
            .iter()
            .find(|view| {
                view.revision.id() == revision.revision_id
                    && view.generation == revision.request_generation
            })
            .map(|view| view.ledger.clone())
            .ok_or_else(|| import_input_error("import input revision is no longer retained"))
    }

    fn publish_import_view(
        &mut self,
        snapshot: &SourceSnapshot,
        context: ImportDiscoveryContext,
        accepted_reads: Arc<[AcceptedReadManifestEntry]>,
        ledger: ImportObservationLedger,
        generation: u64,
        frontier_round: u64,
    ) -> CompileResult<ImportInputRevision> {
        let sources: Arc<[ModuleRevision]> = snapshot.source_revision().modules().to_vec().into();
        let provenance = accepted_reads
            .iter()
            .map(|entry| (entry.module(), entry))
            .collect::<BTreeMap<_, _>>();
        crate::import_discovery::validate_accepted_import_manifest(&accepted_reads)?;
        if provenance.len() != accepted_reads.len() {
            return Err(import_input_error(
                "accepted read manifest contains duplicate logical modules",
            ));
        }
        if sources
            .iter()
            .any(|source| !provenance.contains_key(&source.module))
        {
            return Err(import_input_error(
                "every module source leaf requires accepted-read provenance",
            ));
        }
        if ledger
            .iter()
            .any(|observation| observation.request().context() != &context)
        {
            return Err(import_input_error(
                "import observation belongs to a different discovery epoch",
            ));
        }
        for source in ledger.iter().filter_map(ImportObservation::accepted_source) {
            crate::import_discovery::accepted_import_module(source, &accepted_reads)?;
        }
        let revision = Revision::new(self.next_revision, generation);
        self.next_revision += 1;
        let mut leaves = Vec::new();
        let mut accepted_topology = ledger
            .iter()
            .map(|observation| {
                let request = observation.request();
                let outcome = if let Some(source) = observation.accepted_source() {
                    AcceptedImportTopologyOutcome::Resolved(
                        crate::import_discovery::accepted_import_module(source, &accepted_reads)
                            .expect("accepted import topology was validated above"),
                    )
                } else {
                    use crate::ImportObservationStatus as S;
                    match observation.status() {
                        S::Absent => AcceptedImportTopologyOutcome::Absent,
                        S::PresentReadable { .. } => unreachable!(
                            "a readable import observation retains its accepted source"
                        ),
                        S::PresentUnreadable(_) => AcceptedImportTopologyOutcome::PresentUnreadable,
                        S::DeniedLexical => AcceptedImportTopologyOutcome::DeniedLexical,
                        S::DeniedCanonical { .. } => AcceptedImportTopologyOutcome::DeniedCanonical,
                        S::InvalidPhysicalType { .. } => {
                            AcceptedImportTopologyOutcome::InvalidPhysicalType
                        }
                        S::UnstableRead(_) => AcceptedImportTopologyOutcome::UnstableRead,
                        S::Cancelled => AcceptedImportTopologyOutcome::Cancelled,
                    }
                };
                AcceptedImportTopologyFact {
                    importer: request.occurrence().importer().clone(),
                    exact_specifier: Arc::from(request.exact_specifier()),
                    normalized_specifier: Arc::from(request.normalized_specifier()),
                    outcome,
                }
            })
            .collect::<Vec<_>>();
        accepted_topology.sort();
        let accepted_topology: Arc<[AcceptedImportTopologyFact]> = accepted_topology.into();
        {
            let mut store = lock_import_store(&self.import_store);
            let ImportInputStore {
                next_stamp,
                context_stamps,
                provenance_stamps,
                observation_stamps,
                topology_stamps,
                ..
            } = &mut *store;
            leaves.push((
                accepted_import_topology_input(),
                exact_value_stamp(next_stamp, topology_stamps, &accepted_topology),
            ));
            leaves.push((
                import_context_input(),
                exact_value_stamp(next_stamp, context_stamps, &context),
            ));
            for source in sources.iter() {
                let accepted = provenance[&source.module];
                leaves.push((
                    accepted_read_input(&source.module),
                    exact_value_stamp(next_stamp, provenance_stamps, accepted),
                ));
            }
            for accepted in accepted_reads.iter() {
                leaves.push((
                    accepted_import_provenance_input(accepted.metadata_identity()),
                    exact_value_stamp(next_stamp, provenance_stamps, accepted),
                ));
            }
            for observation in ledger.iter() {
                leaves.push((
                    import_observation_input(observation.request()),
                    exact_value_stamp(next_stamp, observation_stamps, observation),
                ));
            }
        }
        leaves.extend(publish_module_inputs(
            &self.module_store,
            revision,
            snapshot,
        ));
        self.runtime
            .publish_revision(revision, leaves)
            .map_err(|error| {
                import_input_error(format!("cannot publish import revision: {error:?}"))
            })?;
        let view = Arc::new(ImportInputView {
            revision,
            generation,
            context,
            sources,
            accepted_reads,
            ledger,
        });
        let mut store = lock_import_store(&self.import_store);
        store.revisions.push_back(view);
        while store.revisions.len() > IMPORT_INPUT_REVISION_RETENTION {
            store.revisions.pop_front();
        }
        let retained = store.revisions.iter().cloned().collect::<Vec<_>>();
        store
            .context_stamps
            .retain(|(candidate, _)| retained.iter().any(|view| &view.context == candidate));
        store.provenance_stamps.retain(|(candidate, _)| {
            retained
                .iter()
                .any(|view| view.accepted_reads.contains(candidate))
        });
        store.observation_stamps.retain(|(candidate, _)| {
            retained
                .iter()
                .any(|view| view.ledger.iter().any(|value| value == candidate))
        });
        let published = ImportInputRevision {
            revision_id: revision.id(),
            request_generation: generation,
            frontier_round,
        };
        self.current_import_revision = Some(published);
        Ok(published)
    }

    pub(crate) fn source_revision(
        &mut self,
        source: &super::session::ExactSourceInput,
        snapshot: &SourceSnapshot,
    ) -> Revision {
        #[cfg(test)]
        {
            self.current_test_import_revision = None;
        }
        // The parse family is allocated with the shared runtime now so callers
        // can migrate without creating a peer executor.
        let _parse_migration_family = &self.parse;
        let stamp = self
            .source_stamps
            .iter()
            .find_map(|(candidate, stamp)| (candidate == source).then_some(*stamp))
            .unwrap_or_else(|| {
                let stamp = self.next_source_stamp;
                self.next_source_stamp += 1;
                self.source_stamps.push_back((source.clone(), stamp));
                stamp
            });
        let revision = Revision::new(self.next_revision, 1);
        self.next_revision += 1;
        let mut leaves = vec![(InputIdentity::new(Self::SOURCE_INPUT, "current"), stamp)];
        leaves.extend(publish_module_inputs(
            &self.module_store,
            revision,
            snapshot,
        ));
        self.runtime
            .publish_revision(revision, leaves)
            .expect("compiler input revisions are immutable and uniquely numbered");
        revision
    }

    pub(crate) fn parse_program(
        &self,
        revision: Revision,
        root: &ModuleId,
        modules: impl IntoIterator<Item = ModuleId>,
    ) -> (
        Result<Arc<ParsedProgram>, crate::CompileErrors>,
        ParsedModulesWork,
    ) {
        let snapshot = self
            .module_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revisions
            .iter()
            .find(|view| view.revision == revision)
            .expect("parse projection retains its module input revision")
            .snapshot
            .clone();
        let mut parsed = Vec::new();
        let mut errors = crate::CompileErrors::new();
        let mut work = ParsedModulesWork::default();
        for module in modules {
            work.modules_considered += 1;
            work.previous_module_lookups += 1;
            let current_file_id = snapshot
                .files()
                .find_map(|source| {
                    (snapshot.module_id(source.file_id) == Some(&module)).then_some(source.file_id)
                })
                .expect("parse demand belongs to the published source revision");
            let attempt = self.runtime.request_registered(
                &self.parse_modules,
                revision,
                ModuleQueryKey(module.clone()),
                CancellationToken::new(),
            );
            let Some(terminal) = attempt.terminal() else {
                errors.push(import_input_error(format!(
                    "ParseModule({module}) aborted: {:?}",
                    attempt.abort()
                )));
                continue;
            };
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("ParseModule publishes typed values")
            };
            let computed = matches!(attempt.execution(), RequestExecution::Computed);
            if computed {
                work.modules_reparsed += 1;
                work.syntax.lexer_invocations += value.work.lexer_invocations;
                work.syntax.parser_invocations += value.work.parser_invocations;
                work.syntax.lexed_bytes += value.work.lexed_bytes;
                work.syntax.tokens += value.work.tokens;
            }
            match &value.result {
                Ok(module) => {
                    let projected = crate::parsed_modules::rebind_parsed_module(&snapshot, module);
                    if !computed {
                        if Arc::ptr_eq(&projected, module) {
                            work.modules_reused += 1;
                        } else {
                            work.modules_rebound += 1;
                        }
                    }
                    parsed.push(projected);
                }
                Err(module_errors) => {
                    if !computed {
                        work.modules_reused += 1;
                    }
                    errors.extend(
                        module_errors.clone().map_spans(|span| {
                            Span::with_file(current_file_id, span.start, span.end)
                        }),
                    )
                }
            }
        }
        let result = if errors.is_empty() {
            ParsedProgram::new(root.clone(), parsed)
                .map(Arc::new)
                .map_err(crate::CompileErrors::from)
        } else {
            Err(errors)
        };
        (result, work)
    }

    pub(crate) fn module_rirs(
        &self,
        revision: Revision,
        modules: impl IntoIterator<Item = ModuleId>,
    ) -> (
        Result<Vec<Arc<ModuleRirOutput>>, crate::CompileErrors>,
        crate::CanonicalRirWork,
    ) {
        let mut outputs = Vec::new();
        let mut errors = crate::CompileErrors::new();
        let mut work = crate::CanonicalRirWork::default();
        for module in modules {
            let attempt = self.runtime.request_registered(
                &self.module_rirs,
                revision,
                ModuleQueryKey(module.clone()),
                CancellationToken::new(),
            );
            let Some(terminal) = attempt.terminal() else {
                errors.push(import_input_error(format!(
                    "ModuleRir({module}) aborted: {:?}",
                    attempt.abort()
                )));
                continue;
            };
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("ModuleRir publishes typed values")
            };
            if matches!(attempt.execution(), RequestExecution::Computed) {
                work.accumulate(value.work);
            }
            match &value.result {
                Ok(output) => {
                    outputs.push(output.clone());
                }
                Err(module_errors) => errors.extend(module_errors.clone()),
            }
        }
        if errors.is_empty() {
            (Ok(outputs), work)
        } else {
            (Err(errors), work)
        }
    }

    pub(crate) fn projected_module_indexes(
        &self,
        revision: Revision,
        program: &ParsedProgram,
    ) -> Result<Vec<ProjectedModuleIndex>, crate::CompileErrors> {
        let mut projections = Vec::with_capacity(program.modules().len());
        let mut errors = crate::CompileErrors::new();
        for module in program.modules() {
            let index_attempt = self.runtime.request_registered(
                &self.module_indexes,
                revision,
                ModuleQueryKey(module.module_id().clone()),
                CancellationToken::new(),
            );
            let Some(index_terminal) = index_attempt.terminal() else {
                errors.push(import_input_error(format!(
                    "ModuleIndex({}) aborted: {:?}",
                    module.module_id(),
                    index_attempt.abort()
                )));
                continue;
            };
            let rue_query::QueryOutcome::Success(indexed) = index_terminal.outcome() else {
                unreachable!("ModuleIndex publishes typed values")
            };
            let index = match &indexed.0 {
                Ok(index) => index,
                Err(module_errors) => {
                    errors.extend(module_errors.clone());
                    continue;
                }
            };
            if index.revision != *module.revision() {
                errors.push(import_input_error(format!(
                    "ModuleIndex({}) belongs to a foreign source revision",
                    module.module_id()
                )));
                continue;
            }
            let keys = index
                .definitions
                .iter()
                .map(|entry| (entry.namespace, entry.name.clone()))
                .collect::<BTreeSet<_>>();
            let mut definitions = Vec::with_capacity(index.definitions.len());
            for (namespace, name) in keys {
                let lookup_attempt = self.runtime.request_registered(
                    &self.lookup_names,
                    revision,
                    LookupNameKey {
                        module: module.module_id().clone(),
                        namespace,
                        name: name.clone(),
                    },
                    CancellationToken::new(),
                );
                let Some(lookup_terminal) = lookup_attempt.terminal() else {
                    errors.push(import_input_error(format!(
                        "LookupName({}) aborted: {:?}",
                        module.module_id(),
                        lookup_attempt.abort()
                    )));
                    continue;
                };
                let rue_query::QueryOutcome::Success(found) = lookup_terminal.outcome() else {
                    unreachable!("LookupName publishes typed values")
                };
                match &found.0 {
                    Ok(found) => {
                        let current = index
                            .definitions
                            .iter()
                            .filter(|entry| entry.namespace == namespace && entry.name == name)
                            .cloned()
                            .collect::<Vec<_>>();
                        let current_facts = current
                            .iter()
                            .map(|entry| LookupNameFact {
                                namespace: entry.namespace,
                                kind: entry.kind,
                                visibility: entry.visibility,
                                name: entry.name.clone(),
                            })
                            .collect::<Vec<_>>();
                        if current_facts.as_slice() == found.as_ref() {
                            definitions.extend(current);
                        } else {
                            errors.push(import_input_error(format!(
                                "LookupName({}::{name}) disagrees with current locators",
                                module.module_id()
                            )));
                        }
                    }
                    Err(failure) => errors.push(import_input_error(format!(
                        "LookupName({}::{name}) failed: {failure:?}",
                        module.module_id()
                    ))),
                }
            }
            definitions.sort_by(|left, right| {
                left.declaration_span
                    .start
                    .cmp(&right.declaration_span.start)
                    .then(left.declaration_span.end.cmp(&right.declaration_span.end))
                    .then(left.namespace.cmp(&right.namespace))
                    .then(left.name.cmp(&right.name))
            });
            if definitions.len() != index.definitions.len() {
                errors.push(import_input_error(format!(
                    "LookupName projection for {} is incomplete",
                    module.module_id()
                )));
                continue;
            }
            let file_id = module.file_id();
            for entry in &mut definitions {
                entry.name_span =
                    rue_span::Span::with_file(file_id, entry.name_span.start, entry.name_span.end);
                entry.declaration_span = rue_span::Span::with_file(
                    file_id,
                    entry.declaration_span.start,
                    entry.declaration_span.end,
                );
            }
            projections.push(ProjectedModuleIndex {
                revision: index.revision.clone(),
                definitions: definitions.into(),
            });
        }
        if errors.is_empty() {
            Ok(projections)
        } else {
            Err(errors)
        }
    }

    #[cfg(test)]
    fn module_terminals(
        &self,
        revision: Revision,
        module: ModuleId,
    ) -> (Arc<ParsedModule>, Arc<ModuleIndex>, Arc<ModuleRirOutput>) {
        let parse = self.runtime.request_registered(
            &self.parse_modules,
            revision,
            ModuleQueryKey(module.clone()),
            CancellationToken::new(),
        );
        let index = self.runtime.request_registered(
            &self.module_indexes,
            revision,
            ModuleQueryKey(module.clone()),
            CancellationToken::new(),
        );
        let rir = self.runtime.request_registered(
            &self.module_rirs,
            revision,
            ModuleQueryKey(module),
            CancellationToken::new(),
        );
        let parse = match parse.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            _ => unreachable!(),
        };
        let index = match index.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.0.clone().unwrap(),
            _ => unreachable!(),
        };
        let rir = match rir.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            _ => unreachable!(),
        };
        (parse, index, rir)
    }

    pub(crate) fn select_parse(
        &mut self,
        attempt: &QueryRequestAttempt<super::session::ParseQueryRecord>,
    ) {
        self.parse.select(attempt);
        // Exact source stamps live exactly as long as a parse memo key (or the
        // current request before selection). They are never independently FIFO
        // evicted while a terminal can still observe the stamp.
        self.source_stamps
            .retain(|(source, _)| self.parse.any_retained_key(|key| key.source() == source));
        debug_assert!(self.source_stamps.len() <= self.parse.retention().memo_nodes);
    }

    pub(crate) fn parse_retention(&self) -> crate::typed_query_store::QueryStoreRetention {
        let retention = self.parse.retention();
        crate::typed_query_store::QueryStoreRetention {
            retained: retention.terminals,
            protected: self.parse.protected_count(),
            pinned: 0,
            tombstones: 0,
            evictions: self.runtime.metrics().evictions as usize,
        }
    }
}

#[cfg(test)]
pub(crate) fn projected_declaration_shells_for_test(
    merged: &crate::canonical_merge::CanonicalMergedProgram,
) -> Result<Vec<rue_air::SemanticDeclarationShell>, crate::CompileErrors> {
    let snapshot = merged.definitions().source_snapshot();
    let mut database = RevisionedQueryDatabase::default();
    let revision =
        database.source_revision(&super::session::ExactSourceInput::new(snapshot), snapshot);
    database
        .projected_declaration_shells(revision, merged.ast(), CancellationToken::new())
        .map_err(|failure| {
            crate::CompileErrors::from(CompileError::without_span(ErrorKind::InternalError(
                format!("test declaration-shell query failed: {failure:?}"),
            )))
        })
}

#[cfg(test)]
pub(crate) fn execution(attempt: &QueryRequestAttempt<impl Sized>) -> RequestExecution {
    attempt.execution()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CompilerSession, DiscoverySourceAssembler, FileMetadataFingerprint, ImportDiscoveryContext,
        ImportObservation, PhysicalFileIdentity, SourceMetadata,
    };
    use rue_span::FileId;
    use std::collections::{BTreeSet, HashMap};

    fn source_snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
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

    fn semantic_configuration() -> crate::semantic_query_nucleus::SemanticQueryConfiguration {
        crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: rue_target::Target::X86_64Linux,
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        }
    }

    fn declaration_candidate(
        database: &RevisionedQueryDatabase,
        revision: Revision,
        module: &ModuleId,
        category: crate::declaration_candidate::DeclarationCandidateCategory,
        name: &str,
    ) -> crate::declaration_candidate::DeclarationCandidateKey {
        let attempt = database.runtime.request_registered(
            &database.declaration_occurrence_indexes,
            revision,
            ModuleQueryKey(module.clone()),
            CancellationToken::new(),
        );
        let rue_query::QueryOutcome::Success(value) = attempt.terminal().unwrap().outcome() else {
            unreachable!()
        };
        let DeclarationOccurrenceIndexValue::Available(index) = value else {
            panic!("declaration occurrence index unavailable")
        };
        index
            .capabilities
            .keys()
            .find(|candidate| candidate.category == category && candidate.name.as_ref() == name)
            .cloned()
            .unwrap_or_else(|| panic!("missing {category:?} candidate `{name}`"))
    }

    fn request_semantic_nucleus(
        database: &RevisionedQueryDatabase,
        revision: Revision,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> crate::semantic_query_nucleus::SemanticNucleusValue {
        request_semantic_nucleus_observed(database, revision, key).0
    }

    fn request_semantic_nucleus_observed(
        database: &RevisionedQueryDatabase,
        revision: Revision,
        key: crate::semantic_query_nucleus::SemanticNucleusKey,
    ) -> (
        crate::semantic_query_nucleus::SemanticNucleusValue,
        QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue>,
    ) {
        let attempt = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            key,
            CancellationToken::new(),
        );
        let terminal = attempt
            .terminal()
            .unwrap_or_else(|| panic!("semantic nucleus aborted: {:?}", attempt.abort()));
        let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
            unreachable!()
        };
        (value.clone(), attempt)
    }

    fn assert_direct_semantic_observation(
        label: &str,
        attempt: &QueryRequestAttempt<crate::semantic_query_nucleus::SemanticNucleusValue>,
        required_families: &[&str],
        allowed_families: &[&str],
        maximum_dependencies: usize,
    ) {
        let actual = attempt
            .dependencies()
            .iter()
            .map(|dependency| dependency.node.family())
            .collect::<BTreeSet<_>>();
        let required = required_families.iter().copied().collect::<BTreeSet<_>>();
        let allowed = allowed_families.iter().copied().collect::<BTreeSet<_>>();
        assert!(
            required.is_subset(&actual),
            "{label} omitted a required direct dependency family: required={required:?}, actual={actual:?}"
        );
        assert!(
            actual.is_subset(&allowed),
            "{label} observed an unexpected dependency family: actual={actual:?}, allowed={allowed:?}; batch, root, full-plan, and unrelated discovery dependencies are forbidden"
        );
        assert!(
            attempt.dependencies().len() <= maximum_dependencies,
            "{label} observed broad same-family discovery: dependencies={:?}",
            attempt.dependencies()
        );
        assert!(
            attempt.inputs().is_empty(),
            "{label} read inputs directly instead of through its precise query dependencies: {:?}",
            attempt.inputs()
        );
    }

    fn retired_declaration_exports(
        source: &SourceSnapshot,
    ) -> Vec<rue_air::SemanticDeclarationExport> {
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(source).unwrap();
        let merged = crate::merge_parsed_modules(&parsed).unwrap();
        let rir = crate::lower_canonical_rir(&merged).unwrap();
        let bound = rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            crate::PreviewFeatures::new(),
        )
        .bind_declarations_for_test()
        .unwrap();
        bound
            .with_declaration_semantics(|exports, _| exports.to_vec())
            .unwrap()
    }

    fn retired_declaration_failure(source: &SourceSnapshot) -> String {
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(source).unwrap();
        let merged = crate::merge_parsed_modules(&parsed).unwrap();
        let rir = crate::lower_canonical_rir(&merged).unwrap();
        let errors = match rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            crate::PreviewFeatures::new(),
        )
        .bind_declarations_for_test()
        {
            Err(errors) => errors,
            Ok(_) => panic!("retired fixture unexpectedly passed declaration binding"),
        };
        errors
            .first()
            .expect("retired fixture must produce one declaration failure")
            .to_string()
    }

    fn export_type_agrees(
        retired: &rue_air::SemanticExportType,
        keyed: &crate::durable_semantics::DurableType,
    ) -> bool {
        use crate::durable_semantics::DurableType as K;
        use rue_air::SemanticExportType as R;
        match (retired, keyed) {
            (R::I8, K::I8)
            | (R::I16, K::I16)
            | (R::I32, K::I32)
            | (R::I64, K::I64)
            | (R::U8, K::U8)
            | (R::U16, K::U16)
            | (R::U32, K::U32)
            | (R::U64, K::U64)
            | (R::Bool, K::Bool)
            | (R::Unit, K::Unit)
            | (R::Never, K::Never)
            | (R::ComptimeType, K::ComptimeType) => true,
            (R::GenericParameter(left), K::GenericParameter(right)) => left == right,
            (R::Nominal(left), K::Nominal(right)) => {
                left.name.as_ref() == right.name() && left.kind == right.kind()
            }
            (
                R::Array {
                    element: left,
                    len: left_len,
                },
                K::Array {
                    element: right,
                    len: right_len,
                },
            ) => left_len == right_len && export_type_agrees(left, right),
            (R::PtrConst(left), K::PtrConst(right)) | (R::PtrMut(left), K::PtrMut(right)) => {
                export_type_agrees(left, right)
            }
            _ => false,
        }
    }

    fn signature_agrees(
        retired: &rue_air::SemanticDeclarationPayload,
        keyed: &crate::semantic_query_nucleus::DeclarationSignatureProjection,
    ) -> bool {
        use crate::semantic_query_nucleus::DeclarationSignatureProjection as K;
        use rue_air::SemanticDeclarationPayload as R;
        match (retired, keyed) {
            (
                R::Callable {
                    parameters: left,
                    result: left_result,
                    has_self: left_self,
                    is_unchecked: left_unchecked,
                },
                K::Callable {
                    parameters: right,
                    result: right_result,
                    has_self: right_self,
                    is_unchecked: right_unchecked,
                    ..
                },
            ) => {
                left_self == right_self
                    && left_unchecked == right_unchecked
                    && export_type_agrees(left_result, right_result)
                    && left.len() == right.len()
                    && left.iter().zip(right.iter()).all(|(left, right)| {
                        let mode_agrees = matches!(
                            (left.mode, right.mode),
                            (
                                rue_air::SemanticParameterMode::Value,
                                crate::durable_semantics::DurableParameterMode::Value
                            ) | (
                                rue_air::SemanticParameterMode::Borrow,
                                crate::durable_semantics::DurableParameterMode::Borrow
                            ) | (
                                rue_air::SemanticParameterMode::Inout,
                                crate::durable_semantics::DurableParameterMode::Inout
                            )
                        );
                        mode_agrees
                            && left.is_comptime == right.is_comptime
                            && export_type_agrees(&left.ty, &right.ty)
                    })
            }
            (
                R::Struct {
                    fields: left,
                    is_copy: left_copy,
                    is_linear: left_linear,
                },
                K::Struct {
                    fields: right,
                    is_copy: right_copy,
                    is_linear: right_linear,
                    ..
                },
            ) => {
                left_copy == right_copy
                    && left_linear == right_linear
                    && left.len() == right.len()
                    && left.iter().zip(right.iter()).all(
                        |((left_name, left_ty), (right_name, right_ty))| {
                            left_name == right_name && export_type_agrees(left_ty, right_ty)
                        },
                    )
            }
            (R::Enum { variants: left }, K::Enum { variants: right }) => {
                left.len() == right.len()
                    && left.iter().zip(right.iter()).all(
                        |((left_name, left_payload), (right_name, right_payload))| {
                            left_name == right_name
                                && left_payload.len() == right_payload.len()
                                && left_payload
                                    .iter()
                                    .zip(right_payload.iter())
                                    .all(|(left, right)| export_type_agrees(left, right))
                        },
                    )
            }
            (R::Destructor, K::Destructor) => true,
            _ => false,
        }
    }

    fn nucleus_failure_message(
        value: &crate::semantic_query_nucleus::SemanticNucleusValue,
    ) -> Option<String> {
        use crate::semantic_query_nucleus::{
            SemanticNucleusFailure as F, SemanticNucleusValue as V,
        };
        match value {
            V::Failure(
                F::Diagnostic(kind)
                | F::DiagnosticAtParameter { kind, .. }
                | F::DiagnosticAtDeclaration { kind, .. }
                | F::OwnershipGate { kind, .. }
                | F::DiagnosticWithHelp { kind, .. },
            ) => Some(kind.to_string()),
            _ => None,
        }
    }

    #[test]
    fn direct_identity_and_signature_families_match_retired_air_per_declaration() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct S { value: i32, fn get(borrow self, delta: i32) -> i32 { self.value + delta } fn make(value: i32) -> S { S { value } } } enum E { A, B } drop fn S(self) {} fn free(value: i32) -> i32 { value } fn main() {}",
            )],
            1,
        );
        let retired = retired_declaration_exports(&source);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );

        for (category, kind, name) in [
            (
                Category::Function,
                crate::StableDefinitionKind::Function,
                "free",
            ),
            (Category::Struct, crate::StableDefinitionKind::Struct, "S"),
            (Category::Enum, crate::StableDefinitionKind::Enum, "E"),
            (Category::Method, crate::StableDefinitionKind::Method, "get"),
            (
                Category::AssociatedFunction,
                crate::StableDefinitionKind::AssociatedFunction,
                "make",
            ),
            (
                Category::Destructor,
                crate::StableDefinitionKind::Destructor,
                "S",
            ),
        ] {
            let declaration = declaration_candidate(&database, revision, &module, category, name);
            let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            };
            let (identity, identity_attempt) = request_semantic_nucleus_observed(
                &database,
                revision,
                Key::Identity(query.clone()),
            );
            if category == Category::Destructor {
                assert_direct_semantic_observation(
                    "destructor identity",
                    &identity_attempt,
                    &["compiler.declaration-shell", "compiler.semantic-nucleus"],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.lookup-name",
                        "compiler.module-index",
                        "compiler.parse-module",
                        "compiler.raw-declaration-signature",
                        "compiler.semantic-nucleus",
                    ],
                    7,
                );
            } else {
                assert_direct_semantic_observation(
                    "direct identity",
                    &identity_attempt,
                    &["compiler.declaration-shell"],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.parse-module",
                    ],
                    3,
                );
            }
            let V::Identity(identity) = identity else {
                panic!("direct identity failed for {kind:?} {name}: {identity:?}")
            };
            let retired = retired
                .iter()
                .find(|export| {
                    export.identity.kind == kind && export.identity.name.as_ref() == name
                })
                .unwrap_or_else(|| panic!("retired AIR omitted {kind:?} {name}"));
            assert_eq!(identity.key.namespace(), retired.identity.namespace);
            assert_eq!(identity.key.kind(), retired.identity.kind);
            assert_eq!(identity.key.name(), retired.identity.name.as_ref());
            assert_eq!(
                identity.key.owner().map(|owner| owner.name()),
                retired.identity.owner.as_deref()
            );
            assert_eq!(identity.is_public, retired.identity.is_public);

            let (signature, signature_attempt) =
                request_semantic_nucleus_observed(&database, revision, Key::Signature(query));
            match category {
                Category::Destructor => assert_direct_semantic_observation(
                    "destructor signature",
                    &signature_attempt,
                    &[
                        "compiler.declaration-shell",
                        "compiler.lookup-name",
                        "compiler.raw-declaration-signature",
                    ],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.lookup-name",
                        "compiler.module-index",
                        "compiler.parse-module",
                        "compiler.raw-declaration-signature",
                        "compiler.semantic-nucleus",
                    ],
                    10,
                ),
                Category::Method | Category::AssociatedFunction => {
                    assert_direct_semantic_observation(
                        "owned callable signature",
                        &signature_attempt,
                        &[
                            "compiler.declaration-shell",
                            "compiler.raw-declaration-signature",
                            "compiler.semantic-nucleus",
                        ],
                        &[
                            "compiler.declaration-occurrence-index",
                            "compiler.declaration-shell",
                            "compiler.lookup-name",
                            "compiler.module-index",
                            "compiler.parse-module",
                            "compiler.raw-declaration-signature",
                            "compiler.semantic-nucleus",
                        ],
                        9,
                    )
                }
                _ => assert_direct_semantic_observation(
                    "direct signature",
                    &signature_attempt,
                    &[
                        "compiler.declaration-shell",
                        "compiler.raw-declaration-signature",
                    ],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.parse-module",
                        "compiler.raw-declaration-signature",
                    ],
                    4,
                ),
            }
            let V::Signature(signature) = signature else {
                panic!("direct signature failed for {kind:?} {name}: {signature:?}")
            };
            assert!(
                signature_agrees(&retired.payload, &signature.signature),
                "retired/keyed signature disagreement for {kind:?} {name}: retired={:?}, keyed={:?}",
                retired.payload,
                signature.signature,
            );
        }
    }

    #[test]
    fn direct_const_family_matches_retired_evaluation() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
            SemanticNucleusValue as V,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const SELECTED: i32 = 40 + 2; fn main() -> i32 { SELECTED }",
            )],
            1,
        );
        let retired = retired_declaration_exports(&source);
        let retired = retired
            .iter()
            .find(|export| {
                export.identity.kind == crate::StableDefinitionKind::ValueConst
                    && export.identity.name.as_ref() == "SELECTED"
            })
            .expect("retired AIR omitted SELECTED");
        let rue_air::SemanticDeclarationPayload::Const {
            ty: retired_ty,
            value: rue_air::SemanticExportConstValue::Integer(retired_value),
        } = &retired.payload
        else {
            panic!("retired AIR classified SELECTED unexpectedly: {retired:?}")
        };

        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration = declaration_candidate(
            &database,
            revision,
            &module,
            Category::ConstCandidate,
            "SELECTED",
        );
        let configuration = crate::semantic_query_nucleus::SemanticQueryConfiguration {
            target: rue_target::Target::host().expect("retired AIR requires a supported host"),
            preview_features: crate::StablePreviewFeatures::new(&crate::PreviewFeatures::default()),
        };
        let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration,
            }),
        );
        assert_direct_semantic_observation(
            "const evaluation",
            &keyed_attempt,
            &[
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.raw-const-syntax",
            ],
            &[
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.lookup-name",
                "compiler.module-index",
                "compiler.parse-module",
                "compiler.raw-const-syntax",
            ],
            6,
        );
        let V::ConstResolution(Resolution::Value {
            ty: keyed_ty,
            value,
            ..
        }) = keyed
        else {
            panic!("direct const terminal failed: {keyed:?}")
        };
        let crate::durable_semantics::DurableConstValue::Integer(keyed_value) = *value else {
            panic!("direct const terminal returned a non-integer value")
        };
        assert!(export_type_agrees(retired_ty, &keyed_ty));
        assert_eq!(*retired_value, keyed_value);
    }

    #[test]
    fn direct_target_selected_comptime_matches_retired_air_oracle() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            ComptimeCallQueryKey, ComptimeCallResultProjection as ResultProjection,
            SemanticNucleusKey as Key, SemanticNucleusValue as V,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn selected(comptime seed: i32) -> i32 { match @target_arch() { Arch.X86_64 => seed + 64, Arch.Aarch64 => seed + 32 } } fn main() -> i32 { selected(0) }",
            )],
            1,
        );
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&source).unwrap();
        let merged = crate::merge_parsed_modules(&parsed).unwrap();
        let rir = crate::lower_canonical_rir(&merged).unwrap();
        let retired = rue_air::Sema::new_synthetic(
            rir.rir(),
            rir.semantic_symbols().interner(),
            crate::PreviewFeatures::new(),
        )
        .analyze_all_for_test()
        .unwrap();
        let retired = retired
            .functions
            .iter()
            .flat_map(|function| function.air.iter())
            .find_map(|(_, instruction)| match &instruction.data {
                rue_air::AirInstData::EnumVariant { variant_index, .. } => match variant_index {
                    0 => Some(64),
                    1 => Some(32),
                    _ => None,
                },
                _ => None,
            })
            .expect("retired AIR did not lower the target-selected Arch variant");
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let mut configuration = semantic_configuration();
        configuration.target =
            rue_target::Target::host().expect("retired AIR requires a supported host");
        let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
            &database,
            revision,
            Key::ComptimeCall(ComptimeCallQueryKey {
                declaration: crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration: declaration_candidate(
                        &database,
                        revision,
                        &module,
                        Category::Function,
                        "selected",
                    ),
                    configuration,
                },
                type_arguments: Arc::from([]),
                value_arguments: Arc::from([(
                    Arc::from("seed"),
                    crate::durable_semantics::DurableConstValue::Integer(0),
                )]),
            }),
        );
        assert_direct_semantic_observation(
            "target-selected comptime call",
            &keyed_attempt,
            &[
                "compiler.declaration-shell",
                "compiler.raw-declaration-body",
                "compiler.semantic-nucleus",
            ],
            &[
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.parse-module",
                "compiler.raw-declaration-body",
                "compiler.raw-declaration-signature",
                "compiler.semantic-nucleus",
            ],
            7,
        );
        let V::ComptimeCall(crate::semantic_query_nucleus::ComptimeCallProjection {
            result:
                ResultProjection::Value(crate::durable_semantics::DurableConstValue::Integer(keyed)),
            ..
        }) = keyed
        else {
            panic!("direct target-selected const failed: {keyed:?}")
        };
        assert_eq!(i128::from(retired), keyed);
    }

    #[test]
    fn direct_ownership_terminals_match_retired_air_acceptance_and_failure() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

        for (source_text, should_accept) in [
            (
                "enum Maybe { Some, None } fn Gated(comptime T: type) -> type { @require_droppable(T); T } const G = Gated(Maybe); fn main() {}",
                true,
            ),
            (
                "linear struct Token { v: i32 } fn Gated(comptime T: type) -> type { @require_droppable(T); T } const G = Gated(Token); fn main() {}",
                false,
            ),
        ] {
            let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
            let module = ModuleId::from_logical_path("main.rue").unwrap();
            let mut database = RevisionedQueryDatabase::default();
            let revision = database.source_revision(
                &super::super::session::ExactSourceInput::new(&source),
                &source,
            );
            let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::ConstCandidate,
                    "G",
                ),
                configuration: semantic_configuration(),
            };
            let (resolution, resolution_attempt) = request_semantic_nucleus_observed(
                &database,
                revision,
                Key::ConstResolution(producer.clone()),
            );
            assert_direct_semantic_observation(
                "ownership-gated const producer",
                &resolution_attempt,
                &[
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.raw-const-syntax",
                    "compiler.semantic-nucleus",
                ],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.raw-const-syntax",
                    "compiler.raw-declaration-body",
                    "compiler.raw-declaration-signature",
                    "compiler.semantic-nucleus",
                ],
                14,
            );
            let V::ConstResolution(
                crate::semantic_query_nucleus::ConstResolutionProjection::Value {
                    deferred_ownership,
                    ..
                },
            ) = resolution
            else {
                panic!("direct const producer failed before its ownership gate: {resolution:?}")
            };
            let [gate] = deferred_ownership.as_ref() else {
                panic!("expected one direct ownership gate: {deferred_ownership:?}")
            };
            let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
                &database,
                revision,
                Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                    producer,
                    gate: gate.clone(),
                }),
            );
            assert_direct_semantic_observation(
                "deferred ownership terminal",
                &keyed_attempt,
                &[
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.semantic-nucleus",
                ],
                &[
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.lookup-name",
                    "compiler.module-index",
                    "compiler.parse-module",
                    "compiler.raw-const-syntax",
                    "compiler.raw-declaration-body",
                    "compiler.raw-declaration-signature",
                    "compiler.semantic-nucleus",
                ],
                18,
            );
            if should_accept {
                let retired = retired_declaration_exports(&source);
                assert!(
                    retired
                        .iter()
                        .any(|export| export.identity.name.as_ref() == "G"),
                    "retired AIR omitted accepted G"
                );
                assert_eq!(keyed, V::DeferredOwnership);
            } else {
                let retired = retired_declaration_failure(&source);
                assert_eq!(
                    nucleus_failure_message(&keyed).as_deref(),
                    Some(retired.as_str())
                );
            }
        }
    }

    #[test]
    fn direct_family_failures_match_retired_air_without_root_prevalidation() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{SemanticNucleusKey as Key, SemanticNucleusValue as V};

        for (source_text, category, name, identity_terminal) in [
            (
                "drop fn Missing(self) {} fn main() {}",
                Category::Destructor,
                "Missing",
                false,
            ),
            (
                "struct S {} drop fn S(self) {} drop fn S(self) {} fn main() {}",
                Category::Destructor,
                "S",
                true,
            ),
            (
                "struct S { fn make(a: i32, a: i32) {} } fn main() {}",
                Category::AssociatedFunction,
                "make",
                false,
            ),
        ] {
            let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
            let retired = retired_declaration_failure(&source);
            let module = ModuleId::from_logical_path("main.rue").unwrap();
            let mut database = RevisionedQueryDatabase::default();
            let revision = database.source_revision(
                &super::super::session::ExactSourceInput::new(&source),
                &source,
            );
            let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(&database, revision, &module, category, name),
                configuration: semantic_configuration(),
            };
            let (keyed, keyed_attempt) = request_semantic_nucleus_observed(
                &database,
                revision,
                if identity_terminal {
                    Key::Identity(query)
                } else {
                    Key::Signature(query)
                },
            );
            if identity_terminal {
                assert_direct_semantic_observation(
                    "deterministic destructor identity failure",
                    &keyed_attempt,
                    &["compiler.declaration-shell", "compiler.semantic-nucleus"],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.lookup-name",
                        "compiler.module-index",
                        "compiler.parse-module",
                        "compiler.semantic-nucleus",
                    ],
                    6,
                );
            } else if category == Category::Destructor {
                assert_direct_semantic_observation(
                    "deterministic destructor signature failure",
                    &keyed_attempt,
                    &["compiler.declaration-shell", "compiler.lookup-name"],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.lookup-name",
                        "compiler.module-index",
                        "compiler.parse-module",
                    ],
                    5,
                );
            } else {
                assert_direct_semantic_observation(
                    "deterministic parameter failure",
                    &keyed_attempt,
                    &[
                        "compiler.declaration-shell",
                        "compiler.raw-declaration-signature",
                    ],
                    &[
                        "compiler.declaration-occurrence-index",
                        "compiler.declaration-shell",
                        "compiler.parse-module",
                        "compiler.raw-declaration-signature",
                    ],
                    4,
                );
            }
            assert!(matches!(keyed, V::Failure(_)));
            assert_eq!(
                nucleus_failure_message(&keyed).as_deref(),
                Some(retired.as_str()),
                "direct keyed failure diverged for {category:?} {name}: {keyed:?}"
            );
        }
    }

    #[test]
    fn direct_declaration_import_family_matches_independent_import_graph_oracle() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = source_snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const dep = @import(\"dep.rue\"); fn main() -> i32 { dep.value }",
                ),
                (
                    2,
                    "/project/dep.rue",
                    "dep.rue",
                    "pub const value: i32 = 42;",
                ),
            ],
            1,
        );
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&source).unwrap();
        let retired = crate::test_support::test_fixture_import_graph(&parsed).unwrap();
        let main = ModuleId::from_logical_path("main.rue").unwrap();
        let expected = retired
            .records()
            .iter()
            .find(|record| record.importer() == &main && record.normalized_specifier() == "dep.rue")
            .expect("retired import graph omitted dep.rue")
            .resolution()
            .clone();

        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        database.adopt_test_import_graph_for_revision(revision, retired);
        let revision = database.current_semantic_revision().unwrap();
        let requested = database.runtime.request_registered(
            &database.declaration_imports,
            revision,
            declaration_import_key(&main, Category::ConstCandidate, "dep", None, 0, "dep.rue"),
            CancellationToken::new(),
        );
        let rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(actual)) =
            requested.terminal().unwrap().outcome()
        else {
            panic!("direct declaration-import terminal failed: {requested:?}")
        };
        assert_eq!(actual, &expected);
        assert_eq!(
            requested
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.parse-module",
            ]),
            "direct import oracle must not pass through a batch/root semantic adapter"
        );
        assert_eq!(requested.dependencies().len(), 3);
        assert_eq!(requested.inputs().len(), 1);
        assert_eq!(requested.inputs()[0].input, test_import_graph_input());
    }

    #[test]
    fn direct_semantic_keys_own_declaration_validity() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let cases = [
            (
                "struct S { x: i32, x: i64 }",
                Category::Struct,
                "S",
                "duplicate-field",
            ),
            ("enum E { A, A }", Category::Enum, "E", "duplicate-variant"),
            (
                "@copy linear struct L { x: i32 }",
                Category::Struct,
                "L",
                "linear-copy",
            ),
        ];
        for (source_text, category, name, expected) in cases {
            let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
            let module = ModuleId::from_logical_path("main.rue").unwrap();
            let mut database = RevisionedQueryDatabase::default();
            let revision = database.source_revision(
                &super::super::session::ExactSourceInput::new(&source),
                &source,
            );
            let declaration = declaration_candidate(&database, revision, &module, category, name);
            let value = request_semantic_nucleus(
                &database,
                revision,
                Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration,
                    configuration: semantic_configuration(),
                }),
            );
            let valid = matches!(
                (&*expected, &value),
                (
                    "duplicate-field",
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DuplicateField { .. }
                    ))
                ) | (
                    "duplicate-variant",
                    Value::Failure(Failure::Diagnostic(
                        rue_error::ErrorKind::DuplicateVariant { .. }
                    ))
                ) | (
                    "linear-copy",
                    Value::Failure(Failure::Diagnostic(rue_error::ErrorKind::LinearStructCopy(
                        _
                    )))
                )
            );
            assert!(valid, "direct signature did not own {expected}: {value:?}");
        }

        for (source_text, name, expected) in [
            (
                "drop fn Missing(self) {}",
                "Missing",
                "unknown-destructor-owner",
            ),
            (
                "struct S {} drop fn S(self) {} drop fn S(self) {}",
                "S",
                "duplicate-destructor",
            ),
        ] {
            let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
            let module = ModuleId::from_logical_path("main.rue").unwrap();
            let mut database = RevisionedQueryDatabase::default();
            let revision = database.source_revision(
                &super::super::session::ExactSourceInput::new(&source),
                &source,
            );
            let declaration =
                declaration_candidate(&database, revision, &module, Category::Destructor, name);
            let query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            };
            for key in [Key::Signature(query.clone()), Key::Identity(query.clone())] {
                let value = request_semantic_nucleus(&database, revision, key);
                let valid = match expected {
                    "unknown-destructor-owner" => matches!(
                        &value,
                        Value::Failure(Failure::Diagnostic(
                            rue_error::ErrorKind::DestructorUnknownType { .. }
                        ))
                    ),
                    "duplicate-destructor" => matches!(
                        &value,
                        Value::Failure(Failure::DiagnosticAtDeclaration {
                            kind: rue_error::ErrorKind::DuplicateDestructor { .. },
                            declaration,
                        }) if declaration.duplicate_discriminator == 1
                    ),
                    _ => false,
                };
                assert!(
                    valid,
                    "direct destructor terminal did not own {expected}: {value:?}"
                );
            }
        }

        for (source_text, category, name) in [
            (
                "struct S { fn m(self, a: i32, a: i32) {} }",
                Category::Method,
                "m",
            ),
            (
                "struct S { fn make(a: i32, a: i32) {} }",
                Category::AssociatedFunction,
                "make",
            ),
        ] {
            let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
            let module = ModuleId::from_logical_path("main.rue").unwrap();
            let mut database = RevisionedQueryDatabase::default();
            let revision = database.source_revision(
                &super::super::session::ExactSourceInput::new(&source),
                &source,
            );
            let declaration = declaration_candidate(&database, revision, &module, category, name);
            let value = request_semantic_nucleus(
                &database,
                revision,
                Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration,
                    configuration: semantic_configuration(),
                }),
            );
            assert!(
                matches!(
                    value,
                    Value::Failure(Failure::DiagnosticAtParameter {
                        kind: rue_error::ErrorKind::DuplicateParameter { .. },
                        ordinal: 1,
                    })
                ),
                "direct nested signature lost its duplicate occurrence: {value:?}"
            );
        }

        for (source_text, expected_duplicate) in [
            ("const C: i32 = 1; const C: i32 = 2;", true),
            ("fn C() -> i32 { 0 } const C: i32 = 1;", false),
        ] {
            let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text)], 1);
            let module = ModuleId::from_logical_path("main.rue").unwrap();
            let mut database = RevisionedQueryDatabase::default();
            let revision = database.source_revision(
                &super::super::session::ExactSourceInput::new(&source),
                &source,
            );
            let declaration =
                declaration_candidate(&database, revision, &module, Category::ConstCandidate, "C");
            let value = request_semantic_nucleus(
                &database,
                revision,
                Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration,
                    configuration: semantic_configuration(),
                }),
            );
            assert!(
                if expected_duplicate {
                    matches!(
                        value,
                        Value::Failure(Failure::Diagnostic(
                            rue_error::ErrorKind::DuplicateConstant { .. }
                        ))
                    )
                } else {
                    matches!(
                        value,
                        Value::Failure(Failure::Diagnostic(
                            rue_error::ErrorKind::DuplicateFunctionDefinition { .. }
                        ))
                    )
                },
                "direct const key did not own name validity: {value:?}"
            );
        }
    }

    #[test]
    fn direct_const_keys_preserve_structured_evaluator_failures() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            ConstResolutionProjection, SemanticNucleusFailure as Failure,
            SemanticNucleusKey as Key, SemanticNucleusValue as Value,
        };
        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct P { x: i32 }\
                 const SIZE: i32 = @size_of(i32);\
                 const AGG: P = P { x: 1 };\
                 const ZERO: i32 = 5 / 0;\
                 const OVF: i32 = 2147483647 + 1;\
                 const LOCAL: u8 = { let y: u8 = 255; y + 1 };\
                 const TARGET: i32 = if @target_arch() == Arch.Linux { 1 } else { 0 };\
                 const BOOL: bool = true != false;",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let query = |name: &str| {
            let declaration =
                declaration_candidate(&database, revision, &module, Category::ConstCandidate, name);
            request_semantic_nucleus(
                &database,
                revision,
                Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration,
                    configuration: semantic_configuration(),
                }),
            )
        };
        assert!(matches!(
            query("SIZE"),
            Value::Failure(Failure::Diagnostic(
                rue_error::ErrorKind::ConstExprNotSupported { .. }
            ))
        ));
        assert!(matches!(
            query("AGG"),
            Value::Failure(Failure::Diagnostic(
                rue_error::ErrorKind::ConstExprNotSupported { .. }
            ))
        ));
        for name in ["ZERO", "OVF", "LOCAL"] {
            assert!(matches!(
                query(name),
                Value::Failure(Failure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { .. }
                ))
            ));
        }
        assert!(matches!(
            query("TARGET"),
            Value::Failure(Failure::Diagnostic(
                rue_error::ErrorKind::UnknownVariant { .. }
            ))
        ));
        assert!(matches!(
            query("BOOL"),
            Value::ConstResolution(ConstResolutionProjection::Value {
                value,
                ..
            }) if matches!(*value, crate::durable_semantics::DurableConstValue::Bool(true))
        ));
    }

    #[test]
    fn semantic_nucleus_resolves_exact_signatures_without_whole_module_semantics() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::durable_semantics::DurableType as T;
        use crate::semantic_query_nucleus::{
            DeclarationSignatureProjection as Signature, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct Node { next: ptr const Node, } fn choose(comptime T: type, value: T) -> T { value }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let configuration = semantic_configuration();

        let node = declaration_candidate(&database, revision, &module, Category::Struct, "Node");
        let node_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: node,
            configuration: configuration.clone(),
        };
        let identity =
            request_semantic_nucleus(&database, revision, Key::Identity(node_query.clone()));
        let Value::Identity(identity) = identity else {
            panic!("expected Node identity, got {identity:?}")
        };
        let signature = request_semantic_nucleus(&database, revision, Key::Signature(node_query));
        assert_eq!(
            signature,
            Value::Signature(crate::semantic_query_nucleus::ResolvedDeclarationSignature {
                signature: Signature::Struct {
                    fields: vec![(
                        Arc::from("next"),
                        T::PtrConst(Box::new(T::Nominal(identity.key.clone())))
                    )]
                    .into(),
                    is_copy: false,
                    is_linear: false,
                    is_repr_c: false,
                },
                anonymous_nominals: Arc::from([]),
                dependencies: vec![
                    crate::semantic_query_nucleus::SemanticDeclarationDependency {
                        source: identity.key.clone(),
                        kind: rue_air::DeclarationTypeDependencyKind::Field,
                        target: crate::semantic_query_nucleus::SemanticDeclarationDependencyTarget::NamedType(
                            identity.key,
                        ),
                    },
                ]
                .into(),
                deferred_ownership: Arc::from([]),
            })
        );

        let choose =
            declaration_candidate(&database, revision, &module, Category::Function, "choose");
        let signature = request_semantic_nucleus(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: choose,
                configuration,
            }),
        );
        let Value::Signature(crate::semantic_query_nucleus::ResolvedDeclarationSignature {
            signature: Signature::Callable {
                parameters, result, ..
            },
            ..
        }) = signature
        else {
            panic!("expected callable signature, got {signature:?}")
        };
        assert_eq!(parameters[0].ty, T::ComptimeType);
        assert_eq!(parameters[1].ty, T::GenericParameter(0));
        assert_eq!(result, T::GenericParameter(0));
    }

    #[test]
    fn nominal_well_formedness_is_a_keyed_query_and_preserves_indirection() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct Bad { next: [Bad; 0] } struct Good { next: ptr const Good }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let query = |declaration| crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration,
            configuration: semantic_configuration(),
        };

        let bad = declaration_candidate(&database, revision, &module, Category::Struct, "Bad");
        assert!(matches!(
            request_semantic_nucleus(
                &database,
                revision,
                Key::NominalWellFormedness(query(bad)),
            ),
            Value::Failure(Failure::Diagnostic(
                rue_error::ErrorKind::RecursiveTypeInfiniteSize { ref name, ref cycle }
            )) if name == "Bad" && cycle == "Bad -> Bad"
        ));

        let good = declaration_candidate(&database, revision, &module, Category::Struct, "Good");
        assert_eq!(
            request_semantic_nucleus(&database, revision, Key::NominalWellFormedness(query(good)),),
            Value::NominalWellFormedness,
        );
    }

    #[test]
    fn require_droppable_propagates_signature_cycles_and_accepts_deferred_pointer_graphs() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let cycle_source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn Loop(comptime T: type) -> type { @require_droppable(Loop(T)); struct { value: ptr const T } } const X = Loop(i32);",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut cycle_database = RevisionedQueryDatabase::default();
        let cycle_revision = cycle_database.source_revision(
            &super::super::session::ExactSourceInput::new(&cycle_source),
            &cycle_source,
        );
        let alias = declaration_candidate(
            &cycle_database,
            cycle_revision,
            &module,
            Category::ConstCandidate,
            "X",
        );
        let cycle = request_semantic_nucleus(
            &cycle_database,
            cycle_revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: alias,
                configuration: semantic_configuration(),
            }),
        );
        assert!(
            matches!(
                &cycle,
                Value::Failure(Failure::Cycle(nodes))
                    if nodes.iter().any(|name| name.as_ref() == "Loop")
            ),
            "unexpected cycle result: {cycle:?}"
        );

        let control_source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn Wrap(comptime T: type) -> type { @require_droppable(T); struct { value: ptr const T } } struct Node { next: ptr const Wrap(Node) }",
            )],
            1,
        );
        let mut control_database = RevisionedQueryDatabase::default();
        let control_revision = control_database.source_revision(
            &super::super::session::ExactSourceInput::new(&control_source),
            &control_source,
        );
        let node = declaration_candidate(
            &control_database,
            control_revision,
            &module,
            Category::Struct,
            "Node",
        );
        let producer = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: node,
            configuration: semantic_configuration(),
        };
        let signature = request_semantic_nucleus(
            &control_database,
            control_revision,
            Key::Signature(producer.clone()),
        );
        let Value::Signature(signature) = signature else {
            panic!("expected deferred pointer signature, got {signature:?}")
        };
        let [gate] = signature.deferred_ownership.as_ref() else {
            panic!("expected one deferred ownership gate: {signature:?}")
        };
        assert_eq!(
            request_semantic_nucleus(
                &control_database,
                control_revision,
                Key::DeferredOwnership(crate::semantic_query_nucleus::DeferredOwnershipQueryKey {
                    producer,
                    gate: gate.clone(),
                }),
            ),
            Value::DeferredOwnership,
        );
    }

    #[test]
    fn signature_engine_cycles_publish_family_owned_domain_failures() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn A(x: B(i32)) -> i32 { 0 } fn B(x: A(i32)) -> i32 { 0 }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let declaration =
            declaration_candidate(&database, revision, &module, Category::Function, "A");
        let result = request_semantic_nucleus(
            &database,
            revision,
            Key::Signature(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration,
                configuration: semantic_configuration(),
            }),
        );
        assert!(
            matches!(
                &result,
                Value::Failure(Failure::SignatureReentry { signature, cycle })
                    if signature.name() == "B"
                        && cycle.as_ref() == [Arc::from("A"), Arc::from("B"), Arc::from("A")]
            ),
            "unexpected cycle diagnostic: {result:?}"
        );
    }

    #[test]
    fn semantic_nucleus_evaluates_only_selected_const_dependencies_and_reports_cycles() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::durable_semantics::DurableConstValue as Const;
        use crate::semantic_query_nucleus::{
            ConstResolutionProjection as Resolution, SemanticNucleusFailure as Failure,
            SemanticNucleusKey as Key, SemanticNucleusValue as Value,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const base: i32 = 20; const selected: i32 = if true { base + 22 } else { missing }; const left: i32 = right; const right: i32 = left;",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let configuration = semantic_configuration();
        let query = |name: &str| {
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    revision,
                    &module,
                    Category::ConstCandidate,
                    name,
                ),
                configuration: configuration.clone(),
            })
        };

        let selected = request_semantic_nucleus(&database, revision, query("selected"));
        assert!(matches!(
            selected,
            Value::ConstResolution(Resolution::Value {
                value,
                ..
            }) if matches!(value.as_ref(), Const::Integer(42))
        ));
        let cycle = request_semantic_nucleus(&database, revision, query("left"));
        assert!(
            matches!(cycle, Value::Failure(Failure::Cycle(ref nodes)) if !nodes.is_empty()),
            "expected a domain cycle, got {cycle:?}"
        );
    }

    #[test]
    fn semantic_nucleus_selects_declaration_time_target_branches_from_exact_configuration() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::durable_semantics::{DurableConstValue as Const, DurableType as Type};
        use crate::semantic_query_nucleus::{
            ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const arch: i32 = match @target_arch() { Arch.X86_64 => 64, Arch.Aarch64 => 32 }; const os: i32 = if @target_os() == Os.Macos { 2 } else { 1 }; const model = match @target_data_model() { DataModel.Ilp32 => i8, DataModel.Lp64 => i64, DataModel.Llp64 => i16 };",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let request = |database: &RevisionedQueryDatabase,
                       target: rue_target::Target,
                       name: &str| {
            let mut configuration = semantic_configuration();
            configuration.target = target;
            request_semantic_nucleus(
                database,
                revision,
                Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                    declaration: declaration_candidate(
                        database,
                        revision,
                        &module,
                        Category::ConstCandidate,
                        name,
                    ),
                    configuration,
                }),
            )
        };

        assert!(matches!(
            request(&database, rue_target::Target::X86_64Linux, "arch"),
            Value::ConstResolution(Resolution::Value {
                value,
                ty: Type::I32,
                ..
            }) if matches!(value.as_ref(), Const::Integer(64))
        ));
        assert!(matches!(
            request(&database, rue_target::Target::Aarch64Macos, "arch"),
            Value::ConstResolution(Resolution::Value {
                value,
                ty: Type::I32,
                ..
            }) if matches!(value.as_ref(), Const::Integer(32))
        ));
        assert!(matches!(
            request(&database, rue_target::Target::Aarch64Macos, "os"),
            Value::ConstResolution(Resolution::Value {
                value,
                ty: Type::I32,
                ..
            }) if matches!(value.as_ref(), Const::Integer(2))
        ));
        assert!(matches!(
            request(&database, rue_target::Target::Aarch64Linux, "model"),
            Value::ConstResolution(Resolution::Value {
                value,
                ty: Type::ComptimeType,
                ..
            }) if matches!(value.as_ref(), Const::Type(Type::I64))
        ));
    }

    #[test]
    fn semantic_nucleus_demand_does_not_touch_unrelated_declarations() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::durable_semantics::DurableConstValue as Const;
        use crate::semantic_query_nucleus::{
            ConstResolutionProjection as Resolution, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let mut text = String::from("const base: i32 = 20; const selected: i32 = base + 22;\n");
        for index in 0..128 {
            text.push_str(&format!("const unrelated{index}: i32 = missing{index};\n"));
        }
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", &text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let selected = declaration_candidate(
            &database,
            revision,
            &module,
            Category::ConstCandidate,
            "selected",
        );
        let value = request_semantic_nucleus(
            &database,
            revision,
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: selected,
                configuration: semantic_configuration(),
            }),
        );
        assert!(matches!(
            value,
            Value::ConstResolution(Resolution::Value {
                value,
                ..
            }) if matches!(value.as_ref(), Const::Integer(42))
        ));
        assert_eq!(
            database.semantic_nucleus.retention().terminals,
            2,
            "only `selected` and its exact `base` dependency may publish semantic terminals"
        );
    }

    #[test]
    fn semantic_nucleus_lifecycle_distinguishes_terminals_from_control_flow() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::durable_semantics::DurableConstValue as Const;
        use crate::semantic_query_nucleus::{
            ConstResolutionProjection as Resolution, SemanticNucleusFailure as Failure,
            SemanticNucleusKey as Key, SemanticNucleusValue as Value,
        };

        let source_text = (0..=MODULE_QUERY_MEMO_RETENTION)
            .map(|index| format!("const c{index}: i32 = {index};"))
            .chain([
                "const bad: i32 = missing;".to_owned(),
                "const canceled: i32 = 7;".to_owned(),
            ])
            .collect::<Vec<_>>()
            .join("\n");
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", &source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database =
            RevisionedQueryDatabase::with_declaration_memo_retention(MODULE_QUERY_MEMO_RETENTION);
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let configuration = semantic_configuration();
        let query = |name: &str| crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &database,
                revision,
                &module,
                Category::ConstCandidate,
                name,
            ),
            configuration: configuration.clone(),
        };

        let c0 = Key::ConstResolution(query("c0"));
        let cold = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            c0.clone(),
            CancellationToken::new(),
        );
        assert_eq!(execution(&cold), RequestExecution::Computed);
        let cold_terminal = cold.terminal().unwrap();
        let cold_stamp = cold_terminal.stamp();
        let rue_query::QueryOutcome::Success(cold_value) = cold_terminal.outcome() else {
            unreachable!()
        };
        assert!(matches!(
            cold_value,
            Value::ConstResolution(Resolution::Value {
                value,
                ..
            }) if matches!(value.as_ref(), Const::Integer(0))
        ));

        let warm = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            c0.clone(),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(warm.terminal().unwrap().stamp(), cold_stamp);
        assert_eq!(warm.terminal().unwrap().outcome(), cold_terminal.outcome());

        let bad = Key::ConstResolution(query("bad"));
        let failed = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            bad.clone(),
            CancellationToken::new(),
        );
        let failed_terminal = failed.terminal().unwrap();
        assert_eq!(failed_terminal.kind(), QueryTerminalKind::Failure);
        assert!(matches!(
            failed_terminal.outcome(),
            rue_query::QueryOutcome::Success(Value::Failure(Failure::Resolution(_)))
        ));
        let failed_again = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            bad,
            CancellationToken::new(),
        );
        assert_eq!(execution(&failed_again), RequestExecution::Reused);
        assert_eq!(
            failed_again.terminal().unwrap().stamp(),
            failed_terminal.stamp(),
            "deterministic semantic failures are reusable terminals"
        );

        let canceled_key = Key::ConstResolution(query("canceled"));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let canceled = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            canceled_key.clone(),
            cancellation,
        );
        assert_eq!(execution(&canceled), RequestExecution::Aborted);
        assert!(canceled.terminal().is_none());
        let after_cancel = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            canceled_key,
            CancellationToken::new(),
        );
        assert_eq!(execution(&after_cancel), RequestExecution::Computed);

        let cycle = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            Key::EngineCycleProbe(query("canceled")),
            CancellationToken::new(),
        );
        assert_eq!(execution(&cycle), RequestExecution::Aborted);
        assert!(matches!(cycle.abort(), Some(QueryAbort::Cycle(_))));
        assert!(cycle.terminal().is_none());

        for index in 1..=MODULE_QUERY_MEMO_RETENTION {
            let requested = database.runtime.request_registered(
                &database.semantic_nucleus,
                revision,
                Key::ConstResolution(query(&format!("c{index}"))),
                CancellationToken::new(),
            );
            assert!(requested.terminal().is_some());
        }
        assert_eq!(
            database.semantic_nucleus.retention().terminals,
            MODULE_QUERY_MEMO_RETENTION
        );
        let after_eviction = database.runtime.request_registered(
            &database.semantic_nucleus,
            revision,
            c0,
            CancellationToken::new(),
        );
        assert_eq!(execution(&after_eviction), RequestExecution::Computed);
        assert_eq!(
            after_eviction.terminal().unwrap().outcome(),
            cold_terminal.outcome()
        );

        let broken = source_snapshot(
            &[(1, "/main.rue", "main.rue", "const value: i32 = missing;")],
            1,
        );
        let fixed = source_snapshot(&[(1, "/main.rue", "main.rue", "const value: i32 = 42;")], 1);
        let mut recovery = RevisionedQueryDatabase::default();
        let broken_revision = recovery.source_revision(
            &super::super::session::ExactSourceInput::new(&broken),
            &broken,
        );
        let broken_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &recovery,
                broken_revision,
                &module,
                Category::ConstCandidate,
                "value",
            ),
            configuration: configuration.clone(),
        };
        assert!(matches!(
            request_semantic_nucleus(
                &recovery,
                broken_revision,
                Key::ConstResolution(broken_query)
            ),
            Value::Failure(Failure::Resolution(_))
        ));
        let fixed_revision = recovery.source_revision(
            &super::super::session::ExactSourceInput::new(&fixed),
            &fixed,
        );
        let fixed_query = crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
            declaration: declaration_candidate(
                &recovery,
                fixed_revision,
                &module,
                Category::ConstCandidate,
                "value",
            ),
            configuration,
        };
        assert!(matches!(
            request_semantic_nucleus(&recovery, fixed_revision, Key::ConstResolution(fixed_query)),
            Value::ConstResolution(Resolution::Value {
                value,
                ..
            }) if matches!(value.as_ref(), Const::Integer(42))
        ));
    }

    #[test]
    fn declaration_shell_queries_are_keyed_exact_and_payload_stable() {
        let first = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "struct Box { fn get(self) -> i32 { 1 } } const item = 1; fn main() {}",
            )],
            1,
        );
        let edited = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "// shifted file\nstruct Box { fn // comment-only signature trivia\n get(self) -> i32 { 999 } } const item = @import(\"x.rue\"); // shifted again\n fn main() { let x = 2; }",
            )],
            1,
        );
        let main = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let indexed = database.runtime.request_registered(
            &database.declaration_occurrence_indexes,
            first_revision,
            ModuleQueryKey(main.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&indexed), RequestExecution::Computed);
        assert_eq!(indexed.dependencies().len(), 1);
        let terminal = indexed.terminal().unwrap();
        let rue_query::QueryOutcome::Success(indexed_value) = terminal.outcome() else {
            unreachable!()
        };
        let DeclarationOccurrenceIndexValue::Available(indexed_value) = indexed_value else {
            panic!("expected available occurrence index")
        };
        let keys = indexed_value
            .capabilities
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 4);
        let mut shell_stamps = BTreeMap::new();
        for key in &keys {
            let first = database.runtime.request_registered(
                &database.declaration_shells,
                first_revision,
                DeclarationShellQueryKey(key.clone()),
                CancellationToken::new(),
            );
            assert_eq!(execution(&first), RequestExecution::Computed);
            shell_stamps.insert(key.stable_identity(), first.terminal().unwrap().stamp());
            assert_eq!(
                first
                    .dependencies()
                    .iter()
                    .map(|dependency| dependency.node.family())
                    .collect::<Vec<_>>(),
                vec![
                    "compiler.declaration-occurrence-index",
                    "compiler.parse-module"
                ]
            );
            let warm = database.runtime.request_registered(
                &database.declaration_shells,
                first_revision,
                DeclarationShellQueryKey(key.clone()),
                CancellationToken::new(),
            );
            assert_eq!(execution(&warm), RequestExecution::Reused);
        }

        let edited_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&edited),
            &edited,
        );
        let edited_index = database.runtime.request_registered(
            &database.declaration_occurrence_indexes,
            edited_revision,
            ModuleQueryKey(main),
            CancellationToken::new(),
        );
        let rue_query::QueryOutcome::Success(edited_value) =
            edited_index.terminal().unwrap().outcome()
        else {
            unreachable!()
        };
        let DeclarationOccurrenceIndexValue::Available(edited_value) = edited_value else {
            panic!("expected available edited occurrence index")
        };
        assert_eq!(&indexed_value.capabilities, &edited_value.capabilities);
        for key in &keys {
            let revalidated = database.runtime.request_registered(
                &database.declaration_shells,
                edited_revision,
                DeclarationShellQueryKey(key.clone()),
                CancellationToken::new(),
            );
            let terminal = revalidated.terminal().unwrap();
            assert_eq!(
                terminal.stamp(),
                shell_stamps[&key.stable_identity()],
                "payload-only edits must preserve the shell publication stamp"
            );
        }
    }

    #[test]
    fn canceled_declaration_shell_request_publishes_no_terminal_and_recovers() {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let main = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let indexed = database.runtime.request_registered(
            &database.declaration_occurrence_indexes,
            revision,
            ModuleQueryKey(main),
            CancellationToken::new(),
        );
        let rue_query::QueryOutcome::Success(indexed) = indexed.terminal().unwrap().outcome()
        else {
            unreachable!()
        };
        let DeclarationOccurrenceIndexValue::Available(indexed) = indexed else {
            panic!("expected available occurrence index")
        };
        let key = indexed.capabilities.keys().next().unwrap().clone();
        let canceled = CancellationToken::new();
        canceled.cancel();
        let aborted = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key.clone()),
            canceled,
        );
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(aborted.terminal().is_none());
        let recovered = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key),
            CancellationToken::new(),
        );
        assert_eq!(execution(&recovered), RequestExecution::Computed);
        assert!(recovered.terminal().is_some());
    }

    #[test]
    fn absent_declaration_shell_is_a_typed_position_free_failure_terminal() {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() {}")], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let key = crate::declaration_candidate::DeclarationCandidateKey {
            module,
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from("missing"),
            owner: None,
            duplicate_discriminator: 0,
        };
        let requested = database.runtime.request_registered(
            &database.declaration_shells,
            revision,
            DeclarationShellQueryKey(key.clone()),
            CancellationToken::new(),
        );
        let terminal = requested.terminal().unwrap();
        assert_eq!(terminal.kind(), QueryTerminalKind::Failure);
        assert!(terminal.diagnostics().is_empty());
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(DeclarationShellQueryValue::Failure(
                crate::declaration_candidate::DeclarationShellFailure::Absent(absent)
            )) if absent == &key
        ));
    }

    #[test]
    fn raw_const_syntax_queries_select_exactly_and_reuse_across_unrelated_edits() {
        let first = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const selected: ptr const u8 = @import(\"dep.rue\").value; const other = 1; fn main() {}",
            )],
            1,
        );
        let edited = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const selected: ptr const u8 = @import(\"dep.rue\").value; const other = 999; fn main() { let x = 2; }",
            )],
            1,
        );
        let key = crate::declaration_candidate::DeclarationCandidateKey {
            module: ModuleId::from_logical_path("main.rue").unwrap(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name: Arc::from("selected"),
            owner: None,
            duplicate_discriminator: 0,
        };
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let parsed = database.runtime.request_registered(
            &database.parse_modules,
            first_revision,
            ModuleQueryKey(key.module.clone()),
            CancellationToken::new(),
        );
        let parsed_module = match parsed.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            rue_query::QueryOutcome::Failure(_) => unreachable!(),
        };
        assert_eq!(
            parsed_module.raw_const_syntax_materialization_count(),
            0,
            "module indexing must retain only private locators"
        );
        let selected = database.runtime.request_registered(
            &database.raw_const_syntax,
            first_revision,
            RawConstSyntaxQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&selected), RequestExecution::Computed);
        assert_eq!(
            selected
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<Vec<_>>(),
            vec![
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.parse-module",
            ]
        );
        let terminal = selected.terminal().unwrap();
        let first_stamp = terminal.stamp();
        assert_eq!(terminal.kind(), QueryTerminalKind::Success);
        assert!(terminal.diagnostics().is_empty());
        assert!(matches!(
            terminal.outcome(),
            rue_query::QueryOutcome::Success(RawConstSyntaxQueryValue::Available(syntax))
                if syntax.declared_type.as_deref() == Some("ptr const u8")
                    && syntax.initializer.as_ref() == "@import(\"dep.rue\").value"
        ));
        assert_eq!(
            parsed_module.raw_const_syntax_materialization_count(),
            1,
            "demanding one key must materialize only that constant"
        );

        let warm = database.runtime.request_registered(
            &database.raw_const_syntax,
            first_revision,
            RawConstSyntaxQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(parsed_module.raw_const_syntax_materialization_count(), 1);

        let edited_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&edited),
            &edited,
        );
        let revalidated = database.runtime.request_registered(
            &database.raw_const_syntax,
            edited_revision,
            RawConstSyntaxQueryKey(key),
            CancellationToken::new(),
        );
        assert_eq!(
            revalidated.terminal().unwrap().stamp(),
            first_stamp,
            "an unrelated declaration edit must preserve the exact syntax terminal stamp"
        );
    }

    #[test]
    fn raw_const_syntax_duplicate_discriminators_select_distinct_occurrences() {
        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const duplicate = 11; const duplicate = 22;",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        for (duplicate_discriminator, expected) in [(0, "11"), (1, "22")] {
            let key = crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category:
                    crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
                name: Arc::from("duplicate"),
                owner: None,
                duplicate_discriminator,
            };
            let requested = database.runtime.request_registered(
                &database.raw_const_syntax,
                revision,
                RawConstSyntaxQueryKey(key),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(RawConstSyntaxQueryValue::Available(syntax))
                    if syntax.initializer.as_ref() == expected
            ));
        }
    }

    #[test]
    fn raw_const_syntax_failures_are_stable_and_position_free() {
        use crate::declaration_candidate::{
            DeclarationCandidateCategory as Category, DeclarationOccurrenceFailure,
            RawConstSyntaxFailure,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const present = 1; fn callable() {}",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let key =
            |category, name: &'static str| crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category,
                name: Arc::from(name),
                owner: None,
                duplicate_discriminator: 0,
            };
        let absent = key(Category::ConstCandidate, "absent");
        let non_const = key(Category::Function, "callable");
        for (requested_key, expected) in [
            (absent.clone(), RawConstSyntaxFailure::Absent(absent)),
            (
                non_const.clone(),
                RawConstSyntaxFailure::CategoryMismatch(non_const),
            ),
        ] {
            let requested = database.runtime.request_registered(
                &database.raw_const_syntax,
                revision,
                RawConstSyntaxQueryKey(requested_key),
                CancellationToken::new(),
            );
            let terminal = requested.terminal().unwrap();
            assert_eq!(terminal.kind(), QueryTerminalKind::Failure);
            assert!(terminal.diagnostics().is_empty());
            assert!(matches!(
                terminal.outcome(),
                rue_query::QueryOutcome::Success(RawConstSyntaxQueryValue::Failure(actual))
                    if actual == &expected
            ));
        }

        let rejected = source_snapshot(&[(1, "/main.rue", "main.rue", "const broken = ;")], 1);
        let rejected_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&rejected),
            &rejected,
        );
        let rejected_key = key(Category::ConstCandidate, "broken");
        let requested = database.runtime.request_registered(
            &database.raw_const_syntax,
            rejected_revision,
            RawConstSyntaxQueryKey(rejected_key),
            CancellationToken::new(),
        );
        assert!(matches!(
            requested.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(RawConstSyntaxQueryValue::Failure(
                RawConstSyntaxFailure::OccurrencesUnavailable(
                    DeclarationOccurrenceFailure::ParseRejected { module: failed_module }
                )
            )) if failed_module == &module
        ));
    }

    #[test]
    fn canceled_and_evicted_raw_const_syntax_requests_recover() {
        let source_text = (0..=MODULE_QUERY_MEMO_RETENTION)
            .map(|index| format!("const c{index} = {index};"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", &source_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database =
            RevisionedQueryDatabase::with_declaration_memo_retention(MODULE_QUERY_MEMO_RETENTION);
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let key = |index| crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::ConstCandidate,
            name: Arc::from(format!("c{index}")),
            owner: None,
            duplicate_discriminator: 0,
        };

        let canceled = CancellationToken::new();
        canceled.cancel();
        let aborted = database.runtime.request_registered(
            &database.raw_const_syntax,
            revision,
            RawConstSyntaxQueryKey(key(0)),
            canceled,
        );
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(aborted.terminal().is_none());

        for index in 0..=MODULE_QUERY_MEMO_RETENTION {
            let requested = database.runtime.request_registered(
                &database.raw_const_syntax,
                revision,
                RawConstSyntaxQueryKey(key(index)),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(RawConstSyntaxQueryValue::Available(_))
            ));
        }
        assert_eq!(
            database.raw_const_syntax.retention().terminals,
            MODULE_QUERY_MEMO_RETENTION
        );
        assert!(database.runtime.metrics().evictions >= 2);

        let recovered = database.runtime.request_registered(
            &database.raw_const_syntax,
            revision,
            RawConstSyntaxQueryKey(key(0)),
            CancellationToken::new(),
        );
        assert_eq!(execution(&recovered), RequestExecution::Computed);
        assert!(matches!(
            recovered.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(RawConstSyntaxQueryValue::Available(syntax))
                if syntax.initializer.as_ref() == "0"
        ));
    }

    fn raw_signature_text(
        syntax: &crate::declaration_candidate::RawDeclarationSignatureSyntax,
    ) -> String {
        syntax
            .declaration_fragments
            .iter()
            .map(AsRef::as_ref)
            .collect::<String>()
    }

    #[test]
    fn raw_declaration_signature_is_exact_lazy_and_red_green() {
        fn program(selected_type: &str, unrelated_body: u32) -> String {
            let mut source = String::new();
            for index in 0..128 {
                let body = if index == 64 { unrelated_body } else { index };
                source.push_str(&format!("fn unrelated{index}() -> i32 {{ {body} }}\n"));
            }
            source.push_str(&format!(
                "fn selected(value: {selected_type}) -> {selected_type} {{ value }}\n"
            ));
            source
        }

        let first_text = program("i32", 64);
        let unrelated_edit_text = program("i32", 999);
        let selected_edit_text = program("i64", 999);
        let first = source_snapshot(&[(1, "/main.rue", "main.rue", &first_text)], 1);
        let unrelated_edit =
            source_snapshot(&[(1, "/main.rue", "main.rue", &unrelated_edit_text)], 1);
        let selected_edit =
            source_snapshot(&[(1, "/main.rue", "main.rue", &selected_edit_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from("selected"),
            owner: None,
            duplicate_discriminator: 0,
        };
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let parsed = database.runtime.request_registered(
            &database.parse_modules,
            first_revision,
            ModuleQueryKey(module),
            CancellationToken::new(),
        );
        let parsed_module = match parsed.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            rue_query::QueryOutcome::Failure(_) => unreachable!(),
        };
        assert_eq!(
            parsed_module.raw_declaration_signature_terminal_materialization_count(),
            0,
            "indexing 129 declarations must allocate no raw-signature terminal fragments"
        );

        let first_request = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            first_revision,
            RawDeclarationSignatureQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&first_request), RequestExecution::Computed);
        assert_eq!(
            first_request
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<Vec<_>>(),
            vec![
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.parse-module",
            ]
        );
        let first_terminal = first_request.terminal().unwrap();
        let first_stamp = first_terminal.stamp();
        assert!(matches!(
            first_terminal.outcome(),
            rue_query::QueryOutcome::Success(
                RawDeclarationSignatureQueryValue::Available(syntax)
            ) if raw_signature_text(syntax).trim_end() == "fn selected(value: i32) -> i32"
                && syntax.extern_abi.is_none()
        ));
        assert_eq!(
            parsed_module.raw_declaration_signature_terminal_materialization_count(),
            1,
            "one exact cold demand must materialize one raw-signature terminal"
        );

        let warm = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            first_revision,
            RawDeclarationSignatureQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(
            parsed_module.raw_declaration_signature_terminal_materialization_count(),
            1,
            "warm reuse must not rematerialize raw-signature terminal fragments"
        );

        let unrelated_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&unrelated_edit),
            &unrelated_edit,
        );
        let unrelated_request = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            unrelated_revision,
            RawDeclarationSignatureQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(unrelated_request.terminal().unwrap().stamp(), first_stamp);

        let selected_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&selected_edit),
            &selected_edit,
        );
        let selected_request = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            selected_revision,
            RawDeclarationSignatureQueryKey(key),
            CancellationToken::new(),
        );
        let selected_terminal = selected_request.terminal().unwrap();
        assert_ne!(selected_terminal.stamp(), first_stamp);
        assert!(matches!(
            selected_terminal.outcome(),
            rue_query::QueryOutcome::Success(
                RawDeclarationSignatureQueryValue::Available(syntax)
            ) if raw_signature_text(syntax).trim_end() == "fn selected(value: i64) -> i64"
        ));
    }

    #[test]
    fn raw_declaration_signatures_cover_categories_without_struct_method_peers() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "@copy linear struct Box { value: i32, fn get(borrow self) -> i32 { self.value } @allow(unused_function) fn make(value: i32) -> Box { Box { value } } }\n\
                 enum Choice { Empty, Value(i32, u64) }\n\
                 drop fn Box(self) {}\n\
                 extern \"C\" { fn foreign(value: ptr const u8) -> i32; }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let owner = crate::declaration_candidate::DeclarationCandidateOwner {
            category: Category::Struct,
            name: Arc::from("Box"),
        };
        let key = |category, name: &'static str, owner| {
            crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category,
                name: Arc::from(name),
                owner,
                duplicate_discriminator: 0,
            }
        };
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let request = |key| {
            database.runtime.request_registered(
                &database.raw_declaration_signatures,
                revision,
                RawDeclarationSignatureQueryKey(key),
                CancellationToken::new(),
            )
        };

        let structure = request(key(Category::Struct, "Box", None));
        let structure_terminal = structure.terminal().unwrap();
        let structure_stamp = structure_terminal.stamp();
        let structure = match structure_terminal.outcome() {
            rue_query::QueryOutcome::Success(RawDeclarationSignatureQueryValue::Available(
                syntax,
            )) => syntax,
            other => panic!("expected struct signature, got {other:?}"),
        };
        let structure_text = raw_signature_text(structure);
        assert!(structure_text.contains("@copy linear struct Box"));
        assert!(structure_text.contains("value: i32"));
        assert!(!structure_text.contains("fn get"));
        assert!(!structure_text.contains("fn make"));
        assert!(structure_text.trim_end().ends_with('}'));
        assert_eq!(structure.declaration_fragments.len(), 2);

        for (candidate, expected) in [
            (
                key(Category::Method, "get", Some(owner.clone())),
                "fn get(borrow self) -> i32",
            ),
            (
                key(Category::AssociatedFunction, "make", Some(owner.clone())),
                "@allow(unused_function) fn make(value: i32) -> Box",
            ),
            (
                key(Category::Enum, "Choice", None),
                "enum Choice { Empty, Value(i32, u64) }",
            ),
            (
                key(Category::Destructor, "Box", Some(owner)),
                "drop fn Box(self)",
            ),
        ] {
            let requested = request(candidate);
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(
                    RawDeclarationSignatureQueryValue::Available(syntax)
                ) if raw_signature_text(syntax).trim_end() == expected
                    && syntax.extern_abi.is_none()
            ));
        }

        let foreign = request(key(Category::ExternFunction, "foreign", None));
        assert!(matches!(
            foreign.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(
                RawDeclarationSignatureQueryValue::Available(syntax)
            ) if raw_signature_text(syntax) == "fn foreign(value: ptr const u8) -> i32;"
                && syntax.extern_abi.as_deref() == Some("\"C\"")
        ));

        let peer_signature_edit = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "@copy linear struct Box { value: i32, fn get(borrow self) -> u64 { 0 } @allow(unused_function) fn make(value: i32) -> Box { Box { value } } }\n\
                 enum Choice { Empty, Value(i32, u64) }\n\
                 drop fn Box(self) {}\n\
                 extern \"C\" { fn foreign(value: ptr const u8) -> i32; }",
            )],
            1,
        );
        let peer_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&peer_signature_edit),
            &peer_signature_edit,
        );
        let unchanged_structure = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            peer_revision,
            RawDeclarationSignatureQueryKey(key(Category::Struct, "Box", None)),
            CancellationToken::new(),
        );
        assert_eq!(
            unchanged_structure.terminal().unwrap().stamp(),
            structure_stamp,
            "a peer method signature must not change the struct signature terminal"
        );
    }

    #[test]
    fn raw_declaration_signature_boundaries_exclude_body_and_method_trivia() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let first = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn free(value: i32) -> i32 // free boundary one\n\
                     { value }\n\
                 struct Box { value: i32, // before first method one\n\
                     fn get(borrow self) -> i32 // method boundary one\n\
                         { self.value }\n\
                     // between methods one\n\
                     fn make(value: i32) -> Box // associated boundary one\n\
                         { Box { value } }\n\
                     // after last method one\n\
                 }\n\
                 drop fn Box(self) // destructor boundary one\n\
                     {}",
            )],
            1,
        );
        let trivia_edit = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn free(value: i32) -> i32         // free boundary two\n\
\n\
                 { value }\n\
                 struct Box { value: i32,             // before first method two\n\
\n\
                     fn get(borrow self) -> i32       // method boundary two\n\
\n\
                     { self.value }\n\
                         // between methods two\n\
                     fn make(value: i32) -> Box       // associated boundary two\n\
\n\
                     { Box { value } }\n\
                         // after last method two\n\
\n\
                 }\n\
                 drop fn Box(self)                    // destructor boundary two\n\
\n\
                 {}",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let owner = crate::declaration_candidate::DeclarationCandidateOwner {
            category: Category::Struct,
            name: Arc::from("Box"),
        };
        let key = |category, name: &'static str, owner| {
            crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category,
                name: Arc::from(name),
                owner,
                duplicate_discriminator: 0,
            }
        };
        let cases = [
            (
                key(Category::Function, "free", None),
                "fn free(value: i32) -> i32",
            ),
            (
                key(Category::Struct, "Box", None),
                "struct Box { value: i32}",
            ),
            (
                key(Category::Method, "get", Some(owner.clone())),
                "fn get(borrow self) -> i32",
            ),
            (
                key(Category::AssociatedFunction, "make", Some(owner.clone())),
                "fn make(value: i32) -> Box",
            ),
            (
                key(Category::Destructor, "Box", Some(owner)),
                "drop fn Box(self)",
            ),
        ];
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let mut first_terminals = Vec::new();
        for (candidate, expected) in &cases {
            let requested = database.runtime.request_registered(
                &database.raw_declaration_signatures,
                first_revision,
                RawDeclarationSignatureQueryKey(candidate.clone()),
                CancellationToken::new(),
            );
            let terminal = requested.terminal().unwrap();
            assert!(matches!(
                terminal.outcome(),
                rue_query::QueryOutcome::Success(
                    RawDeclarationSignatureQueryValue::Available(syntax)
                ) if raw_signature_text(syntax) == *expected
            ));
            first_terminals.push((candidate.clone(), terminal.stamp()));
        }

        let edited_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&trivia_edit),
            &trivia_edit,
        );
        for (candidate, first_stamp) in first_terminals {
            let requested = database.runtime.request_registered(
                &database.raw_declaration_signatures,
                edited_revision,
                RawDeclarationSignatureQueryKey(candidate),
                CancellationToken::new(),
            );
            assert_eq!(
                requested.terminal().unwrap().stamp(),
                first_stamp,
                "body-boundary and method-adjacent trivia must stay outside the signature terminal"
            );
        }
    }

    #[test]
    fn raw_declaration_signature_duplicate_discriminators_are_exact() {
        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn duplicate(value: i32) {} fn duplicate(value: i64) {}",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        for (duplicate_discriminator, expected) in [
            (0, "fn duplicate(value: i32)"),
            (1, "fn duplicate(value: i64)"),
        ] {
            let key = crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
                name: Arc::from("duplicate"),
                owner: None,
                duplicate_discriminator,
            };
            let requested = database.runtime.request_registered(
                &database.raw_declaration_signatures,
                revision,
                RawDeclarationSignatureQueryKey(key),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(
                    RawDeclarationSignatureQueryValue::Available(syntax)
                ) if raw_signature_text(syntax).trim_end() == expected
            ));
        }
    }

    #[test]
    fn raw_declaration_signature_failures_cancel_and_recover() {
        use crate::declaration_candidate::{
            DeclarationCandidateCategory as Category, DeclarationOccurrenceFailure,
            RawDeclarationSignatureFailure,
        };

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const value = 1; fn present() {}",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key =
            |category, name: &'static str| crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category,
                name: Arc::from(name),
                owner: None,
                duplicate_discriminator: 0,
            };
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let constant = key(Category::ConstCandidate, "value");
        let absent = key(Category::Function, "absent");
        for (candidate, expected) in [
            (
                constant.clone(),
                RawDeclarationSignatureFailure::CategoryMismatch(constant),
            ),
            (
                absent.clone(),
                RawDeclarationSignatureFailure::Absent(absent),
            ),
        ] {
            let requested = database.runtime.request_registered(
                &database.raw_declaration_signatures,
                revision,
                RawDeclarationSignatureQueryKey(candidate),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(
                    RawDeclarationSignatureQueryValue::Failure(actual)
                ) if actual == &expected
            ));
        }

        let present = key(Category::Function, "present");
        let canceled = CancellationToken::new();
        canceled.cancel();
        let aborted = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            revision,
            RawDeclarationSignatureQueryKey(present.clone()),
            canceled,
        );
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(aborted.terminal().is_none());
        let recovered = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            revision,
            RawDeclarationSignatureQueryKey(present),
            CancellationToken::new(),
        );
        assert!(matches!(
            recovered.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(RawDeclarationSignatureQueryValue::Available(_))
        ));

        let rejected = source_snapshot(&[(1, "/main.rue", "main.rue", "fn broken(")], 1);
        let rejected_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&rejected),
            &rejected,
        );
        let rejected_key = key(Category::Function, "broken");
        let requested = database.runtime.request_registered(
            &database.raw_declaration_signatures,
            rejected_revision,
            RawDeclarationSignatureQueryKey(rejected_key),
            CancellationToken::new(),
        );
        assert!(matches!(
            requested.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(
                RawDeclarationSignatureQueryValue::Failure(
                    RawDeclarationSignatureFailure::OccurrencesUnavailable(
                        DeclarationOccurrenceFailure::ParseRejected { module: failed_module }
                    )
                )
            ) if failed_module == &module
        ));
    }

    #[test]
    fn raw_declaration_body_is_exact_lazy_and_red_green() {
        fn program(selected_body: &str, unrelated_body: u32) -> String {
            let mut source = String::new();
            for index in 0..128 {
                let body = if index == 64 { unrelated_body } else { index };
                source.push_str(&format!("fn unrelated{index}() -> i32 {{ {body} }}\n"));
            }
            source.push_str(&format!(
                "fn selected(comptime T: type) -> type // boundary trivia\n{{ {selected_body} }}\n"
            ));
            source
        }

        let first_text = program("struct { value: T }", 64);
        let unrelated_edit_text = program("struct { value: T }", 999);
        let selected_edit_text = program("enum { Value(T), Empty }", 999);
        let first = source_snapshot(&[(1, "/main.rue", "main.rue", &first_text)], 1);
        let unrelated_edit =
            source_snapshot(&[(1, "/main.rue", "main.rue", &unrelated_edit_text)], 1);
        let selected_edit =
            source_snapshot(&[(1, "/main.rue", "main.rue", &selected_edit_text)], 1);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from("selected"),
            owner: None,
            duplicate_discriminator: 0,
        };
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let parsed = database.runtime.request_registered(
            &database.parse_modules,
            first_revision,
            ModuleQueryKey(module),
            CancellationToken::new(),
        );
        let parsed_module = match parsed.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            rue_query::QueryOutcome::Failure(_) => unreachable!(),
        };
        assert_eq!(
            parsed_module.raw_declaration_body_terminal_materialization_count(),
            0,
            "indexing must not allocate raw-body terminal text"
        );

        let first_request = database.runtime.request_registered(
            &database.raw_declaration_bodies,
            first_revision,
            RawDeclarationBodyQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&first_request), RequestExecution::Computed);
        assert_eq!(
            first_request
                .dependencies()
                .iter()
                .map(|dependency| dependency.node.family())
                .collect::<Vec<_>>(),
            vec![
                "compiler.declaration-occurrence-index",
                "compiler.declaration-shell",
                "compiler.parse-module",
            ]
        );
        let first_terminal = first_request.terminal().unwrap();
        let first_stamp = first_terminal.stamp();
        assert!(matches!(
            first_terminal.outcome(),
            rue_query::QueryOutcome::Success(RawDeclarationBodyQueryValue::Available(syntax))
                if syntax.body.as_ref() == "{ struct { value: T } }"
        ));
        assert_eq!(
            parsed_module.raw_declaration_body_terminal_materialization_count(),
            1
        );

        let warm = database.runtime.request_registered(
            &database.raw_declaration_bodies,
            first_revision,
            RawDeclarationBodyQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(
            parsed_module.raw_declaration_body_terminal_materialization_count(),
            1,
            "warm reuse must not rematerialize body text"
        );

        let unrelated_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&unrelated_edit),
            &unrelated_edit,
        );
        let unrelated_request = database.runtime.request_registered(
            &database.raw_declaration_bodies,
            unrelated_revision,
            RawDeclarationBodyQueryKey(key.clone()),
            CancellationToken::new(),
        );
        assert_eq!(unrelated_request.terminal().unwrap().stamp(), first_stamp);

        let selected_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&selected_edit),
            &selected_edit,
        );
        let selected_request = database.runtime.request_registered(
            &database.raw_declaration_bodies,
            selected_revision,
            RawDeclarationBodyQueryKey(key),
            CancellationToken::new(),
        );
        let selected_terminal = selected_request.terminal().unwrap();
        assert_ne!(selected_terminal.stamp(), first_stamp);
        assert!(matches!(
            selected_terminal.outcome(),
            rue_query::QueryOutcome::Success(RawDeclarationBodyQueryValue::Available(syntax))
                if syntax.body.as_ref() == "{ enum { Value(T), Empty } }"
        ));
    }

    #[test]
    fn raw_declaration_bodies_cover_body_categories_and_boundaries() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn free() -> i32 // excluded one\n{ 1 }\n\
                 struct Box { value: i32, fn get(borrow self) -> i32 // excluded two\n{ self.value } fn make(value: i32) -> Box { Box { value } } }\n\
                 drop fn Box(self) // excluded three\n{ let x = self; }\n\
                 const value = 1; extern \"C\" { fn foreign() -> i32; }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let owner = crate::declaration_candidate::DeclarationCandidateOwner {
            category: Category::Struct,
            name: Arc::from("Box"),
        };
        let key = |category, name: &'static str, owner| {
            crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category,
                name: Arc::from(name),
                owner,
                duplicate_discriminator: 0,
            }
        };
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        for (candidate, expected) in [
            (key(Category::Function, "free", None), "{ 1 }"),
            (
                key(Category::Method, "get", Some(owner.clone())),
                "{ self.value }",
            ),
            (
                key(Category::AssociatedFunction, "make", Some(owner.clone())),
                "{ Box { value } }",
            ),
            (
                key(Category::Destructor, "Box", Some(owner)),
                "{ let x = self; }",
            ),
        ] {
            let requested = database.runtime.request_registered(
                &database.raw_declaration_bodies,
                revision,
                RawDeclarationBodyQueryKey(candidate),
                CancellationToken::new(),
            );
            match requested.terminal().unwrap().outcome() {
                rue_query::QueryOutcome::Success(RawDeclarationBodyQueryValue::Available(
                    syntax,
                )) => assert_eq!(syntax.body.as_ref(), expected),
                other => panic!("expected raw body {expected:?}, got {other:?}"),
            }
        }

        for candidate in [
            key(Category::Struct, "Box", None),
            key(Category::ConstCandidate, "value", None),
            key(Category::ExternFunction, "foreign", None),
        ] {
            let requested = database.runtime.request_registered(
                &database.raw_declaration_bodies,
                revision,
                RawDeclarationBodyQueryKey(candidate.clone()),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(RawDeclarationBodyQueryValue::Failure(
                    crate::declaration_candidate::RawDeclarationBodyFailure::CategoryMismatch(
                        actual
                    )
                )) if actual == &candidate
            ));
        }
    }

    #[test]
    fn raw_declaration_body_duplicate_discriminators_cancel_and_recover() {
        let source = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "fn duplicate() -> i32 { 11 } fn duplicate() -> i32 { 22 }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = |duplicate_discriminator| crate::declaration_candidate::DeclarationCandidateKey {
            module: module.clone(),
            category: crate::declaration_candidate::DeclarationCandidateCategory::Function,
            name: Arc::from("duplicate"),
            owner: None,
            duplicate_discriminator,
        };
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );

        let canceled = CancellationToken::new();
        canceled.cancel();
        let aborted = database.runtime.request_registered(
            &database.raw_declaration_bodies,
            revision,
            RawDeclarationBodyQueryKey(key(0)),
            canceled,
        );
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(aborted.terminal().is_none());

        for (discriminator, expected) in [(0, "{ 11 }"), (1, "{ 22 }")] {
            let requested = database.runtime.request_registered(
                &database.raw_declaration_bodies,
                revision,
                RawDeclarationBodyQueryKey(key(discriminator)),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(RawDeclarationBodyQueryValue::Available(syntax))
                    if syntax.body.as_ref() == expected
            ));
        }
    }

    #[test]
    fn declaration_shell_batches_over_64_entries_reuse_without_thrashing() {
        let source_text = (0..129)
            .map(|index| format!("fn f{index}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", source_text.as_str())], 1);
        let main = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let indexed = database.runtime.request_registered(
            &database.declaration_occurrence_indexes,
            revision,
            ModuleQueryKey(main),
            CancellationToken::new(),
        );
        let rue_query::QueryOutcome::Success(indexed) = indexed.terminal().unwrap().outcome()
        else {
            unreachable!()
        };
        let DeclarationOccurrenceIndexValue::Available(indexed) = indexed else {
            panic!("expected available occurrence index")
        };
        let keys = indexed.capabilities.keys().cloned().collect::<Vec<_>>();
        let mut first_stamps = Vec::with_capacity(keys.len());
        for key in &keys {
            let requested = database.runtime.request_registered(
                &database.declaration_shells,
                revision,
                DeclarationShellQueryKey(key.clone()),
                CancellationToken::new(),
            );
            assert_eq!(execution(&requested), RequestExecution::Computed);
            first_stamps.push(requested.terminal().unwrap().stamp());
        }
        for (key, first_stamp) in keys.iter().zip(first_stamps) {
            let warm = database.runtime.request_registered(
                &database.declaration_shells,
                revision,
                DeclarationShellQueryKey(key.clone()),
                CancellationToken::new(),
            );
            assert_eq!(execution(&warm), RequestExecution::Reused);
            assert_eq!(warm.terminal().unwrap().stamp(), first_stamp);
        }
    }

    #[test]
    fn editing_one_demanded_module_reuses_other_module_terminals() {
        let first = source_snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn a() {}"),
                (2, "/b.rue", "b.rue", "fn b() -> i32 { 1 }"),
            ],
            1,
        );
        let second = source_snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn a() {}"),
                (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let a = ModuleId::from_logical_path("a.rue").unwrap();
        let b = ModuleId::from_logical_path("b.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let (first_a_parse, first_a_index, first_a_rir) =
            database.module_terminals(first_revision, a.clone());
        let (first_b_parse, first_b_index, first_b_rir) =
            database.module_terminals(first_revision, b.clone());

        let second_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&second),
            &second,
        );
        let (second_a_parse, second_a_index, second_a_rir) =
            database.module_terminals(second_revision, a);
        let (second_b_parse, second_b_index, second_b_rir) =
            database.module_terminals(second_revision, b);

        assert!(Arc::ptr_eq(&first_a_parse, &second_a_parse));
        assert!(Arc::ptr_eq(&first_a_index, &second_a_index));
        assert!(Arc::ptr_eq(&first_a_rir, &second_a_rir));
        assert!(!Arc::ptr_eq(&first_b_parse, &second_b_parse));
        assert!(!Arc::ptr_eq(&first_b_index, &second_b_index));
        assert!(!Arc::ptr_eq(&first_b_rir, &second_b_rir));
    }

    #[test]
    fn file_id_renumbering_reuses_terminals_and_rebinds_current_projections() {
        let first = source_snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
                (2, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
            ],
            1,
        );
        let second = source_snapshot(
            &[
                (1, "/inserted.rue", "inserted.rue", "fn inserted() {}"),
                (2, "/a.rue", "a.rue", "fn a() -> i32 { 1 }"),
                (3, "/b.rue", "b.rue", "fn b() -> i32 { 2 }"),
            ],
            2,
        );
        let a = ModuleId::from_logical_path("a.rue").unwrap();
        let b = ModuleId::from_logical_path("b.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let (first_parse, first_index, first_rir) =
            database.module_terminals(first_revision, a.clone());
        let _ = database.module_terminals(first_revision, b.clone());

        let second_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&second),
            &second,
        );
        let (second_parse, second_index, second_rir) =
            database.module_terminals(second_revision, a.clone());
        assert!(Arc::ptr_eq(&first_parse, &second_parse));
        assert!(Arc::ptr_eq(&first_index, &second_index));
        assert!(Arc::ptr_eq(&first_rir, &second_rir));
        assert_eq!(second_parse.file_id(), FileId::new(1));

        let (program, parse_work) =
            database.parse_program(second_revision, &a, [a.clone(), b.clone()]);
        let program = program.unwrap();
        assert_eq!(parse_work.syntax.parser_invocations, 0);
        assert_eq!(parse_work.modules_rebound, 2);
        let projected_a = program.module(&a).unwrap();
        assert_eq!(projected_a.file_id(), FileId::new(2));
        assert!(
            projected_a
                .tokens()
                .iter()
                .all(|token| token.span.file_id == FileId::new(2))
        );
        assert!(projected_a.ast().items.iter().all(|item| match item {
            rue_parser::Item::Function(function) => {
                function.span.file_id == FileId::new(2)
                    && function.body.span().file_id == FileId::new(2)
            }
            _ => false,
        }));
        assert!(
            projected_a
                .definitions()
                .candidates()
                .iter()
                .all(|definition| {
                    definition.name_span().file_id == FileId::new(2)
                        && definition.declaration_span().file_id == FileId::new(2)
                })
        );

        let indexes = database
            .projected_module_indexes(second_revision, &program)
            .unwrap();
        let a_index = indexes
            .iter()
            .find(|index| index.revision.module == a)
            .unwrap();
        assert!(a_index.definitions.iter().all(|definition| {
            definition.name_span.file_id == FileId::new(2)
                && definition.declaration_span.file_id == FileId::new(2)
        }));
        let merged = crate::canonical_merge::merge_parsed_modules_reusing_indexes(
            &program, &indexes, None, None,
        )
        .unwrap();
        let ordered_modules = program
            .modules()
            .iter()
            .map(|module| module.module_id().clone())
            .collect::<Vec<_>>();
        let (module_rirs, query_work) = database.module_rirs(second_revision, ordered_modules);
        let module_rirs = module_rirs.unwrap();
        assert_eq!(query_work.modules_visited, 0);
        let projected_rir = crate::canonical_lower::project_module_rirs_with_work(
            &merged,
            &module_rirs,
            query_work,
        )
        .unwrap();
        assert_eq!(projected_rir.work().modules_visited, 0);
        assert_eq!(projected_rir.work().items_visited, 0);
        assert_eq!(projected_rir.work().modules_projected, 2);
        assert_eq!(
            projected_rir.work().instructions_appended,
            projected_rir.rir().len()
        );
        assert_eq!(
            projected_rir.work().payload_words_appended,
            projected_rir.rir().extra_len()
        );
        assert!(
            projected_rir
                .rir()
                .iter()
                .any(|(_, instruction)| instruction.span.file_id == FileId::new(2))
        );
    }

    #[test]
    fn reused_parse_failures_are_rebound_to_the_current_file_id() {
        let first = source_snapshot(&[(1, "/broken.rue", "broken.rue", "fn broken( {")], 1);
        let second = source_snapshot(
            &[
                (1, "/inserted.rue", "inserted.rue", "fn inserted() {}"),
                (2, "/broken.rue", "broken.rue", "fn broken( {"),
            ],
            2,
        );
        let broken = ModuleId::from_logical_path("broken.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let (first_error, first_work) =
            database.parse_program(first_revision, &broken, std::iter::once(broken.clone()));
        assert_eq!(first_work.syntax.parser_invocations, 1);
        assert_eq!(
            first_error
                .unwrap_err()
                .first()
                .unwrap()
                .span()
                .unwrap()
                .file_id,
            FileId::new(1)
        );

        let second_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&second),
            &second,
        );
        let (second_error, second_work) =
            database.parse_program(second_revision, &broken, std::iter::once(broken.clone()));
        assert_eq!(second_work.syntax.parser_invocations, 0);
        assert_eq!(second_work.modules_reused, 1);
        assert_eq!(
            second_error
                .unwrap_err()
                .first()
                .unwrap()
                .span()
                .unwrap()
                .file_id,
            FileId::new(2)
        );
    }

    #[test]
    fn module_rir_terminal_adapter_preserves_failed_lowering_work() {
        let source = source_snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(&source).unwrap();
        let faulty = parsed.modules()[0].with_test_foreign_ast_symbol();
        let value = module_rir_value_from_lowering(lower_module_rir_with_work(faulty));
        assert!(value.result.is_err());
        assert_eq!(value.work.modules_visited, 1);
        assert_eq!(value.work.items_visited, 1);
        assert_eq!(value.work.modules_projected, 0);
    }

    #[test]
    fn invalid_undemanded_module_is_neither_parsed_nor_lowered() {
        let base = source_snapshot(&[(1, "/a.rue", "a.rue", "fn a() {}")], 1);
        let snapshot = source_snapshot(
            &[
                (1, "/a.rue", "a.rue", "fn a() {}"),
                (2, "/broken.rue", "broken.rue", "fn broken( {"),
            ],
            1,
        );
        let a = ModuleId::from_logical_path("a.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let base_revision =
            database.source_revision(&super::super::session::ExactSourceInput::new(&base), &base);
        let (base_parse, base_index, base_rir) =
            database.module_terminals(base_revision, a.clone());
        assert_eq!(database.runtime.metrics().claims, 3);
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&snapshot),
            &snapshot,
        );
        let (parsed, work) = database.parse_program(revision, &a, [a.clone()]);
        assert!(parsed.is_ok());
        assert_eq!(work.syntax.parser_invocations, 0);
        assert!(database.module_rirs(revision, [a]).0.is_ok());
        assert_eq!(database.runtime.metrics().claims, 3);
        let demanded = ModuleId::from_logical_path("a.rue").unwrap();
        let (next_parse, next_index, next_rir) = database.module_terminals(revision, demanded);
        assert!(Arc::ptr_eq(&base_parse, &next_parse));
        assert!(Arc::ptr_eq(&base_index, &next_index));
        assert!(Arc::ptr_eq(&base_rir, &next_rir));
    }

    #[test]
    fn module_index_projection_requests_and_reuses_lookup_name_terminals() {
        let source = source_snapshot(
            &[(1, "/main.rue", "main.rue", "fn alpha() {} fn beta() {}")],
            1,
        );
        let main = ModuleId::from_logical_path("main.rue").unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&source),
            &source,
        );
        let (program, _) = database.parse_program(revision, &main, [main.clone()]);
        let program = program.unwrap();
        let after_parse = database.runtime.metrics().claims;
        let first = database
            .projected_module_indexes(revision, &program)
            .unwrap();
        assert_eq!(first[0].definitions.len(), 2);
        assert_eq!(
            database.runtime.metrics().claims - after_parse,
            3,
            "one ModuleIndex plus two production LookupName terminals"
        );
        let after_first_projection = database.runtime.metrics().claims;
        let second = database
            .projected_module_indexes(revision, &program)
            .unwrap();
        assert_eq!(first[0].definitions, second[0].definitions);
        assert_eq!(database.runtime.metrics().claims, after_first_projection);
    }

    #[test]
    fn lookup_name_retains_position_free_facts_across_trivia_shifts() {
        let first = source_snapshot(
            &[(1, "/main.rue", "main.rue", "pub struct Base { value: i32 }")],
            1,
        );
        let shifted = source_snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "// leading trivia moves every current locator\npub struct Base { value: i32 }",
            )],
            1,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = LookupNameKey {
            module: module.clone(),
            namespace: DefinitionNamespace::ModuleItem,
            name: Arc::from("Base"),
        };
        let mut database = RevisionedQueryDatabase::default();
        let first_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&first),
            &first,
        );
        let first_lookup = database.runtime.request_registered(
            &database.lookup_names,
            first_revision,
            key.clone(),
            CancellationToken::new(),
        );
        let first_stamp = first_lookup.terminal().unwrap().stamp();
        let (first_program, _) =
            database.parse_program(first_revision, &module, std::iter::once(module.clone()));
        let first_locator = database
            .projected_module_indexes(first_revision, &first_program.unwrap())
            .unwrap()[0]
            .definitions[0]
            .name_span;

        let shifted_revision = database.source_revision(
            &super::super::session::ExactSourceInput::new(&shifted),
            &shifted,
        );
        let shifted_lookup = database.runtime.request_registered(
            &database.lookup_names,
            shifted_revision,
            key,
            CancellationToken::new(),
        );
        assert_eq!(
            shifted_lookup.terminal().unwrap().stamp(),
            first_stamp,
            "trivia-only locator changes must not invalidate the retained name fact"
        );
        let (shifted_program, _) =
            database.parse_program(shifted_revision, &module, std::iter::once(module.clone()));
        let shifted_locator = database
            .projected_module_indexes(shifted_revision, &shifted_program.unwrap())
            .unwrap()[0]
            .definitions[0]
            .name_span;
        assert!(shifted_locator.start > first_locator.start);
    }

    fn import_fixture(
        epoch: u64,
        source: &str,
    ) -> (
        CompilerSession,
        DiscoverySourceAssembler,
        ImportDiscoveryContext,
    ) {
        let context =
            ImportDiscoveryContext::new(epoch, "/project", Some("/sdk"), "test-policy").unwrap();
        let assembler = DiscoverySourceAssembler::new(
            context.clone(),
            "/project/main.rue",
            "/physical/main.rue",
            PhysicalFileIdentity::new(1, 1),
            FileMetadataFingerprint::new(1, 2, 3),
            Arc::new(source.to_owned()),
        )
        .unwrap();
        (CompilerSession::new(), assembler, context)
    }

    fn begin_and_plan(
        session: &mut CompilerSession,
        assembler: &DiscoverySourceAssembler,
        context: ImportDiscoveryContext,
    ) -> (ImportInputRevision, ImportDiscoveryPlan) {
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let revision = session
            .begin_import_input_request(&snapshot, context.clone(), reads.clone())
            .unwrap();
        let plan = session
            .stage_import_discovery(
                &snapshot,
                context,
                reads,
                ImportObservationLedger::default(),
            )
            .unwrap();
        (revision, plan)
    }

    fn begin_database_plan(
        database: &mut RevisionedQueryDatabase,
        assembler: &DiscoverySourceAssembler,
        context: ImportDiscoveryContext,
    ) -> (
        SourceSnapshot,
        Arc<[crate::AcceptedReadManifestEntry]>,
        ImportInputRevision,
        ImportDiscoveryPlan,
    ) {
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let revision = database
            .begin_import_inputs(&snapshot, context.clone(), reads.clone())
            .unwrap();
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let root = ModuleId::from_logical_path("main.rue").unwrap();
        let modules = snapshot
            .source_revision()
            .modules()
            .iter()
            .map(|module| module.module.clone())
            .collect::<Vec<_>>();
        let (program, _) = database.parse_program(runtime_revision, &root, modules);
        let plan = ImportDiscoveryPlan::new(&program.unwrap(), context).unwrap();
        (snapshot, reads, revision, plan)
    }

    fn publish_manifest_observations(
        database: &mut RevisionedQueryDatabase,
        snapshot: &SourceSnapshot,
        reads: Arc<[crate::AcceptedReadManifestEntry]>,
        plan: &ImportDiscoveryPlan,
        mut revision: ImportInputRevision,
    ) -> ImportInputRevision {
        let roots = ImportDemandRoots::whole_plan(plan);
        loop {
            let frontier = database
                .import_frontier(revision, plan, ImportDemandMode::Rooted, &roots)
                .unwrap();
            if frontier.requests().is_empty() {
                return revision;
            }
            let observations = frontier
                .requests()
                .iter()
                .cloned()
                .map(|request| {
                    let Some(entry) = reads
                        .iter()
                        .find(|entry| entry.requested_path() == request.requested_path())
                    else {
                        return ImportObservation::absent(request);
                    };
                    let file_id = snapshot
                        .files()
                        .find(|source| snapshot.module_id(source.file_id) == Some(entry.module()))
                        .unwrap()
                        .file_id;
                    let accepted = crate::AcceptedImportSource::new(
                        entry.requested_path(),
                        entry.canonical_path(),
                        entry.metadata_identity(),
                        entry.metadata_fingerprint(),
                        snapshot.shared_source_text(file_id).unwrap(),
                    )
                    .unwrap();
                    ImportObservation::accepted(request, accepted).unwrap()
                })
                .collect();
            revision = database
                .publish_import_batch(&frontier, snapshot, reads.clone(), observations)
                .unwrap();
        }
    }

    fn publish_remapped_observations(
        database: &mut RevisionedQueryDatabase,
        snapshot: &SourceSnapshot,
        reads: Arc<[crate::AcceptedReadManifestEntry]>,
        plan: &ImportDiscoveryPlan,
        mut revision: ImportInputRevision,
        remaps: &[(&str, PhysicalFileIdentity)],
    ) -> ImportInputRevision {
        let roots = ImportDemandRoots::whole_plan(plan);
        loop {
            let frontier = database
                .import_frontier(revision, plan, ImportDemandMode::Rooted, &roots)
                .unwrap();
            if frontier.requests().is_empty() {
                return revision;
            }
            let observations = frontier
                .requests()
                .iter()
                .cloned()
                .map(|request| {
                    let Some((_, identity)) = remaps
                        .iter()
                        .find(|(path, _)| *path == request.requested_path())
                    else {
                        return ImportObservation::absent(request);
                    };
                    let entry = reads
                        .iter()
                        .find(|entry| entry.metadata_identity() == *identity)
                        .unwrap();
                    let file_id = snapshot
                        .files()
                        .find(|source| snapshot.module_id(source.file_id) == Some(entry.module()))
                        .unwrap()
                        .file_id;
                    let accepted = crate::AcceptedImportSource::new(
                        request.requested_path(),
                        entry.canonical_path(),
                        entry.metadata_identity(),
                        entry.metadata_fingerprint(),
                        snapshot.shared_source_text(file_id).unwrap(),
                    )
                    .unwrap();
                    ImportObservation::accepted(request, accepted).unwrap()
                })
                .collect();
            revision = database
                .publish_import_batch(&frontier, snapshot, reads.clone(), observations)
                .unwrap();
        }
    }

    fn declaration_import_key(
        module: &ModuleId,
        category: crate::declaration_candidate::DeclarationCandidateCategory,
        name: impl Into<Arc<str>>,
        owner: Option<crate::declaration_candidate::DeclarationCandidateOwner>,
        occurrence: u32,
        specifier: &str,
    ) -> DeclarationImportQueryKey {
        DeclarationImportQueryKey(crate::declaration_candidate::DeclarationImportSiteKey {
            declaration: crate::declaration_candidate::DeclarationCandidateKey {
                module: module.clone(),
                category,
                name: name.into(),
                owner,
                duplicate_discriminator: 0,
            },
            occurrence,
            specifier: Arc::from(specifier),
        })
    }

    #[test]
    fn declaration_imports_are_exact_lazy_and_distinguish_duplicate_specifiers() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = "const selected = if true { @import(\"same\") } else { @import(\"same\") }; const untouched = @import(\"other\"); fn main() {}";
        let (_, assembler, context) = import_fixture(301, source);
        let mut database = RevisionedQueryDatabase::default();
        let (snapshot, reads, revision, plan) =
            begin_database_plan(&mut database, &assembler, context);
        let revision =
            publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let parsed = database.runtime.request_registered(
            &database.parse_modules,
            runtime_revision,
            ModuleQueryKey(module.clone()),
            CancellationToken::new(),
        );
        let parsed_module = match parsed.terminal().unwrap().outcome() {
            rue_query::QueryOutcome::Success(value) => value.result.clone().unwrap(),
            rue_query::QueryOutcome::Failure(_) => unreachable!(),
        };
        assert_eq!(
            parsed_module.declaration_import_locator_materialization_count(),
            0,
            "indexing and import discovery must retain only fixed parser locators"
        );

        let first_key = declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "same",
        );
        let second_key = declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            1,
            "same",
        );
        assert_ne!(first_key.stable_identity(), second_key.stable_identity());
        for key in [first_key.clone(), second_key] {
            let requested = database.runtime.request_registered(
                &database.declaration_imports,
                runtime_revision,
                key,
                CancellationToken::new(),
            );
            assert_eq!(execution(&requested), RequestExecution::Computed);
            assert_eq!(
                requested
                    .dependencies()
                    .iter()
                    .map(|dependency| dependency.node.family())
                    .collect::<Vec<_>>(),
                vec![
                    "compiler.declaration-occurrence-index",
                    "compiler.declaration-shell",
                    "compiler.parse-module",
                    "compiler.resolve-import",
                ]
            );
            let terminal = requested.terminal().unwrap();
            assert_eq!(terminal.kind(), QueryTerminalKind::Success);
            assert!(matches!(
                terminal.outcome(),
                rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                    crate::CanonicalImportResolution::Missing
                ))
            ));
        }
        assert_eq!(
            parsed_module.declaration_import_locator_materialization_count(),
            2,
            "only the two demanded sites in selected may materialize"
        );
        let warm = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            first_key,
            CancellationToken::new(),
        );
        assert_eq!(execution(&warm), RequestExecution::Reused);
        assert_eq!(
            parsed_module.declaration_import_locator_materialization_count(),
            2
        );
        let out_of_range = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            declaration_import_key(
                &module,
                Category::ConstCandidate,
                "selected",
                None,
                2,
                "same",
            ),
            CancellationToken::new(),
        );
        assert!(matches!(
            out_of_range.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                crate::declaration_candidate::DeclarationImportFailure::SiteOutOfRange {
                    available: 2,
                    ..
                }
            ))
        ));
        let wrong_specifier = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            declaration_import_key(
                &module,
                Category::ConstCandidate,
                "selected",
                None,
                0,
                "different",
            ),
            CancellationToken::new(),
        );
        assert!(matches!(
            wrong_specifier.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                crate::declaration_candidate::DeclarationImportFailure::SpecifierMismatch {
                    actual,
                    ..
                }
            )) if actual.as_ref() == "same"
        ));
    }

    #[test]
    fn declaration_import_relocation_reuses_and_stale_absolute_site_fails_typed() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let first_source = "const selected = @import(\"missing\"); fn main() {}";
        let (_, first_assembler, first_context) = import_fixture(302, first_source);
        let mut database = RevisionedQueryDatabase::default();
        let (first_snapshot, first_reads, first_revision, first_plan) =
            begin_database_plan(&mut database, &first_assembler, first_context);
        let old_occurrence = first_plan.groups()[0][0].occurrence().clone();
        assert_ne!(
            ResolveImportKey {
                occurrence: old_occurrence.clone(),
                mode: ImportDemandMode::Rooted,
            }
            .stable_identity(),
            ResolveImportKey {
                occurrence: old_occurrence.clone(),
                mode: ImportDemandMode::Speculative,
            }
            .stable_identity(),
            "resolve-import stable identities must include demand mode"
        );
        let first_revision = publish_manifest_observations(
            &mut database,
            &first_snapshot,
            first_reads,
            &first_plan,
            first_revision,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "missing",
        );
        let first = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                first_revision.revision_id,
                first_revision.request_generation,
            ),
            key.clone(),
            CancellationToken::new(),
        );
        let first_stamp = first.terminal().unwrap().stamp();

        let shifted_source =
            "// position-only relocation\n\nconst selected = @import(\"missing\"); fn main() {}";
        let (_, shifted_assembler, shifted_context) = import_fixture(303, shifted_source);
        let (shifted_snapshot, shifted_reads, shifted_revision, shifted_plan) =
            begin_database_plan(&mut database, &shifted_assembler, shifted_context);
        let shifted_revision = publish_manifest_observations(
            &mut database,
            &shifted_snapshot,
            shifted_reads,
            &shifted_plan,
            shifted_revision,
        );
        let shifted_runtime = Revision::new(
            shifted_revision.revision_id,
            shifted_revision.request_generation,
        );
        let relocated = database.runtime.request_registered(
            &database.declaration_imports,
            shifted_runtime,
            key,
            CancellationToken::new(),
        );
        assert_eq!(
            relocated.terminal().unwrap().stamp(),
            first_stamp,
            "position-free declaration import results must stay green across trivia relocation"
        );

        let stale = database.runtime.request_registered(
            &database.resolve_imports,
            shifted_runtime,
            ResolveImportKey {
                occurrence: old_occurrence,
                mode: ImportDemandMode::Rooted,
            },
            CancellationToken::new(),
        );
        assert!(matches!(
            stale.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(ResolveImportValue {
                site_found: false,
                ..
            })
        ));
    }

    #[test]
    fn declaration_import_recovers_when_resolution_observations_arrive() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let (_, assembler, context) =
            import_fixture(306, "const selected = @import(\"missing\"); fn main() {}");
        let mut database = RevisionedQueryDatabase::default();
        let (snapshot, reads, revision, plan) =
            begin_database_plan(&mut database, &assembler, context);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "missing",
        );
        let pending = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(revision.revision_id, revision.request_generation),
            key.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(
            pending.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(_)
            ))
        ));

        let completed =
            publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
        let recovered = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(completed.revision_id, completed.request_generation),
            key,
            CancellationToken::new(),
        );
        assert_eq!(execution(&recovered), RequestExecution::Computed);
        assert!(matches!(
            recovered.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Missing
            ))
        ));
    }

    #[test]
    fn semantic_import_is_typed_missing_input_and_recovers_on_successor_revision() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;
        use crate::semantic_query_nucleus::{
            SemanticNucleusFailure as Failure, SemanticNucleusKey as Key,
            SemanticNucleusValue as Value,
        };

        let (_, assembler, context) =
            import_fixture(307, "const selected = @import(\"missing\"); fn main() {}");
        let mut database = RevisionedQueryDatabase::default();
        let (snapshot, reads, revision, plan) =
            begin_database_plan(&mut database, &assembler, context);
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let query =
            Key::ConstResolution(crate::semantic_query_nucleus::DeclarationSemanticQueryKey {
                declaration: declaration_candidate(
                    &database,
                    runtime_revision,
                    &module,
                    Category::ConstCandidate,
                    "selected",
                ),
                configuration: semantic_configuration(),
            });

        let pending = database.runtime.request_registered(
            &database.semantic_nucleus,
            runtime_revision,
            query.clone(),
            CancellationToken::new(),
        );
        assert_eq!(execution(&pending), RequestExecution::Aborted);
        assert!(matches!(pending.abort(), Some(QueryAbort::MissingInput(_))));
        assert!(pending.terminal().is_none());

        let completed =
            publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
        let recovered = database.runtime.request_registered(
            &database.semantic_nucleus,
            Revision::new(completed.revision_id, completed.request_generation),
            query,
            CancellationToken::new(),
        );
        assert_eq!(execution(&recovered), RequestExecution::Computed);
        assert!(matches!(
            recovered.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(Value::Failure(Failure::Resolution(message)))
                if message.as_ref() == "cannot find module `missing`"
        ));
    }

    #[test]
    fn declaration_imports_preserve_canonical_ambiguity_and_category_boundaries() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = "const selected = @import(\"dep\"); struct Box { value: i32, fn get(borrow self) { @import(\"method\"); } fn make() -> Box { @import(\"associated\"); Box { value: 0 } } } drop fn Box(self) { @import(\"drop\"); } fn free() { @import(\"free\"); } enum Choice { A } extern \"C\" { fn foreign() -> i32; }";
        let (_, mut assembler, context) = import_fixture(304, source);
        assembler
            .add_explicit(
                "/project/dep.rue",
                "/physical/dep-file.rue",
                PhysicalFileIdentity::new(2, 1),
                FileMetadataFingerprint::new(2, 2, 3),
                Arc::new("const value = 1;".to_owned()),
            )
            .unwrap();
        assembler
            .add_explicit(
                "/project/dep/_dep.rue",
                "/physical/dep-dir.rue",
                PhysicalFileIdentity::new(3, 1),
                FileMetadataFingerprint::new(3, 2, 3),
                Arc::new("const value = 2;".to_owned()),
            )
            .unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let (snapshot, reads, revision, plan) =
            begin_database_plan(&mut database, &assembler, context);
        let revision =
            publish_manifest_observations(&mut database, &snapshot, reads, &plan, revision);
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let selected = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            declaration_import_key(
                &module,
                Category::ConstCandidate,
                "selected",
                None,
                0,
                "dep",
            ),
            CancellationToken::new(),
        );
        let selected_terminal = selected.terminal().unwrap();
        assert!(
            matches!(
                selected_terminal.outcome(),
                rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                    crate::CanonicalImportResolution::Ambiguous { .. }
                ))
            ),
            "unexpected declaration import outcome: {:#?}",
            selected_terminal.outcome()
        );

        let owner = crate::declaration_candidate::DeclarationCandidateOwner {
            category: Category::Struct,
            name: Arc::from("Box"),
        };
        for (category, name, owner, specifier) in [
            (Category::Method, "get", Some(owner.clone()), "method"),
            (
                Category::AssociatedFunction,
                "make",
                Some(owner.clone()),
                "associated",
            ),
            (Category::Destructor, "Box", Some(owner), "drop"),
            (Category::Function, "free", None, "free"),
        ] {
            let requested = database.runtime.request_registered(
                &database.declaration_imports,
                runtime_revision,
                declaration_import_key(&module, category, name, owner, 0, specifier),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                    crate::CanonicalImportResolution::Missing
                ))
            ));
        }

        for (category, name) in [
            (Category::Struct, "Box"),
            (Category::Enum, "Choice"),
            (Category::ExternFunction, "foreign"),
        ] {
            let key = declaration_import_key(&module, category, name, None, 0, "none");
            let requested = database.runtime.request_registered(
                &database.declaration_imports,
                runtime_revision,
                key.clone(),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                    crate::declaration_candidate::DeclarationImportFailure::CategoryMismatch(
                        actual
                    )
                )) if actual == &key.0
            ));
        }
    }

    #[test]
    fn resolved_declaration_import_observes_only_winning_physical_provenance() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = "const selected = @import(\"dep.rue\"); fn main() {}";
        let (_, mut first_assembler, first_context) = import_fixture(307, source);
        first_assembler
            .add_explicit(
                "/project/dep.rue",
                "/physical/dep.rue",
                PhysicalFileIdentity::new(2, 1),
                FileMetadataFingerprint::new(4, 5, 6),
                Arc::new("const value = 1;".to_owned()),
            )
            .unwrap();
        let mut database = RevisionedQueryDatabase::default();
        let (first_snapshot, first_reads, first_revision, first_plan) =
            begin_database_plan(&mut database, &first_assembler, first_context);
        let first_revision = publish_manifest_observations(
            &mut database,
            &first_snapshot,
            first_reads,
            &first_plan,
            first_revision,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "dep.rue",
        );
        let first = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                first_revision.revision_id,
                first_revision.request_generation,
            ),
            key.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(
            first.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Resolved(target)
            )) if target.as_str() == "dep.rue"
        ));
        let first_stamp = first.terminal().unwrap().stamp();

        let (_, mut remapped_assembler, remapped_context) = import_fixture(307, source);
        remapped_assembler
            .add_explicit(
                "/project/other.rue",
                "/physical/other.rue",
                PhysicalFileIdentity::new(2, 1),
                FileMetadataFingerprint::new(4, 5, 6),
                Arc::new("const value = 1;".to_owned()),
            )
            .unwrap();
        let (remapped_snapshot, remapped_reads, remapped_revision, remapped_plan) =
            begin_database_plan(&mut database, &remapped_assembler, remapped_context);
        let remapped_revision = publish_remapped_observations(
            &mut database,
            &remapped_snapshot,
            remapped_reads,
            &remapped_plan,
            remapped_revision,
            &[("/project/dep.rue", PhysicalFileIdentity::new(2, 1))],
        );
        let remapped = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                remapped_revision.revision_id,
                remapped_revision.request_generation,
            ),
            key.clone(),
            CancellationToken::new(),
        );
        let remapped_terminal = remapped.terminal().unwrap();
        assert_ne!(remapped_terminal.stamp(), first_stamp);
        assert!(matches!(
            remapped_terminal.outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Resolved(target)
            )) if target.as_str() == "other.rue"
        ));
        let remapped_stamp = remapped_terminal.stamp();

        let mut green_assembler = remapped_assembler.clone();
        green_assembler
            .add_explicit(
                "/project/unrelated.rue",
                "/physical/unrelated.rue",
                PhysicalFileIdentity::new(9, 1),
                FileMetadataFingerprint::new(9, 2, 3),
                Arc::new("const unrelated = 9;".to_owned()),
            )
            .unwrap();
        let green_context =
            ImportDiscoveryContext::new(307, "/project", Some("/sdk"), "test-policy").unwrap();
        let (green_snapshot, green_reads, green_revision, green_plan) =
            begin_database_plan(&mut database, &green_assembler, green_context);
        let green_revision = publish_remapped_observations(
            &mut database,
            &green_snapshot,
            green_reads,
            &green_plan,
            green_revision,
            &[("/project/dep.rue", PhysicalFileIdentity::new(2, 1))],
        );
        let green = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                green_revision.revision_id,
                green_revision.request_generation,
            ),
            key,
            CancellationToken::new(),
        );
        assert_eq!(green.terminal().unwrap().stamp(), remapped_stamp);
    }

    #[test]
    fn ambiguous_declaration_import_observes_both_winning_provenance_leaves() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source = "const selected = @import(\"dep\"); fn main() {}";
        let (_, mut first_assembler, first_context) = import_fixture(308, source);
        for (path, canonical, identity, value) in [
            (
                "/project/dep.rue",
                "/physical/dep-file.rue",
                PhysicalFileIdentity::new(2, 1),
                1,
            ),
            (
                "/project/dep/_dep.rue",
                "/physical/dep-dir.rue",
                PhysicalFileIdentity::new(3, 1),
                2,
            ),
        ] {
            first_assembler
                .add_explicit(
                    path,
                    canonical,
                    identity,
                    FileMetadataFingerprint::new(value, 5, 6),
                    Arc::new(format!("const value = {value};")),
                )
                .unwrap();
        }
        let mut database = RevisionedQueryDatabase::default();
        let (first_snapshot, first_reads, first_revision, first_plan) =
            begin_database_plan(&mut database, &first_assembler, first_context);
        let first_revision = publish_manifest_observations(
            &mut database,
            &first_snapshot,
            first_reads,
            &first_plan,
            first_revision,
        );
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = declaration_import_key(
            &module,
            Category::ConstCandidate,
            "selected",
            None,
            0,
            "dep",
        );
        let first = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                first_revision.revision_id,
                first_revision.request_generation,
            ),
            key.clone(),
            CancellationToken::new(),
        );
        assert!(matches!(
            first.terminal().unwrap().outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Ambiguous {
                    file_module,
                    directory_module,
                }
            )) if file_module.as_str() == "dep.rue"
                && directory_module.as_str() == "dep/_dep.rue"
        ));
        let first_stamp = first.terminal().unwrap().stamp();

        let (_, mut remapped_assembler, remapped_context) = import_fixture(308, source);
        for (path, canonical, identity, value) in [
            (
                "/project/left.rue",
                "/physical/left.rue",
                PhysicalFileIdentity::new(2, 1),
                1,
            ),
            (
                "/project/right.rue",
                "/physical/right.rue",
                PhysicalFileIdentity::new(3, 1),
                2,
            ),
        ] {
            remapped_assembler
                .add_explicit(
                    path,
                    canonical,
                    identity,
                    FileMetadataFingerprint::new(value, 5, 6),
                    Arc::new(format!("const value = {value};")),
                )
                .unwrap();
        }
        let (remapped_snapshot, remapped_reads, remapped_revision, remapped_plan) =
            begin_database_plan(&mut database, &remapped_assembler, remapped_context);
        let remaps = [
            ("/project/dep.rue", PhysicalFileIdentity::new(2, 1)),
            ("/project/dep/_dep.rue", PhysicalFileIdentity::new(3, 1)),
        ];
        let remapped_revision = publish_remapped_observations(
            &mut database,
            &remapped_snapshot,
            remapped_reads,
            &remapped_plan,
            remapped_revision,
            &remaps,
        );
        let remapped = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                remapped_revision.revision_id,
                remapped_revision.request_generation,
            ),
            key.clone(),
            CancellationToken::new(),
        );
        let remapped_terminal = remapped.terminal().unwrap();
        assert_ne!(remapped_terminal.stamp(), first_stamp);
        assert!(matches!(
            remapped_terminal.outcome(),
            rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Available(
                crate::CanonicalImportResolution::Ambiguous {
                    file_module,
                    directory_module,
                }
            )) if file_module.as_str() == "left.rue"
                && directory_module.as_str() == "right.rue"
        ));
        let remapped_stamp = remapped_terminal.stamp();

        let mut green_assembler = remapped_assembler.clone();
        green_assembler
            .add_explicit(
                "/project/unrelated.rue",
                "/physical/unrelated.rue",
                PhysicalFileIdentity::new(9, 1),
                FileMetadataFingerprint::new(9, 2, 3),
                Arc::new("const unrelated = 9;".to_owned()),
            )
            .unwrap();
        let green_context =
            ImportDiscoveryContext::new(308, "/project", Some("/sdk"), "test-policy").unwrap();
        let (green_snapshot, green_reads, green_revision, green_plan) =
            begin_database_plan(&mut database, &green_assembler, green_context);
        let green_revision = publish_remapped_observations(
            &mut database,
            &green_snapshot,
            green_reads,
            &green_plan,
            green_revision,
            &remaps,
        );
        let green = database.runtime.request_registered(
            &database.declaration_imports,
            Revision::new(
                green_revision.revision_id,
                green_revision.request_generation,
            ),
            key,
            CancellationToken::new(),
        );
        assert_eq!(green.terminal().unwrap().stamp(), remapped_stamp);
    }

    #[test]
    fn import_publication_rejects_duplicate_and_unmatched_physical_provenance() {
        let source = "const selected = @import(\"dep\"); fn main() {}";
        let (_, assembler, context) = import_fixture(309, source);
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let duplicated = reads
            .iter()
            .cloned()
            .chain(std::iter::once(reads[0].clone()))
            .collect::<Vec<_>>();
        let mut database = RevisionedQueryDatabase::default();
        assert!(
            database
                .begin_import_inputs(&snapshot, context.clone(), duplicated.into())
                .is_err(),
            "duplicate physical provenance must fail before revision publication"
        );

        let (snapshot, reads, revision, plan) =
            begin_database_plan(&mut database, &assembler, context);
        let roots = ImportDemandRoots::whole_plan(&plan);
        let frontier = database
            .import_frontier(revision, &plan, ImportDemandMode::Rooted, &roots)
            .unwrap();
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, request)| {
                if index == 0 {
                    let accepted = crate::AcceptedImportSource::new(
                        request.requested_path(),
                        request.requested_path(),
                        PhysicalFileIdentity::new(99, 99),
                        FileMetadataFingerprint::new(1, 2, 3),
                        Arc::new("const value = 1;".to_owned()),
                    )
                    .unwrap();
                    ImportObservation::accepted(request, accepted).unwrap()
                } else {
                    ImportObservation::absent(request)
                }
            })
            .collect();
        assert!(
            database
                .publish_import_batch(&frontier, &snapshot, reads, observations)
                .is_err(),
            "accepted observations without exact manifest provenance must not publish"
        );
        assert_eq!(database.current_import_revision(), Some(revision));
    }

    #[test]
    fn canceled_and_evicted_declaration_import_requests_recover() {
        use crate::declaration_candidate::DeclarationCandidateCategory as Category;

        let source_text = (0..=MODULE_QUERY_MEMO_RETENTION)
            .map(|index| format!("const c{index} = @import(\"x{index}\");"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, assembler, context) = import_fixture(305, &source_text);
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let mut database =
            RevisionedQueryDatabase::with_declaration_memo_retention(MODULE_QUERY_MEMO_RETENTION);
        let revision = database
            .begin_import_inputs(&snapshot, context, reads)
            .unwrap();
        let runtime_revision = Revision::new(revision.revision_id, revision.request_generation);
        let module = ModuleId::from_logical_path("main.rue").unwrap();
        let key = |index| {
            declaration_import_key(
                &module,
                Category::ConstCandidate,
                format!("c{index}"),
                None,
                0,
                &format!("x{index}"),
            )
        };

        let canceled = CancellationToken::new();
        canceled.cancel();
        let aborted = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            key(0),
            canceled,
        );
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(aborted.terminal().is_none());

        for index in 0..=MODULE_QUERY_MEMO_RETENTION {
            let requested = database.runtime.request_registered(
                &database.declaration_imports,
                runtime_revision,
                key(index),
                CancellationToken::new(),
            );
            assert!(matches!(
                requested.terminal().unwrap().outcome(),
                rue_query::QueryOutcome::Success(DeclarationImportQueryValue::Failure(
                    crate::declaration_candidate::DeclarationImportFailure::ResolutionUnavailable(
                        _
                    )
                ))
            ));
        }
        assert_eq!(
            database.declaration_imports.retention().terminals,
            MODULE_QUERY_MEMO_RETENTION
        );
        let recovered = database.runtime.request_registered(
            &database.declaration_imports,
            runtime_revision,
            key(0),
            CancellationToken::new(),
        );
        assert_eq!(execution(&recovered), RequestExecution::Computed);
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct Key(&'static str);

    #[derive(Debug, Clone)]
    struct Record {
        key: Key,
        value: u64,
        diagnostic_payload: u64,
        failed: bool,
    }

    #[derive(Debug)]
    struct Family;

    impl TypedQueryFamily for Family {
        type Key = Key;
        type Record = Record;
        const MAX_TERMINALS: usize = 4;

        fn key(record: &Self::Record) -> &Self::Key {
            &record.key
        }

        fn terminal_kind(record: &Self::Record) -> TerminalKind {
            if record.failed {
                TerminalKind::Failure
            } else {
                TerminalKind::Success
            }
        }

        fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.value == right.value
        }

        fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.diagnostic_payload == right.diagnostic_payload
        }

        fn record_is_consistent(record: &Self::Record) -> bool {
            !record.key.0.is_empty()
        }
    }

    #[test]
    fn wide_root_imports_form_one_exact_compiler_frontier() {
        let source = r#"
            const a = @import("a");
            const b = @import("b");
            const c = @import("c");
            const d = @import("d");
            fn main() -> i32 { 0 }
        "#;
        let (mut session, assembler, context) = import_fixture(1, source);
        let (revision, plan) = begin_and_plan(&mut session, &assembler, context);
        let frontier = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                ImportDemandMode::Rooted,
                &plan.demand_roots(),
            )
            .unwrap();
        assert_eq!(frontier.revision(), revision);
        assert_eq!(frontier.revision().frontier_round(), 0);
        assert!(!frontier.requests().is_empty());
        assert_eq!(
            frontier
                .requests()
                .iter()
                .map(|request| request.occurrence())
                .collect::<BTreeSet<_>>()
                .len(),
            4,
            "all same-depth roots must be returned in one host batch"
        );

        let mut reversed = frontier
            .requests()
            .iter()
            .cloned()
            .map(ImportObservation::absent)
            .collect::<Vec<_>>();
        reversed.reverse();
        assert!(
            session
                .publish_import_observation_batch(
                    &frontier,
                    &assembler.snapshot().unwrap(),
                    assembler.accepted_read_manifest(),
                    reversed,
                )
                .unwrap_err()
                .to_string()
                .contains("exactly preserve")
        );

        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(ImportObservation::absent)
            .collect();
        let successor = session
            .publish_import_observation_batch(
                &frontier,
                &assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
                observations,
            )
            .unwrap();
        assert_eq!(successor.frontier_round(), 1);
        assert_eq!(
            session
                .import_observation_ledger(successor)
                .unwrap()
                .iter()
                .count(),
            frontier.requests().len()
        );
    }

    #[test]
    fn speculative_frontiers_are_effect_free_and_cannot_publish_host_results() {
        let (mut session, assembler, context) = import_fixture(
            2,
            r#"const helper = @import("helper"); fn main() -> i32 { 0 }"#,
        );
        let (revision, plan) = begin_and_plan(&mut session, &assembler, context);
        let speculative = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                ImportDemandMode::Speculative,
                &plan.demand_roots(),
            )
            .unwrap();
        assert!(speculative.requests().is_empty());
        assert!(speculative.speculative_blocked());
        assert_eq!(
            session
                .import_observation_ledger(revision)
                .unwrap()
                .iter()
                .count(),
            0
        );
        assert!(
            session
                .publish_import_observation_batch(
                    &speculative,
                    &assembler.snapshot().unwrap(),
                    assembler.accepted_read_manifest(),
                    Vec::new(),
                )
                .unwrap_err()
                .to_string()
                .contains("speculative")
        );

        let rooted = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                ImportDemandMode::Rooted,
                &plan.demand_roots(),
            )
            .unwrap();
        assert!(!rooted.requests().is_empty());
        assert_eq!(rooted.revision(), revision);
    }

    #[test]
    fn resolve_import_recomputes_when_only_discovery_context_changes() {
        let (mut session, assembler, first_context) = import_fixture(
            24,
            r#"const standard = @import("std"); fn main() -> i32 { 0 }"#,
        );
        let (first_revision, first_plan) =
            begin_and_plan(&mut session, &assembler, first_context.clone());
        let first = session
            .import_demand_frontier_for_roots(
                first_revision,
                &first_plan,
                ImportDemandMode::Rooted,
                &first_plan.demand_roots(),
            )
            .unwrap();
        assert!(
            first
                .requests()
                .iter()
                .any(|request| request.requested_path().starts_with("/sdk/"))
        );

        let second_context =
            ImportDiscoveryContext::new(24, "/project", Some("/other-sdk"), "other-policy")
                .unwrap();
        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let second_revision = session
            .begin_import_input_request(&snapshot, second_context.clone(), reads.clone())
            .unwrap();
        let second_plan = session
            .stage_import_discovery(
                &snapshot,
                second_context.clone(),
                reads,
                ImportObservationLedger::default(),
            )
            .unwrap();
        let second = session
            .import_demand_frontier_for_roots(
                second_revision,
                &second_plan,
                ImportDemandMode::Rooted,
                &second_plan.demand_roots(),
            )
            .unwrap();
        assert!(
            second
                .requests()
                .iter()
                .all(|request| request.context() == &second_context)
        );
        assert!(
            second
                .requests()
                .iter()
                .any(|request| request.requested_path().starts_with("/other-sdk/"))
        );
        assert!(
            !second
                .requests()
                .iter()
                .any(|request| request.requested_path().starts_with("/sdk/"))
        );
    }

    #[test]
    fn explicit_occurrence_roots_select_one_of_twenty_seven_without_speculative_io() {
        let mut source = String::new();
        for index in 0..27 {
            source.push_str(&format!(
                "pub const m{index} = @import(\"m{index}.rue\");\n"
            ));
        }
        source.push_str("fn main() -> i32 { 0 }\n");
        let (mut session, assembler, context) = import_fixture(21, &source);
        let (revision, plan) = begin_and_plan(&mut session, &assembler, context);
        let selected = plan
            .groups()
            .iter()
            .find(|group| group[0].exact_specifier() == "m7.rue")
            .unwrap()[0]
            .occurrence()
            .clone();
        let roots = ImportDemandRoots::new([selected.clone()]);

        let speculative = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                ImportDemandMode::Speculative,
                &roots,
            )
            .unwrap();
        assert!(speculative.requests().is_empty());
        assert!(speculative.speculative_blocked());
        assert!(
            session
                .import_observation_ledger(revision)
                .unwrap()
                .is_empty()
        );

        let rooted = session
            .import_demand_frontier_for_roots(revision, &plan, ImportDemandMode::Rooted, &roots)
            .unwrap();
        assert!(!rooted.requests().is_empty());
        assert!(
            rooted
                .requests()
                .iter()
                .all(|request| request.occurrence() == &selected)
        );
    }

    #[test]
    fn new_request_generation_has_no_carried_ledger_authority() {
        let (mut session, assembler, context) = import_fixture(
            22,
            r#"const missing = @import("missing.rue"); fn main() -> i32 { 0 }"#,
        );
        let (first_revision, first_plan) =
            begin_and_plan(&mut session, &assembler, context.clone());
        let first = session
            .import_demand_frontier_for_roots(
                first_revision,
                &first_plan,
                ImportDemandMode::Rooted,
                &first_plan.demand_roots(),
            )
            .unwrap();
        let successor = session
            .publish_import_observation_batch(
                &first,
                &assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
                first
                    .requests()
                    .iter()
                    .cloned()
                    .map(ImportObservation::absent)
                    .collect(),
            )
            .unwrap();
        let stale = session.import_observation_ledger(successor).unwrap();
        assert!(!stale.is_empty());

        let snapshot = assembler.snapshot().unwrap();
        let reads = assembler.accepted_read_manifest();
        let fresh_revision = session
            .begin_import_input_request(&snapshot, context.clone(), reads.clone())
            .unwrap();
        let fresh_ledger = session.import_observation_ledger(fresh_revision).unwrap();
        assert!(fresh_ledger.is_empty());
        let fresh_plan = session
            .stage_import_discovery(&snapshot, context, reads, fresh_ledger)
            .unwrap();
        let reread = session
            .import_demand_frontier_for_roots(
                fresh_revision,
                &fresh_plan,
                ImportDemandMode::Rooted,
                &fresh_plan.demand_roots(),
            )
            .unwrap();
        assert_eq!(
            reread
                .requests()
                .iter()
                .map(ImportDiscoveryRequest::requested_path)
                .collect::<Vec<_>>(),
            first
                .requests()
                .iter()
                .map(ImportDiscoveryRequest::requested_path)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn duplicate_occurrences_share_one_host_operation_and_fan_out_typed_results() {
        let (mut session, assembler, context) = import_fixture(
            23,
            r#"
                const first = @import("shared.rue");
                const second = @import("shared.rue");
                fn main() -> i32 { 0 }
            "#,
        );
        let (revision, plan) = begin_and_plan(&mut session, &assembler, context);
        let frontier = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                ImportDemandMode::Rooted,
                &plan.demand_roots(),
            )
            .unwrap();
        assert_eq!(frontier.requests().len(), 1, "one host candidate operation");

        let successor = session
            .publish_import_observation_batch(
                &frontier,
                &assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
                vec![ImportObservation::absent(frontier.requests()[0].clone())],
            )
            .unwrap();
        let ledger = session.import_observation_ledger(successor).unwrap();
        assert_eq!(
            ledger.len(),
            2,
            "result fans out to both source occurrences"
        );
        assert_eq!(
            ledger
                .iter()
                .map(|observation| observation.request().occurrence())
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
    }

    #[test]
    fn successor_revisions_carry_observations_but_new_epochs_reread() {
        let source = r#"const helper = @import("helper"); fn main() -> i32 { 0 }"#;
        let (mut session, assembler, context) = import_fixture(3, source);
        let (revision, plan) = begin_and_plan(&mut session, &assembler, context);
        let first = session
            .import_demand_frontier_for_roots(
                revision,
                &plan,
                ImportDemandMode::Rooted,
                &plan.demand_roots(),
            )
            .unwrap();
        let first_paths = first
            .requests()
            .iter()
            .map(|request| request.requested_path().to_owned())
            .collect::<BTreeSet<_>>();
        let successor = session
            .publish_import_observation_batch(
                &first,
                &assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
                first
                    .requests()
                    .iter()
                    .cloned()
                    .map(ImportObservation::absent)
                    .collect(),
            )
            .unwrap();
        let carried = session.import_observation_ledger(successor).unwrap();
        assert_eq!(carried.iter().count(), first.requests().len());
        let successor_plan = session
            .stage_import_discovery(
                &assembler.snapshot().unwrap(),
                plan.context().clone(),
                assembler.accepted_read_manifest(),
                carried,
            )
            .unwrap();
        let next = session
            .import_demand_frontier_for_roots(
                successor,
                &successor_plan,
                ImportDemandMode::Rooted,
                &successor_plan.demand_roots(),
            )
            .unwrap();
        assert!(
            next.requests()
                .iter()
                .all(|request| !first_paths.contains(request.requested_path()))
        );

        let new_context =
            ImportDiscoveryContext::new(4, "/project", Some("/sdk"), "test-policy").unwrap();
        let new_snapshot = assembler.snapshot().unwrap();
        let new_revision = session
            .begin_import_input_request(
                &new_snapshot,
                new_context.clone(),
                assembler.accepted_read_manifest(),
            )
            .unwrap();
        let new_plan = session
            .stage_import_discovery(
                &new_snapshot,
                new_context,
                assembler.accepted_read_manifest(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let reread = session
            .import_demand_frontier_for_roots(
                new_revision,
                &new_plan,
                ImportDemandMode::Rooted,
                &new_plan.demand_roots(),
            )
            .unwrap();
        assert_eq!(
            reread
                .requests()
                .iter()
                .map(|request| request.requested_path().to_owned())
                .collect::<BTreeSet<_>>(),
            first_paths
        );
    }

    #[test]
    fn selected_state_shim_uses_runtime_attempts_and_last_good_publication() {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("test", "leaf");
        let first_revision = Revision::new(1, 1);
        let second_revision = Revision::new(2, 2);
        runtime
            .publish_revision(first_revision, [(input.clone(), 1)])
            .unwrap();
        runtime
            .publish_revision(second_revision, [(input.clone(), 2)])
            .unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.test-family");

        let computed = family.request(first_revision, Key("key"), |context| {
            context.input(input.clone())?;
            Ok(Record {
                key: Key("key"),
                value: 7,
                diagnostic_payload: 11,
                failed: false,
            })
        });
        assert_eq!(execution(&computed), RequestExecution::Computed);
        assert_eq!(computed.inputs().len(), 1);

        let reused = family.request(first_revision, Key("key"), |_| {
            panic!("the exact keyed terminal must be runtime-reused")
        });
        assert_eq!(execution(&reused), RequestExecution::Reused);
        assert!(reused.work().is_empty());

        let failed = family.request(second_revision, Key("key"), |context| {
            context.input(input)?;
            Ok(Record {
                key: Key("key"),
                value: 9,
                diagnostic_payload: 12,
                failed: true,
            })
        });
        assert_eq!(
            failed.terminal().unwrap().kind(),
            QueryTerminalKind::Failure
        );
        assert!(family.current_record().unwrap().failed);
        assert_eq!(family.last_good_record().unwrap().value, 7);

        let aborted = family.request(second_revision, Key("abort"), |_| Err(QueryAbort::Canceled));
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(family.current_record().is_none());
        assert_eq!(family.last_good_record().unwrap().value, 7);

        let recovered = family.request(second_revision, Key("recovered"), |context| {
            context.input(InputIdentity::new("test", "leaf"))?;
            Ok(Record {
                key: Key("recovered"),
                value: 10,
                diagnostic_payload: 13,
                failed: false,
            })
        });
        assert_eq!(execution(&recovered), RequestExecution::Computed);
        assert_eq!(family.current_record().unwrap().value, 10);
        assert_eq!(family.last_good_record().unwrap().value, 10);
        assert_eq!(family.retention().memo_nodes, 2);
    }

    #[test]
    fn aborted_attempt_view_projects_runtime_work_without_forging_typed_work() {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("test", "prefix");
        let revision = Revision::new(10, 1);
        runtime
            .publish_revision(revision, [(input.clone(), 3)])
            .unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.abort-prefix");
        let prepared = family.prepare(Key("prefix"));
        let attempt = prepared.execute(revision, AttemptId(77), |context| {
            context.input(input)?;
            context.record_work(rue_query::WorkItem::new("runtime-prefix", 2));
            Err(QueryAbort::Canceled)
        });
        family.select(&attempt);
        let structural = QueryStructuralWork::Parse(crate::ParsedModulesWork {
            modules_considered: 1,
            ..crate::ParsedModulesWork::default()
        });
        let view = family.attempt_view(AttemptId(77), attempt.clone(), structural.clone());
        assert_eq!(view.origin_id(), AttemptId(77));
        assert_eq!(
            view.outcome(),
            AttemptOutcomeKind::Aborted(AbortedQueryReason::Canceled)
        );
        assert!(view.dependencies().is_empty());
        assert_eq!(view.runtime_observations().len(), 1);
        assert!(matches!(
            &view.runtime_observations()[0],
            RuntimeObservation::Input(input) if input.stamp == 3
        ));
        assert_eq!(view.work(), &QueryStructuralWork::None);
        assert_eq!(
            view.runtime_work(),
            &[(Arc::<str>::from("runtime-prefix"), 2)]
        );
        assert_eq!(attempt.work(), &[(Arc::<str>::from("runtime-prefix"), 2)]);
        assert_eq!(family.retained_aborted_len(), 0);
    }

    #[test]
    fn runtime_frozen_origin_survives_reuse_without_a_peer_registry() {
        let runtime = QueryRuntime::new(1);
        let revision = Revision::new(11, 1);
        runtime.publish_revision(revision, []).unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.origin");
        let computed = family
            .prepare(Key("origin"))
            .execute(revision, AttemptId(41), |_| {
                Ok(Record {
                    key: Key("origin"),
                    value: 1,
                    diagnostic_payload: 1,
                    failed: false,
                })
            });
        family.select(&computed);
        assert_eq!(computed.origin_request_id(), 41);
        let reused = family
            .prepare(Key("origin"))
            .execute(revision, AttemptId(42), |_| {
                panic!("retained terminal must be reused")
            });
        assert_eq!(reused.execution(), RequestExecution::Reused);
        assert_eq!(reused.origin_request_id(), 41);
        let view = family.attempt_view(AttemptId(42), reused, QueryStructuralWork::None);
        assert_eq!(view.origin_id(), AttemptId(41));
        assert_eq!(
            family.origin_attempt_ids().collect::<Vec<_>>(),
            vec![AttemptId(41)]
        );
    }

    // Selecting a result ends the attempt-carried bridge lease immediately, so a
    // completed attempt retained in the session ledger no longer pins its
    // terminal. Without the release the ledgered attempt would keep the terminal
    // retained for the record's whole life, defeating the retention cap.
    #[test]
    fn selecting_a_result_releases_the_attempt_bridge_lease_before_ledgering() {
        let runtime = QueryRuntime::new(1);
        let revision = Revision::new(20, 1);
        runtime.publish_revision(revision, []).unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.bridge-release");

        // Produce and select a result, then hold the attempt alive for the rest
        // of the test — mimicking the bounded attempt ledger retaining it.
        let bridged = family
            .prepare(Key("bridged"))
            .execute(revision, AttemptId(1), |_| {
                Ok(Record {
                    key: Key("bridged"),
                    value: 1,
                    diagnostic_payload: 1,
                    failed: false,
                })
            });
        family.select(&bridged);

        // Move the selection to a succession of other results. The bridged
        // terminal is no longer selection-protected; if `select` released its
        // bridge lease, it is now wholly unprotected even though `bridged` lives.
        for (offset, name) in ["f0", "f1", "f2", "f3", "f4", "f5"].into_iter().enumerate() {
            let value = 100 + offset as u64;
            let filler = family.prepare(Key(name)).execute(
                revision,
                AttemptId(10 + offset as u64),
                move |_| {
                    Ok(Record {
                        key: Key(name),
                        value,
                        diagnostic_payload: 1,
                        failed: false,
                    })
                },
            );
            family.select(&filler);
        }

        // The bridged terminal was evicted under the cap: a fresh request
        // recomputes it. Under the pre-fix bridge leak the still-alive ledgered
        // attempt would keep pinning it and this request would reuse instead.
        let recomputed = family
            .prepare(Key("bridged"))
            .execute(revision, AttemptId(99), |_| {
                Ok(Record {
                    key: Key("bridged"),
                    value: 1,
                    diagnostic_payload: 1,
                    failed: false,
                })
            });
        assert_eq!(
            recomputed.execution(),
            RequestExecution::Computed,
            "a selected result's bridge lease must end at selection, not at attempt drop"
        );

        // The bridged attempt was alive throughout: the release was driven by
        // selection, not by the attempt dropping.
        assert!(bridged.terminal().is_some());
    }
}
