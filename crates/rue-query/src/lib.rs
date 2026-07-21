//! Parallel, demand-driven query execution primitives.
//!
//! This crate owns execution mechanics only. Compiler query families keep
//! their typed keys, results, equality, and algorithms outside the runtime.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// A logical key suitable for a retained query family.
///
/// `Hash` must agree with `Eq`: keys that compare equal must hash equal. The
/// memo map is keyed by the typed key itself, so hash collisions never conflate
/// distinct keys — they are resolved by exact `Self::eq`. Implementors that
/// embed `Arc<[T]>` or map/set payloads must derive or write `Hash`
/// consistently with their `Eq`.
pub trait QueryKey: Clone + Eq + Hash + Send + Sync + 'static {
    /// A deterministic user-visible identity within the family.
    ///
    /// This text is presentation only and may collide. Exact `Self::eq`
    /// remains authoritative for memo-node lookup.
    fn stable_identity(&self) -> String;
}

/// Collision-free identity of one immutable input leaf.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputIdentity {
    family: Arc<str>,
    key: Arc<str>,
}

impl InputIdentity {
    /// Creates a family/key input identity.
    pub fn new(family: impl Into<Arc<str>>, key: impl Into<Arc<str>>) -> Self {
        Self {
            family: family.into(),
            key: key.into(),
        }
    }

    /// Stable input-family name.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Stable key within the input family.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Exact value stamp of one leaf in an immutable input revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputObservation {
    /// Collision-free input identity.
    pub input: InputIdentity,
    /// Family-owned exact value stamp. This is not a memo-node identity.
    pub stamp: u64,
}

/// An immutable input revision pinned by one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision {
    id: u64,
    compatibility: u64,
}

impl Revision {
    /// Creates a revision. Equal compatibility tokens assert equivalent inputs.
    pub const fn new(id: u64, compatibility: u64) -> Self {
        Self { id, compatibility }
    }

    /// The immutable publication identity.
    pub const fn id(self) -> u64 {
        self.id
    }

    /// Returns whether retained work may be validated across two revisions.
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.compatibility == other.compatibility
    }
}

/// Canonical user-visible identity of one logical memo node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIdentity {
    family: Arc<str>,
    key: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExactNodeIdentity {
    display: NodeIdentity,
    incarnation: u64,
}

impl NodeIdentity {
    /// Stable family name.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Family-defined stable key identity.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Identity observed by a dependent query.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Observation {
    /// Logical dependency node.
    pub node: NodeIdentity,
    /// Opaque session-local node incarnation, preventing stamp ABA after eviction.
    pub incarnation: u64,
    /// Red/green terminal publication stamp.
    pub stamp: u64,
}

/// A deterministic query failure which is safe to retain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryFailure {
    /// Stable failure class.
    pub code: Arc<str>,
    /// Canonical failure payload.
    pub payload: Arc<str>,
}

impl QueryFailure {
    /// Creates a retained failure.
    pub fn new(code: impl Into<Arc<str>>, payload: impl Into<Arc<str>>) -> Self {
        Self {
            code: code.into(),
            payload: payload.into(),
        }
    }
}

/// A semantic diagnostic plus a separately replaceable presentation position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDiagnostic {
    /// Stable producer-defined identity.
    pub identity: Arc<str>,
    /// Canonical semantic payload.
    pub payload: Arc<str>,
    /// Current presentation position, excluded from red/green equality.
    pub presentation: Option<PresentationPosition>,
}

impl QueryDiagnostic {
    /// Creates a diagnostic.
    pub fn new(
        identity: impl Into<Arc<str>>,
        payload: impl Into<Arc<str>>,
        presentation: Option<PresentationPosition>,
    ) -> Self {
        Self {
            identity: identity.into(),
            payload: payload.into(),
            presentation,
        }
    }
}

/// Current source position used only when presenting a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PresentationPosition {
    /// Stable source identity.
    pub source: Arc<str>,
    /// Current byte offset.
    pub offset: u32,
}

impl PresentationPosition {
    /// Creates a presentation position.
    pub fn new(source: impl Into<Arc<str>>, offset: u32) -> Self {
        Self {
            source: source.into(),
            offset,
        }
    }
}

/// One deterministic structural-work contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    /// Stable metric identity.
    pub identity: Arc<str>,
    /// Additive amount.
    pub amount: u64,
}

impl WorkItem {
    /// Creates one contribution.
    pub fn new(identity: impl Into<Arc<str>>, amount: u64) -> Self {
        Self {
            identity: identity.into(),
            amount,
        }
    }
}

/// Canonical success or retained deterministic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryOutcome<V> {
    /// Successfully computed value.
    Success(V),
    /// Deterministic family failure.
    Failure(QueryFailure),
}

/// Family-owned semantic classification of a terminal record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTerminalKind {
    /// A successful family record.
    Success,
    /// A deterministic family failure record.
    Failure,
}

/// Private computation output awaiting atomic publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutput<V> {
    outcome: QueryOutcome<V>,
    kind: QueryTerminalKind,
    diagnostics: Vec<QueryDiagnostic>,
    work: Vec<WorkItem>,
}

impl<V> QueryOutput<V> {
    /// Creates a successful output.
    pub fn success(value: V) -> Self {
        Self {
            outcome: QueryOutcome::Success(value),
            kind: QueryTerminalKind::Success,
            diagnostics: Vec::new(),
            work: Vec::new(),
        }
    }

    /// Creates a deterministic terminal failure.
    pub fn failure(failure: QueryFailure) -> Self {
        Self {
            outcome: QueryOutcome::Failure(failure),
            kind: QueryTerminalKind::Failure,
            diagnostics: Vec::new(),
            work: Vec::new(),
        }
    }

    /// Attaches diagnostics. Publication sorts them deterministically.
    pub fn with_diagnostics(mut self, diagnostics: Vec<QueryDiagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Attaches structural work. Publication reduces it by stable identity.
    pub fn with_work(mut self, work: Vec<WorkItem>) -> Self {
        self.work = work;
        self
    }

    /// Applies the typed family's semantic success/failure classification.
    /// This is useful when both variants retain the same typed record shape.
    pub fn with_terminal_kind(mut self, kind: QueryTerminalKind) -> Self {
        self.kind = kind;
        self
    }
}

/// An immutable published terminal.
#[derive(Debug)]
pub struct QueryTerminal<V> {
    family_token: FamilyToken,
    node: NodeIdentity,
    node_incarnation: u64,
    revision: Revision,
    stamp: u64,
    origin_request: u64,
    outcome: QueryOutcome<V>,
    kind: QueryTerminalKind,
    diagnostics: Arc<[QueryDiagnostic]>,
    work: Arc<[(Arc<str>, u64)]>,
    dependencies: Arc<[Observation]>,
    inputs: Arc<[InputObservation]>,
    pins: AtomicUsize,
}

impl<V> QueryTerminal<V> {
    /// Canonical display identity of the node which owns this attempt.
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }

    /// Exact revision under which the computing task ran.
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Red/green publication identity.
    pub const fn stamp(&self) -> u64 {
        self.stamp
    }

    /// Runtime request which originally computed this immutable terminal.
    pub fn origin_request_id(&self) -> u64 {
        self.origin_request
    }

    /// Canonical success or deterministic failure.
    pub fn outcome(&self) -> &QueryOutcome<V> {
        &self.outcome
    }

    /// Family-owned semantic success/failure classification.
    pub fn kind(&self) -> QueryTerminalKind {
        self.kind
    }

    /// Deterministically sorted diagnostics with current positions.
    pub fn diagnostics(&self) -> &[QueryDiagnostic] {
        &self.diagnostics
    }

    /// Deterministically reduced structural work.
    pub fn work(&self) -> &[(Arc<str>, u64)] {
        &self.work
    }

    /// Exact dependency observations made by the computing task.
    pub fn dependencies(&self) -> &[Observation] {
        &self.dependencies
    }

    /// Exact input leaves read directly by the computing body.
    pub fn inputs(&self) -> &[InputObservation] {
        &self.inputs
    }
}

/// A non-terminal query-control result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryAbort {
    /// This request was canceled. No terminal was published.
    Canceled,
    /// Exact nodes participating in a true dependency cycle.
    Cycle(Arc<[NodeIdentity]>),
    /// The family belongs to a different runtime.
    ForeignRuntime,
    /// The requested immutable revision has not been published or was retired.
    UnpublishedRevision(Revision),
    /// A query requested a leaf absent from its pinned immutable revision.
    MissingInput(InputIdentity),
}

/// How one immutable request attempt reached its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestExecution {
    /// This request owned and ran the query body.
    Computed,
    /// This request reused a validated retained terminal.
    Reused,
    /// This request parked behind the compatible exact-key owner.
    Joined,
    /// This request ended without publishing a terminal.
    Aborted,
}

/// Runtime-owned immutable record of one nested query request.
///
/// The value remains owned by its typed terminal. This type-erased lifecycle
/// is sufficient for diagnostics, metrics, and provenance without forging a
/// second typed memo record in a compatibility adapter.
#[derive(Debug, Clone)]
pub struct NestedQueryAttempt {
    id: u64,
    node: NodeIdentity,
    node_incarnation: Option<u64>,
    origin_request: u64,
    execution: RequestExecution,
    terminal_revision: Option<Revision>,
    terminal_stamp: Option<u64>,
    abort: Option<QueryAbort>,
    dependencies: Arc<[Observation]>,
    inputs: Arc<[InputObservation]>,
    work: Arc<[(Arc<str>, u64)]>,
}

impl NestedQueryAttempt {
    /// Runtime-local identity of this nested request.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Display identity of the node requested by this lifecycle.
    pub fn node(&self) -> &NodeIdentity {
        &self.node
    }

    /// Opaque exact node incarnation when the request returned a terminal.
    pub fn node_incarnation(&self) -> Option<u64> {
        self.node_incarnation
    }

    /// Caller-owned origin frozen into a computed terminal, or inherited from
    /// the terminal reused or joined by this request.
    pub fn origin_request_id(&self) -> u64 {
        self.origin_request
    }

    /// Computed, reused, joined, or aborted lifecycle classification.
    pub fn execution(&self) -> RequestExecution {
        self.execution
    }

    /// Revision of the terminal returned by this request.
    pub fn terminal_revision(&self) -> Option<Revision> {
        self.terminal_revision
    }

    /// Red/green stamp of the terminal returned by this request.
    pub fn terminal_stamp(&self) -> Option<u64> {
        self.terminal_stamp
    }

    /// Non-terminal control result, present exactly for aborted attempts.
    pub fn abort(&self) -> Option<&QueryAbort> {
        self.abort.as_ref()
    }

    /// Exact dependency prefix observed by this nested request.
    pub fn dependencies(&self) -> &[Observation] {
        &self.dependencies
    }

    /// Exact direct-input prefix observed by this nested request.
    pub fn inputs(&self) -> &[InputObservation] {
        &self.inputs
    }

    /// Runtime-owned work prefix for this nested request.
    pub fn work(&self) -> &[(Arc<str>, u64)] {
        &self.work
    }
}

/// Runtime-owned immutable record of one top-level query request.
#[derive(Debug)]
pub struct QueryRequestAttempt<V> {
    id: u64,
    origin_request: u64,
    execution: RequestExecution,
    terminal: Option<Arc<QueryTerminal<V>>>,
    abort: Option<QueryAbort>,
    dependencies: Arc<[Observation]>,
    inputs: Arc<[InputObservation]>,
    work: Arc<[(Arc<str>, u64)]>,
    nested_attempts: Arc<[NestedQueryAttempt]>,
}

impl<V> QueryRequestAttempt<V> {
    /// Session-local request identity.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Caller-owned origin identity frozen by the runtime for this request.
    pub fn origin_request_id(&self) -> u64 {
        self.origin_request
    }

    /// Computed, reused, joined, or aborted lifecycle classification.
    pub fn execution(&self) -> RequestExecution {
        self.execution
    }

    /// Published terminal, absent for a non-terminal abort.
    pub fn terminal(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.terminal.as_ref()
    }

    /// Non-terminal control result, present exactly for aborted attempts.
    pub fn abort(&self) -> Option<&QueryAbort> {
        self.abort.as_ref()
    }

    /// Exact dependency prefix observed by this request.
    pub fn dependencies(&self) -> &[Observation] {
        &self.dependencies
    }

    /// Exact input prefix observed directly, or propagated through an aborted
    /// nested request which had no terminal dependency to represent it.
    pub fn inputs(&self) -> &[InputObservation] {
        &self.inputs
    }

    /// Exact structural-work prefix owned by this request.
    ///
    /// Reuse and join attempts carry no historical work. Computed and aborted
    /// attempts retain work performed by their own task before termination.
    pub fn work(&self) -> &[(Arc<str>, u64)] {
        &self.work
    }

    /// Runtime-owned nested request ledger in deterministic completion order.
    /// Descendants precede the parent nested request which demanded them.
    pub fn nested_attempts(&self) -> &[NestedQueryAttempt] {
        &self.nested_attempts
    }

    /// Origin terminal revision for reuse/join provenance.
    pub fn origin_revision(&self) -> Option<Revision> {
        self.terminal.as_ref().map(|terminal| terminal.revision())
    }

    /// Converts this attempt to the legacy terminal-or-abort call shape.
    pub fn into_result(self) -> Result<Arc<QueryTerminal<V>>, QueryAbort> {
        match (self.terminal, self.abort) {
            (Some(terminal), None) => Ok(terminal),
            (None, Some(abort)) => Err(abort),
            _ => unreachable!("request attempt has exactly one outcome"),
        }
    }
}

/// Cooperative request cancellation.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

#[derive(Debug, Default)]
struct CancellationInner {
    canceled: AtomicBool,
    watchers: Mutex<Vec<Weak<WaitCell>>>,
}

impl CancellationToken {
    /// Creates a live token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels this waiter/request and wakes its parked joins.
    pub fn cancel(&self) {
        if self.inner.canceled.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut watchers = lock(&self.inner.watchers);
        watchers.retain(|watcher| {
            let Some(waiter) = watcher.upgrade() else {
                return false;
            };
            waiter.cv.notify_all();
            true
        });
    }

    /// Whether cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.inner.canceled.load(Ordering::Acquire)
    }

    fn watch(&self, waiter: &Arc<WaitCell>) {
        lock(&self.inner.watchers).push(Arc::downgrade(waiter));
        if self.is_canceled() {
            waiter.cv.notify_all();
        }
    }
}

/// Deterministic structural execution counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    /// New computations claimed.
    pub claims: u64,
    /// Compatible in-flight requests joined.
    pub joins: u64,
    /// Compatible retained terminals reused.
    pub reuses: u64,
    /// Query bodies which completed before publication checks.
    pub body_completions: u64,
    /// Recomputations whose observable stamp stayed red.
    pub red_publications: u64,
    /// New or observably changed green publications.
    pub green_publications: u64,
    /// Canceled computations or waiters.
    pub cancellations: u64,
    /// True dependency cycles reported.
    pub cycles: u64,
    /// Retained terminal attempts evicted.
    pub evictions: u64,
    /// Current retained terminal attempts.
    pub retained_terminals: u64,
    /// Peak simultaneously executing query bodies.
    pub peak_active_bodies: u64,
    /// Times a parked joiner released its permit.
    pub donated_permits: u64,
    /// Immutable revision views currently retained by the runtime.
    pub retained_revisions: u64,
    /// Configured immutable-revision view bound.
    pub revision_limit: u64,
}

