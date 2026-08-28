//! Public query outcome, diagnostic, and attempt-report types.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::*;

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
    pub(crate) outcome: QueryOutcome<V>,
    pub(crate) kind: QueryTerminalKind,
    pub(crate) diagnostics: Vec<QueryDiagnostic>,
    pub(crate) work: Vec<WorkItem>,
    pub(crate) retained_value_charge: Option<u64>,
}

impl<V> QueryOutput<V> {
    /// Creates a successful output.
    pub fn success(value: V) -> Self {
        Self {
            outcome: QueryOutcome::Success(value),
            kind: QueryTerminalKind::Success,
            diagnostics: Vec::new(),
            work: Vec::new(),
            retained_value_charge: None,
        }
    }

    /// Creates a deterministic terminal failure.
    pub fn failure(failure: QueryFailure) -> Self {
        Self {
            outcome: QueryOutcome::Failure(failure),
            kind: QueryTerminalKind::Failure,
            diagnostics: Vec::new(),
            work: Vec::new(),
            retained_value_charge: None,
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

    /// Adds this success value's deterministic reachable heap charge.
    ///
    /// The terminal envelope already includes the inline `V` storage, so this
    /// charge is only for heap-owned allocations reachable through the value.
    /// The runtime adds the terminal envelope, diagnostics,
    /// structural work, dependencies, and inputs automatically. Shared
    /// allocations may be charged in full by every terminal which reaches them.
    /// Failure values ignore this override because their canonical strings are
    /// charged directly.
    pub fn with_retained_value_charge(mut self, bytes: u64) -> Self {
        self.retained_value_charge = Some(bytes);
        self
    }

    /// Applies the typed family's semantic success/failure classification.
    /// This is useful when both variants retain the same typed record shape.
    pub fn with_terminal_kind(mut self, kind: QueryTerminalKind) -> Self {
        self.kind = kind;
        self
    }
}

/// One non-semantic resource whose ownership follows a computed query attempt.
///
/// Evaluators register a handoff through
/// [`QueryContext::register_attempt_handoff`] while the attempt is active. The
/// resulting terminal retains the handoff internally while it is pending. The
/// runtime calls [`Self::commit`] only when the whole top-level rooted task that
/// observed it completes successfully. Speculative validation does not commit;
/// a later observed reuse can. Cancellation or panic while committing rolls the
/// attempted prefix back to pending for a later root, while terminal eviction
/// calls [`Self::abort`] to release a still-pending resource.
///
/// Handoffs are attempt-local control resources. They are never stored in
/// [`QueryOutput`] or [`QueryTerminal`], do not participate in equality or
/// stamps, and receive no [`QueryContext`], so committing one cannot execute a
/// nested dependency query. Public query requests made reentrantly by a commit
/// or abort callback fail immediately with [`QueryAbort::Canceled`]; callbacks
/// cannot publish work outside the enclosing root's rollback boundary.
pub trait QueryAttemptHandoff: fmt::Debug + Send + 'static {
    /// Transfer the resource at successful rooted-task completion.
    ///
    /// If this method unwinds, the runtime calls [`Self::abort`] in reverse order
    /// on this callback and the earlier attempted prefix. Callbacks not yet
    /// attempted remain untouched. Lifecycles whose attempted callbacks roll
    /// back are restored to pending before the original panic resumes, so
    /// `commit` may be called again on a later root. If an abort callback also
    /// unwinds, that lifecycle is permanently marked aborted; any terminal graph
    /// containing it becomes unavailable and cannot be reused as a partial
    /// publication.
    fn commit(&mut self);

    /// Roll back a partial commit, or release a pending resource on eviction.
    ///
    /// Implementations should not unwind. An unwind fails the lifecycle closed
    /// as described on [`Self::commit`]; it is never restored to pending.
    fn abort(&mut self);
}

/// An immutable published terminal.
#[derive(Debug)]
pub struct QueryTerminal<V> {
    pub(crate) family_token: FamilyToken,
    pub(crate) node: NodeIdentity,
    pub(crate) node_incarnation: u64,
    pub(crate) revision: Revision,
    pub(crate) stamp: u64,
    pub(crate) origin_request: u64,
    pub(crate) outcome: QueryOutcome<V>,
    pub(crate) kind: QueryTerminalKind,
    pub(crate) diagnostics: Arc<[QueryDiagnostic]>,
    pub(crate) work: Arc<[(Arc<str>, u64)]>,
    pub(crate) dependencies: Arc<[Observation]>,
    pub(crate) inputs: Arc<[InputObservation]>,
    /// Whether any input observation in this terminal's transitive cone
    /// recorded a missing leaf (stamp 0). A strictly-additive successor
    /// revision can change the meaning of a missing-leaf observation by
    /// adding that leaf, so only terminals with a fully present-observing
    /// cone may carry validation certificates across revisions (ADR-0073).
    pub(crate) cone_missing_observation: bool,
    pub(crate) retained_charge: u64,
    pub(crate) dependency_pin_charge: u64,
    pub(crate) pins: AtomicUsize,
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

