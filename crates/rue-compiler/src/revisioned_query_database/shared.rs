use super::*;

#[derive(Debug, Default)]
pub(super) struct BodyReachabilityMeter {
    frontier_scans: std::sync::atomic::AtomicU64,
    frontier_scan_keys: std::sync::atomic::AtomicU64,
    frontier_batches: std::sync::atomic::AtomicU64,
    frontier_keys: std::sync::atomic::AtomicU64,
    frontier_width_one: std::sync::atomic::AtomicU64,
    frontier_width_two_to_three: std::sync::atomic::AtomicU64,
    frontier_width_four_to_seven: std::sync::atomic::AtomicU64,
    frontier_width_eight_or_more: std::sync::atomic::AtomicU64,
    transactions_prefetched: std::sync::atomic::AtomicU64,
    transactions_serial: std::sync::atomic::AtomicU64,
}

impl BodyReachabilityMeter {
    pub(super) fn accrue(&self, work: &[(Arc<str>, u64)]) {
        use std::sync::atomic::Ordering::Relaxed;

        for (identity, amount) in work {
            let counter = match identity.as_ref() {
                "reachability.frontier.scans" => &self.frontier_scans,
                "reachability.frontier.scan-keys" => &self.frontier_scan_keys,
                "reachability.frontier.batches" => &self.frontier_batches,
                "reachability.frontier.keys" => &self.frontier_keys,
                "reachability.frontier.width-1" => &self.frontier_width_one,
                "reachability.frontier.width-2-3" => &self.frontier_width_two_to_three,
                "reachability.frontier.width-4-7" => &self.frontier_width_four_to_seven,
                "reachability.frontier.width-8-plus" => &self.frontier_width_eight_or_more,
                "reachability.transactions.prefetched" => &self.transactions_prefetched,
                "reachability.transactions.serial" => &self.transactions_serial,
                _ => continue,
            };
            counter.fetch_add(*amount, Relaxed);
        }
    }

    pub(super) fn snapshot(&self) -> crate::unstable::SemanticReachabilityMetrics {
        use std::sync::atomic::Ordering::Relaxed;

        crate::unstable::SemanticReachabilityMetrics {
            frontier_scans: self.frontier_scans.load(Relaxed),
            frontier_scan_keys: self.frontier_scan_keys.load(Relaxed),
            frontier_batches: self.frontier_batches.load(Relaxed),
            frontier_keys: self.frontier_keys.load(Relaxed),
            frontier_width_one: self.frontier_width_one.load(Relaxed),
            frontier_width_two_to_three: self.frontier_width_two_to_three.load(Relaxed),
            frontier_width_four_to_seven: self.frontier_width_four_to_seven.load(Relaxed),
            frontier_width_eight_or_more: self.frontier_width_eight_or_more.load(Relaxed),
            transactions_prefetched: self.transactions_prefetched.load(Relaxed),
            transactions_serial: self.transactions_serial.load(Relaxed),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct TestBodyClosureAnonymousDigestForcing {
    pub(super) sealed: bool,
    pub(super) digests: BTreeMap<crate::AnonymousNominalKey, u128>,
}

#[cfg(test)]
pub(crate) struct TestBodyTransactionFailureGuard(pub(crate) Arc<std::sync::atomic::AtomicBool>);

#[cfg(test)]
pub(super) static TEST_CGEN_CANCEL_AFTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
#[cfg(test)]
pub(super) static TEST_CGEN_VISITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static TEST_CGEN_ATTEMPTED_SIBLINGS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static TEST_CGEN_POST_CANCEL_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static TEST_CGEN_FRONTIER_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
pub(super) static TEST_CGEN_PHASE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
pub(super) static TEST_CGEN_FRONTIER_ONLY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
#[derive(Debug, Default)]
struct FrontierRendezvousState {
    arrivals: usize,
    frontier_arrivals: usize,
    released: bool,
    timed_out: bool,
}

#[cfg(test)]
pub(crate) struct FrontierRendezvous {
    state: Mutex<FrontierRendezvousState>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
impl FrontierRendezvous {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FrontierRendezvousState::default()),
            changed: std::sync::Condvar::new(),
        })
    }