/// Bounded retained ownership for one query family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyRetention {
    /// Logical memo nodes currently owned by the family.
    pub memo_nodes: usize,
    /// Terminal attempts currently retained across those nodes.
    pub terminals: usize,
    /// Configured terminal bound. Protected roots may exceed it temporarily.
    pub terminal_limit: usize,
}

#[derive(Debug, Default)]
struct Metrics {
    claims: AtomicU64,
    joins: AtomicU64,
    reuses: AtomicU64,
    body_completions: AtomicU64,
    red_publications: AtomicU64,
    green_publications: AtomicU64,
    cancellations: AtomicU64,
    cycles: AtomicU64,
    evictions: AtomicU64,
    retained_terminals: AtomicU64,
    active_bodies: AtomicU64,
    peak_active_bodies: AtomicU64,
    donated_permits: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> RuntimeMetrics {
        RuntimeMetrics {
            claims: self.claims.load(Ordering::Relaxed),
            joins: self.joins.load(Ordering::Relaxed),
            reuses: self.reuses.load(Ordering::Relaxed),
            body_completions: self.body_completions.load(Ordering::Relaxed),
            red_publications: self.red_publications.load(Ordering::Relaxed),
            green_publications: self.green_publications.load(Ordering::Relaxed),
            cancellations: self.cancellations.load(Ordering::Relaxed),
            cycles: self.cycles.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            retained_terminals: self.retained_terminals.load(Ordering::Relaxed),
            peak_active_bodies: self.peak_active_bodies.load(Ordering::Relaxed),
            donated_permits: self.donated_permits.load(Ordering::Relaxed),
            retained_revisions: 0,
            revision_limit: REVISION_RETENTION_LIMIT as u64,
        }
    }

    fn body_entered(&self) {
        let active = self.active_bodies.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_active_bodies.fetch_max(active, Ordering::AcqRel);
    }

    fn body_left(&self) {
        self.active_bodies.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Shared execution substrate for all query families in one database.
#[derive(Debug, Clone)]
pub struct QueryRuntime {
    core: Arc<RuntimeCore>,
}

#[derive(Debug)]
struct RuntimeCore {
    identity: u64,
    permits: PermitBudget,
    wait_graph: Mutex<BTreeMap<TaskId, WaitEdge>>,
    family_names: Mutex<BTreeSet<Arc<str>>>,
    revisions: Mutex<RevisionStore>,
    nodes: Mutex<BTreeMap<u64, Weak<dyn ErasedNode>>>,
    next_task: AtomicU64,
    next_family: AtomicU64,
    next_node: AtomicU64,
    metrics: Metrics,
    #[cfg(test)]
    test_events: TestEvents,
}

const REVISION_RETENTION_LIMIT: usize = 64;

#[derive(Debug)]
struct RevisionStore {
    entries: BTreeMap<u64, RevisionEntry>,
    retired_through: u64,
}

#[derive(Debug)]
struct RevisionEntry {
    revision: Revision,
    inputs: Arc<BTreeMap<InputIdentity, u64>>,
    active_requests: usize,
}

struct RevisionLease {
    core: Arc<RuntimeCore>,
    revision: Revision,
}

impl Drop for RevisionLease {
    fn drop(&mut self) {
        let mut revisions = lock(&self.core.revisions);
        let entry = revisions
            .entries
            .get_mut(&self.revision.id)
            .filter(|entry| entry.revision == self.revision)
            .expect("active request pins its published revision");
        entry.active_requests -= 1;
        self.core.enforce_revision_retention(&mut revisions);
    }
}

#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug, Default)]
struct TestEvents {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl QueryRuntime {
    /// Creates a runtime with one shared structured concurrency budget.
    pub fn new(max_concurrency: usize) -> Self {
        assert!(max_concurrency > 0, "query concurrency must be nonzero");
        Self {
            core: Arc::new(RuntimeCore {
                identity: NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed),
                permits: PermitBudget::new(max_concurrency),
                wait_graph: Mutex::new(BTreeMap::new()),
                family_names: Mutex::new(BTreeSet::new()),
                revisions: Mutex::new(RevisionStore {
                    entries: BTreeMap::new(),
                    retired_through: 0,
                }),
                nodes: Mutex::new(BTreeMap::new()),
                next_task: AtomicU64::new(1),
                next_family: AtomicU64::new(1),
                next_node: AtomicU64::new(1),
                metrics: Metrics::default(),
                #[cfg(test)]
                test_events: TestEvents::default(),
            }),
        }
    }

    /// Creates a typed family with deterministic FIFO terminal retention.
    pub fn family<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Eq + Send + Sync + 'static,
    {
        self.family_with_equality(stable_name, retention_limit, PartialEq::eq)
    }

    /// Creates a typed family with family-owned canonical value equality.
    ///
    /// Compiler families use this when their retained success values do not
    /// implement blanket `Eq`, or when only a canonical projection participates
    /// in red/green publication.
    pub fn family_with_equality<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_with_optional_evaluator(stable_name, retention_limit, value_equal, None)
    }

    /// Creates a typed family with a canonical family-owned evaluator.
    ///
    /// Registered evaluators can be demanded by dependency validation from an
    /// exact retained key. They therefore support root-only red propagation;
    /// no per-request `FnOnce` is retained or reconstructed.
    pub fn family_with_evaluator<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Eq + Send + Sync + 'static,
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

    /// Creates a typed family with family-owned equality and evaluator.
    pub fn family_with_equality_and_evaluator<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        evaluator: E,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        E: Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
            + Send
            + Sync
            + 'static,
    {
        self.family_with_optional_evaluator(
            stable_name,
            retention_limit,
            value_equal,
            Some(Arc::new(evaluator)),
        )
    }

    fn family_with_optional_evaluator<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let name = stable_name.into();
        if name.is_empty() {
            return Err(FamilyError::EmptyName);
        }
        if !lock(&self.core.family_names).insert(name.clone()) {
            return Err(FamilyError::DuplicateName(name));
        }
        Ok(QueryFamily {
            core: self.core.clone(),
            inner: Arc::new(FamilyInner {
                name,
                token: FamilyToken {
                    runtime: self.core.identity,
                    family: self.core.next_family.fetch_add(1, Ordering::Relaxed),
                },
                retention_limit,
                value_equal,
                evaluator,
                nodes: Mutex::new(HashMap::new()),
                retention: Mutex::new(VecDeque::new()),
                retained_count: AtomicUsize::new(0),
                retained_nodes: AtomicUsize::new(0),
                retained_revisions: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Publishes the complete immutable leaf view for one revision.
    ///
    /// Re-publishing the same view is idempotent. A different view for an
    /// existing revision is rejected, so a pinned query can never observe a
    /// mutable input revision.
    pub fn publish_revision(
        &self,
        revision: Revision,
        inputs: impl IntoIterator<Item = (InputIdentity, u64)>,
    ) -> Result<(), RevisionError> {
        let mut exact = BTreeMap::new();
        for (input, stamp) in inputs {
            if stamp == 0 {
                return Err(RevisionError::ReservedInputStamp(input));
            }
            if let Some(previous) = exact.insert(input.clone(), stamp)
                && previous != stamp
            {
                return Err(RevisionError::ConflictingInput(input));
            }
        }
        let inputs = Arc::new(exact);
        let mut revisions = lock(&self.core.revisions);
        match revisions.entries.get(&revision.id) {
            Some(previous)
                if previous.revision == revision && previous.inputs.as_ref() == inputs.as_ref() =>
            {
                Ok(())
            }
            Some(_) => Err(RevisionError::AlreadyPublished(revision)),
            None => {
                if revision.id <= revisions.retired_through {
                    return Err(RevisionError::Retired(revision));
                }
                revisions.entries.insert(
                    revision.id,
                    RevisionEntry {
                        revision,
                        inputs,
                        active_requests: 0,
                    },
                );
                self.core.enforce_revision_retention(&mut revisions);
                Ok(())
            }
        }
    }

    /// Executes or reuses one top-level query in a newly allocated task.
    pub fn query<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        compute: F,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        self.request(family, revision, key, cancellation, compute)
            .into_result()
    }

    /// Executes one top-level request and retains its lifecycle even on abort.
    pub fn request<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        compute: F,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        self.request_with_origin(family, revision, key, cancellation, None, compute)
    }

    /// Executes a top-level request with a caller-owned provenance identity.
    ///
    /// The runtime freezes this identity into computed terminals and all
    /// lifecycle attempts, avoiding a separately retained origin registry in
    /// compatibility adapters.
    pub fn request_with_origin<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        origin_request: Option<u64>,
        compute: F,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        self.request_impl(
            family,
            revision,
            key,
            cancellation,
            origin_request,
            Some(compute),
        )
    }

    /// Executes a top-level request through a family-owned evaluator.
    pub fn request_registered<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.request_registered_with_origin(family, revision, key, cancellation, None)
    }

    /// Executes a registered top-level request with caller-owned provenance.
    pub fn request_registered_with_origin<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        origin_request: Option<u64>,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free requests require a registered evaluator"
        );
        self.request_impl::<K, V, fn(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>>(
            family,
            revision,
            key,
            cancellation,
            origin_request,
            None,
        )
    }

    fn request_impl<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        revision: Revision,
        key: K,
        cancellation: CancellationToken,
        origin_request: Option<u64>,
        compute: Option<F>,
    ) -> QueryRequestAttempt<V>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        if !Arc::ptr_eq(&self.core, &family.core) {
            return QueryRequestAttempt {
                id: 0,
                origin_request: origin_request.unwrap_or(0),
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(QueryAbort::ForeignRuntime),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                work: Arc::from([]),
                nested_attempts: Arc::from([]),
            };
        }
        let id = self.core.next_task.fetch_add(1, Ordering::Relaxed);
        let origin_request = origin_request.unwrap_or(id);
        let Some(_revision_lease) = self.core.pin_revision(revision) else {
            return QueryRequestAttempt {
                id,
                origin_request,
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(QueryAbort::UnpublishedRevision(revision)),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                work: Arc::from([]),
                nested_attempts: Arc::from([]),
            };
        };
        let task = Arc::new(Task {
            id: TaskId(id),
            core: self.core.clone(),
            revision,
            cancellation,
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
            nested_attempts: Mutex::new(Vec::new()),
        });
        let result = match compute {
            Some(compute) => family.query_task(task.clone(), key, origin_request, compute),
            None => family.query_task_registered(task.clone(), key, origin_request),
        };
        let nested_attempts: Arc<[NestedQueryAttempt]> = lock(&task.nested_attempts).clone().into();
        match result {
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => {
                let origin_request = terminal.origin_request_id();
                QueryRequestAttempt {
                    id,
                    origin_request,
                    execution,
                    dependencies: terminal.dependencies.clone(),
                    inputs: terminal.inputs.clone(),
                    work: work.into(),
                    terminal: Some(terminal),
                    abort: None,
                    nested_attempts,
                }
            }
            TaskQueryResult::Aborted {
                abort,
                dependencies,
                inputs,
                work,
            } => QueryRequestAttempt {
                id,
                origin_request,
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(abort),
                dependencies: dependencies.into(),
                inputs: inputs.into(),
                work: work.into(),
                nested_attempts,
            },
        }
    }

    /// Returns a point-in-time structural metrics snapshot.
    pub fn metrics(&self) -> RuntimeMetrics {
        let mut metrics = self.core.metrics.snapshot();
        metrics.retained_revisions = lock(&self.core.revisions).entries.len() as u64;
        metrics
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn wait_for_metrics(&self, predicate: impl Fn(RuntimeMetrics) -> bool) {
        let mut generation = lock(&self.core.test_events.generation);
        while !predicate(self.metrics()) {
            generation = wait(&self.core.test_events.changed, generation);
        }
    }
}

/// An immutable revision cannot be changed after publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionError {
    /// The revision identity is already bound to different leaf values.
    AlreadyPublished(Revision),
    /// One publication supplied two different values for the same exact leaf.
    ConflictingInput(InputIdentity),
    /// Stamp zero is reserved for a recorded absent optional leaf.
    ReservedInputStamp(InputIdentity),
    /// This publication identity is older than the bounded retired watermark.
    Retired(Revision),
}

/// Invalid family declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyError {
    /// Family names are stable identities and cannot be empty.
    EmptyName,
    /// A runtime already owns this stable family name.
    DuplicateName(Arc<str>),
}

/// A typed memo table sharing its runtime's scheduler and wait graph.
pub struct QueryFamily<K: QueryKey, V: Clone + Send + Sync + 'static> {
    core: Arc<RuntimeCore>,
    inner: Arc<FamilyInner<K, V>>,
}

/// Non-owning handle for evaluator graphs with cross-family back edges.
pub struct WeakQueryFamily<K: QueryKey, V: Clone + Send + Sync + 'static> {
    core: Weak<RuntimeCore>,
    inner: Weak<FamilyInner<K, V>>,
}

impl<K, V> Clone for WeakQueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<K, V> WeakQueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Upgrades this handle while the family remains owned by its database.
    pub fn upgrade(&self) -> Option<QueryFamily<K, V>> {
        Some(QueryFamily {
            core: self.core.upgrade()?,
            inner: self.inner.upgrade()?,
        })
    }
}

impl<K, V> fmt::Debug for QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryFamily")
            .field("name", &self.inner.name)
            .field("retention_limit", &self.inner.retention_limit)
            .finish_non_exhaustive()
    }
}

impl<K, V> Clone for QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            inner: self.inner.clone(),
        }
    }
}

struct FamilyInner<K: QueryKey, V: Clone + Send + Sync + 'static> {
    name: Arc<str>,
    token: FamilyToken,
    retention_limit: usize,
    value_equal: fn(&V, &V) -> bool,
    evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
    // Hashed typed-key memo index. Exact `K` equality is authoritative: the map
    // is keyed by the typed key itself, so hash collisions resolve through `Eq`
    // and never conflate distinct keys. Default `RandomState` (SipHash, keyed
    // per process) is used deliberately for adversarial resistance. The map is
    // unordered: eviction order lives in `retention` below (the memo index never
    // encoded eviction order), so no companion order structure is required.
    nodes: Mutex<HashMap<K, Arc<Node<K, V>>>>,
    retention: Mutex<VecDeque<RetentionEntry<K, V>>>,
    retained_count: AtomicUsize,
    retained_nodes: AtomicUsize,
    retained_revisions: Mutex<BTreeMap<Revision, usize>>,
}

impl<K, V> fmt::Debug for FamilyInner<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FamilyInner")
            .field("name", &self.name)
            .field("retention_limit", &self.retention_limit)
            .field("has_evaluator", &self.evaluator.is_some())
            .finish_non_exhaustive()
    }
}

type FamilyEvaluator<K, V> = dyn Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
    + Send
    + Sync;

struct Node<K, V> {
    /// Typed key owning this node, retained so eviction can locate the node in
    /// the hashed memo index without a linear scan.
    key: K,
    identity: NodeIdentity,
    incarnation: u64,
    users: AtomicUsize,
    wait: Arc<WaitCell>,
    demand: Option<Arc<dyn Fn(Arc<Task>, u64) -> TaskQueryResult<V> + Send + Sync>>,
    state: Mutex<NodeState<V>>,
}

impl<K, V> fmt::Debug for Node<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Node")
            .field("identity", &self.identity)
            .field("incarnation", &self.incarnation)
            .finish_non_exhaustive()
    }
}

trait ErasedNode: fmt::Debug + Send + Sync {
    fn validated_stamp(
        &self,
        core: &RuntimeCore,
        task: &Arc<Task>,
        active: &mut BTreeSet<u64>,
    ) -> Result<Option<u64>, QueryAbort>;
}

