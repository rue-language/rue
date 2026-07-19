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
    CancellationToken, InputIdentity, QueryAbort, QueryFamily, QueryKey, QueryOutput,
    QueryRequestAttempt, QueryRuntime, QuerySelection, QueryTerminalKind, RequestExecution,
    Revision,
};

use crate::{
    AcceptedReadManifestEntry, CompileError, CompileResult, DefinitionKind, DefinitionNamespace,
    ErrorKind, ImportDemandFrontier, ImportDemandMode, ImportDemandRoots, ImportDiscoveryContext,
    ImportDiscoveryPlan, ImportDiscoveryRequest, ImportInputRevision, ImportObservation,
    ImportObservationLedger, ModuleId, ModuleRevision, SourceSnapshot, Span, SyntaxWork,
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

impl<K> QueryKey for CompatibilityKey<K>
where
    K: Clone + Eq + Send + Sync + 'static,
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
    parse_modules: QueryFamily<ModuleQueryKey, ParseModuleValue>,
    module_indexes: QueryFamily<ModuleQueryKey, ModuleIndexValue>,
    module_rirs: QueryFamily<ModuleQueryKey, ModuleRirValue>,
    resolve_imports: QueryFamily<ResolveImportKey, ResolveImportValue>,
    lookup_names: QueryFamily<LookupNameKey, LookupNameValue>,
    next_import_request: u64,
    current_import_revision: Option<ImportInputRevision>,
    pub(crate) parse: RevisionedFamily<super::session::ParseQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl Default for ModuleInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            next_stamp: 1,
            stamps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolveImportKey {
    occurrence: crate::ImportOccurrenceKey,
    mode: ImportDemandMode,
}

impl QueryKey for ResolveImportKey {
    fn stable_identity(&self) -> String {
        format!(
            "{}:{}..{}:{}",
            self.occurrence.importer(),
            self.occurrence.source_offset(),
            self.occurrence.source_end(),
            self.occurrence.specifier()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolveImportValue {
    groups: Arc<[Arc<[ImportDiscoveryRequest]>]>,
    requests: Arc<[ImportDiscoveryRequest]>,
    speculative_blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
struct LookupNameValue(Result<Arc<[ModuleIndexEntry]>, crate::CompileErrors>);

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

#[derive(Debug)]
struct ImportInputStore {
    revisions: VecDeque<Arc<ImportInputView>>,
    next_stamp: u64,
    context_stamps: Vec<(ImportDiscoveryContext, u64)>,
    provenance_stamps: Vec<(AcceptedReadManifestEntry, u64)>,
    observation_stamps: Vec<(ImportObservation, u64)>,
}

impl Default for ImportInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            next_stamp: 1,
            context_stamps: Vec::new(),
            provenance_stamps: Vec::new(),
            observation_stamps: Vec::new(),
        }
    }
}

fn module_source_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new("module-source", Arc::<str>::from(module.as_str()))
}

fn import_context_input() -> InputIdentity {
    InputIdentity::new("import-discovery-context", "current")
}

fn accepted_read_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new(
        "accepted-read-provenance",
        Arc::<str>::from(module.as_str()),
    )
}

fn import_observation_input(request: &ImportDiscoveryRequest) -> InputIdentity {
    InputIdentity::new("import-observation", request.runtime_input_key())
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

impl Default for RevisionedQueryDatabase {
    fn default() -> Self {
        let runtime = QueryRuntime::new(1);
        let module_store = Arc::new(Mutex::new(ModuleInputStore::default()));
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
        let index_for_lookup = module_indexes.clone();
        let lookup_names = runtime
            .family_with_evaluator(
                "compiler.lookup-name",
                MODULE_QUERY_MEMO_RETENTION,
                move |context, _, key: &LookupNameKey| {
                    let indexed = context
                        .query_registered(&index_for_lookup, ModuleQueryKey(key.module.clone()))?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let result = indexed.0.as_ref().map(|index| {
                        index
                            .definitions
                            .iter()
                            .filter(|entry| {
                                entry.namespace == key.namespace && entry.name == key.name
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                            .into()
                    });
                    Ok(QueryOutput::success(LookupNameValue(
                        result.map_err(Clone::clone),
                    )))
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
        let index_for_import = module_indexes.clone();
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
                    let indexed = context.query_registered(
                        &index_for_import,
                        ModuleQueryKey(key.occurrence.importer().clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let site = indexed.0.as_ref().ok().and_then(|index| {
                        index.imports.iter().find(|site| {
                            site.importer() == key.occurrence.importer()
                                && site.source_offset() == key.occurrence.source_offset()
                                && site.source_end() == key.occurrence.source_end()
                                && site.specifier() == key.occurrence.specifier()
                        })
                    });
                    let site = site.expect("ResolveImport key is absent from ModuleIndex");
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
                    Ok(QueryOutput::success(ResolveImportValue {
                        groups: groups.into(),
                        requests: if speculative_blocked {
                            Arc::from([])
                        } else {
                            pending.into()
                        },
                        speculative_blocked,
                    }))
                },
            )
            .expect("the ResolveImport family has one canonical name");
        Self {
            parse: RevisionedFamily::new(&runtime, "compiler.parse"),
            runtime,
            next_revision: 1,
            next_source_stamp: 1,
            source_stamps: VecDeque::new(),
            import_store,
            module_store,
            parse_modules,
            module_indexes,
            module_rirs,
            resolve_imports,
            lookup_names,
            next_import_request: 0,
            current_import_revision: None,
        }
    }
}

impl RevisionedQueryDatabase {
    pub(crate) const SOURCE_INPUT: &'static str = "selected-source";

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
            // RUE-1026 DELETION GATE: this selected-revision compatibility
            // shim owns one synchronous request and therefore has no caller
            // cancellation token to thread yet. Canonical multi-request
            // consumers must supply their token when this shim is deleted.
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
        let revision = Revision::new(self.next_revision, generation);
        self.next_revision += 1;
        let mut leaves = Vec::new();
        {
            let mut store = lock_import_store(&self.import_store);
            let ImportInputStore {
                next_stamp,
                context_stamps,
                provenance_stamps,
                observation_stamps,
                ..
            } = &mut *store;
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
                        name,
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
                    Ok(found) => definitions.extend(found.iter().cloned()),
                    Err(module_errors) => errors.extend(module_errors.clone()),
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

    #[derive(Debug, Clone, PartialEq, Eq)]
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
            .import_demand_frontier(revision, &plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(revision, &plan, ImportDemandMode::Speculative)
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
            .import_demand_frontier(revision, &plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(first_revision, &first_plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(second_revision, &second_plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(first_revision, &first_plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(fresh_revision, &fresh_plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(revision, &plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(revision, &plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(successor, &successor_plan, ImportDemandMode::Rooted)
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
            .import_demand_frontier(new_revision, &new_plan, ImportDemandMode::Rooted)
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
}
