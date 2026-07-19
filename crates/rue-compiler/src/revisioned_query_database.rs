//! Phase 1 compatibility layer over the canonical revisioned query runtime.
//!
//! This module deliberately preserves the existing compiler family's typed
//! record shape while moving key identity, execution, immutable attempts,
//! dependency recording, and current/last-good publication into `rue-query`.
//! It is a migration boundary, not a peer database. RUE-1033 / ADR-0063 Phase
//! 12 deletes this selected-state-shaped shim after every family calls the
//! runtime directly.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use rue_query::{
    CancellationToken, InputIdentity, QueryAbort, QueryFamily, QueryKey, QueryOutput,
    QueryRequestAttempt, QueryRuntime, QuerySelection, QueryTerminalKind, RequestExecution,
    Revision,
};

use crate::{
    AcceptedReadManifestEntry, CompileError, CompileResult, ErrorKind, ImportDemandFrontier,
    ImportDemandMode, ImportDemandRoots, ImportDiscoveryContext, ImportDiscoveryPlan,
    ImportDiscoveryRequest, ImportInputRevision, ImportObservation, ImportObservationLedger,
    ModuleId, ModuleRevision, SourceSnapshot,
};

use crate::session::{AttemptId, QueryStructuralWork};
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as CompilerAttemptExecution, AttemptOutcomeKind,
    AttemptView, RuntimeObservation,
};
use crate::typed_query_store::{TerminalKind, TypedQueryFamily};

const IMPORT_INPUT_REVISION_RETENTION: usize = 64;

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
    import_frontiers: QueryFamily<ImportModuleDemandKey, ImportModuleDemandValue>,
    next_import_request: u64,
    current_import_revision: Option<ImportInputRevision>,
    pub(crate) parse: RevisionedFamily<super::session::ParseQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportModuleDemandKey {
    module: ModuleId,
    groups: Arc<[Arc<[ImportDiscoveryRequest]>]>,
    mode: ImportDemandMode,
}

impl QueryKey for ImportModuleDemandKey {
    fn stable_identity(&self) -> String {
        self.module.as_str().to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportModuleDemandValue {
    requests: Arc<[ImportDiscoveryRequest]>,
    speculative_blocked: bool,
}

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
    source_stamps: Vec<(ModuleRevision, u64)>,
    provenance_stamps: Vec<(AcceptedReadManifestEntry, u64)>,
    observation_stamps: Vec<(ImportObservation, u64)>,
}

impl Default for ImportInputStore {
    fn default() -> Self {
        Self {
            revisions: VecDeque::new(),
            next_stamp: 1,
            source_stamps: Vec::new(),
            provenance_stamps: Vec::new(),
            observation_stamps: Vec::new(),
        }
    }
}

fn module_source_input(module: &ModuleId) -> InputIdentity {
    InputIdentity::new("module-source", Arc::<str>::from(module.as_str()))
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

fn pending_module_requests(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> Vec<ImportDiscoveryRequest> {
    let mut pending = Vec::new();
    let mut by_site = BTreeMap::<_, Vec<_>>::new();
    for group in groups {
        by_site
            .entry(group[0].occurrence())
            .or_default()
            .push(group);
    }
    for groups in by_site.values() {
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
                .filter_map(|(request, observation)| {
                    observation.is_none().then_some(request.clone())
                })
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                pending.extend(missing);
                break;
            }
            if observations.iter().any(|observation| {
                observation.is_some_and(|value| value.accepted_source().is_some())
            }) {
                break;
            }
        }
    }
    pending
}

impl Default for RevisionedQueryDatabase {
    fn default() -> Self {
        let runtime = QueryRuntime::new(1);
        let import_store = Arc::new(Mutex::new(ImportInputStore::default()));
        let evaluator_store = import_store.clone();
        let import_frontiers = runtime
            .family_with_evaluator(
                "compiler.import-module-frontier",
                IMPORT_INPUT_REVISION_RETENTION,
                move |context, _, key: &ImportModuleDemandKey| {
                    let view = {
                        let store = lock_import_store(&evaluator_store);
                        store
                            .revisions
                            .iter()
                            .find(|view| view.revision == context.revision())
                            .cloned()
                    }
                    .ok_or_else(|| QueryAbort::UnpublishedRevision(context.revision()))?;
                    context.input(module_source_input(&key.module))?;
                    context.input(accepted_read_input(&key.module))?;
                    for request in key.groups.iter().flat_map(|group| group.iter()) {
                        let present = context
                            .optional_input(import_observation_input(request))
                            .is_some();
                        assert_eq!(present, view.ledger.get(request).is_some());
                    }
                    let pending = pending_module_requests(&key.groups, &view.ledger);
                    let speculative_blocked =
                        key.mode == ImportDemandMode::Speculative && !pending.is_empty();
                    Ok(QueryOutput::success(ImportModuleDemandValue {
                        requests: if speculative_blocked {
                            Arc::from([])
                        } else {
                            pending.into()
                        },
                        speculative_blocked,
                    }))
                },
            )
            .expect("the import frontier family has one canonical name");
        Self {
            parse: RevisionedFamily::new(&runtime, "compiler.parse"),
            runtime,
            next_revision: 1,
            next_source_stamp: 1,
            source_stamps: VecDeque::new(),
            import_store,
            import_frontiers,
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
        for source in plan.source_revision().modules() {
            let groups = plan.groups_for_demand(&source.module, roots);
            if groups.is_empty() {
                continue;
            }
            let key = ImportModuleDemandKey {
                module: source.module.clone(),
                groups,
                mode,
            };
            // RUE-1026 DELETION GATE: this selected-revision compatibility
            // shim owns one synchronous request and therefore has no caller
            // cancellation token to thread yet. Canonical multi-request
            // consumers must supply their token when this shim is deleted.
            let attempt = self.runtime.request_registered(
                &self.import_frontiers,
                runtime_revision,
                key,
                CancellationToken::new(),
            );
            let terminal = attempt.terminal().ok_or_else(|| {
                import_input_error(format!(
                    "import module demand query aborted: {:?}",
                    attempt.abort()
                ))
            })?;
            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                unreachable!("import frontier family publishes typed success values")
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
                source_stamps,
                provenance_stamps,
                observation_stamps,
                ..
            } = &mut *store;
            for source in sources.iter() {
                leaves.push((
                    module_source_input(&source.module),
                    exact_value_stamp(next_stamp, source_stamps, source),
                ));
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
            .source_stamps
            .retain(|(candidate, _)| retained.iter().any(|view| view.sources.contains(candidate)));
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
        self.runtime
            .publish_revision(
                revision,
                [(InputIdentity::new(Self::SOURCE_INPUT, "current"), stamp)],
            )
            .expect("compiler input revisions are immutable and uniquely numbered");
        revision
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
        ImportObservation, PhysicalFileIdentity,
    };
    use std::collections::BTreeSet;

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