impl<K, V> ErasedNode for Node<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn validated_stamp(
        &self,
        core: &RuntimeCore,
        task: &Arc<Task>,
        active: &mut BTreeSet<u64>,
    ) -> Result<Option<u64>, QueryAbort> {
        if !active.insert(self.incarnation) {
            return Ok(None);
        }
        if let Some(demand) = &self.demand {
            let request_id = task.next_nested_request();
            let result = demand(task.clone(), request_id);
            task.record_nested(request_id, || self.identity.clone(), &result);
            active.remove(&self.incarnation);
            return match result {
                TaskQueryResult::Terminal { terminal, .. } => Ok(Some(terminal.stamp)),
                TaskQueryResult::Aborted { abort, .. } => Err(abort),
            };
        }
        let candidates = lock(&self.state)
            .attempts
            .iter()
            .rev()
            .filter_map(|attempt| match &attempt.state {
                AttemptState::Terminal { terminal, .. } => Some(terminal.clone()),
                AttemptState::Computing { .. } => None,
            })
            .collect::<Vec<_>>();
        let stamp = candidates.into_iter().try_fold(None, |stamp, terminal| {
            if stamp.is_some() {
                Ok(stamp)
            } else if core.valid_for_revision_inner(&terminal, task, active)? {
                Ok(Some(terminal.stamp))
            } else {
                Ok(None)
            }
        });
        active.remove(&self.incarnation);
        stamp
    }
}

#[derive(Debug)]
struct WaitCell {
    cv: Condvar,
}

#[derive(Debug)]
struct NodeState<V> {
    next_attempt: u64,
    next_stamp: u64,
    attempts: VecDeque<Attempt<V>>,
}

#[derive(Debug)]
struct Attempt<V> {
    id: u64,
    revision: Revision,
    state: AttemptState<V>,
}

#[derive(Debug)]
enum AttemptState<V> {
    Computing {
        owner: TaskId,
        waiters: usize,
    },
    Terminal {
        terminal: Arc<QueryTerminal<V>>,
        waiters: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FamilyToken {
    runtime: u64,
    family: u64,
}

#[derive(Debug)]
struct RetentionEntry<K, V> {
    node: Weak<Node<K, V>>,
    attempt: u64,
}

enum TaskQueryResult<V> {
    Terminal {
        terminal: Arc<QueryTerminal<V>>,
        execution: RequestExecution,
        work: Vec<(Arc<str>, u64)>,
    },
    Aborted {
        abort: QueryAbort,
        dependencies: Vec<Observation>,
        inputs: Vec<InputObservation>,
        work: Vec<(Arc<str>, u64)>,
    },
}

impl<V> TaskQueryResult<V> {
    fn into_result(self) -> Result<Arc<QueryTerminal<V>>, QueryAbort> {
        match self {
            Self::Terminal { terminal, .. } => Ok(terminal),
            Self::Aborted { abort, .. } => Err(abort),
        }
    }
}

struct NodeLease<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: Weak<FamilyInner<K, V>>,
    key: K,
    node: Arc<Node<K, V>>,
}

impl<K, V> Drop for NodeLease<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if self.node.users.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if !lock(&self.node.state).attempts.is_empty() {
            return;
        }
        let Some(family) = self.family.upgrade() else {
            return;
        };
        let mut nodes = lock(&family.nodes);
        if self.node.users.load(Ordering::Acquire) == 0
            && lock(&self.node.state).attempts.is_empty()
            && nodes
                .get(&self.key)
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &self.node))
        {
            nodes.remove(&self.key);
            family.retained_nodes.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl<K, V> QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a non-owning handle suitable for evaluator dependency graphs.
    pub fn downgrade(&self) -> WeakQueryFamily<K, V> {
        WeakQueryFamily {
            core: Arc::downgrade(&self.core),
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Returns deterministic retained-ownership gauges for this family.
    pub fn retention(&self) -> FamilyRetention {
        FamilyRetention {
            memo_nodes: self.inner.retained_nodes.load(Ordering::Relaxed),
            terminals: self.inner.retained_count.load(Ordering::Relaxed),
            terminal_limit: self.inner.retention_limit,
        }
    }

    /// Whether a currently live memo node has a key matching `predicate`.
    ///
    /// This exact O(n) Phase 1 bridge supports lifetime-coupled input
    /// identities. ADR-0063 Phase 7 replaces it for high-cardinality families.
    pub fn any_retained_key(&self, mut predicate: impl FnMut(&K) -> bool) -> bool {
        lock(&self.inner.nodes).keys().any(|key| predicate(key))
    }

    /// Caller-owned provenance identities for every retained reusable terminal.
    pub fn retained_origin_request_ids(&self) -> BTreeSet<u64> {
        let nodes = lock(&self.inner.nodes)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        nodes
            .iter()
            .flat_map(|node| {
                lock(&node.state)
                    .attempts
                    .iter()
                    .filter_map(|attempt| match &attempt.state {
                        AttemptState::Terminal { terminal, .. } => {
                            Some(terminal.origin_request_id())
                        }
                        AttemptState::Computing { .. } => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn node(&self, key: K) -> Result<NodeLease<K, V>, QueryAbort> {
        // ADR-0063 Phase 7 hashed memo index. Lookup is O(1) expected on the
        // typed key's `Hash`, but exact `K` equality remains authoritative:
        // `HashMap<K, _>` resolves any hash collision through `Eq`, so distinct
        // keys that hash alike still map to distinct nodes. Display identity is
        // never consulted for lookup.
        let mut nodes = lock(&self.inner.nodes);
        let node = if let Some(node) = nodes.get(&key) {
            node.clone()
        } else {
            let stable_key: Arc<str> = key.stable_identity().into();
            let incarnation = self.core.next_node.fetch_add(1, Ordering::Relaxed);
            let demand = self.inner.evaluator.as_ref().map(|_| {
                let core = self.core.clone();
                let family = Arc::downgrade(&self.inner);
                let key = key.clone();
                Arc::new(move |task: Arc<Task>, origin_request: u64| {
                    let Some(inner) = family.upgrade() else {
                        return TaskQueryResult::Aborted {
                            abort: QueryAbort::ForeignRuntime,
                            dependencies: Vec::new(),
                            inputs: Vec::new(),
                            work: Vec::new(),
                        };
                    };
                    QueryFamily {
                        core: core.clone(),
                        inner,
                    }
                    .query_task_registered_for_validation(
                        task,
                        key.clone(),
                        origin_request,
                    )
                })
                    as Arc<dyn Fn(Arc<Task>, u64) -> TaskQueryResult<V> + Send + Sync>
            });
            let node = Arc::new(Node {
                key: key.clone(),
                identity: NodeIdentity {
                    family: self.inner.name.clone(),
                    key: stable_key,
                },
                incarnation,
                users: AtomicUsize::new(0),
                wait: Arc::new(WaitCell { cv: Condvar::new() }),
                demand,
                state: Mutex::new(NodeState {
                    next_attempt: 1,
                    next_stamp: 1,
                    attempts: VecDeque::new(),
                }),
            });
            let erased: Arc<dyn ErasedNode> = node.clone();
            let mut registry = lock(&self.core.nodes);
            registry.retain(|_, node| node.strong_count() > 0);
            registry.insert(incarnation, Arc::downgrade(&erased));
            nodes.insert(key.clone(), node.clone());
            self.inner.retained_nodes.fetch_add(1, Ordering::Relaxed);
            node
        };
        node.users.fetch_add(1, Ordering::AcqRel);
        Ok(NodeLease {
            family: Arc::downgrade(&self.inner),
            key,
            node,
        })
    }

    fn query_task<F>(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
        compute: F,
    ) -> TaskQueryResult<V>
    where
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        assert!(
            self.inner.evaluator.is_none(),
            "registered families must use the closure-free request API"
        );
        self.query_task_impl(task, key, origin_request, Some(compute), true)
    }

    fn query_task_registered(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
    ) -> TaskQueryResult<V> {
        self.query_task_impl::<fn(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>>(
            task,
            key,
            origin_request,
            None,
            true,
        )
    }

    fn query_task_registered_for_validation(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
    ) -> TaskQueryResult<V> {
        self.query_task_impl::<fn(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>>(
            task,
            key,
            origin_request,
            None,
            false,
        )
    }

    fn query_task_impl<F>(
        &self,
        task: Arc<Task>,
        key: K,
        origin_request: u64,
        mut compute: Option<F>,
        observe_result: bool,
    ) -> TaskQueryResult<V>
    where
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        let lease = match self.node(key) {
            Ok(lease) => lease,
            Err(abort) => {
                return TaskQueryResult::Aborted {
                    abort,
                    dependencies: Vec::new(),
                    inputs: Vec::new(),
                    work: Vec::new(),
                };
            }
        };
        let node = &lease.node;
        let exact_node = ExactNodeIdentity {
            display: node.identity.clone(),
            incarnation: node.incarnation,
        };
        if let Some(cycle) = task.stack_cycle(&exact_node) {
            self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
            return TaskQueryResult::Aborted {
                abort: QueryAbort::Cycle(cycle),
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            };
        }
        loop {
            if task.cancellation.is_canceled() {
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: Vec::new(),
                    inputs: Vec::new(),
                    work: Vec::new(),
                };
            }

            enum Action {
                Join { attempt: u64, owner: TaskId },
                Compute { attempt: u64 },
            }

            let candidates = lock(&node.state)
                .attempts
                .iter()
                .rev()
                .filter_map(|attempt| match &attempt.state {
                    AttemptState::Terminal { terminal, .. } => Some(terminal.clone()),
                    AttemptState::Computing { .. } => None,
                })
                .collect::<Vec<_>>();
            for terminal in candidates {
                match self.core.valid_for_revision(&terminal, &task) {
                    Ok(true) => {
                        self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
                        if observe_result {
                            task.observe(&terminal);
                        }
                        return TaskQueryResult::Terminal {
                            terminal,
                            execution: RequestExecution::Reused,
                            work: Vec::new(),
                        };
                    }
                    Ok(false) => {}
                    Err(abort) => {
                        return TaskQueryResult::Aborted {
                            abort,
                            dependencies: Vec::new(),
                            inputs: Vec::new(),
                            work: Vec::new(),
                        };
                    }
                }
            }

            let action = {
                let mut state = lock(&node.state);
                let mut action = None;
                for attempt in state.attempts.iter_mut().rev() {
                    match &mut attempt.state {
                        AttemptState::Terminal { .. } => {}
                        AttemptState::Computing { owner, waiters } => {
                            // A body has not yet frozen its observed leaves, so
                            // only the identical pinned revision may join it.
                            if attempt.revision == task.revision {
                                *waiters += 1;
                                action = Some(Action::Join {
                                    attempt: attempt.id,
                                    owner: *owner,
                                });
                            }
                        }
                    }
                    if action.is_some() {
                        break;
                    }
                }
                action.unwrap_or_else(|| {
                    let attempt = state.next_attempt;
                    state.next_attempt += 1;
                    state.attempts.push_back(Attempt {
                        id: attempt,
                        revision: task.revision,
                        state: AttemptState::Computing {
                            owner: task.id,
                            waiters: 0,
                        },
                    });
                    Action::Compute { attempt }
                })
            };

            match action {
                Action::Join { attempt, owner } => {
                    self.core.metrics.joins.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    self.core.test_changed();
                    match self.join(&task, node, attempt, owner) {
                        Err(abort) => {
                            return TaskQueryResult::Aborted {
                                abort,
                                dependencies: Vec::new(),
                                inputs: Vec::new(),
                                work: Vec::new(),
                            };
                        }
                        Ok(Some(terminal)) => {
                            if observe_result {
                                task.observe(&terminal);
                            }
                            return TaskQueryResult::Terminal {
                                terminal,
                                execution: RequestExecution::Joined,
                                work: Vec::new(),
                            };
                        }
                        Ok(None) => continue,
                    }
                }
                Action::Compute { attempt } => {
                    self.core.metrics.claims.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    self.core.test_changed();
                    let acquired_here = task.acquire_permit(&self.core);
                    task.push(exact_node.clone());
                    self.core.metrics.body_entered();
                    let context = QueryContext {
                        task: task.clone(),
                        not_send_or_sync: PhantomData,
                    };
                    let body = catch_unwind(AssertUnwindSafe(|| match &self.inner.evaluator {
                        Some(evaluator) => evaluator(&context, self, &lease.key),
                        None => compute.take().expect("query body executes at most once")(&context),
                    }));
                    self.core.metrics.body_left();
                    self.core
                        .metrics
                        .body_completions
                        .fetch_add(1, Ordering::Relaxed);
                    let (dependencies, inputs, work_prefix) = task.pop(&exact_node);

                    let result = match body {
                        Ok(result) if !task.cancellation.is_canceled() => result,
                        Ok(_) => Err(QueryAbort::Canceled),
                        Err(payload) => {
                            self.abort_attempt(node, attempt);
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            resume_unwind(payload)
                        }
                    };

                    match result {
                        Ok(output) => {
                            let terminal = self.publish(
                                node,
                                attempt,
                                task.revision,
                                origin_request,
                                output,
                                dependencies,
                                inputs,
                            );
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            let mut work = work_prefix;
                            work.extend(terminal.work().iter().cloned());
                            let work = canonical_reduced_work(work);
                            if observe_result {
                                task.observe(&terminal);
                            }
                            task.observe_work(&work);
                            return TaskQueryResult::Terminal {
                                terminal,
                                execution: RequestExecution::Computed,
                                work,
                            };
                        }
                        Err(abort) => {
                            self.abort_attempt(node, attempt);
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            if matches!(abort, QueryAbort::Canceled) {
                                self.core
                                    .metrics
                                    .cancellations
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            task.observe_abort_prefix(&dependencies, &inputs, &work_prefix);
                            return TaskQueryResult::Aborted {
                                abort,
                                dependencies,
                                inputs,
                                work: work_prefix,
                            };
                        }
                    }
                }
            }
        }
    }

    fn join(
        &self,
        task: &Arc<Task>,
        node: &Arc<Node<K, V>>,
        attempt_id: u64,
        owner: TaskId,
    ) -> Result<Option<Arc<QueryTerminal<V>>>, QueryAbort> {
        let mut state = lock(&node.state);
        if task.cancellation.is_canceled() {
            decrement_waiter(&mut state, attempt_id);
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Canceled);
        }
        let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) else {
            return Ok(None);
        };
        match &mut attempt.state {
            AttemptState::Terminal { terminal, waiters } => {
                let terminal = terminal.clone();
                *waiters -= 1;
                drop(state);
                self.enforce_retention();
                return Ok(Some(terminal));
            }
            AttemptState::Computing {
                owner: actual_owner,
                ..
            } => assert_eq!(*actual_owner, owner),
        }
        if let Err(cycle) = self.core.begin_wait(
            task.id,
            owner,
            ExactNodeIdentity {
                display: node.identity.clone(),
                incarnation: node.incarnation,
            },
        ) {
            decrement_waiter(&mut state, attempt_id);
            self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Cycle(cycle));
        }
        task.cancellation.watch(&node.wait);
        let donated = task.release_permit(&self.core);
        if donated {
            self.core
                .metrics
                .donated_permits
                .fetch_add(1, Ordering::Relaxed);
        }
        let result = loop {
            if task.cancellation.is_canceled() {
                decrement_waiter(&mut state, attempt_id);
                break Err(QueryAbort::Canceled);
            }
            let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) else {
                break Ok(None);
            };
            match &mut attempt.state {
                AttemptState::Computing { .. } => {
                    state = wait(&node.wait.cv, state);
                }
                AttemptState::Terminal { terminal, waiters } => {
                    let terminal = terminal.clone();
                    *waiters -= 1;
                    break Ok(Some(terminal));
                }
            }
        };
        drop(state);
        self.core.end_wait(task.id);
        if donated {
            task.acquire_permit(&self.core);
        }
        if matches!(result, Err(QueryAbort::Canceled)) {
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
        }
        self.enforce_retention();
        result
    }

    fn abort_attempt(&self, node: &Arc<Node<K, V>>, attempt_id: u64) {
        let mut state = lock(&node.state);
        if let Some(index) = state.attempts.iter().position(|item| item.id == attempt_id) {
            state.attempts.remove(index);
        }
        drop(state);
        node.wait.cv.notify_all();
    }

    fn publish(
        &self,
        node: &Arc<Node<K, V>>,
        attempt_id: u64,
        revision: Revision,
        origin_request: u64,
        output: QueryOutput<V>,
        dependencies: Vec<Observation>,
        inputs: Vec<InputObservation>,
    ) -> Arc<QueryTerminal<V>> {
        let diagnostics = canonical_diagnostics(output.diagnostics);
        let work = canonical_work(output.work);
        let mut state = lock(&node.state);
        let previous = state
            .attempts
            .iter()
            .rev()
            .find_map(|attempt| match &attempt.state {
                AttemptState::Terminal { terminal, .. } => Some(terminal.clone()),
                AttemptState::Computing { .. } => None,
            });
        let red = previous.as_ref().is_some_and(|terminal| {
            terminal.kind == output.kind
                && outcomes_equal(self.inner.value_equal, &terminal.outcome, &output.outcome)
                && semantic_diagnostics_equal(&terminal.diagnostics, &diagnostics)
        });
        let stamp = if red {
            previous.expect("red publication has a predecessor").stamp
        } else {
            let stamp = state.next_stamp;
            state.next_stamp += 1;
            stamp
        };
        let terminal = Arc::new(QueryTerminal {
            family_token: self.inner.token,
            node: node.identity.clone(),
            node_incarnation: node.incarnation,
            revision,
            stamp,
            origin_request,
            outcome: output.outcome,
            kind: output.kind,
            diagnostics: diagnostics.into(),
            work: work.into(),
            dependencies: dependencies.into(),
            inputs: inputs.into(),
            pins: AtomicUsize::new(0),
        });
        let attempt = state
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == attempt_id)
            .expect("only the claiming task may publish its attempt");
        let waiters = match &attempt.state {
            AttemptState::Computing { waiters, .. } => *waiters,
            AttemptState::Terminal { .. } => panic!("only a computing attempt may publish"),
        };
        attempt.state = AttemptState::Terminal {
            terminal: terminal.clone(),
            waiters,
        };
        drop(state);

        if red {
            self.core
                .metrics
                .red_publications
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.core
                .metrics
                .green_publications
                .fetch_add(1, Ordering::Relaxed);
        }
        self.core
            .metrics
            .retained_terminals
            .fetch_add(1, Ordering::Relaxed);
        self.inner.retained_count.fetch_add(1, Ordering::Relaxed);
        lock(&self.inner.retention).push_back(RetentionEntry {
            node: Arc::downgrade(node),
            attempt: attempt_id,
        });
        node.wait.cv.notify_all();
        self.enforce_retention();
        terminal
    }

    fn enforce_retention(&self) {
        let mut retention = lock(&self.inner.retention);
        let mut remaining = retention.len();
        while self.inner.retained_count.load(Ordering::Relaxed) > self.inner.retention_limit
            && remaining > 0
        {
            remaining -= 1;
            let entry = retention.pop_front().expect("retention scan is nonempty");
            let Some(node) = entry.node.upgrade() else {
                continue;
            };
            let mut state = lock(&node.state);
            let Some(index) = state
                .attempts
                .iter()
                .position(|item| item.id == entry.attempt)
            else {
                continue;
            };
            let protected = match &state.attempts[index].state {
                AttemptState::Computing { .. } => true,
                AttemptState::Terminal { terminal, waiters } => {
                    *waiters > 0
                        || terminal.pins.load(Ordering::Acquire) > 0
                        || lock(&self.inner.retained_revisions).contains_key(&terminal.revision)
                }
            };
            if protected {
                drop(state);
                retention.push_back(entry);
                continue;
            }
            state.attempts.remove(index);
            let empty = state.attempts.is_empty();
            drop(state);
            self.core.metrics.evictions.fetch_add(1, Ordering::Relaxed);
            self.core
                .metrics
                .retained_terminals
                .fetch_sub(1, Ordering::Relaxed);
            self.inner.retained_count.fetch_sub(1, Ordering::Relaxed);
            if empty && node.users.load(Ordering::Acquire) == 0 {
                // Reverse-locate the node by its owned typed key (O(1)); the
                // `ptr_eq` guard ensures a newer incarnation reinserted under
                // the same key is never evicted in its place.
                let mut nodes = lock(&self.inner.nodes);
                if node.users.load(Ordering::Acquire) == 0
                    && lock(&node.state).attempts.is_empty()
                    && nodes
                        .get(&node.key)
                        .is_some_and(|candidate| Arc::ptr_eq(candidate, &node))
                {
                    nodes.remove(&node.key);
                    self.inner.retained_nodes.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Pins a retained terminal against eviction.
    pub fn pin_terminal(
        &self,
        terminal: &Arc<QueryTerminal<V>>,
    ) -> Result<TerminalPin<K, V>, PinError> {
        if terminal.family_token != self.inner.token {
            return Err(PinError::ForeignFamily);
        }
        terminal.pins.fetch_add(1, Ordering::AcqRel);
        Ok(TerminalPin {
            family: self.clone(),
            terminal: terminal.clone(),
        })
    }

    /// Pins terminals computed under this exact retained revision.
    pub fn retain_revision(&self, revision: Revision) -> RevisionPin<K, V> {
        *lock(&self.inner.retained_revisions)
            .entry(revision)
            .or_default() += 1;
        RevisionPin {
            family: self.clone(),
            revision,
            view: self.core.pin_revision(revision),
        }
    }

    /// Creates request/session-owned current and last-good publication roots.
    pub fn selection(&self) -> QuerySelection<K, V> {
        QuerySelection {
            family: self.clone(),
            current: None,
            last_good: None,
        }
    }
}

/// An explicit retained terminal root.
pub struct TerminalPin<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    terminal: Arc<QueryTerminal<V>>,
}

impl<K, V> TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// The immutable terminal protected by this root.
    pub fn terminal(&self) -> &Arc<QueryTerminal<V>> {
        &self.terminal
    }
}

/// A terminal cannot be pinned by a different family or runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinError {
    /// The terminal's unforgeable family token does not match.
    ForeignFamily,
}

impl<K, V> Drop for TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.terminal.pins.fetch_sub(1, Ordering::AcqRel);
        self.family.enforce_retention();
    }
}

/// An explicit current/last-good revision root.
pub struct RevisionPin<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    revision: Revision,
    view: Option<RevisionLease>,
}