    pub(super) fn arrive_and_wait(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.arrivals = state.arrivals.saturating_add(1);
        state.frontier_arrivals = state.frontier_arrivals.saturating_add(1);
        self.changed.notify_all();
        while !state.released {
            let (next, timeout) = self
                .changed
                .wait_timeout(state, std::time::Duration::from_secs(5))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() {
                state.timed_out = true;
                state.released = true;
                self.changed.notify_all();
            }
        }
    }

    pub(crate) fn wait_for_arrivals(&self, expected: usize) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.arrivals < expected {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                state.timed_out = true;
                state.released = true;
                self.changed.notify_all();
                return false;
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && state.arrivals < expected {
                state.timed_out = true;
                state.released = true;
                self.changed.notify_all();
                return false;
            }
        }
        true
    }

    pub(crate) fn arrivals(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .arrivals
    }

    pub(crate) fn frontier_arrivals(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frontier_arrivals
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .timed_out
    }

    pub(crate) fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.released = true;
        self.changed.notify_all();
    }
}

#[cfg(test)]
pub(super) static TEST_FRONTIER_RENDEZVOUS: std::sync::OnceLock<
    Mutex<Option<Arc<FrontierRendezvous>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) struct FrontierRendezvousGuard;

#[cfg(test)]
impl Drop for FrontierRendezvousGuard {
    fn drop(&mut self) {
        if let Some(rendezvous) = TEST_FRONTIER_RENDEZVOUS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            rendezvous.release();
        }
    }
}
#[cfg(test)]
pub(crate) struct TestConstraintGenerationCancellationGuard;

