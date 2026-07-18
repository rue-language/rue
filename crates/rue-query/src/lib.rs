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
pub trait QueryKey: Clone + Eq + Hash + Send + Sync + 'static {
    /// A stable, collision-free textual identity within the family.
    fn stable_identity(&self) -> String;
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

/// Stable identity of one logical memo node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIdentity {
    family: Arc<str>,
    key: Arc<str>,
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

/// Private computation output awaiting atomic publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutput<V> {
    outcome: QueryOutcome<V>,
    diagnostics: Vec<QueryDiagnostic>,
    work: Vec<WorkItem>,
}

impl<V> QueryOutput<V> {
    /// Creates a successful output.
    pub fn success(value: V) -> Self {
        Self {
            outcome: QueryOutcome::Success(value),
            diagnostics: Vec::new(),
            work: Vec::new(),
        }
    }

    /// Creates a deterministic terminal failure.
    pub fn failure(failure: QueryFailure) -> Self {
        Self {
            outcome: QueryOutcome::Failure(failure),
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
}

/// An immutable published terminal.
#[derive(Debug)]
pub struct QueryTerminal<V> {
    family_token: FamilyToken,
    node: NodeIdentity,
    node_incarnation: u64,
    revision: Revision,
    stamp: u64,
    outcome: QueryOutcome<V>,
    diagnostics: Arc<[QueryDiagnostic]>,
    work: Arc<[(Arc<str>, u64)]>,
    dependencies: Arc<[Observation]>,
    pins: AtomicUsize,
}

impl<V> QueryTerminal<V> {
    /// Logical node which owns this attempt.
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

    /// Canonical success or deterministic failure.
    pub fn outcome(&self) -> &QueryOutcome<V> {
        &self.outcome
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
    next_task: AtomicU64,
    next_family: AtomicU64,
    next_node: AtomicU64,
    metrics: Metrics,
    #[cfg(test)]
    test_events: TestEvents,
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
                nodes: Mutex::new(HashMap::new()),
                retention: Mutex::new(VecDeque::new()),
                retained_count: AtomicUsize::new(0),
                retained_nodes: AtomicUsize::new(0),
                retained_revisions: Mutex::new(BTreeMap::new()),
            }),
        })
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
        V: Clone + Eq + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        if !Arc::ptr_eq(&self.core, &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        let task = Arc::new(Task {
            id: TaskId(self.core.next_task.fetch_add(1, Ordering::Relaxed)),
            core: self.core.clone(),
            revision,
            cancellation,
            owns_permit: AtomicBool::new(false),
            stack: Mutex::new(Vec::new()),
        });
        family.query_task(task, key, compute)
    }

    /// Returns a point-in-time structural metrics snapshot.
    pub fn metrics(&self) -> RuntimeMetrics {
        self.core.metrics.snapshot()
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

/// Invalid family declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyError {
    /// Family names are stable identities and cannot be empty.
    EmptyName,
    /// A runtime already owns this stable family name.
    DuplicateName(Arc<str>),
}

/// A typed memo table sharing its runtime's scheduler and wait graph.
pub struct QueryFamily<K: QueryKey, V: Clone + Eq + Send + Sync + 'static> {
    core: Arc<RuntimeCore>,
    inner: Arc<FamilyInner<K, V>>,
}

impl<K, V> fmt::Debug for QueryFamily<K, V>
where
    K: QueryKey,
    V: Clone + Eq + Send + Sync + 'static,
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
    V: Clone + Eq + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            inner: self.inner.clone(),
        }
    }
}

#[derive(Debug)]
struct FamilyInner<K: QueryKey, V: Clone + Eq + Send + Sync + 'static> {
    name: Arc<str>,
    token: FamilyToken,
    retention_limit: usize,
    nodes: Mutex<HashMap<K, Arc<Node<V>>>>,
    retention: Mutex<VecDeque<RetentionEntry<V>>>,
    retained_count: AtomicUsize,
    retained_nodes: AtomicUsize,
    retained_revisions: Mutex<BTreeMap<Revision, usize>>,
}

#[derive(Debug)]
struct Node<V> {
    identity: NodeIdentity,
    incarnation: u64,
    users: AtomicUsize,
    wait: Arc<WaitCell>,
    state: Mutex<NodeState<V>>,
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
struct RetentionEntry<V> {
    node: Weak<Node<V>>,
    attempt: u64,
}

struct NodeLease<K: QueryKey, V: Clone + Eq + Send + Sync + 'static> {
    family: Weak<FamilyInner<K, V>>,
    key: K,
    node: Arc<Node<V>>,
}