impl<K, V> Drop for RevisionPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let mut revisions = lock(&self.family.inner.retained_revisions);
        let count = revisions
            .get_mut(&self.revision)
            .expect("revision pin owns a retained root");
        *count -= 1;
        if *count == 0 {
            revisions.remove(&self.revision);
        }
        drop(revisions);
        self.family.enforce_retention();
        // The revision-view lease drops after terminal retention bookkeeping.
        let _ = &self.view;
    }
}

/// Request/session publication over immutable terminal attempts.
///
/// This deliberately lives above memo nodes. Selecting a failed current
/// attempt preserves the preceding successful terminal as last-good.
pub struct QuerySelection<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    current: Option<TerminalPin<K, V>>,
    last_good: Option<TerminalPin<K, V>>,
}

impl<K, V> QuerySelection<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Publishes one immutable attempt as the request's current result.
    pub fn publish(&mut self, terminal: &Arc<QueryTerminal<V>>) -> Result<(), PinError> {
        let current = self.family.pin_terminal(terminal)?;
        if terminal.kind() == QueryTerminalKind::Success {
            self.last_good = Some(self.family.pin_terminal(terminal)?);
        }
        self.current = Some(current);
        Ok(())
    }

    /// Current selected attempt, including a deterministic failure.
    pub fn current(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.current.as_ref().map(TerminalPin::terminal)
    }

    /// Most recently selected successful attempt.
    pub fn last_good(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.last_good.as_ref().map(TerminalPin::terminal)
    }

    /// Clears request-current publication after a non-terminal abort while
    /// preserving the independently pinned last-good success.
    pub fn clear_current(&mut self) {
        self.current = None;
    }
}

/// Task-scoped access to nested dependency queries.
#[derive(Debug)]
pub struct QueryContext {
    task: Arc<Task>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl QueryContext {
    /// Exact immutable revision pinned by this task.
    pub fn revision(&self) -> Revision {
        self.task.revision
    }

    /// Fails cooperatively when the request is canceled.
    pub fn check_canceled(&self) -> Result<(), QueryAbort> {
        if self.task.cancellation.is_canceled() {
            Err(QueryAbort::Canceled)
        } else {
            Ok(())
        }
    }

    /// Reads and records one exact leaf from this task's pinned revision.
    pub fn input(&self, input: InputIdentity) -> Result<u64, QueryAbort> {
        let stamp = self
            .task
            .core
            .revision_input(self.task.revision, &input)
            .ok_or_else(|| QueryAbort::MissingInput(input.clone()))?;
        self.task.observe_input(input, stamp);
        Ok(stamp)
    }

    /// Reads and records an optional exact leaf from this task's revision.
    ///
    /// Absence is recorded as a negative observation. A successor which adds
    /// the leaf therefore invalidates the terminal without turning absence
    /// into a query failure. External-input coordinators use this to publish a
    /// typed demand terminal; speculative work can instead park on the same
    /// negative observation without emitting a host request.
    pub fn optional_input(&self, input: InputIdentity) -> Option<u64> {
        let stamp = self.task.core.revision_input(self.task.revision, &input);
        self.task.observe_input(input, stamp.unwrap_or(0));
        stamp
    }

    /// Records structural work as it is completed by the active body.
    ///
    /// Unlike terminal-attached output work, this prefix survives cancellation
    /// and deterministic aborts and is owned by the runtime attempt.
    pub fn record_work(&self, item: WorkItem) {
        self.task.record_work(item);
    }

    /// Requests a dependency in the same task and pinned revision.
    pub fn query<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
        compute: F,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        let request_id = self.task.next_nested_request();
        let result = if Arc::ptr_eq(&self.task_runtime(), &family.core) {
            family.query_task(self.task.clone(), key.clone(), request_id, compute)
        } else {
            TaskQueryResult::Aborted {
                abort: QueryAbort::ForeignRuntime,
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            }
        };
        // Lazy display identity: the hot path reuses the terminal's identity in
        // `record_nested`; the key is only formatted if this request aborted.
        self.task.record_nested(
            request_id,
            move || NodeIdentity {
                family: family.inner.name.clone(),
                key: key.stable_identity().into(),
            },
            &result,
        );
        result.into_result()
    }

    /// Requests a dependency through its canonical family-owned evaluator.
    pub fn query_registered<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free dependency requests require a registered evaluator"
        );
        let request_id = self.task.next_nested_request();
        let result = if Arc::ptr_eq(&self.task_runtime(), &family.core) {
            family.query_task_registered(self.task.clone(), key.clone(), request_id)
        } else {
            TaskQueryResult::Aborted {
                abort: QueryAbort::ForeignRuntime,
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            }
        };
        // Lazy display identity: the hot path reuses the terminal's identity in
        // `record_nested`; the key is only formatted if this request aborted.
        self.task.record_nested(
            request_id,
            move || NodeIdentity {
                family: family.inner.name.clone(),
                key: key.stable_identity().into(),
            },
            &result,
        );
        result.into_result()
    }

    fn task_runtime(&self) -> Arc<RuntimeCore> {
        self.task.core.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TaskId(u64);

#[derive(Debug)]
struct Task {
    id: TaskId,
    core: Arc<RuntimeCore>,
    revision: Revision,
    cancellation: CancellationToken,
    owns_permit: AtomicBool,
    stack: Mutex<Vec<TaskFrame>>,
    nested_attempts: Mutex<Vec<NestedQueryAttempt>>,
}

#[derive(Debug)]
struct TaskFrame {
    node: ExactNodeIdentity,
    dependencies: BTreeMap<ExactNodeIdentity, u64>,
    inputs: BTreeMap<InputIdentity, u64>,
    work: BTreeMap<Arc<str>, u64>,
}

impl Task {
    fn next_nested_request(&self) -> u64 {
        self.core.next_task.fetch_add(1, Ordering::Relaxed)
    }

    /// Records a nested request's lifecycle. The display identity is materialized
    /// lazily: the hot memo-hit/compute path reuses the terminal's already-built
    /// `NodeIdentity`, so no `stable_identity()` is formatted per request. Only
    /// the cold abort branch invokes `fallback_node`, which formats the key.
    fn record_nested<V>(
        &self,
        id: u64,
        fallback_node: impl FnOnce() -> NodeIdentity,
        result: &TaskQueryResult<V>,
    ) {
        let attempt = match result {
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => NestedQueryAttempt {
                id,
                node: terminal.node.clone(),
                node_incarnation: Some(terminal.node_incarnation),
                origin_request: terminal.origin_request_id(),
                execution: *execution,
                terminal_revision: Some(terminal.revision()),
                terminal_stamp: Some(terminal.stamp()),
                abort: None,
                dependencies: terminal.dependencies.clone(),
                inputs: terminal.inputs.clone(),
                work: work.clone().into(),
            },
            TaskQueryResult::Aborted {
                abort,
                dependencies,
                inputs,
                work,
            } => NestedQueryAttempt {
                id,
                node: fallback_node(),
                node_incarnation: None,
                origin_request: id,
                execution: RequestExecution::Aborted,
                terminal_revision: None,
                terminal_stamp: None,
                abort: Some(abort.clone()),
                dependencies: dependencies.clone().into(),
                inputs: inputs.clone().into(),
                work: work.clone().into(),
            },
        };
        lock(&self.nested_attempts).push(attempt);
    }

    fn acquire_permit(&self, core: &Arc<RuntimeCore>) -> bool {
        if self.owns_permit.load(Ordering::Acquire) {
            return false;
        }
        core.permits.acquire();
        assert!(!self.owns_permit.swap(true, Ordering::AcqRel));
        true
    }

    fn release_permit(&self, core: &Arc<RuntimeCore>) -> bool {
        if !self.owns_permit.swap(false, Ordering::AcqRel) {
            return false;
        }
        core.permits.release();
        true
    }

    fn push(&self, node: ExactNodeIdentity) {
        lock(&self.stack).push(TaskFrame {
            node,
            dependencies: BTreeMap::new(),
            inputs: BTreeMap::new(),
            work: BTreeMap::new(),
        });
    }

    fn pop(
        &self,
        expected: &ExactNodeIdentity,
    ) -> (
        Vec<Observation>,
        Vec<InputObservation>,
        Vec<(Arc<str>, u64)>,
    ) {
        let frame = lock(&self.stack)
            .pop()
            .expect("query computation owns one dependency frame");
        assert_eq!(&frame.node, expected);
        let dependencies = frame
            .dependencies
            .into_iter()
            .map(|(node, stamp)| Observation {
                node: node.display,
                incarnation: node.incarnation,
                stamp,
            })
            .collect();
        let inputs = frame
            .inputs
            .into_iter()
            .map(|(input, stamp)| InputObservation { input, stamp })
            .collect();
        (dependencies, inputs, frame.work.into_iter().collect())
    }

    fn observe<V>(&self, terminal: &QueryTerminal<V>) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            frame.dependencies.insert(
                ExactNodeIdentity {
                    display: terminal.node.clone(),
                    incarnation: terminal.node_incarnation,
                },
                terminal.stamp,
            );
        }
    }

    fn observe_work(&self, work: &[(Arc<str>, u64)]) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            for (identity, amount) in work {
                *frame.work.entry(identity.clone()).or_default() += amount;
            }
        }
    }

    fn record_work(&self, item: WorkItem) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("work recording occurs only inside a query computation");
        *frame.work.entry(item.identity).or_default() += item.amount;
    }

    fn observe_abort_prefix(
        &self,
        dependencies: &[Observation],
        inputs: &[InputObservation],
        work: &[(Arc<str>, u64)],
    ) {
        let mut stack = lock(&self.stack);
        let Some(frame) = stack.last_mut() else {
            return;
        };
        for dependency in dependencies {
            frame.dependencies.insert(
                ExactNodeIdentity {
                    display: dependency.node.clone(),
                    incarnation: dependency.incarnation,
                },
                dependency.stamp,
            );
        }
        for input in inputs {
            if let Some(previous) = frame.inputs.insert(input.input.clone(), input.stamp) {
                assert_eq!(previous, input.stamp);
            }
        }
        for (identity, amount) in work {
            *frame.work.entry(identity.clone()).or_default() += amount;
        }
    }

    fn observe_input(&self, input: InputIdentity, stamp: u64) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("input reads occur only inside a query computation");
        if let Some(previous) = frame.inputs.insert(input, stamp) {
            assert_eq!(previous, stamp);
        }
    }

    fn stack_cycle(&self, node: &ExactNodeIdentity) -> Option<Arc<[NodeIdentity]>> {
        let stack = lock(&self.stack);
        let start = stack.iter().position(|frame| &frame.node == node)?;
        Some(canonical_cycle(
            stack[start..]
                .iter()
                .map(|frame| frame.node.display.clone())
                .chain(std::iter::once(node.display.clone())),
        ))
    }
}