#[cfg(test)]
impl Drop for TestConstraintGenerationCancellationGuard {
    fn drop(&mut self) {
        TEST_CGEN_CANCEL_AFTER.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_FRONTIER_STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_PHASE.store(0, std::sync::atomic::Ordering::SeqCst);
        TEST_CGEN_FRONTIER_ONLY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Drop for TestBodyTransactionFailureGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(super) type SemanticNucleusFamily = QueryFamily<
    crate::semantic_query_nucleus::SemanticNucleusKey,
    crate::semantic_query_nucleus::SemanticNucleusValue,
>;

/// Compiler-family registration wrapper. Every compiler success value is
/// charged by a deterministic structural traversal of its owned representation;
/// `QueryRuntime` itself keeps the zero-cost inline-only default for generic
/// users that do not register an estimator.
pub(super) struct CompilerQueryRuntime(pub(super) QueryRuntime);

impl Deref for CompilerQueryRuntime {
    type Target = QueryRuntime;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl CompilerQueryRuntime {
    pub(super) fn family_with_equality_and_evaluator<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, rue_query::FamilyError>
    where
        K: QueryKey,
        V: Clone + RetainedCharge + Send + Sync + 'static,
        E: Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
            + Send
            + Sync
            + 'static,
    {
        self.0
            .family_with_equality_and_evaluator_and_retained_charge(
                stable_name,
                retention_limit,
                value_equal,
                RetainedCharge::retained_charge,
                evaluator,
            )
    }

    pub(super) fn family_with_evaluator<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, rue_query::FamilyError>
    where
        K: QueryKey,
        V: Clone + Eq + RetainedCharge + Send + Sync + 'static,
        E: Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
            + Send
            + Sync
            + 'static,
    {
        self.family_with_equality_and_evaluator(
            stable_name,
            retention_limit,
            PartialEq::eq,
            evaluator,
        )
    }

    #[cfg(test)]
    pub(super) fn family_with_equality<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
    ) -> Result<QueryFamily<K, V>, rue_query::FamilyError>
    where
        K: QueryKey,
        V: Clone + RetainedCharge + Send + Sync + 'static,
    {
        self.0.family_with_equality_and_retained_charge(
            stable_name,
            retention_limit,
            value_equal,
            RetainedCharge::retained_charge,
        )
    }

    pub(super) fn content_addressed_family_with_equality<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
    ) -> Result<QueryFamily<K, V>, rue_query::FamilyError>
    where
        K: QueryKey,
        V: Clone + RetainedCharge + Send + Sync + 'static,
    {
        self.0
            .content_addressed_family_with_equality_and_retained_charge(
                stable_name,
                retention_limit,
                value_equal,
                RetainedCharge::retained_charge,
            )
    }
}

pub(super) const IMPORT_INPUT_REVISION_RETENTION: usize = 64;
pub(super) const MODULE_QUERY_MEMO_RETENTION: usize = 4096;
// The published body-closure lease owns the exact registered dependency cone
// of the current reachability root. Body families therefore need only a small
// unrooted history; a large reached program grows past this floor through its
// exact pins and releases superseded/deleted members atomically.
pub(super) const BODY_QUERY_MEMO_RETENTION: usize = 8;

pub(super) const BODY_CLOSURE_MEMO_RETENTION: usize = 8;
// Declaration-keyed families scale with the program's declaration universe
// exactly as body-keyed families scale with reached bodies, and the
// body-produced-anonymous fallback resolves through declaration shells and
// semantic-nucleus terminals mid-traversal, so evicting them fails the same
// projection the body retention protects. Module-keyed families stay at the
// module-scaled retention; real programs have orders of magnitude fewer
// modules than declarations.
pub(super) const DECLARATION_QUERY_MEMO_RETENTION: usize = 65536;
// ResolveImport is keyed by parser occurrence plus demand mode, not by module.
// Large programs can have thousands of sites per module universe (Caldera has
// 4,093 occurrences), and rooted/speculative variants may both be retained.
// Exact rooted membership should eventually replace this fixed bound.
pub(super) const IMPORT_OCCURRENCE_QUERY_MEMO_RETENTION: usize = DECLARATION_QUERY_MEMO_RETENTION;
// A semantic batch commonly requests hundreds of exact declaration shells.
// Keep one large batch reusable after its active pins drop; the runtime still
// bounds global retention deterministically.
pub(super) const MODULE_INPUT_REVISION_RETENTION: usize = 4096;

#[derive(Debug, Clone)]
pub(crate) struct CompatibilityKey<K> {
    pub(super) key: K,
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

impl QueryKey for CompatibilityKey<crate::session::ParseQueryKey> {
    fn stable_identity(&self) -> String {
        // Display only. Exact K equality chooses the memo node and the runtime
        // incarnation makes cycle/wait identity collision-safe — but the
        // string is what cycle and wait-graph reports print, so it must name
        // the query rather than a constant (RUE-1142).
        self.key.compatibility_identity()
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        // Agrees with `PartialEq`/`Hash` above, which compare only `key`.
        self.key.hash(hasher);
    }
}

pub(super) fn record_equal<F: TypedQueryFamily>(left: &F::Record, right: &F::Record) -> bool {
    F::terminal_kind(left) == F::terminal_kind(right)
        && F::outcome_equal(left, right)
        && F::diagnostics_equal(left, right)
}

#[derive(Debug)]
pub(super) struct RuntimeAttemptView<F: TypedQueryFamily> {
    pub(super) id: AttemptId,
    pub(super) origin: AttemptId,
    pub(super) attempt: Arc<QueryRequestAttempt<F::Record>>,
    pub(super) work: QueryStructuralWork,
    pub(super) runtime_observations: Arc<[RuntimeObservation]>,
    pub(super) runtime_work: Arc<[(Arc<str>, u64)]>,
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

/// Wave-parsed modules awaiting their one canonical parse-query consumption.
pub(super) type ParseStage =
    Arc<Mutex<AHashMap<ModuleId, crate::parsed_modules::StagedModuleParse>>>;

#[cfg(test)]
pub(super) type DeclarationBodyPlanFailureInjection = Arc<
    Mutex<
        Option<(
            crate::declaration_candidate::DeclarationCandidateKey,
            crate::CompileErrors,
        )>,
    >,
>;

pub(crate) struct RevisionedQueryDatabase {
    pub(super) runtime: QueryRuntime,
    pub(super) next_revision: u64,
    pub(super) next_source_stamp: u64,
    pub(super) source_stamps: VecDeque<(crate::session::ExactSourceInput, u64)>,
    pub(super) import_store: Arc<Mutex<ImportInputStore>>,
    pub(super) module_store: Arc<Mutex<ModuleInputStore>>,
    /// RUE-1576: the optimized-CFG and codegen collections' retained child
    /// cones, borrowed as fallback authority by the backend scopes that run
    /// after each within one rooted compile.
    pub(super) cfg_collection_root: Arc<Mutex<PublishedCollectionRoot>>,
    pub(super) codegen_collection_root: Arc<Mutex<PublishedCollectionRoot>>,
    /// Times the declaration publication could not retain its projection cone
    /// and fell back to unassisted validation. Expected zero; nonzero means
    /// the seam handoff silently degraded and the demand cascades returned.
    pub(super) publication_cone_retention_failures: Arc<std::sync::atomic::AtomicU64>,
    /// Wave-parsed modules staged for `compiler.parse-module` (ADR-0075). Each
    /// entry is consumed at most once, and only on exact `SourceId` identity,
    /// so the stage is a work handoff rather than a second parse authority.
    pub(super) parse_stage: ParseStage,
    #[cfg(test)]
    pub(super) test_import_store: Arc<Mutex<TestImportInputStore>>,
    #[cfg(test)]
    pub(super) declaration_body_plan_failure_injection: DeclarationBodyPlanFailureInjection,
    pub(super) parse_modules: QueryFamily<ModuleQueryKey, ParseModuleValue>,
    pub(super) parse_module_batches: QueryFamily<ParseModuleBatchKey, ParseModuleBatchValue>,
    pub(super) module_source_bases:
        QueryFamily<ModuleQueryKey, Option<rue_air::DurableBodySourceLocator>>,
    pub(super) module_indexes: QueryFamily<ModuleQueryKey, ModuleIndexValue>,
    #[allow(dead_code)]
    pub(super) declaration_occurrence_indexes:
        QueryFamily<ModuleQueryKey, DeclarationOccurrenceIndexValue>,
    #[allow(dead_code)]
    pub(super) declaration_orders: QueryFamily<ModuleQueryKey, DeclarationOrderValue>,
    #[allow(dead_code)]
    pub(super) declaration_shells:
        QueryFamily<DeclarationShellQueryKey, DeclarationShellQueryValue>,
    #[cfg(test)]
    pub(super) stable_declaration_classifications: QueryFamily<
        StableDeclarationClassificationQueryKey,
        StableDeclarationClassificationQueryValue,
    >,
    #[allow(dead_code)]
    pub(super) warning_body_references:
        QueryFamily<crate::body_query::BodyQueryKey, WarningBodyReferencesValue>,
    pub(super) warning_body_reference_batches:
        QueryFamily<WarningBodyReferencesBatchKey, WarningBodyReferencesBatchValue>,
    #[cfg(test)]
    pub(super) body_inputs:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::BodyInputValue>,
    #[allow(dead_code)]
    pub(super) body_source_bases:
        QueryFamily<crate::body_query::BodyQueryKey, Option<crate::body_query::BodySourceLocator>>,
    // The registered `body-toolchain-demands` node (RUE-1112). It projects one
    // reached body's canonical declaration artifact to the sorted, deduplicated
    // set of trusted toolchain modules its typed fallible-intrinsic set demands,
    // plus the demanding body's stable requester anchor. Its only input is the
    // packed artifact query, so the dependency edge is honest for invalidation,
    // metrics, and future parallel scheduling. It does no presence check or I/O.
    // The rooted body-closure attempt queries it BEFORE each body transaction, checks
    // the demanded modules against the satisfied catalogue, and parks the absent
    // ones without entering the transaction.
    pub(super) body_toolchain_demands:
        QueryFamily<crate::body_query::BodyQueryKey, crate::BodyToolchainDemand>,
    pub(super) body_transactions:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::BodyTransaction>,
    pub(super) shared_durable_payloads: Arc<SharedDurablePayloadCache>,
    #[allow(dead_code)]
    pub(super) body_analysis_bundles:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::BodyAnalysisBundle>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) body_reachability: QueryFamily<
        crate::body_query::BodyClosureQueryKey,
        crate::body_query::BodyReachabilityOutput,
    >,
    #[allow(dead_code)]
    pub(super) body_closures:
        QueryFamily<crate::body_query::BodyClosureQueryKey, crate::body_query::BodyClosureOutput>,
    pub(super) body_closure_publications: QueryFamily<
        crate::body_query::BodyClosurePublicationKey,
        Arc<rue_query::QueryTerminal<crate::body_query::BodyClosureOutput>>,
    >,
    pub(super) body_reachability_meter: Arc<BodyReachabilityMeter>,
    pub(super) body_produced_anonymous:
        QueryFamily<crate::body_query::BodyQueryKey, crate::body_query::ProducedAnonymous>,
    pub(super) declaration_body_plan_artifacts:
        QueryFamily<DeclarationBodyPlanQueryKey, DeclarationBodyPlanArtifactsValue>,
    #[cfg(test)]
    pub(super) declaration_body_plan_astgen_evaluations: Arc<std::sync::atomic::AtomicU64>,
    pub(super) resolve_imports: QueryFamily<ResolveImportKey, ResolveImportValue>,
    #[cfg(test)]
    pub(super) declaration_imports:
        QueryFamily<DeclarationImportQueryKey, DeclarationImportQueryValue>,
    pub(super) semantic_nucleus: QueryFamily<
        crate::semantic_query_nucleus::SemanticNucleusKey,
        crate::semantic_query_nucleus::SemanticNucleusValue,
    >,
    pub(super) declaration_semantics_publications: QueryFamily<
        SemanticNucleusProjectionKey,
        Arc<rue_query::QueryTerminal<SemanticNucleusProjectionValue>>,
    >,
    #[allow(dead_code)]
    pub(super) type_shapes:
        QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::TypeShapeValue>,
    #[allow(dead_code)]
    pub(super) type_facts:
        QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::TypeFactsValue>,
    #[allow(dead_code)]
    pub(super) layouts:
        QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::LayoutValue>,
    #[allow(dead_code)]
    pub(super) call_abis:
        QueryFamily<crate::type_queries::CallAbiQueryKey, crate::type_queries::CallAbiValue>,
    #[allow(dead_code)]
    pub(super) drop_glues:
        QueryFamily<crate::type_queries::TypeQueryKey, crate::type_queries::DropGlueValue>,
    #[allow(dead_code)]
    pub(super) cfgs: QueryFamily<crate::cfg_query::CfgQueryKey, crate::cfg_query::CfgValue>,
    pub(super) raw_cfg_batches: QueryFamily<RawCfgBatchKey, RawCfgBatchOutput>,
    #[allow(dead_code)]
    pub(super) optimized_cfgs:
        QueryFamily<crate::cfg_query::OptimizedCfgQueryKey, crate::cfg_query::CfgValue>,
    pub(super) optimized_cfg_batches: QueryFamily<OptimizedCfgBatchKey, OptimizedCfgBatchOutput>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) codegen_units: QueryFamily<
        crate::codegen_query::CodegenUnitQueryKey,
        crate::codegen_query::CodegenUnitValue,
    >,
    pub(super) codegen_unit_batches: QueryFamily<CodegenUnitBatchKey, CodegenUnitBatchOutput>,
    #[allow(dead_code)] // The registered batch evaluator owns production reads.
    pub(super) object_projections: QueryFamily<
        crate::object_query::ObjectProjectionQueryKey,
        crate::object_query::ObjectProjectionValue,
    >,
    pub(super) object_projection_batches:
        QueryFamily<ObjectProjectionBatchKey, ObjectProjectionBatchOutput>,
    pub(super) backend_root_publications: QueryFamily<BackendRootPublicationKey, bool>,
    /// One-shot rendezvous inside the registered CodegenUnit evaluator. Unit
    /// tests use it to force an exact-key owner to remain live while a second
    /// request joins or cancels. It changes scheduling only: both requests still
    /// execute the production family and evaluator.
    #[cfg(test)]
    pub(super) codegen_evaluator_gate: Arc<Mutex<Option<Arc<TestCodegenEvaluatorGate>>>>,
    /// Multi-child rendezvous proving that the production CodegenUnit root
    /// enters independent evaluators concurrently only when the shared query
    /// budget has more than one worker.
    #[cfg(test)]
    pub(super) codegen_batch_evaluator_gate: Arc<Mutex<Option<Arc<TestBackendBatchEvaluatorGate>>>>,
    pub(super) lookup_names: QueryFamily<LookupNameKey, LookupNameValue>,
    /// Per-`(module, import-path)` binding-resolution family. Registered query
    /// machinery for the exact provider boundary. Production body import
    /// dependencies are mediated by module-binding name and semantic-nucleus
    /// terminals; the provider captures these exact family handles for a
    /// request-local body task.
    #[allow(dead_code)]
    pub(super) lookup_imports: QueryFamily<LookupImportKey, LookupImportValue>,
    /// Test-only log of every module whose immutable name index was built,
    /// proving in-module fan-out (ADR-0066 §4).
    #[cfg(test)]
    pub(super) module_index_build_log: Arc<Mutex<Vec<ModuleId>>>,
    /// Test-only log of every consulted name-lookup key evaluated, proving that
    /// editing a module revalidates only retained lookups against that module.
    #[cfg(test)]
    pub(super) lookup_name_eval_log: Arc<Mutex<Vec<LookupNameKey>>>,
    /// Test-only log of every consulted import-path key evaluated.
    #[cfg(test)]
    pub(super) lookup_import_eval_log: Arc<Mutex<Vec<LookupImportKey>>>,
    /// Test-only per-database digest substitutions. The closure evaluator still
    /// performs the production stable-content relocation for every unforced
    /// identity; focused collision tests force only their two exact producer
    /// keys without installing process-global mutable state.
    #[cfg(test)]
    pub(super) body_closure_anonymous_digest_forcing:
        Arc<Mutex<TestBodyClosureAnonymousDigestForcing>>,
    pub(super) next_import_request: u64,
    pub(super) current_import_revision: Option<ImportInputRevision>,
    /// The exact import-input revision adopted by the latest successful close.
    /// Candidate request views may replace `current_import_revision` while open,
    /// but this independently protects and restores the public semantic view.
    pub(super) committed_import_revision: Option<ImportInputRevision>,
    /// Runtime-view root for `committed_import_revision`. Module/import stores
    /// have their own bounded histories, but every query against those inputs
    /// also requires the shared runtime revision itself to remain published.
    pub(super) committed_import_revision_pin: Option<
        rue_query::RevisionPin<
            CompatibilityKey<crate::session::ParseQueryKey>,
            crate::session::ParseQueryRecord,
        >,
    >,
    /// Compatibility namespace shared by ordinary snapshot publication and
    /// rooted import publication. The first rooted request can bind an
    /// existing ordinary-update lineage to its observation context; later
    /// context changes mint the context's regime token.
    pub(super) active_compatibility_token: u64,
    /// Whether an ordinary update has established the compatibility lineage.
    pub(super) ordinary_lineage_published: bool,
    pub(super) active_import_context: Option<ImportDiscoveryContext>,
    #[cfg(test)]
    pub(super) current_test_import_revision: Option<Revision>,
    /// Cumulative count of import occurrences the demand frontier has rooted
    /// through [`Self::import_frontier`] (RUE-1112). One `ResolveImport`
    /// projection is dispatched per rooted occurrence, so this measures the
    /// per-round discovery-frontier breadth. The trusted-toolchain re-close roots
    /// only in the newly appended leaves and modules newly discovered from them,
    /// so a predecessor occurrence contributes to this counter exactly once — at
    /// the initial close — and never again during acquisition. An ordinary
    /// acquisition is bounded the same way: after its first round each round
    /// roots only in the occurrences the plan just gained plus those still open,
    /// and the whole plan is rooted once more at the end to witness closure, so
    /// this stays linear in the import graph rather than rounds times graph.
    pub(super) import_frontier_roots_requested: u64,
    /// Cumulative count of occurrences the close-time exact-import projection has
    /// dispatched a `ResolveImport` query for through [`Self::exact_import_groups`]
    /// (RUE-1112). A trusted-toolchain successor close projects only the newly
    /// appended occurrences, so a predecessor occurrence contributes here exactly
    /// once — at the initial close — and never again during acquisition.
    pub(super) exact_import_groups_dispatched: u64,
    /// Cumulative leaves published through the complete publication path.
    pub(super) import_view_full_leaves_published: u64,
    /// Cumulative delta leaves published through the successor overlay path.
    pub(super) import_view_overlay_leaves_published: u64,
    /// Cumulative ledger observations DEEP-COPIED while cloning a view's
    /// carried ledger (the cloned value's recorded head — its own delta). The
    /// persistent ledger shares frozen predecessor segments by `Arc`, so this
    /// counts only per-step delta entries and stays flat across predecessor
    /// topologies. Atomic because shared-state accessors count from `&self`.
    pub(super) import_view_ledger_entries_cloned: std::sync::atomic::AtomicU64,
    /// Predecessor source entries element-compared by the overlay publication's
    /// FALLBACK diff (a host-rebuilt snapshot); the structural-authority path
    /// never increments this, so the acquisition profile can require it zero.
    pub(super) import_view_source_entries_compared: std::sync::atomic::AtomicU64,
    /// Predecessor accepted-read entries element-compared by the overlay
    /// publication's FALLBACK provenance diff (a host-rebuilt manifest); the
    /// structural-authority path never increments this, so the acquisition
    /// profile can require it zero.
    pub(super) import_view_read_entries_compared: std::sync::atomic::AtomicU64,
    /// Module-identity and physical-identity resolution work. Both
    /// questions are asked once per module by the parse projection and by
    /// discovery authorization, so answering either by a scan is quadratic in
    /// the depth of an import chain while dispatching nothing that any other
    /// counter can see. Shared by `Arc` because the parse-module and
    /// resolve-import evaluators are `&self`-free closures installed at
    /// construction.
    pub(super) identity_resolution: Arc<crate::source_snapshot::IdentityResolutionMeter>,
    /// The module revisions appended by overlay publications since the last
    /// committed close (RUE-1112): the session-owned recorded-additions lineage.
    /// The successor stage/close derive their module delta from THIS record —
    /// each exact issued batch and the trusted publish append here at
    /// publication — instead of re-deriving it by scanning complete views.
    pub(super) lineage_additions: Vec<ModuleRevision>,
    /// Provider-op observation counters (RUE-1091, ADR-0066 §4), shared by the
    /// registered production body evaluator and focused provider probes.
    /// Exposed through the unstable surface as direct witnesses of the exact
    /// provider work performed by compiled bodies.
    pub(super) provider_observation_meter: Arc<ProviderObservationCounters>,
    /// Session-held retention lease over the lookup families (RUE-1091,
    /// ADR-0066 §4). A rooted semantic publication promotes its exact observed
    /// lookup-pin set here. Behind a `Mutex` because promotion is a `&self`
    /// operation on the shared database.
    pub(super) lookup_root_lease: Arc<Mutex<PublishedRootLookupLease>>,
    #[allow(dead_code)]
    pub(super) body_closure_root: Arc<Mutex<PublishedBodyClosureRoot>>,
    #[allow(dead_code)]
    pub(super) body_reachability_root: Arc<Mutex<PublishedBodyReachabilityRoot>>,
    /// The exact backend terminal cone selected by the latest successful
    /// rooted codegen collection. The candidate set is populated while each
    /// request lease is still live, then installed before the predecessor is
    /// released so programs wider than the family retention floor stay warm.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) backend_root: Arc<Mutex<PublishedBackendRoot>>,
    pub(super) backend_root_publication_gate: BackendRootPublicationGate,
    pub(super) next_backend_root_epoch: std::sync::atomic::AtomicU64,
    pub(super) next_optimized_cfg_batch_generation: std::sync::atomic::AtomicU64,
    /// Session-scoped injection shared with structured body-query children.
    /// Keeping it on the database avoids both thread-local scheduler escapes
    /// and cross-test interference between independent compiler sessions.
    #[cfg(test)]
    pub(super) inject_body_transaction_failure: Arc<std::sync::atomic::AtomicBool>,
    /// Test-only witness that the rooted-publication promotion hook took its
    /// non-empty branch — i.e. entered the promotion path at all (RUE-1091). The
    /// hook checks the observed set for emptiness FIRST, before formatting the
    /// root identity or taking the lease lock, and increments this only past that
    /// gate.
    #[cfg(test)]
    /// Test-only probe family hosting one provider-observation task so a driven
    /// body's recorded query edges are inspectable through the task terminal's
    /// `dependencies()`.
    #[cfg(test)]
    pub(super) provider_probe: QueryFamily<ProviderProbeKey, ProviderProbeValue>,
    pub(super) parse: QueryFamily<
        CompatibilityKey<crate::session::ParseQueryKey>,
        crate::session::ParseQueryRecord,
    >,
    pub(super) parse_selection: QuerySelection<
        CompatibilityKey<crate::session::ParseQueryKey>,
        crate::session::ParseQueryRecord,
    >,
}