impl<K, V> Drop for NodeLease<K, V>
where
    K: QueryKey,
    V: Clone + Eq + Send + Sync + 'static,
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
    V: Clone + Eq + Send + Sync + 'static,
{
    /// Returns deterministic retained-ownership gauges for this family.
    pub fn retention(&self) -> FamilyRetention {
        FamilyRetention {
            memo_nodes: self.inner.retained_nodes.load(Ordering::Relaxed),
            terminals: self.inner.retained_count.load(Ordering::Relaxed),
            terminal_limit: self.inner.retention_limit,
        }
    }

    fn node(&self, key: K) -> NodeLease<K, V> {
        let mut nodes = lock(&self.inner.nodes);
        let mut created = false;
        let node = nodes
            .entry(key.clone())
            .or_insert_with(|| {
                created = true;
                Arc::new(Node {
                    identity: NodeIdentity {
                        family: self.inner.name.clone(),
                        key: key.stable_identity().into(),
                    },
                    incarnation: self.core.next_node.fetch_add(1, Ordering::Relaxed),
                    users: AtomicUsize::new(0),
                    wait: Arc::new(WaitCell { cv: Condvar::new() }),
                    state: Mutex::new(NodeState {
                        next_attempt: 1,
                        next_stamp: 1,
                        attempts: VecDeque::new(),
                    }),
                })
            })
            .clone();
        if created {
            self.inner.retained_nodes.fetch_add(1, Ordering::Relaxed);
        }
        node.users.fetch_add(1, Ordering::AcqRel);
        NodeLease {
            family: Arc::downgrade(&self.inner),
            key,
            node,
        }
    }

    fn query_task<F>(
        &self,
        task: Arc<Task>,
        key: K,
        compute: F,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        let lease = self.node(key);
        let node = &lease.node;
        if let Some(cycle) = task.stack_cycle(&node.identity) {
            self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Cycle(cycle));
        }
        let mut compute = Some(compute);
        loop {
            if task.cancellation.is_canceled() {
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Canceled);
            }

            enum Action<V> {
                Return(Arc<QueryTerminal<V>>),
                Join { attempt: u64, owner: TaskId },
                Compute { attempt: u64 },
            }

            let action = {
                let mut state = lock(&node.state);
                let mut action = None;
                for attempt in state.attempts.iter_mut().rev() {
                    if !attempt.revision.is_compatible_with(task.revision) {
                        continue;
                    }
                    match &mut attempt.state {
                        AttemptState::Terminal { terminal, .. } => {
                            action = Some(Action::Return(terminal.clone()));
                        }
                        AttemptState::Computing { owner, waiters } => {
                            *waiters += 1;
                            action = Some(Action::Join {
                                attempt: attempt.id,
                                owner: *owner,
                            });
                        }
                    }
                    break;
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
                Action::Return(terminal) => {
                    self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
                    task.observe(&terminal);
                    return Ok(terminal);
                }
                Action::Join { attempt, owner } => {
                    self.core.metrics.joins.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    self.core.test_changed();
                    match self.join(&task, node, attempt, owner)? {
                        Some(terminal) => {
                            task.observe(&terminal);
                            return Ok(terminal);
                        }
                        None => continue,
                    }
                }
                Action::Compute { attempt } => {
                    self.core.metrics.claims.fetch_add(1, Ordering::Relaxed);
                    #[cfg(test)]
                    self.core.test_changed();
                    let acquired_here = task.acquire_permit(&self.core);
                    task.push(node.identity.clone());
                    self.core.metrics.body_entered();
                    let context = QueryContext {
                        task: task.clone(),
                        not_send_or_sync: PhantomData,
                    };
                    let body = catch_unwind(AssertUnwindSafe(|| {
                        compute.take().expect("query body executes at most once")(&context)
                    }));
                    self.core.metrics.body_left();
                    self.core
                        .metrics
                        .body_completions
                        .fetch_add(1, Ordering::Relaxed);
                    let dependencies = task.pop(&node.identity);

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
                            let terminal =
                                self.publish(node, attempt, task.revision, output, dependencies);
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            task.observe(&terminal);
                            return Ok(terminal);
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
                            return Err(abort);
                        }
                    }
                }
            }
        }
    }

    fn join(
        &self,
        task: &Arc<Task>,
        node: &Arc<Node<V>>,
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
        if let Err(cycle) = self.core.begin_wait(task.id, owner, node.identity.clone()) {
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

    fn abort_attempt(&self, node: &Arc<Node<V>>, attempt_id: u64) {
        let mut state = lock(&node.state);
        if let Some(index) = state.attempts.iter().position(|item| item.id == attempt_id) {
            state.attempts.remove(index);
        }
        drop(state);
        node.wait.cv.notify_all();
    }

    fn publish(
        &self,
        node: &Arc<Node<V>>,
        attempt_id: u64,
        revision: Revision,
        output: QueryOutput<V>,
        dependencies: Vec<Observation>,
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
            terminal.outcome == output.outcome
                && semantic_diagnostics_equal(&terminal.diagnostics, &diagnostics)
                && terminal.dependencies.as_ref() == dependencies.as_slice()
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
            outcome: output.outcome,
            diagnostics: diagnostics.into(),
            work: work.into(),
            dependencies: dependencies.into(),
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
                let mut nodes = lock(&self.inner.nodes);
                let key = nodes.iter().find_map(|(key, candidate)| {
                    Arc::ptr_eq(candidate, &node).then(|| key.clone())
                });
                if let Some(key) = key
                    && node.users.load(Ordering::Acquire) == 0
                    && lock(&node.state).attempts.is_empty()
                    && nodes
                        .get(&key)
                        .is_some_and(|candidate| Arc::ptr_eq(candidate, &node))
                {
                    nodes.remove(&key);
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
        }
    }
}

/// An explicit retained terminal root.
pub struct TerminalPin<K: QueryKey, V: Clone + Eq + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    terminal: Arc<QueryTerminal<V>>,
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
    V: Clone + Eq + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.terminal.pins.fetch_sub(1, Ordering::AcqRel);
        self.family.enforce_retention();
    }
}