#[derive(Debug)]
struct WaitEdge {
    owner: TaskId,
    node: ExactNodeIdentity,
}

impl RuntimeCore {
    fn revision_input(&self, revision: Revision, input: &InputIdentity) -> Option<u64> {
        lock(&self.revisions)
            .entries
            .get(&revision.id)
            .filter(|entry| entry.revision == revision)
            .and_then(|entry| entry.inputs.get(input).copied())
    }

    fn valid_for_revision<V>(
        &self,
        terminal: &QueryTerminal<V>,
        task: &Arc<Task>,
    ) -> Result<bool, QueryAbort> {
        self.valid_for_revision_inner(terminal, task, &mut BTreeSet::new())
    }

    fn valid_for_revision_inner<V>(
        &self,
        terminal: &QueryTerminal<V>,
        task: &Arc<Task>,
        active: &mut BTreeSet<u64>,
    ) -> Result<bool, QueryAbort> {
        if !terminal.revision.is_compatible_with(task.revision) {
            return Ok(false);
        }
        // Compatibility tokens are only a scheduling hint. Direct inputs are
        // checked exactly, while dependency stamps are validated recursively
        // against the current compatible terminal of the exact child node.
        let revisions = lock(&self.revisions);
        let Some(entry) = revisions
            .entries
            .get(&task.revision.id)
            .filter(|entry| entry.revision == task.revision)
        else {
            return Ok(false);
        };
        let direct_inputs_valid = terminal.inputs.iter().all(|observed| {
            if observed.stamp == 0 {
                !entry.inputs.contains_key(&observed.input)
            } else {
                entry.inputs.get(&observed.input) == Some(&observed.stamp)
            }
        });
        drop(revisions);
        if !direct_inputs_valid {
            return Ok(false);
        }
        for observed in terminal.dependencies.iter() {
            let node = lock(&self.nodes)
                .get(&observed.incarnation)
                .and_then(Weak::upgrade);
            let stamp = match node {
                Some(node) => match node.validated_stamp(self, task, active) {
                    Ok(stamp) => stamp,
                    // A registered descendant can depend on an externally
                    // supplied query body. If that body is unavailable while
                    // recursively validating an ancestor, the ancestor is
                    // dirty rather than canceled: its caller-supplied body may
                    // schedule or provide the missing descendant. An explicit
                    // cancellation token still aborts the whole request.
                    Err(QueryAbort::Canceled) if !task.cancellation.is_canceled() => {
                        return Ok(false);
                    }
                    Err(abort) => return Err(abort),
                },
                None => None,
            };
            if stamp != Some(observed.stamp) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn pin_revision(self: &Arc<Self>, revision: Revision) -> Option<RevisionLease> {
        let mut revisions = lock(&self.revisions);
        let entry = revisions
            .entries
            .get_mut(&revision.id)
            .filter(|entry| entry.revision == revision)?;
        entry.active_requests += 1;
        Some(RevisionLease {
            core: self.clone(),
            revision,
        })
    }

    fn enforce_revision_retention(&self, revisions: &mut RevisionStore) {
        while revisions.entries.len() > REVISION_RETENTION_LIMIT {
            let Some(id) = revisions
                .entries
                .iter()
                .find_map(|(id, entry)| (entry.active_requests == 0).then_some(*id))
            else {
                break;
            };
            revisions.entries.remove(&id);
            revisions.retired_through = revisions.retired_through.max(id);
        }
    }

    #[cfg(test)]
    fn test_changed(&self) {
        *lock(&self.test_events.generation) += 1;
        self.test_events.changed.notify_all();
    }

    fn begin_wait(
        &self,
        waiter: TaskId,
        owner: TaskId,
        node: ExactNodeIdentity,
    ) -> Result<(), Arc<[NodeIdentity]>> {
        let mut graph = lock(&self.wait_graph);
        graph.insert(waiter, WaitEdge { owner, node });
        let mut cursor = owner;
        let mut nodes = Vec::new();
        while let Some(edge) = graph.get(&cursor) {
            nodes.push(edge.node.display.clone());
            cursor = edge.owner;
            if cursor == waiter {
                nodes.push(
                    graph
                        .get(&waiter)
                        .expect("new wait edge is present")
                        .node
                        .display
                        .clone(),
                );
                graph.remove(&waiter);
                return Err(canonical_cycle(nodes));
            }
        }
        Ok(())
    }

    fn end_wait(&self, waiter: TaskId) {
        lock(&self.wait_graph).remove(&waiter);
    }
}

#[derive(Debug)]
struct PermitBudget {
    maximum: usize,
    used: Mutex<usize>,
    available: Condvar,
}

impl PermitBudget {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            used: Mutex::new(0),
            available: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut used = lock(&self.used);
        while *used == self.maximum {
            used = wait(&self.available, used);
        }
        *used += 1;
    }

    fn release(&self) {
        let mut used = lock(&self.used);
        assert!(*used > 0, "cannot release an unowned query permit");
        *used -= 1;
        drop(used);
        self.available.notify_one();
    }
}

fn decrement_waiter<V>(state: &mut NodeState<V>, attempt_id: u64) {
    if let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) {
        match &mut attempt.state {
            AttemptState::Computing { waiters, .. } | AttemptState::Terminal { waiters, .. } => {
                *waiters -= 1
            }
        }
    }
}

fn canonical_diagnostics(mut diagnostics: Vec<QueryDiagnostic>) -> Vec<QueryDiagnostic> {
    diagnostics.sort_by(|left, right| {
        (&left.identity, &left.payload, &left.presentation).cmp(&(
            &right.identity,
            &right.payload,
            &right.presentation,
        ))
    });
    diagnostics
}

fn semantic_diagnostics_equal(left: &[QueryDiagnostic], right: &[QueryDiagnostic]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.identity == right.identity && left.payload == right.payload)
}

fn outcomes_equal<V>(
    value_equal: fn(&V, &V) -> bool,
    left: &QueryOutcome<V>,
    right: &QueryOutcome<V>,
) -> bool {
    match (left, right) {
        (QueryOutcome::Success(left), QueryOutcome::Success(right)) => value_equal(left, right),
        (QueryOutcome::Failure(left), QueryOutcome::Failure(right)) => left == right,
        (QueryOutcome::Success(_), QueryOutcome::Failure(_))
        | (QueryOutcome::Failure(_), QueryOutcome::Success(_)) => false,
    }
}

fn canonical_work(work: Vec<WorkItem>) -> Vec<(Arc<str>, u64)> {
    let mut aggregate = BTreeMap::<Arc<str>, u64>::new();
    for item in work {
        *aggregate.entry(item.identity).or_default() += item.amount;
    }
    aggregate.into_iter().collect()
}

fn canonical_reduced_work(work: Vec<(Arc<str>, u64)>) -> Vec<(Arc<str>, u64)> {
    let mut aggregate = BTreeMap::<Arc<str>, u64>::new();
    for (identity, amount) in work {
        *aggregate.entry(identity).or_default() += amount;
    }
    aggregate.into_iter().collect()
}