    /// Opaque session-local incarnation of the node which owns this terminal,
    /// preventing stamp ABA after eviction.
    pub const fn node_incarnation(&self) -> u64 {
        self.node_incarnation
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

    /// Deterministic runtime-wide retained artifact charge.
    pub const fn retained_charge(&self) -> u64 {
        self.retained_charge
    }

    /// Retained dependency and input observation edges charged to this terminal.
    pub const fn dependency_pin_charge(&self) -> u64 {
        self.dependency_pin_charge
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

/// Result of a non-evaluating registered-query observation.
///
/// A ready hit returns the exact retained terminal and records the same
/// dependency/lease as an ordinary observed reuse. `Miss` means the key has no
/// retained node or current terminal; `NotReady` means a retained node exists
/// but its current-revision result is still being published or has an unsafe
/// handoff. Neither negative result creates a node or starts work.
#[derive(Debug, Clone)]
pub enum ReadyQueryProbe<V> {
    Ready(Arc<QueryTerminal<V>>),
    Miss,
    NotReady,
}

/// Runtime-owned immutable record of one nested query request.
///
/// The value remains owned by its typed terminal. This type-erased lifecycle
/// is sufficient for diagnostics, metrics, and provenance without forging a
/// second typed memo record in a compatibility adapter.
#[derive(Debug, Clone)]
pub struct NestedQueryAttempt {
    pub(crate) id: u64,
    pub(crate) node: NodeIdentity,
    pub(crate) node_incarnation: Option<u64>,
    pub(crate) origin_request: u64,
    pub(crate) execution: RequestExecution,
    pub(crate) terminal_revision: Option<Revision>,
    pub(crate) terminal_stamp: Option<u64>,
    pub(crate) abort: Option<QueryAbort>,
    pub(crate) dependencies: Arc<[Observation]>,
    pub(crate) inputs: Arc<[InputObservation]>,
    pub(crate) work: Arc<[(Arc<str>, u64)]>,
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
pub struct QueryRequestAttempt<V> {
    pub(crate) id: u64,
    pub(crate) origin_request: u64,
    pub(crate) execution: RequestExecution,
    pub(crate) terminal: Option<Arc<QueryTerminal<V>>>,
    pub(crate) abort: Option<QueryAbort>,
    pub(crate) dependencies: Arc<[Observation]>,
    pub(crate) inputs: Arc<[InputObservation]>,
    pub(crate) work: Arc<[(Arc<str>, u64)]>,
    pub(crate) nested_attempts: Arc<[NestedQueryAttempt]>,
    /// A retention lease on `terminal`, acquired while the producing task (and
    /// its request-scoped leases) were still alive, so the published result stays
    /// retained across the gap between this request completing and the caller
    /// registering a successor protection (a session/revision selection root via
    /// [`QuerySelection::publish`]). Its sole job is to bridge that gap.
    ///
    /// Once a successor protection exists, the bridge is redundant and must end
    /// promptly: an attempt record may be held for a long time (the compiler
    /// ledgers up to 256 completed attempts), and a lingering bridge pin would
    /// keep an otherwise-evictable terminal retained for the record's whole life.
    /// [`release_result_lease`](QueryRequestAttempt::release_result_lease) ends it
    /// explicitly the instant selection has pinned the same terminal — a pure
    /// narrowing of protection with no window in which the result is unprotected.
    /// Callers that never register a successor simply drop the attempt, releasing
    /// the bridge at teardown. Held behind a `Mutex` so the release is a `&self`
    /// operation on the possibly-shared attempt record.
    ///
    /// `None` for aborted or setup-failed requests, which carry no terminal.
    /// Type-erased over the family key so the attempt need not name `K`.
    pub(crate) result_lease: Mutex<Option<Box<dyn ObservedLease>>>,
}

impl<V: fmt::Debug> fmt::Debug for QueryRequestAttempt<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueryRequestAttempt")
            .field("id", &self.id)
            .field("origin_request", &self.origin_request)
            .field("execution", &self.execution)
            .field("terminal", &self.terminal)
            .field("abort", &self.abort)
            .field("dependencies", &self.dependencies)
            .field("inputs", &self.inputs)
            .field("work", &self.work)
            .field("nested_attempts", &self.nested_attempts)
            .field("result_lease", &lock(&self.result_lease).is_some())
            .finish()
    }
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

    /// Ends the attempt-carried bridge lease now that a successor protection
    /// holds the result.
    ///
    /// Call this immediately after registering a successor protection for the
    /// terminal — a [`QuerySelection::publish`] root, typically — and never
    /// before. The bridge lease exists only to keep the result retained between
    /// request completion and that registration; once the successor pins the same
    /// terminal, continuing to hold the bridge just pins an otherwise-evictable
    /// terminal for as long as the (possibly long-lived, ledgered) attempt record
    /// survives. Releasing here is a pure narrowing of protection: the successor
    /// still holds the terminal, so there is no instant in which it is
    /// unprotected. Idempotent, and a no-op for aborted or setup-failed attempts
    /// that never held a bridge lease. The released pin's own decrement and single
    /// retention pass run here, outside the attempt lock.
    pub fn release_result_lease(&self) {
        let released = lock(&self.result_lease).take();
        drop(released);
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
    pub(crate) inner: Arc<CancellationInner>,
}

#[derive(Debug, Default)]
pub(crate) struct CancellationInner {
    canceled: AtomicBool,
    next_watcher: AtomicU64,
    pub(crate) watchers: Mutex<Vec<(u64, Weak<WaitCell>)>>,
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
        watchers.retain(|(_, watcher)| {
            let Some(waiter) = watcher.upgrade() else {
                return false;
            };
            waiter.notify_all();
            true
        });
    }

    /// Whether cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.inner.canceled.load(Ordering::Acquire)
    }

    pub(crate) fn watch(&self, waiter: &Arc<WaitCell>) -> u64 {
        let id = self.inner.next_watcher.fetch_add(1, Ordering::Relaxed);
        lock(&self.inner.watchers).push((id, Arc::downgrade(waiter)));
        if self.is_canceled() {
            waiter.notify_all();
        }
        id
    }

    pub(crate) fn unwatch(&self, id: u64) {
        lock(&self.inner.watchers).retain(|(current, _)| *current != id);
    }
}

pub(crate) fn canonical_diagnostics(mut diagnostics: Vec<QueryDiagnostic>) -> Vec<QueryDiagnostic> {
    diagnostics.sort_by(|left, right| {
        (&left.identity, &left.payload, &left.presentation).cmp(&(
            &right.identity,
            &right.payload,
            &right.presentation,
        ))
    });
    diagnostics
}

pub(crate) fn semantic_diagnostics_equal(
    left: &[QueryDiagnostic],
    right: &[QueryDiagnostic],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.identity == right.identity && left.payload == right.payload)
}

pub(crate) fn outcomes_equal<V>(
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

/// Aggregate one attempt's work contributions, name-ordered.
///
/// A tree keyed by the metric name allocated a node per distinct identity and
/// compared names on the way to each, then a second allocation collected the
/// result out again. An attempt records a handful of identities, so sorting the
/// contributions and merging equal neighbours reaches the same name-ordered
/// vector with one allocation and `n log n` comparisons of a short list.
pub(crate) fn canonical_work(work: Vec<WorkItem>) -> Vec<(Arc<str>, u64)> {
    merge_sorted_by_identity(
        work.into_iter()
            .map(|item| (item.identity, item.amount))
            .collect(),
    )
}

pub(crate) fn canonical_reduced_work(work: Vec<(Arc<str>, u64)>) -> Vec<(Arc<str>, u64)> {
    merge_sorted_by_identity(work)
}

pub(crate) fn merge_sorted_by_identity(mut work: Vec<(Arc<str>, u64)>) -> Vec<(Arc<str>, u64)> {
    if work.len() > 1 {
        work.sort_by(|(left, _), (right, _)| left.cmp(right));
        let mut written = 0;
        for index in 1..work.len() {
            if work[written].0 == work[index].0 {
                let amount = work[index].1;
                work[written].1 += amount;
            } else {
                written += 1;
                work.swap(written, index);
            }
        }
        work.truncate(written + 1);
    }
    work
}

/// Canonicalizes the members of one detected cycle for rendering.
///
/// This is presentation, and it is deliberately the one place that orders
/// identities by their *text* rather than by the ADR-0074 structural pair. A
/// cycle is about to be shown to a person, its member list is tiny, and
/// consumers match members by name, so the rendered order and the rendered
/// de-duplication must both stay exactly what they were when identity was the
/// formatted family/key pair. Formatting here is the intended cold path and is
/// what the display-identity counters are for.
pub(crate) fn canonical_cycle(
    nodes: impl IntoIterator<Item = NodeIdentity>,
) -> Arc<[NodeIdentity]> {
    let mut nodes = nodes.into_iter().collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        left.family()
            .cmp(right.family())
            .then_with(|| left.key().cmp(right.key()))
    });
    nodes.dedup_by(|left, right| left.family() == right.family() && left.key() == right.key());
    nodes.into()
}