/// An explicit current/last-good revision root.
pub struct RevisionPin<K: QueryKey, V: Clone + Eq + Send + Sync + 'static> {
    family: QueryFamily<K, V>,
    revision: Revision,
}

impl<K, V> Drop for RevisionPin<K, V>
where
    K: QueryKey,
    V: Clone + Eq + Send + Sync + 'static,
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

    /// Requests a dependency in the same task and pinned revision.
    pub fn query<K, V, F>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
        compute: F,
    ) -> Result<Arc<QueryTerminal<V>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Eq + Send + Sync + 'static,
        F: FnOnce(&QueryContext) -> Result<QueryOutput<V>, QueryAbort>,
    {
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        family.query_task(self.task.clone(), key, compute)
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
}

#[derive(Debug)]
struct TaskFrame {
    node: NodeIdentity,
    dependencies: BTreeMap<NodeIdentity, (u64, u64)>,
}

impl Task {
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

    fn push(&self, node: NodeIdentity) {
        lock(&self.stack).push(TaskFrame {
            node,
            dependencies: BTreeMap::new(),
        });
    }

    fn pop(&self, expected: &NodeIdentity) -> Vec<Observation> {
        let frame = lock(&self.stack)
            .pop()
            .expect("query computation owns one dependency frame");
        assert_eq!(&frame.node, expected);
        frame
            .dependencies
            .into_iter()
            .map(|(node, (incarnation, stamp))| Observation {
                node,
                incarnation,
                stamp,
            })
            .collect()
    }

    fn observe<V>(&self, terminal: &QueryTerminal<V>) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            frame.dependencies.insert(
                terminal.node.clone(),
                (terminal.node_incarnation, terminal.stamp),
            );
        }
    }

    fn stack_cycle(&self, node: &NodeIdentity) -> Option<Arc<[NodeIdentity]>> {
        let stack = lock(&self.stack);
        let start = stack.iter().position(|frame| &frame.node == node)?;
        Some(canonical_cycle(
            stack[start..]
                .iter()
                .map(|frame| frame.node.clone())
                .chain(std::iter::once(node.clone())),
        ))
    }
}

#[derive(Debug)]
struct WaitEdge {
    owner: TaskId,
    node: NodeIdentity,
}

impl RuntimeCore {
    #[cfg(test)]
    fn test_changed(&self) {
        *lock(&self.test_events.generation) += 1;
        self.test_events.changed.notify_all();
    }

    fn begin_wait(
        &self,
        waiter: TaskId,
        owner: TaskId,
        node: NodeIdentity,
    ) -> Result<(), Arc<[NodeIdentity]>> {
        let mut graph = lock(&self.wait_graph);
        graph.insert(waiter, WaitEdge { owner, node });
        let mut cursor = owner;
        let mut nodes = Vec::new();
        while let Some(edge) = graph.get(&cursor) {
            nodes.push(edge.node.clone());
            cursor = edge.owner;
            if cursor == waiter {
                nodes.push(
                    graph
                        .get(&waiter)
                        .expect("new wait edge is present")
                        .node
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

fn canonical_work(work: Vec<WorkItem>) -> Vec<(Arc<str>, u64)> {
    let mut aggregate = BTreeMap::<Arc<str>, u64>::new();
    for item in work {
        *aggregate.entry(item.identity).or_default() += item.amount;
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

    #[test]
    fn compatible_exact_key_is_computed_once_and_joined_by_many_waiters() {
        let runtime = QueryRuntime::new(8);
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
        assert_ne!(second.stamp(), third.stamp());
        assert_ne!(second.dependencies(), third.dependencies());
        assert!(runtime.metrics().red_publications >= 2);
    }

    #[test]
    fn canceling_a_waiter_does_not_cancel_shared_work() {
        let runtime = QueryRuntime::new(2);
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
        assert_ne!(first.stamp(), second.stamp());
    }
}