fn canonical_cycle(nodes: impl IntoIterator<Item = NodeIdentity>) -> Arc<[NodeIdentity]> {
    let mut nodes = nodes.into_iter().collect::<Vec<_>>();
    nodes.sort();
    nodes.dedup();
    nodes.into()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct Key(&'static str);

    impl QueryKey for Key {
        fn stable_identity(&self) -> String {
            self.0.to_owned()
        }
    }

    fn revision(id: u64) -> Revision {
        Revision::new(id, id)
    }

    // Deterministic single-hasher probe used only to demonstrate that two keys
    // land in the same bucket. The live memo map keys on std `RandomState`
    // (SipHash); this fixed-seed `DefaultHasher` just makes the collision
    // assertion reproducible.
    fn hash_of<K: std::hash::Hash>(key: &K) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn publish_empty(runtime: &QueryRuntime, revisions: impl IntoIterator<Item = Revision>) {
        for revision in revisions {
            runtime.publish_revision(revision, []).unwrap();
        }
    }

    #[test]
    fn compatible_exact_key_is_computed_once_and_joined_by_many_waiters() {
        let runtime = QueryRuntime::new(8);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("join", 16).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let owner_runtime = runtime.clone();
        let owner_family = family.clone();
        let owner = thread::spawn(move || {
            owner_runtime.query(
                &owner_family,
                revision(1),
                Key("same"),
                CancellationToken::new(),
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(42))
                },
            )
        });
        started_rx.recv().unwrap();

        let mut joiners = Vec::new();
        for _ in 0..7 {
            let runtime = runtime.clone();
            let family = family.clone();
            joiners.push(thread::spawn(move || {
                runtime.query(
                    &family,
                    revision(1),
                    Key("same"),
                    CancellationToken::new(),
                    |_| panic!("a compatible joiner must not compute"),
                )
            }));
        }
        runtime.wait_for_metrics(|metrics| metrics.joins == 7);
        finish_tx.send(()).unwrap();

        let terminal = owner.join().unwrap().unwrap();
        assert_eq!(terminal.outcome(), &QueryOutcome::Success(42));
        for joiner in joiners {
            assert!(Arc::ptr_eq(&terminal, &joiner.join().unwrap().unwrap()));
        }
        assert_eq!(runtime.metrics().claims, 1);
        assert_eq!(runtime.metrics().body_completions, 1);
    }

    #[test]
    fn different_ready_keys_execute_concurrently() {
        let runtime = QueryRuntime::new(2);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("overlap", 4).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles = [Key("a"), Key("b")].map(|key| {
            let runtime = runtime.clone();
            let family = family.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                runtime.query(&family, revision(1), key, CancellationToken::new(), |_| {
                    barrier.wait();
                    Ok(QueryOutput::success(1))
                })
            })
        });
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(runtime.metrics().peak_active_bodies, 2);
    }

    #[test]
    fn one_permit_joiner_donates_to_already_claimed_owner() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let outer = runtime.family::<Key, u64>("outer", 4).unwrap();
        let target = runtime.family::<Key, u64>("target", 4).unwrap();
        let (outer_started_tx, outer_started_rx) = mpsc::channel();
        let (enter_join_tx, enter_join_rx) = mpsc::channel();
        let (target_ran_tx, target_ran_rx) = mpsc::channel();

        let outer_runtime = runtime.clone();
        let outer_family = outer.clone();
        let outer_target = target.clone();
        let outer_thread = thread::spawn(move || {
            outer_runtime.query(
                &outer_family,
                revision(1),
                Key("outer"),
                CancellationToken::new(),
                |context| {
                    outer_started_tx.send(()).unwrap();
                    enter_join_rx.recv().unwrap();
                    let dependency = context.query(&outer_target, Key("queued"), |_| {
                        panic!("the previously claimed owner must compute")
                    })?;
                    assert_eq!(dependency.outcome(), &QueryOutcome::Success(7));
                    Ok(QueryOutput::success(8))
                },
            )
        });
        outer_started_rx.recv().unwrap();

        let owner_runtime = runtime.clone();
        let owner_target = target.clone();
        let owner = thread::spawn(move || {
            owner_runtime.query(
                &owner_target,
                revision(1),
                Key("queued"),
                CancellationToken::new(),
                |_| {
                    target_ran_tx.send(()).unwrap();
                    Ok(QueryOutput::success(7))
                },
            )
        });
        runtime.wait_for_metrics(|metrics| metrics.claims == 2);
        enter_join_tx.send(()).unwrap();
        target_ran_rx.recv().unwrap();
        assert_eq!(
            owner.join().unwrap().unwrap().outcome(),
            &QueryOutcome::Success(7)
        );
        assert_eq!(
            outer_thread.join().unwrap().unwrap().outcome(),
            &QueryOutcome::Success(8)
        );
        assert_eq!(runtime.metrics().donated_permits, 1);
    }

    #[test]
    fn one_permit_cross_task_cycle_is_distinct_from_queued_owner_starvation() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let left = runtime.family::<Key, u64>("one-cycle-left", 4).unwrap();
        let right = runtime.family::<Key, u64>("one-cycle-right", 4).unwrap();
        let (left_started_tx, left_started_rx) = mpsc::channel();
        let (enter_wait_tx, enter_wait_rx) = mpsc::channel();

        let left_runtime = runtime.clone();
        let left_root = left.clone();
        let left_dependency = right.clone();
        let left_task = thread::spawn(move || {
            left_runtime.query(
                &left_root,
                revision(1),
                Key("a"),
                CancellationToken::new(),
                |context| {
                    left_started_tx.send(()).unwrap();
                    enter_wait_rx.recv().unwrap();
                    context.query(&left_dependency, Key("b"), |_| Ok(QueryOutput::success(2)))?;
                    Ok(QueryOutput::success(1))
                },
            )
        });
        left_started_rx.recv().unwrap();

        let right_runtime = runtime.clone();
        let right_root = right.clone();
        let right_dependency = left.clone();
        let right_task = thread::spawn(move || {
            right_runtime.query(
                &right_root,
                revision(1),
                Key("b"),
                CancellationToken::new(),
                |context| {
                    context.query(&right_dependency, Key("a"), |_| {
                        panic!("the left root is already owned")
                    })?;
                    Ok(QueryOutput::success(2))
                },
            )
        });
        runtime.wait_for_metrics(|metrics| metrics.claims == 2);
        enter_wait_tx.send(()).unwrap();

        assert_eq!(
            left_task.join().unwrap().unwrap().outcome(),
            &QueryOutcome::Success(1)
        );
        let QueryAbort::Cycle(nodes) = right_task.join().unwrap().unwrap_err() else {
            panic!("the queued owner must observe the true cross-task cycle");
        };
        assert_eq!(nodes.len(), 2);
        assert_eq!(runtime.metrics().cycles, 1);
        assert_eq!(runtime.metrics().donated_permits, 1);
    }

    #[test]
    fn exact_stack_cycle_is_not_reported_as_starvation() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("self-cycle", 4).unwrap();
        let nested = family.clone();
        let result = runtime.query(
            &family,
            revision(1),
            Key("a"),
            CancellationToken::new(),
            move |context| {
                context.query(&nested, Key("a"), |_| Ok(QueryOutput::success(1)))?;
                Ok(QueryOutput::success(2))
            },
        );
        let QueryAbort::Cycle(nodes) = result.unwrap_err() else {
            panic!("expected a true cycle");
        };
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].key(), "a");
    }

    #[test]
    fn cross_task_wait_cycle_is_detected_without_deadlock() {
        let runtime = QueryRuntime::new(2);
        publish_empty(&runtime, [revision(1)]);
        let left = runtime.family::<Key, u64>("cycle-left", 4).unwrap();
        let right = runtime.family::<Key, u64>("cycle-right", 4).unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let left_runtime = runtime.clone();
        let left_root = left.clone();
        let left_dependency = right.clone();
        let left_barrier = barrier.clone();
        let a = thread::spawn(move || {
            left_runtime.query(
                &left_root,
                revision(1),
                Key("a"),
                CancellationToken::new(),
                |context| {
                    left_barrier.wait();
                    context.query(&left_dependency, Key("b"), |_| Ok(QueryOutput::success(2)))?;
                    Ok(QueryOutput::success(1))
                },
            )
        });

        let right_runtime = runtime.clone();
        let right_root = right.clone();
        let right_dependency = left.clone();
        let right_barrier = barrier;
        let b = thread::spawn(move || {
            right_runtime.query(
                &right_root,
                revision(1),
                Key("b"),
                CancellationToken::new(),
                |context| {
                    right_barrier.wait();
                    context.query(&right_dependency, Key("a"), |_| Ok(QueryOutput::success(1)))?;
                    Ok(QueryOutput::success(2))
                },
            )
        });
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert!(
            results
                .iter()
                .any(|result| matches!(result, Err(QueryAbort::Cycle(_))))
        );
        assert!(runtime.metrics().cycles >= 1);
    }

    #[test]
    fn incompatible_revisions_do_not_join_and_can_overlap() {
        let runtime = QueryRuntime::new(2);
        publish_empty(&runtime, [revision(1), revision(2)]);
        let family = runtime.family::<Key, u64>("revisions", 4).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles = [revision(1), revision(2)].map(|revision| {
            let runtime = runtime.clone();
            let family = family.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                runtime.query(
                    &family,
                    revision,
                    Key("same"),
                    CancellationToken::new(),
                    |_| {
                        barrier.wait();
                        Ok(QueryOutput::success(revision.id()))
                    },
                )
            })
        });
        let values = handles.map(|handle| handle.join().unwrap().unwrap());
        assert_eq!(values[0].revision(), revision(1));
        assert_eq!(values[1].revision(), revision(2));
        assert_eq!(runtime.metrics().claims, 2);
        assert_eq!(runtime.metrics().joins, 0);
        assert_eq!(runtime.metrics().peak_active_bodies, 2);
    }

    #[test]
    fn red_publication_preserves_stamp_across_revision_and_position_changes() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1), revision(2), revision(3)]);
        let leaf = runtime.family::<Key, u64>("red-leaf", 8).unwrap();
        let parent = runtime.family::<Key, u64>("red-parent", 8).unwrap();

        let run = |revision, value, offset| {
            let leaf = leaf.clone();
            runtime.query(
                &parent,
                revision,
                Key("parent"),
                CancellationToken::new(),
                move |context| {
                    let _child = context.query(&leaf, Key("leaf"), |_| {
                        Ok(QueryOutput::success(value).with_diagnostics(vec![
                            QueryDiagnostic::new(
                                "leaf-warning",
                                "same semantic payload",
                                Some(PresentationPosition::new("main.rue", offset)),
                            ),
                        ]))
                    })?;
                    Ok(QueryOutput::success(10).with_work(vec![WorkItem::new("visited", 1)]))
                },
            )
        };

        let first = run(revision(1), 5, 1).unwrap();
        let second = run(revision(2), 5, 99).unwrap();
        assert_eq!(first.stamp(), second.stamp());
        assert_eq!(first.dependencies(), second.dependencies());
        let leaf_second = runtime
            .query(
                &leaf,
                revision(2),
                Key("leaf"),
                CancellationToken::new(),
                |_| panic!("compatible terminal is retained"),
            )
            .unwrap();
        assert_eq!(
            leaf_second.diagnostics()[0]
                .presentation
                .as_ref()
                .unwrap()
                .offset,
            99
        );

        let third = run(revision(3), 6, 100).unwrap();
        // The green child forces the parent body to run, but equal parent
        // semantics retain the parent's red stamp.
        assert_eq!(second.stamp(), third.stamp());
        assert_ne!(second.dependencies(), third.dependencies());
        assert_eq!(runtime.metrics().claims, 6);
        assert!(runtime.metrics().red_publications >= 2);
    }

    #[test]
    fn canceling_a_waiter_does_not_cancel_shared_work() {
        let runtime = QueryRuntime::new(2);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("waiter-cancel", 4).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let owner_runtime = runtime.clone();
        let owner_family = family.clone();
        let owner = thread::spawn(move || {
            owner_runtime.query(
                &owner_family,
                revision(1),
                Key("shared"),
                CancellationToken::new(),
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(1))
                },
            )
        });
        started_rx.recv().unwrap();
        let waiter_token = CancellationToken::new();
        let waiter_runtime = runtime.clone();
        let waiter_family = family.clone();
        let waiter_cancel = waiter_token.clone();
        let waiter = thread::spawn(move || {
            waiter_runtime.query(
                &waiter_family,
                revision(1),
                Key("shared"),
                waiter_token,
                |_| panic!("waiter cannot become owner while shared work is live"),
            )
        });
        runtime.wait_for_metrics(|metrics| metrics.joins == 1);
        waiter_cancel.cancel();
        assert_eq!(waiter.join().unwrap().unwrap_err(), QueryAbort::Canceled);
        finish_tx.send(()).unwrap();
        assert_eq!(
            owner.join().unwrap().unwrap().outcome(),
            &QueryOutcome::Success(1)
        );
    }

    #[test]
    fn canceled_owner_does_not_publish_and_live_waiter_reclaims() {
        let runtime = QueryRuntime::new(2);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("owner-cancel", 4).unwrap();
        let owner_token = CancellationToken::new();
        let owner_cancel = owner_token.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let owner_runtime = runtime.clone();
        let owner_family = family.clone();
        let owner = thread::spawn(move || {
            owner_runtime.query(
                &owner_family,
                revision(1),
                Key("shared"),
                owner_token,
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(1))
                },
            )
        });
        started_rx.recv().unwrap();

        let waiter_runtime = runtime.clone();
        let waiter_family = family.clone();
        let waiter = thread::spawn(move || {
            waiter_runtime.query(
                &waiter_family,
                revision(1),
                Key("shared"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(2)),
            )
        });
        runtime.wait_for_metrics(|metrics| metrics.joins == 1);
        owner_cancel.cancel();
        finish_tx.send(()).unwrap();
        assert_eq!(owner.join().unwrap().unwrap_err(), QueryAbort::Canceled);
        assert_eq!(
            waiter.join().unwrap().unwrap().outcome(),
            &QueryOutcome::Success(2)
        );
        assert_eq!(runtime.metrics().green_publications, 1);
    }

    #[test]
    fn failures_are_reused_and_retention_respects_pins() {
        let runtime = QueryRuntime::new(1);
        publish_empty(
            &runtime,
            [
                revision(1),
                Revision::new(99, 1),
                revision(2),
                revision(3),
                revision(4),
            ],
        );
        let family = runtime.family::<Key, u64>("retention", 2).unwrap();
        let failed = runtime
            .query(
                &family,
                revision(1),
                Key("failure"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::failure(QueryFailure::new("E1", "broken"))),
            )
            .unwrap();
        let reused_failure = runtime
            .query(
                &family,
                Revision::new(99, 1),
                Key("failure"),
                CancellationToken::new(),
                |_| panic!("a retained failure validates like a success"),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&failed, &reused_failure));
        let terminal_pin = family.pin_terminal(&failed).unwrap();
        let second_pin = family.retain_revision(revision(2));
        let third_pin = family.retain_revision(revision(3));
        for (id, key) in [(2, "second"), (3, "third"), (4, "fourth")] {
            runtime
                .query(
                    &family,
                    revision(id),
                    Key(key),
                    CancellationToken::new(),
                    |_| Ok(QueryOutput::success(id)),
                )
                .unwrap();
        }
        assert_eq!(
            failed.outcome(),
            &QueryOutcome::Failure(QueryFailure::new("E1", "broken"))
        );
        assert!(runtime.metrics().retained_terminals >= 3);
        drop(terminal_pin);
        drop(second_pin);
        drop(third_pin);
        assert_eq!(runtime.metrics().retained_terminals, 2);
        assert!(runtime.metrics().evictions >= 2);
    }

    #[test]
    fn terminal_diagnostics_and_work_are_worker_order_independent() {
        fn run(workers: usize, reverse: bool) -> Arc<QueryTerminal<u64>> {
            let runtime = QueryRuntime::new(workers);
            publish_empty(&runtime, [revision(1)]);
            let family = runtime.family::<Key, u64>("determinism", 2).unwrap();
            runtime
                .query(
                    &family,
                    revision(1),
                    Key("root"),
                    CancellationToken::new(),
                    move |_| {
                        let mut diagnostics = vec![
                            QueryDiagnostic::new("b", "two", None),
                            QueryDiagnostic::new("a", "one", None),
                        ];
                        let mut work = vec![
                            WorkItem::new("lowered", 2),
                            WorkItem::new("visited", 1),
                            WorkItem::new("lowered", 3),
                        ];
                        if reverse {
                            diagnostics.reverse();
                            work.reverse();
                        }
                        Ok(QueryOutput::success(7)
                            .with_diagnostics(diagnostics)
                            .with_work(work))
                    },
                )
                .unwrap()
        }

        let serial = run(1, false);
        let parallel = run(8, true);
        assert_eq!(serial.outcome(), parallel.outcome());
        assert_eq!(serial.diagnostics(), parallel.diagnostics());
        assert_eq!(serial.work(), parallel.work());
        assert_eq!(serial.dependencies(), parallel.dependencies());
        assert_eq!(serial.stamp(), parallel.stamp());
    }

    #[test]
    fn family_names_and_key_text_form_collision_free_node_identities() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let left = runtime.family::<Key, u64>("left", 2).unwrap();
        let right = runtime.family::<Key, u64>("right", 2).unwrap();
        assert!(matches!(
            runtime.family::<Key, u64>("left", 2),
            Err(FamilyError::DuplicateName(_))
        ));
        let left = runtime
            .query(
                &left,
                revision(1),
                Key("same"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(1)),
            )
            .unwrap();
        let right = runtime
            .query(
                &right,
                revision(1),
                Key("same"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(1)),
            )
            .unwrap();
        assert_ne!(left.node(), right.node());
        assert_eq!(left.node().key(), right.node().key());
    }

    #[test]
    fn active_joiner_protects_terminal_until_wake_with_zero_retention() {
        let runtime = QueryRuntime::new(2);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime
            .family::<Key, u64>("zero-retention-join", 0)
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let owner_runtime = runtime.clone();
        let owner_family = family.clone();
        let owner = thread::spawn(move || {
            owner_runtime.query(
                &owner_family,
                revision(1),
                Key("shared"),
                CancellationToken::new(),
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(11))
                },
            )
        });
        started_rx.recv().unwrap();
        let join_runtime = runtime.clone();
        let join_family = family.clone();
        let joiner = thread::spawn(move || {
            join_runtime.query(
                &join_family,
                revision(1),
                Key("shared"),
                CancellationToken::new(),
                |_| panic!("active waiter must receive the owner's terminal"),
            )
        });
        runtime.wait_for_metrics(|metrics| metrics.joins == 1);
        finish_tx.send(()).unwrap();
        let owner = owner.join().unwrap().unwrap();
        let joined = joiner.join().unwrap().unwrap();
        assert!(Arc::ptr_eq(&owner, &joined));
        assert_eq!(joined.outcome(), &QueryOutcome::Success(11));
        assert_eq!(family.retention().terminals, 0);
        assert_eq!(family.retention().memo_nodes, 0);
    }

    #[test]
    fn zero_retention_reclaims_empty_nodes_under_key_churn() {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        struct NumberKey(u64);

        impl QueryKey for NumberKey {
            fn stable_identity(&self) -> String {
                self.0.to_string()
            }
        }

        let runtime = QueryRuntime::new(1);
        let family = runtime.family::<NumberKey, u64>("key-churn", 0).unwrap();
        for key in 0..256 {
            publish_empty(&runtime, [revision(key + 1)]);
            runtime
                .query(
                    &family,
                    revision(key + 1),
                    NumberKey(key),
                    CancellationToken::new(),
                    |_| Ok(QueryOutput::success(key)),
                )
                .unwrap();
        }
        assert_eq!(
            family.retention(),
            FamilyRetention {
                memo_nodes: 0,
                terminals: 0,
                terminal_limit: 0,
            }
        );
    }

    #[test]
    fn foreign_same_named_family_cannot_pin_terminal() {
        let first_runtime = QueryRuntime::new(1);
        publish_empty(&first_runtime, [revision(1)]);
        let first = first_runtime.family::<Key, u64>("same-name", 1).unwrap();
        let terminal = first_runtime
            .query(
                &first,
                revision(1),
                Key("key"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(1)),
            )
            .unwrap();
        let second_runtime = QueryRuntime::new(1);
        let second = second_runtime.family::<Key, u64>("same-name", 1).unwrap();
        assert!(matches!(
            second.pin_terminal(&terminal),
            Err(PinError::ForeignFamily)
        ));
        assert_eq!(first.retention().terminals, 1);
        assert_eq!(second.retention().terminals, 0);
    }

    #[test]
    fn evicted_and_recreated_node_cannot_repeat_an_old_observation() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1), revision(2)]);
        let leaf = runtime.family::<Key, u64>("aba-leaf", 0).unwrap();
        let parent = runtime.family::<Key, u64>("aba-parent", 4).unwrap();
        let run = |revision, leaf_value| {
            let leaf = leaf.clone();
            runtime.query(
                &parent,
                revision,
                Key("parent"),
                CancellationToken::new(),
                move |context| {
                    context.query(&leaf, Key("leaf"), |_| Ok(QueryOutput::success(leaf_value)))?;
                    Ok(QueryOutput::success(9))
                },
            )
        };
        let first = run(revision(1), 1).unwrap();
        assert_eq!(leaf.retention().memo_nodes, 0);
        let second = run(revision(2), 2).unwrap();
        assert_eq!(first.dependencies()[0].stamp, 1);
        assert_eq!(second.dependencies()[0].stamp, 1);
        assert_ne!(
            first.dependencies()[0].incarnation,
            second.dependencies()[0].incarnation
        );
        assert_ne!(first.dependencies(), second.dependencies());
        // The ABA-safe child identity forced recomputation. Dependency lists
        // are provenance and do not make equal parent semantics green.
        assert_eq!(first.stamp(), second.stamp());
        assert_eq!(runtime.metrics().claims, 4);
    }

    // ADR-0063 Phase 7 hashed-memo-index focused coverage.

    #[test]
    fn forced_hash_collision_keys_resolve_to_distinct_nodes_via_eq() {
        // A key whose `Hash` is a constant: every value collides in one bucket,
        // so the memo map must separate distinct keys through exact `Eq` alone.
        #[derive(Debug, Clone, PartialEq, Eq)]
        struct OneBucketKey(u64);

        impl std::hash::Hash for OneBucketKey {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                state.write_u8(0);
            }
        }

        impl QueryKey for OneBucketKey {
            fn stable_identity(&self) -> String {
                // Distinct display identities to prove lookup never consults the
                // display string: only typed `Eq` chooses the node.
                format!("one-bucket:{}", self.0)
            }
        }

        assert_ne!(OneBucketKey(1), OneBucketKey(2));
        assert_eq!(hash_of(&OneBucketKey(1)), hash_of(&OneBucketKey(2)));

        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime
            .family::<OneBucketKey, u64>("one-bucket", 16)
            .unwrap();
        let first = runtime
            .query(
                &family,
                revision(1),
                OneBucketKey(1),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(10)),
            )
            .unwrap();
        let second = runtime
            .query(
                &family,
                revision(1),
                OneBucketKey(2),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(20)),
            )
            .unwrap();
        // Two colliding keys produced two distinct memo nodes and two bodies.
        assert!(!Arc::ptr_eq(&first, &second));
        assert_ne!(first.outcome(), second.outcome());
        assert_eq!(family.retention().memo_nodes, 2);
        assert_eq!(runtime.metrics().claims, 2);
        // Re-querying an existing colliding key reuses its node (no new claim),
        // proving the `Eq` fallback finds the right bucket entry.
        let reuse = runtime
            .query(
                &family,
                revision(1),
                OneBucketKey(1),
                CancellationToken::new(),
                |_| panic!("colliding key 1 must reuse its retained terminal"),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&first, &reuse));
        assert_eq!(runtime.metrics().claims, 2);
    }

    #[test]
    fn evict_then_recreate_in_hashed_index_yields_new_incarnation() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1), revision(2)]);
        // Zero-retention leaf: its node is evicted from the hashed index as soon
        // as no lease holds it, forcing a fresh incarnation on the next request.
        let leaf = runtime.family::<Key, u64>("recreate-leaf", 0).unwrap();
        let parent = runtime.family::<Key, u64>("recreate-parent", 4).unwrap();
        let observe = |revision| {
            let leaf = leaf.clone();
            runtime
                .query(
                    &parent,
                    revision,
                    Key("root"),
                    CancellationToken::new(),
                    move |context| {
                        context.query(&leaf, Key("leaf"), |_| Ok(QueryOutput::success(1)))?;
                        Ok(QueryOutput::success(0))
                    },
                )
                .unwrap()
        };
        let first = observe(revision(1));
        // The zero-retention leaf node left the hashed index entirely.
        assert_eq!(leaf.retention().memo_nodes, 0);
        let second = observe(revision(2));
        let first_incarnation = first.dependencies()[0].incarnation;
        let second_incarnation = second.dependencies()[0].incarnation;
        assert_ne!(
            first_incarnation, second_incarnation,
            "eviction followed by recreation must mint a distinct incarnation"
        );
    }

    #[test]
    fn concurrent_same_key_requests_join_one_hashed_computation() {
        let runtime = QueryRuntime::new(8);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime.family::<Key, u64>("concurrent-join", 16).unwrap();
        let waiters = 6;
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let owner_runtime = runtime.clone();
        let owner_family = family.clone();
        let owner = thread::spawn(move || {
            owner_runtime
                .query(
                    &owner_family,
                    revision(1),
                    Key("shared"),
                    CancellationToken::new(),
                    |_| {
                        started_tx.send(()).unwrap();
                        finish_rx.recv().unwrap();
                        Ok(QueryOutput::success(7))
                    },
                )
                .unwrap()
        });
        started_rx.recv().unwrap();
        let joins_before = runtime.metrics().joins;
        let mut joiners = Vec::new();
        for _ in 0..waiters {
            let runtime = runtime.clone();
            let family = family.clone();
            joiners.push(thread::spawn(move || {
                runtime
                    .query(
                        &family,
                        revision(1),
                        Key("shared"),
                        CancellationToken::new(),
                        |_| panic!("a joiner on the shared key must not compute"),
                    )
                    .unwrap()
            }));
        }
        while runtime.metrics().joins < joins_before + waiters as u64 {
            thread::yield_now();
        }
        finish_tx.send(()).unwrap();
        let owner_terminal = owner.join().unwrap();
        for joiner in joiners {
            let terminal = joiner.join().unwrap();
            assert!(Arc::ptr_eq(&terminal, &owner_terminal));
        }
        // Exactly one node was claimed for the shared key; all waiters joined it.
        assert_eq!(runtime.metrics().claims, 1);
        assert_eq!(runtime.metrics().joins, waiters as u64);
        assert_eq!(family.retention().memo_nodes, 1);
    }

    #[test]
    fn cross_revision_reuse_validates_every_exact_input_leaf() {
        let runtime = QueryRuntime::new(1);
        let family = runtime.family::<Key, u64>("validated", 4).unwrap();
        let input = InputIdentity::new("source", "main.rue");
        let first_revision = Revision::new(1, 7);
        let equivalent_revision = Revision::new(2, 7);
        let changed_revision = Revision::new(3, 7);
        let missing_revision = Revision::new(4, 7);
        runtime
            .publish_revision(first_revision, [(input.clone(), 11)])
            .unwrap();
        runtime
            .publish_revision(equivalent_revision, [(input.clone(), 11)])
            .unwrap();
        runtime
            .publish_revision(changed_revision, [(input.clone(), 12)])
            .unwrap();
        runtime.publish_revision(missing_revision, []).unwrap();

        let first = runtime
            .query(
                &family,
                first_revision,
                Key("same"),
                CancellationToken::new(),
                |context| {
                    assert_eq!(context.input(input.clone())?, 11);
                    Ok(QueryOutput::success(1))
                },
            )
            .unwrap();
        let reused = runtime
            .query(
                &family,
                equivalent_revision,
                Key("same"),
                CancellationToken::new(),
                |_| panic!("equal compatibility is insufficient; exact leaves prove reuse"),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&first, &reused));

        let recomputed = runtime
            .query(
                &family,
                changed_revision,
                Key("same"),
                CancellationToken::new(),
                |context| {
                    assert_eq!(context.input(input.clone())?, 12);
                    Ok(QueryOutput::success(2))
                },
            )
            .unwrap();
        assert_eq!(recomputed.outcome(), &QueryOutcome::Success(2));
        let missing = runtime.request(
            &family,
            missing_revision,
            Key("same"),
            CancellationToken::new(),
            |context| {
                context.input(input.clone())?;
                unreachable!("missing exact input fails closed")
            },
        );
        assert_eq!(
            missing.abort(),
            Some(&QueryAbort::MissingInput(input.clone()))
        );
        assert_eq!(runtime.metrics().claims, 3);
        assert_eq!(runtime.metrics().reuses, 1);
    }

    #[test]
    fn red_green_validation_is_direct_recursive_and_semantic() {
        let runtime = QueryRuntime::new(1);
        let leaf = runtime.family::<Key, u64>("rg-leaf", 8).unwrap();
        let middle = runtime.family::<Key, u64>("rg-middle", 8).unwrap();
        let root = runtime.family::<Key, u64>("rg-root", 8).unwrap();
        let input = InputIdentity::new("source", "main");
        let revisions = [
            Revision::new(10, 1),
            Revision::new(11, 1),
            Revision::new(12, 1),
        ];
        for (revision, stamp) in revisions.into_iter().zip([1, 2, 3]) {
            runtime
                .publish_revision(revision, [(input.clone(), stamp)])
                .unwrap();
        }

        let initial_leaf = leaf.clone();
        let initial_middle = middle.clone();
        let first = runtime
            .query(
                &root,
                revisions[0],
                Key("root"),
                CancellationToken::new(),
                |context| {
                    let middle = context.query(&initial_middle, Key("middle"), |context| {
                        let leaf = context.query(&initial_leaf, Key("leaf"), |context| {
                            context.input(input.clone())?;
                            Ok(QueryOutput::success(10))
                        })?;
                        let QueryOutcome::Success(value) = leaf.outcome() else {
                            unreachable!()
                        };
                        Ok(QueryOutput::success(*value))
                    })?;
                    assert_eq!(middle.inputs().len(), 0);
                    Ok(QueryOutput::success(99))
                },
            )
            .unwrap();
        assert_eq!(first.inputs().len(), 0);
        assert_eq!(first.dependencies().len(), 1);
        let first_leaf = runtime
            .query(
                &leaf,
                revisions[0],
                Key("leaf"),
                CancellationToken::new(),
                |_| panic!("initial leaf terminal is retained"),
            )
            .unwrap();

        // The direct leaf changes input but recomputes to equal semantics, so
        // it stays red. Recursive validation can then reuse both ancestors.
        let red_leaf = runtime
            .query(
                &leaf,
                revisions[1],
                Key("leaf"),
                CancellationToken::new(),
                |context| {
                    context.input(input.clone())?;
                    Ok(QueryOutput::success(10))
                },
            )
            .unwrap();
        assert_eq!(red_leaf.stamp(), first_leaf.stamp());
        let reused = runtime.request(
            &root,
            revisions[1],
            Key("root"),
            CancellationToken::new(),
            |_| panic!("validated red dependency chain must reuse the root"),
        );
        assert_eq!(reused.execution(), RequestExecution::Reused);

        // A green leaf makes the middle invalid, which recursively makes the
        // root recompute. Equal root semantics still retain the root stamp.
        let green_leaf = runtime
            .query(
                &leaf,
                revisions[2],
                Key("leaf"),
                CancellationToken::new(),
                |context| {
                    context.input(input.clone())?;
                    Ok(QueryOutput::success(11))
                },
            )
            .unwrap();
        assert_ne!(green_leaf.stamp(), red_leaf.stamp());
        let recompute_leaf = leaf.clone();
        let recompute_middle = middle.clone();
        let recomputed = runtime.request(
            &root,
            revisions[2],
            Key("root"),
            CancellationToken::new(),
            |context| {
                let middle = context.query(&recompute_middle, Key("middle"), |context| {
                    let leaf = context.query(&recompute_leaf, Key("leaf"), |_| {
                        panic!("green leaf was already validated")
                    })?;
                    let QueryOutcome::Success(value) = leaf.outcome() else {
                        unreachable!()
                    };
                    Ok(QueryOutput::success(*value))
                })?;
                assert_eq!(middle.outcome(), &QueryOutcome::Success(11));
                Ok(QueryOutput::success(99))
            },
        );
        assert_eq!(recomputed.execution(), RequestExecution::Computed);
        assert_eq!(recomputed.terminal().unwrap().stamp(), first.stamp());
    }

    #[test]
    fn registered_evaluators_propagate_red_from_a_root_only_request() {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("source", "registered");
        let leaf_runs = Arc::new(AtomicUsize::new(0));
        let leaf_input = input.clone();
        let leaf_counter = leaf_runs.clone();
        let leaf = runtime
            .family_with_evaluator::<Key, u64, _>("registered-leaf", 8, move |context, _, _| {
                leaf_counter.fetch_add(1, Ordering::Relaxed);
                let stamp = context.input(leaf_input.clone())?;
                Ok(QueryOutput::success(if stamp < 3 { 10 } else { 11 }))
            })
            .unwrap();

        let middle_runs = Arc::new(AtomicUsize::new(0));
        let middle_leaf = leaf.clone();
        let middle_counter = middle_runs.clone();
        let middle = runtime
            .family_with_evaluator::<Key, u64, _>("registered-middle", 8, move |context, _, _| {
                middle_counter.fetch_add(1, Ordering::Relaxed);
                let leaf = context.query_registered(&middle_leaf, Key("leaf"))?;
                let QueryOutcome::Success(value) = leaf.outcome() else {
                    unreachable!()
                };
                Ok(QueryOutput::success(*value))
            })
            .unwrap();

        let root_runs = Arc::new(AtomicUsize::new(0));
        let root_middle = middle.clone();
        let root_counter = root_runs.clone();
        let root = runtime
            .family_with_evaluator::<Key, u64, _>("registered-root", 8, move |context, _, _| {
                root_counter.fetch_add(1, Ordering::Relaxed);
                context.query_registered(&root_middle, Key("middle"))?;
                Ok(QueryOutput::success(99))
            })
            .unwrap();

        let revisions = [
            Revision::new(30, 1),
            Revision::new(31, 1),
            Revision::new(32, 1),
        ];
        for (revision, stamp) in revisions.into_iter().zip([1, 2, 3]) {
            runtime
                .publish_revision(revision, [(input.clone(), stamp)])
                .unwrap();
        }

        let first =
            runtime.request_registered(&root, revisions[0], Key("root"), CancellationToken::new());
        assert_eq!(first.execution(), RequestExecution::Computed);
        assert_eq!(leaf_runs.load(Ordering::Relaxed), 1);
        assert_eq!(middle_runs.load(Ordering::Relaxed), 1);
        assert_eq!(root_runs.load(Ordering::Relaxed), 1);

        // Only the root is requested. Validation demands the dirty leaf by its
        // recorded exact key. Equal leaf semantics preserve its stamp, so both
        // ancestors validate without running their bodies.
        let red =
            runtime.request_registered(&root, revisions[1], Key("root"), CancellationToken::new());
        assert_eq!(red.execution(), RequestExecution::Reused);
        assert_eq!(leaf_runs.load(Ordering::Relaxed), 2);
        assert_eq!(middle_runs.load(Ordering::Relaxed), 1);
        assert_eq!(root_runs.load(Ordering::Relaxed), 1);
        assert!(
            red.nested_attempts()
                .iter()
                .any(|attempt| attempt.node().family() == "registered-leaf"
                    && attempt.execution() == RequestExecution::Computed)
        );

        // A green leaf invalidates and recomputes each ancestor. The root's
        // own equal semantics remain red even though its body must run.
        let green =
            runtime.request_registered(&root, revisions[2], Key("root"), CancellationToken::new());
        assert_eq!(green.execution(), RequestExecution::Computed);
        assert_eq!(leaf_runs.load(Ordering::Relaxed), 3);
        assert_eq!(middle_runs.load(Ordering::Relaxed), 2);
        assert_eq!(root_runs.load(Ordering::Relaxed), 2);
        assert_eq!(
            green.terminal().unwrap().stamp(),
            first.terminal().unwrap().stamp()
        );
    }

    #[test]
    fn nested_validation_does_not_promote_transitive_dependencies_to_the_caller() {
        let runtime = QueryRuntime::new(1);
        let leaf_input = InputIdentity::new("source", "validation-leaf");
        let root_input = InputIdentity::new("source", "validation-root");
        let leaf_source = leaf_input.clone();
        let leaf = runtime
            .family_with_evaluator::<Key, u64, _>("validation-leaf", 8, move |context, _, _| {
                context.input(leaf_source.clone())?;
                Ok(QueryOutput::success(10)
                    .with_work(vec![WorkItem::new("validation-leaf-work", 1)]))
            })
            .unwrap();
        let middle_leaf = leaf.clone();
        let middle = runtime
            .family_with_evaluator::<Key, u64, _>("validation-middle", 8, move |context, _, _| {
                context.query_registered(&middle_leaf, Key("leaf"))?;
                Ok(QueryOutput::success(20))
            })
            .unwrap();
        let root_middle = middle.clone();
        let root_source = root_input.clone();
        let root = runtime
            .family_with_evaluator::<Key, u64, _>("validation-root", 8, move |context, _, _| {
                context.input(root_source.clone())?;
                context.query_registered(&root_middle, Key("middle"))?;
                Ok(QueryOutput::success(30))
            })
            .unwrap();
        let first_revision = Revision::new(40, 1);
        let second_revision = Revision::new(41, 1);
        runtime
            .publish_revision(
                first_revision,
                [(leaf_input.clone(), 1), (root_input.clone(), 1)],
            )
            .unwrap();
        runtime
            .publish_revision(second_revision, [(leaf_input, 2), (root_input, 2)])
            .unwrap();

        runtime.request_registered(&root, first_revision, Key("root"), CancellationToken::new());
        let recomputed = runtime.request_registered(
            &root,
            second_revision,
            Key("root"),
            CancellationToken::new(),
        );

        assert_eq!(recomputed.execution(), RequestExecution::Computed);
        let dependencies = recomputed.terminal().unwrap().dependencies();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].node.family(), "validation-middle");
        assert!(
            recomputed
                .work()
                .iter()
                .any(
                    |(identity, amount)| identity.as_ref() == "validation-leaf-work"
                        && *amount == 1
                )
        );
    }

    #[test]
    fn unavailable_registered_validation_invalidates_a_supplied_parent() {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("source", "validation-canceled-child");
        let child_input = input.clone();
        let child = runtime
            .family_with_evaluator::<Key, u64, _>(
                "validation-canceled-child",
                8,
                move |context, _, _| {
                    let stamp = context.input(child_input.clone())?;
                    if stamp == 1 {
                        Ok(QueryOutput::success(10))
                    } else {
                        Err(QueryAbort::Canceled)
                    }
                },
            )
            .unwrap();
        let root = runtime
            .family::<Key, u64>("validation-supplied-root", 8)
            .unwrap();
        let first_revision = Revision::new(50, 1);
        let second_revision = Revision::new(51, 1);
        runtime
            .publish_revision(first_revision, [(input.clone(), 1)])
            .unwrap();
        runtime
            .publish_revision(second_revision, [(input, 2)])
            .unwrap();

        let initial_child = child.clone();
        runtime
            .query(
                &root,
                first_revision,
                Key("root"),
                CancellationToken::new(),
                move |context| {
                    context.query_registered(&initial_child, Key("child"))?;
                    Ok(QueryOutput::success(1))
                },
            )
            .unwrap();

        let recomputed = runtime.request(
            &root,
            second_revision,
            Key("root"),
            CancellationToken::new(),
            |_| Ok(QueryOutput::success(2)),
        );
        assert_eq!(recomputed.execution(), RequestExecution::Computed);
        assert_eq!(
            recomputed.terminal().unwrap().outcome(),
            &QueryOutcome::Success(2)
        );
    }

    #[test]
    fn nested_requests_retain_computed_and_reused_lifecycles() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let child = runtime
            .family::<Key, u64>("nested-ledger-child", 4)
            .unwrap();
        let root = runtime.family::<Key, u64>("nested-ledger-root", 4).unwrap();

        let first_child = child.clone();
        let first = runtime.request(
            &root,
            revision(1),
            Key("first"),
            CancellationToken::new(),
            move |context| {
                context.query(&first_child, Key("child"), |_| {
                    Ok(QueryOutput::success(7).with_work(vec![WorkItem::new("child", 2)]))
                })?;
                Ok(QueryOutput::success(1))
            },
        );
        assert_eq!(first.nested_attempts().len(), 1);
        assert_eq!(
            first.nested_attempts()[0].execution(),
            RequestExecution::Computed
        );
        assert_eq!(
            first.nested_attempts()[0].id(),
            first.nested_attempts()[0].origin_request_id()
        );
        assert!(first.nested_attempts()[0].node_incarnation().is_some());
        assert_eq!(
            first.nested_attempts()[0].work(),
            &[(Arc::<str>::from("child"), 2)]
        );

        let reused_child = child.clone();
        let second = runtime.request(
            &root,
            revision(1),
            Key("second"),
            CancellationToken::new(),
            move |context| {
                context.query(&reused_child, Key("child"), |_| {
                    panic!("the retained child must be reused")
                })?;
                Ok(QueryOutput::success(2))
            },
        );
        assert_eq!(second.nested_attempts().len(), 1);
        assert_eq!(
            second.nested_attempts()[0].execution(),
            RequestExecution::Reused
        );
        assert!(second.nested_attempts()[0].work().is_empty());
        assert_eq!(
            second.nested_attempts()[0].origin_request_id(),
            first.nested_attempts()[0].id()
        );
    }

    #[test]
    fn registered_same_family_cycle_aborts_recovers_and_does_not_retain_itself() {
        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1)]);
        let family = runtime
            .family_with_evaluator::<Key, u64, _>(
                "registered-self-cycle",
                4,
                |context, family, key| match key.0 {
                    "left" => {
                        context.query_registered(family, Key("right"))?;
                        Ok(QueryOutput::success(1))
                    }
                    "right" => {
                        context.query_registered(family, Key("left"))?;
                        Ok(QueryOutput::success(2))
                    }
                    "recovery" => Ok(QueryOutput::success(3)),
                    _ => unreachable!(),
                },
            )
            .unwrap();
        let weak = family.downgrade();

        let cycle =
            runtime.request_registered(&family, revision(1), Key("left"), CancellationToken::new());
        assert!(matches!(cycle.abort(), Some(QueryAbort::Cycle(_))));
        assert!(
            cycle
                .nested_attempts()
                .iter()
                .any(|attempt| attempt.execution() == RequestExecution::Aborted)
        );

        let recovery = runtime.request_registered(
            &family,
            revision(1),
            Key("recovery"),
            CancellationToken::new(),
        );
        assert_eq!(recovery.execution(), RequestExecution::Computed);
        assert_eq!(
            recovery.terminal().unwrap().outcome(),
            &QueryOutcome::Success(3)
        );

        drop(recovery);
        drop(cycle);
        drop(family);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn aborted_requests_retain_nested_dependency_input_and_work_prefixes() {
        let runtime = QueryRuntime::new(1);
        let grandchild = runtime.family::<Key, u64>("abort-grandchild", 4).unwrap();
        let child = runtime.family::<Key, u64>("abort-child", 4).unwrap();
        let root = runtime.family::<Key, u64>("abort-root", 4).unwrap();
        let input = InputIdentity::new("source", "abort");
        let revision = Revision::new(20, 1);
        runtime
            .publish_revision(revision, [(input.clone(), 7)])
            .unwrap();

        let nested_child = child.clone();
        let nested_grandchild = grandchild.clone();
        let attempt = runtime.request(
            &root,
            revision,
            Key("root"),
            CancellationToken::new(),
            |context| {
                context.query(&nested_child, Key("child"), |context| {
                    context.input(input)?;
                    context.query(&nested_grandchild, Key("grandchild"), |_| {
                        Ok(QueryOutput::success(1).with_work(vec![WorkItem::new("grandchild", 2)]))
                    })?;
                    context.record_work(WorkItem::new("child-prefix", 3));
                    Err(QueryAbort::Canceled)
                })?;
                unreachable!("nested abort propagates")
            },
        );
        assert_eq!(attempt.execution(), RequestExecution::Aborted);
        assert_eq!(attempt.abort(), Some(&QueryAbort::Canceled));
        assert_eq!(attempt.dependencies().len(), 1);
        assert_eq!(
            attempt.inputs(),
            &[InputObservation {
                input: InputIdentity::new("source", "abort"),
                stamp: 7
            }]
        );
        assert_eq!(
            attempt.work(),
            &[
                (Arc::<str>::from("child-prefix"), 3),
                (Arc::<str>::from("grandchild"), 2),
            ]
        );
        assert_eq!(attempt.nested_attempts().len(), 2);
        assert_eq!(
            attempt.nested_attempts()[0].execution(),
            RequestExecution::Computed
        );
        assert_eq!(
            attempt.nested_attempts()[1].execution(),
            RequestExecution::Aborted
        );
        assert_eq!(
            attempt.nested_attempts()[1].abort(),
            Some(&QueryAbort::Canceled)
        );
        assert_eq!(
            attempt.nested_attempts()[1].inputs(),
            &[InputObservation {
                input: InputIdentity::new("source", "abort"),
                stamp: 7
            }]
        );
    }

    #[test]
    fn unpublished_revisions_fail_closed_and_active_views_are_bounded_and_pinned() {
        let runtime = QueryRuntime::new(1);
        let family = runtime.family::<Key, u64>("revision-liveness", 2).unwrap();
        assert_eq!(
            runtime
                .request(
                    &family,
                    revision(1),
                    Key("missing"),
                    CancellationToken::new(),
                    |_| Ok(QueryOutput::success(1)),
                )
                .abort(),
            Some(&QueryAbort::UnpublishedRevision(revision(1)))
        );

        let active = revision(2);
        publish_empty(&runtime, [active]);
        let revision_pin = family.retain_revision(active);
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let worker_runtime = runtime.clone();
        let worker_family = family.clone();
        let worker = thread::spawn(move || {
            worker_runtime.query(
                &worker_family,
                active,
                Key("active"),
                CancellationToken::new(),
                |_| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok(QueryOutput::success(2))
                },
            )
        });
        started_rx.recv().unwrap();
        for id in 3..=(REVISION_RETENTION_LIMIT as u64 + 10) {
            publish_empty(&runtime, [revision(id)]);
        }
        assert_eq!(
            runtime.metrics().retained_revisions,
            REVISION_RETENTION_LIMIT as u64
        );
        runtime.publish_revision(active, []).unwrap();
        finish_tx.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap().outcome(),
            &QueryOutcome::Success(2)
        );
        assert_eq!(
            runtime
                .query(
                    &family,
                    active,
                    Key("active"),
                    CancellationToken::new(),
                    |_| panic!("explicit revision pin retains the reusable view"),
                )
                .unwrap()
                .outcome(),
            &QueryOutcome::Success(2)
        );
        drop(revision_pin);
        assert_eq!(
            runtime.publish_revision(revision(1), []),
            Err(RevisionError::Retired(revision(1)))
        );
    }

    #[test]
    fn revisions_and_key_identities_fail_closed_on_conflicts() {
        #[derive(Debug, Clone, PartialEq, Eq)]
        struct CollidingKey(u8);

        // Force every key into a single hash bucket. Distinct keys must still
        // resolve to distinct memo nodes through exact `Eq`; hashing is only a
        // bucketing hint. `Hash` agrees with `Eq` (all keys hash identically,
        // which is permitted — only inequality of hashes across equal values
        // would be a bug).
        impl std::hash::Hash for CollidingKey {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                state.write_u8(0);
            }
        }

        impl QueryKey for CollidingKey {
            fn stable_identity(&self) -> String {
                "collision".to_owned()
            }
        }

        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("source", "main.rue");
        assert_eq!(
            runtime.publish_revision(revision(1), [(input.clone(), 1), (input.clone(), 2)]),
            Err(RevisionError::ConflictingInput(input))
        );
        publish_empty(&runtime, [revision(1)]);

        let family = runtime
            .family::<CollidingKey, u64>("colliding-keys", 2)
            .unwrap();
        let first = runtime
            .query(
                &family,
                revision(1),
                CollidingKey(1),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(1)),
            )
            .unwrap();
        let second = runtime
            .query(
                &family,
                revision(1),
                CollidingKey(2),
                CancellationToken::new(),
                |_| Ok(QueryOutput::success(2)),
            )
            .unwrap();
        // The two keys are deliberately unequal yet hash into one bucket, so the
        // memo map must fall back to exact `Eq` to keep them apart.
        assert_ne!(CollidingKey(1), CollidingKey(2));
        assert_eq!(hash_of(&CollidingKey(1)), hash_of(&CollidingKey(2)));
        assert_eq!(first.node().family(), "colliding-keys");
        assert_eq!(first.node().key(), "collision");
        // The schedule-dependent incarnation stays out of canonical display
        // ordering while exact K equality still chooses distinct memo nodes even
        // under a forced hash collision.
        assert_eq!(first.node(), second.node());
        assert!(!Arc::ptr_eq(&first, &second));
        assert_ne!(first.outcome(), second.outcome());
    }

    #[test]
    fn optional_inputs_record_negative_observations_and_invalidate_on_publication() {
        let runtime = QueryRuntime::new(1);
        let family = runtime.family::<Key, bool>("optional-input", 4).unwrap();
        let input = InputIdentity::new("candidate", "helper.rue");
        let absent = revision(20);
        let present = revision(21);
        let absent_again = revision(22);
        runtime.publish_revision(absent, []).unwrap();
        runtime
            .publish_revision(present, [(input.clone(), 7)])
            .unwrap();
        runtime.publish_revision(absent_again, []).unwrap();

        let first = runtime.request(
            &family,
            absent,
            Key("helper"),
            CancellationToken::new(),
            |context| {
                Ok(QueryOutput::success(
                    context.optional_input(input.clone()).is_some(),
                ))
            },
        );
        assert_eq!(first.execution(), RequestExecution::Computed);
        assert_eq!(
            first.inputs(),
            &[InputObservation {
                input: input.clone(),
                stamp: 0,
            }]
        );

        let changed = runtime.request(
            &family,
            present,
            Key("helper"),
            CancellationToken::new(),
            |context| {
                Ok(QueryOutput::success(
                    context.optional_input(input.clone()).is_some(),
                ))
            },
        );
        assert_eq!(changed.execution(), RequestExecution::Computed);
        assert!(matches!(
            changed.terminal().unwrap().outcome(),
            QueryOutcome::Success(true)
        ));

        let absent_result = runtime.request(
            &family,
            absent_again,
            Key("helper"),
            CancellationToken::new(),
            |context| {
                Ok(QueryOutput::success(
                    context.optional_input(input.clone()).is_some(),
                ))
            },
        );
        assert!(matches!(
            absent_result.terminal().unwrap().outcome(),
            QueryOutcome::Success(false)
        ));
        assert_eq!(
            runtime.publish_revision(revision(23), [(input.clone(), 0)]),
            Err(RevisionError::ReservedInputStamp(input))
        );
    }

    #[test]
    fn family_policy_and_selection_preserve_last_good_above_terminals() {
        #[derive(Debug, Clone)]
        struct Value {
            canonical: u64,
            presentation: u64,
        }

        fn canonical_equal(left: &Value, right: &Value) -> bool {
            left.canonical == right.canonical
        }

        let runtime = QueryRuntime::new(1);
        publish_empty(&runtime, [revision(1), revision(2), revision(3)]);
        let family = runtime
            .family_with_equality::<Key, Value>("family-policy", 4, canonical_equal)
            .unwrap();
        let success = runtime
            .query(
                &family,
                revision(1),
                Key("selected"),
                CancellationToken::new(),
                |_| {
                    Ok(QueryOutput::success(Value {
                        canonical: 1,
                        presentation: 10,
                    }))
                },
            )
            .unwrap();
        let red = runtime
            .query(
                &family,
                revision(2),
                Key("selected"),
                CancellationToken::new(),
                |_| {
                    Ok(QueryOutput::success(Value {
                        canonical: 1,
                        presentation: 20,
                    }))
                },
            )
            .unwrap();
        assert_eq!(success.stamp(), red.stamp());
        let QueryOutcome::Success(value) = red.outcome() else {
            unreachable!()
        };
        assert_eq!(value.presentation, 20);

        let failure = runtime
            .query(
                &family,
                revision(3),
                Key("selected"),
                CancellationToken::new(),
                |_| Ok(QueryOutput::failure(QueryFailure::new("E", "failed"))),
            )
            .unwrap();
        let mut selection = family.selection();
        selection.publish(&red).unwrap();
        selection.publish(&failure).unwrap();
        assert!(Arc::ptr_eq(selection.current().unwrap(), &failure));
        assert!(Arc::ptr_eq(selection.last_good().unwrap(), &red));
    }
}
