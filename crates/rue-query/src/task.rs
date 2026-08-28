//! Task, handoff, and lease machinery behind attempt execution.

use std::any::Any;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
use std::time::Instant;

use ahash::{AHashMap, AHashSet};

use crate::*;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

thread_local! {
    static HANDOFF_CALLBACK_PHASE: Cell<Option<HandoffCallbackPhase>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandoffCallbackPhase {
    Commit,
    Abort,
}

pub(crate) struct HandoffCallbackGuard {
    previous: Option<HandoffCallbackPhase>,
}

impl HandoffCallbackGuard {
    fn enter(phase: HandoffCallbackPhase) -> Self {
        let previous = HANDOFF_CALLBACK_PHASE.with(|active| active.replace(Some(phase)));
        assert!(previous.is_none(), "attempt handoff callbacks do not nest");
        Self { previous }
    }

    pub(crate) fn active() -> bool {
        HANDOFF_CALLBACK_PHASE.with(|active| active.get().is_some())
    }
}

impl Drop for HandoffCallbackGuard {
    fn drop(&mut self) {
        HANDOFF_CALLBACK_PHASE.with(|active| active.set(self.previous));
    }
}

pub(crate) fn retain_task_observations(task: &Task) -> RetainedPinSet {
    let leases = lock(&task.leases);
    let mut retained = RetainedPinSet::new();
    for lease in &leases.held {
        retained.lease_erased(lease.duplicate());
    }
    retained
}

pub(crate) fn retain_task_family_observations(task: &Task, family: FamilyToken) -> RetainedPinSet {
    let leases = lock(&task.leases);
    let mut retained = RetainedPinSet::new();
    for lease in &leases.held {
        if lease.family_token() == family {
            retained.lease_erased(lease.duplicate());
        }
    }
    retained
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TaskId(pub(crate) u64);

#[derive(Debug, Default)]
pub(crate) struct ValidationEndorsementScope {
    /// Exact terminal identities are tested for membership on every validation
    /// memo probe. Their runtime-assigned numeric components need no ordering,
    /// so keep expected lookup constant instead of walking an ordered tree.
    pub(crate) identities: AHashSet<(u64, u64, Revision)>,
    /// Published pin sets borrowed as retention authority for this lexical
    /// scope. Holding the Arcs here keeps every indexed identity pinned until
    /// the enclosing guard drops.
    pub(crate) fallbacks: Vec<Arc<RetainedPinSet>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidationEndorsementAuthority {
    Inactive,
    Missing,
    TaskLocal,
    Borrowed,
}

/// Read-mostly retention authority shared by siblings in one structured batch.
///
/// A child publishes its whole proof universe when its registered request
/// reaches a terminal, and a concurrent batch additionally publishes each
/// freshly proved endorsement the moment it is endorsed, so siblings running
/// at the same time stop re-demanding a cone the first toucher has already
/// proved (RUE-1584). Under either path, exact endorsements become visible in
/// the same write transaction as the leases and fallback roots which back
/// them. Later siblings may therefore reuse current validation certificates
/// without rebuilding a cone that the batch already owns. The authority is
/// lexical: the batch and its children hold the sole Arcs, and its pins move
/// into the parent before the join returns.
pub(crate) struct BatchValidationAuthority {
    core: Arc<RuntimeCore>,
    pub(crate) parent: Option<Arc<BatchValidationAuthority>>,
    /// Whether this batch claimed extra workers. A sequential batch drains
    /// its queue one child at a time, so completion publication already
    /// makes every proof visible before the next child starts; per-proof
    /// publication would add write traffic with nothing to read it.
    concurrent: bool,
    pub(crate) state: RwLock<BatchValidationAuthorityState>,
}

#[derive(Default)]
pub(crate) struct BatchValidationAuthorityState {
    pub(crate) endorsements: AHashSet<(u64, u64, Revision)>,
    pub(crate) fallbacks: Vec<Arc<RetainedPinSet>>,
    pub(crate) leases: BatchValidationLeases,
    /// Exact terminals the spawning task already leases (RUE-1584). They are
    /// backed by the parent's held leases — which outlive this authority
    /// because the parent joins every child before its rooted request can
    /// release anything — so children may skip revalidating them without the
    /// authority holding pins of its own.
    seeded_leases: AHashSet<(u64, u64, Revision)>,
}

#[derive(Default)]
pub(crate) struct BatchValidationLeases {
    /// Exact terminal identities have runtime-assigned numeric components and
    /// no observable order. Match task-local lease deduplication instead of
    /// paying ordered-tree insertion for every completed batch child.
    observed: AHashSet<(u64, u64, Revision)>,
    pub(crate) held: Vec<Box<dyn ObservedLease>>,
}

impl Drop for BatchValidationLeases {
    fn drop(&mut self) {
        batched_release(&mut self.held);
    }
}

impl fmt::Debug for BatchValidationAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = read(&self.state);
        formatter
            .debug_struct("BatchValidationAuthority")
            .field("endorsements", &state.endorsements.len())
            .field("fallbacks", &state.fallbacks.len())
            .field("leases", &state.leases.held.len())
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

#[derive(Default)]
pub(crate) struct TaskQueryCache {
    /// One type-erased typed-key map per unforgeable family token. The token is
    /// runtime-unique, so the concrete `K` and `V` behind an entry cannot vary.
    /// Each erased map uses independently keyed AHash: source-derived keys keep
    /// adversarial collision resistance while exact `Eq` remains authoritative.
    /// A task belongs to exactly one runtime, so its monotonic family id is a
    /// complete local key and can use the runtime-owned integer hasher.
    families: HashMap<u64, Box<dyn Any + Send + Sync>, BuildHasherDefault<IncarnationHasher>>,
}

impl fmt::Debug for TaskQueryCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskQueryCache")
            .field("families", &self.families.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
pub(crate) struct PermitTiming {
    active_since: Option<Instant>,
    pub(crate) accumulated_ns: u64,
}

impl PermitTiming {
    fn acquired(&mut self) {
        assert!(self.active_since.replace(Instant::now()).is_none());
    }

    fn released(&mut self) {
        let started = self
            .active_since
            .take()
            .expect("an owned execution permit has an active timing interval");
        self.accumulated_ns = self
            .accumulated_ns
            .saturating_add(duration_ns(started.elapsed()));
    }
}

#[derive(Debug)]
pub(crate) struct Task {
    pub(crate) id: TaskId,
    pub(crate) core: Arc<RuntimeCore>,
    pub(crate) revision: Revision,
    /// Validation epoch of `revision`, resolved once at pinning (ADR-0073).
    pub(crate) revision_epoch: u64,
    pub(crate) cancellation: CancellationToken,
    pub(crate) owns_permit: AtomicBool,
    /// Task-local execution-permit intervals, published once when this task
    /// completes. Donation pauses the interval while the task is waiting.
    pub(crate) permit_timing: Mutex<PermitTiming>,
    /// Task-local maximum, merged into the runtime at task completion.
    pub(crate) longest_query_dependency_chain: AtomicU64,
    /// Top-level tasks publish directly. Batch children are reduced once at
    /// their bounded worker-completion boundary instead of contending on the
    /// runtime metrics for every ready item.
    pub(crate) publish_critical_path: bool,
    pub(crate) stack: Mutex<Vec<TaskFrame>>,
    /// Nodes whose evaluation structurally encloses this task but whose frames
    /// live on an ancestor task's stack. A registered batch runs its children on
    /// their own tasks, so without this the child's stack starts empty and a
    /// dependency cycle crossing a batch boundary is invisible to
    /// [`Task::stack_cycle`]. Carrying the enclosing chain keeps cycle detection
    /// structural and exact — a property of the request's shape, not of which
    /// task happened to reach a node first.
    pub(crate) ancestry: Arc<[ExactNodeIdentity]>,
    pub(crate) nested_attempts: Mutex<Vec<NestedQueryAttempt>>,
    /// Active operational-ledger selections. The top entry is already
    /// intersected with every parent scope, so one binary search decides
    /// whether a nested request row is materialized.
    pub(crate) nested_attempt_filters: Mutex<Vec<Arc<[Arc<str>]>>>,
    /// Lexical task-local registered-validation endorsements. An exact
    /// terminal identity is inserted into every active scope only after a
    /// complete registered-only validation traversal. Published fallback pin
    /// sets may also supply borrowed authority; every scope retains their Arcs
    /// so an indexed identity can never outlive its pin. Consequently the
    /// oldest active scope is the canonical union of all live authority.
    pub(crate) validation_endorsements: Mutex<Vec<ValidationEndorsementScope>>,
    /// Completed siblings' registered proofs and backing leases for the
    /// innermost structured batch containing this task. Nested batches link to
    /// the enclosing authority rather than copying its cone.
    pub(crate) batch_validation_authority: Option<Arc<BatchValidationAuthority>>,
    /// The structured task whose active validation scopes enclose this batch
    /// child. Propagating proof state through this weak, acyclic parent chain
    /// avoids allocating shared state for every ordinary validation traversal
    /// without extending a completed parent's lifetime.
    pub(crate) validation_proof_parent: Option<Weak<Task>>,
    /// Active recursive validation certificates local to this task.
    /// Encountering an unregistered node taints these and every enclosing
    /// task's active traversal through `validation_proof_parent`.
    pub(crate) validation_proofs: Mutex<ValidationProofStack>,
    /// High-frequency validation work accumulated on this rooted request and
    /// merged into the runtime once at task completion.
    pub(crate) validation_work: AtomicValidationWork,
    /// Request-scoped retention leases. This task, which owns one rooted request
    /// and all of its nested observations (nested queries share the task), holds
    /// one pin per distinct terminal it has observed. The pins release together
    /// when the task drops — i.e. when the whole rooted request completes, is
    /// canceled, or is abandoned — so an actively computing terminal is protected
    /// automatically while the request lives, and gains no permanent retention
    /// after it ends.
    pub(crate) leases: Mutex<TaskLeases>,
    /// Exact successful results already resolved by this rooted task, indexed
    /// by their typed family key. A repeat can reuse the task-owned terminal
    /// before touching the shared family memo index; `leases` keeps every
    /// cached terminal pinned for precisely the same task lifetime.
    pub(crate) query_cache: Mutex<TaskQueryCache>,
    /// Pending terminal handoffs observed anywhere in this rooted task,
    /// including nested queries. Only successful top-level completion claims
    /// and commits this aggregate; abort and unwind leave it `Pending`.
    pub(crate) observed_handoffs: Mutex<Vec<Arc<AttemptHandoffLifecycle>>>,
    /// Lifecycle identities whose complete dependency DAG has already been
    /// proven live by this rooted task. Every cached identity remains owned by
    /// an observed root lifecycle for the task's lifetime.
    pub(crate) checked_handoffs: Mutex<AHashSet<usize>>,
    #[cfg(test)]
    pub(crate) handoff_validation_visits: AtomicUsize,
    /// Ordered-index probes used by structural amplification tests. One
    /// identity lookup performs at most one probe, independent of endorsement
    /// count and lexical nesting depth.
    #[cfg(test)]
    pub(crate) validation_endorsement_index_probes: AtomicUsize,
}

pub(crate) struct ParentPermitDonation {
    task: Arc<Task>,
    pub(crate) donated: bool,
}

impl ParentPermitDonation {
    pub(crate) fn new(task: Arc<Task>) -> Self {
        let donated = task.release_permit(&task.core);
        Self { task, donated }
    }
}

impl Drop for ParentPermitDonation {
    fn drop(&mut self) {
        if self.donated {
            self.task.acquire_permit(&self.task.core);
        }
    }
}

pub(crate) struct BatchWorkerClaim {
    core: Arc<RuntimeCore>,
    pub(crate) count: usize,
}

impl BatchWorkerClaim {
    pub(crate) fn new(core: Arc<RuntimeCore>, desired: usize) -> Self {
        let limit = core.permits.maximum.saturating_sub(1);
        let mut current = core.batch_workers.load(Ordering::Acquire);
        let count = loop {
            let count = desired.min(limit.saturating_sub(current));
            match core.batch_workers.compare_exchange_weak(
                current,
                current + count,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break count,
                Err(actual) => current = actual,
            }
        };
        Self { core, count }
    }
}

impl Drop for BatchWorkerClaim {
    fn drop(&mut self) {
        self.core
            .batch_workers
            .fetch_sub(self.count, Ordering::AcqRel);
    }
}

pub(crate) struct StructuredWaitGuard {
    core: Arc<RuntimeCore>,
    parent: TaskId,
    children: Vec<TaskId>,
}

impl StructuredWaitGuard {
    pub(crate) fn new(
        core: Arc<RuntimeCore>,
        parent: TaskId,
        labels: Arc<dyn StructuredWaitLabels>,
        children: impl IntoIterator<Item = (TaskId, usize)>,
    ) -> Result<Self, Arc<[NodeIdentity]>> {
        let mut guard = Self {
            core,
            parent,
            children: Vec::new(),
        };
        for (child, index) in children {
            guard.core.begin_wait(
                parent,
                child,
                WaitEdgeLabel::Structured {
                    labels: labels.clone(),
                    index,
                },
            )?;
            guard.children.push(child);
        }
        Ok(guard)
    }
}

impl Drop for StructuredWaitGuard {
    fn drop(&mut self) {
        for child in self.children.drain(..) {
            self.core.end_wait(self.parent, child);
        }
    }
}

/// Unwind-safe scope for operational nested-attempt ledger filtering.
#[must_use = "dropping the guard immediately restores unfiltered nested-attempt recording"]
pub struct NestedAttemptFilterGuard {
    task: Arc<Task>,
    selection: Arc<[Arc<str>]>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

/// Unwind-safe lexical scope for task-local registered validation proofs.
#[must_use = "dropping the guard restores the preceding validation-authority scope"]
pub struct ValidationEndorsementGuard {
    task: Arc<Task>,
    scope: usize,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl Drop for ValidationEndorsementGuard {
    fn drop(&mut self) {
        let mut scopes = lock(&self.task.validation_endorsements);
        assert_eq!(
            scopes.len(),
            self.scope + 1,
            "validation endorsement guards drop in lexical order"
        );
        scopes.pop();
    }
}

pub(crate) struct ValidationProofGuard {
    task: Arc<Task>,
    depth: usize,
}

impl ValidationProofGuard {
    fn state(&self) -> u8 {
        *lock(&self.task.validation_proofs)
            .get(self.depth)
            .expect("validation proof guard owns one active traversal")
    }

    pub(crate) fn registered_only(&self) -> bool {
        self.state() == VALIDATION_PROOF_REGISTERED
    }

    pub(crate) fn retryable(&self) -> bool {
        self.state() == VALIDATION_PROOF_RETRYABLE
    }
}

impl Drop for ValidationProofGuard {
    fn drop(&mut self) {
        let mut proofs = lock(&self.task.validation_proofs);
        assert_eq!(
            proofs.len(),
            self.depth + 1,
            "validation proof guards drop in lexical order"
        );
        proofs.pop();
    }
}

impl Drop for NestedAttemptFilterGuard {
    fn drop(&mut self) {
        let popped = lock(&self.task.nested_attempt_filters)
            .pop()
            .expect("nested-attempt filter guard owns one active scope");
        assert!(
            Arc::ptr_eq(&popped, &self.selection),
            "nested-attempt filter guards drop in lexical order"
        );
    }
}

/// Terminals leased for the lifetime of one rooted request (its task).
#[derive(Default)]
pub(crate) struct TaskLeases {
    /// `(node incarnation, red/green stamp, terminal revision)` of every
    /// terminal this task has already leased, so re-observing a terminal never
    /// double-pins it. The revision is part of the identity because an
    /// adoption endorsement deliberately shares its predecessor's incarnation
    /// and stamp while being a DISTINCT terminal at the adopting revision —
    /// both must stay leased; collapsing them would leave the endorsement
    /// unprotected. Re-observations of one exact terminal still deduplicate.
    /// The runtime assigns every component and no consumer observes ordering,
    /// so high-frequency lease acquisition uses constant-expected membership.
    pub(crate) observed: AHashSet<(u64, u64, Revision)>,
    /// Live pins, type-erased across families. Dropping the task drops these,
    /// each of which decrements its terminal's pin count and re-enforces the
    /// owning family's retention bound.
    pub(crate) held: Vec<Box<dyn ObservedLease>>,
    /// Family/runtime retention passes deferred until this rooted request has
    /// finished publishing and all of its task leases can be released in the
    /// same one-per-family batch.
    deferred: DeferredEnforcements,
}

impl fmt::Debug for TaskLeases {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskLeases")
            .field("observed", &self.observed.len())
            .field("held", &self.held.len())
            .field("deferred_families", &self.deferred.enforcers.len())
            .finish()
    }
}

impl Drop for TaskLeases {
    /// Batched request-scoped lease release. A completed, canceled, or abandoned
    /// rooted request may hold thousands of observation pins in one family (the
    /// Caldera shape: a rooted request whose body publishes 10k+ terminals in a
    /// single family). Dropping the pins one at a time would run a full
    /// `enforce_retention` scan per pin — O(N²) precisely at request completion,
    /// while the family sits over its bound.
    ///
    /// Instead, release in two phases. First decrement every held pin
    /// (`release_deferred`), which never enforces: a decrement is a pure
    /// narrowing of protection, so ordering among released pins is free and no
    /// still-leased terminal is left unprotected. A release which removes a
    /// terminal's last pin yields a [`FamilyEnforcer`] keyed by its owning
    /// family's stable identity; overlapping pins need no scan. Enforcers are
    /// deduplicated so heterogeneous families collapse to one apiece.
    /// Second, run each distinct family's enforcement exactly once — after all of
    /// that family's decrements are visible. The result is O(pins) decrements
    /// plus O(distinct families) enforcement passes.
    fn drop(&mut self) {
        batched_release_into(&mut self.held, &mut self.deferred);
        std::mem::take(&mut self.deferred).enforce();
    }
}

impl BatchValidationAuthority {
    pub(crate) fn new(
        core: Arc<RuntimeCore>,
        parent: Option<Arc<BatchValidationAuthority>>,
        concurrent: bool,
    ) -> Self {
        Self {
            core,
            parent,
            concurrent,
            state: RwLock::new(BatchValidationAuthorityState::default()),
        }
    }

    /// The innermost enclosing authority whose batch actually claimed extra
    /// workers — the nearest scope where a per-proof publication has
    /// concurrent siblings to read it. Publishing at that level also covers
    /// sequential inner batches nested inside it: visibility probes walk the
    /// parent chain, so an outer sibling racing on the same cone finds the
    /// proof wherever an inner child proved it.
    pub(crate) fn nearest_concurrent(&self) -> Option<&BatchValidationAuthority> {
        let mut authority = Some(self);
        while let Some(current) = authority {
            if current.concurrent {
                return Some(current);
            }
            authority = current.parent.as_deref();
        }
        None
    }

    /// Atomically publishes one just-proved terminal together with a backing
    /// lease minted for it, so concurrently running siblings can borrow the
    /// proof before the proving child completes (RUE-1584). Same transaction
    /// contract as [`Self::publish_child`]: an identity becomes visible only
    /// in the write transaction that already retains its backing. `endorse`
    /// additionally records the exact endorsement; a lease-only publication
    /// still lets siblings skip revalidation of the exact terminal, while
    /// the taint discipline for non-registered-only certificates stands. A
    /// redundant lease is dropped outside the write lock, because releasing
    /// the last pin may enforce the owning family's retention limit and a
    /// sibling can validate through this authority while it owns the family
    /// lock.
    pub(crate) fn publish_proof(
        &self,
        identity: (u64, u64, Revision),
        lease: Box<dyn ObservedLease>,
        endorse: bool,
    ) {
        let mut duplicate = None;
        {
            let mut state = write(&self.state);
            if state.leases.observed.insert(identity) {
                state.leases.held.push(lease);
                self.core.metrics.task_lease_acquired();
            } else {
                duplicate = Some(lease);
            }
            if endorse {
                state.endorsements.insert(identity);
            }
        }
        drop(duplicate);
        #[cfg(test)]
        self.core.interpose(InterposeSite::BatchProofPublished);
    }

    /// Retains one published pin set as shared borrowing and promotion
    /// authority for this batch's siblings (RUE-1584). The Arc's pins back
    /// every stamp it retains, so unlike [`Self::publish_proof`] no separate
    /// lease transfer is needed; pointer identity deduplicates repeated
    /// publications of one set.
    pub(crate) fn publish_fallback(&self, fallback: &Arc<RetainedPinSet>) {
        let mut state = write(&self.state);
        if !state
            .fallbacks
            .iter()
            .any(|retained| Arc::ptr_eq(retained, fallback))
        {
            state.fallbacks.push(fallback.clone());
        }
    }

    /// Seeds this batch's shared authority with every identity the spawning
    /// task has already proven. A batch child starts with no task-local
    /// endorsements, so without the seed every batch begins blind to the
    /// parent's proofs and each child re-demands ubiquitous shared leaves —
    /// primitive layouts, standard-library producers — the parent proved
    /// moments earlier. The seeded identities carry no leases of their own:
    /// they are backed by the parent task's held leases, which outlive this
    /// authority because the parent joins every child before its rooted
    /// request can release them.
    pub(crate) fn seed_from_task(&self, task: &Task) {
        let seeded: Vec<(u64, u64, Revision)> = {
            let scopes = lock(&task.validation_endorsements);
            scopes
                .first()
                .map(|scope| scope.identities.iter().copied().collect())
                .unwrap_or_default()
        };
        // The parent's held leases carry proofs that are not endorsable —
        // shared leaves whose validation cannot participate in a
        // registered-only proof — under the same structured-lifetime backing
        // as the endorsements above (RUE-1584). Without them every batch
        // re-leases those leaves once per child.
        let seeded_leases: Vec<(u64, u64, Revision)> =
            lock(&task.leases).observed.iter().copied().collect();
        if seeded.is_empty() && seeded_leases.is_empty() {
            return;
        }
        let mut state = write(&self.state);
        state.endorsements.extend(seeded);
        state.seeded_leases.extend(seeded_leases);
    }

    /// Atomically publishes one completed child's proof and its retention
    /// backing. A child without a registered-validation scope keeps its state
    /// for the ordinary ordered parent absorption path.
    pub(crate) fn publish_child(&self, child: &Task) {
        let child_endorsements = {
            let mut scopes = lock(&child.validation_endorsements);
            if scopes.is_empty() {
                return;
            }
            std::mem::take(&mut *scopes)
        };
        let endorsement = child_endorsements
            .last()
            .expect("a nonempty endorsement scope has a canonical outer union");
        let mut duplicate_leases = Vec::new();
        let mut child_leases = lock(&child.leases);
        let mut state = write(&self.state);
        for lease in child_leases.held.drain(..) {
            let identity = lease.identity();
            if state.leases.observed.insert(identity) {
                state.leases.held.push(lease);
            } else {
                self.core.metrics.task_leases_released(1);
                // Releasing the last pin may enforce the owning family's
                // retention limit. Do not enter that family lock while the
                // batch authority write lock is held: a sibling can validate
                // through this authority while it owns the family lock.
                duplicate_leases.push(lease);
            }
        }
        child_leases.observed.clear();
        for fallback in &endorsement.fallbacks {
            if !state
                .fallbacks
                .iter()
                .any(|retained| Arc::ptr_eq(retained, fallback))
            {
                state.fallbacks.push(fallback.clone());
            }
        }
        // Publish identities last: every visible proof is now backed either by
        // an exact batch lease or by a fallback Arc retained in this state.
        state
            .endorsements
            .extend(endorsement.identities.iter().copied());
        drop(state);
        drop(child_leases);
        batched_release(&mut duplicate_leases);
    }

    pub(crate) fn retains_endorsement(
        &self,
        incarnation: u64,
        stamp: u64,
        exact_revision: Revision,
    ) -> bool {
        let mut authority = Some(self);
        while let Some(current) = authority {
            let state = read(&current.state);
            // An exact lease held by the batch is the batch-level form of "this
            // task has already leased the exact terminal": the authority holds
            // it for the batch's whole duration and its pins move into the
            // parent at the join, so a sibling may skip revalidation on it even
            // for proofs that are not endorsable (RUE-1584). The certificate
            // taint discipline is unchanged — a borrowed skip of a
            // non-registered-only certificate still taints the enclosing
            // proofs.
            if state
                .endorsements
                .contains(&(incarnation, stamp, exact_revision))
                || state
                    .leases
                    .observed
                    .contains(&(incarnation, stamp, exact_revision))
                || state
                    .seeded_leases
                    .contains(&(incarnation, stamp, exact_revision))
                || state
                    .fallbacks
                    .iter()
                    .any(|fallback| fallback.retains_stamp(incarnation, stamp))
            {
                return true;
            }
            authority = current.parent.as_deref();
        }
        false
    }

    /// Moves the joined batch's proof universe into its parent task before any
    /// completed child can drop. Ordered work, attempt, and handoff absorption
    /// remains separate and therefore preserves input-order reduction.
    pub(crate) fn absorb_into_task(&self, task: &Task) {
        let mut state = write(&self.state);
        let mut task_leases = lock(&task.leases);
        for lease in state.leases.held.drain(..) {
            let identity = lease.identity();
            if task_leases.observed.insert(identity) {
                task_leases.held.push(lease);
            } else {
                self.core.metrics.task_leases_released(1);
            }
        }
        state.leases.observed.clear();
        drop(task_leases);

        let fallbacks = std::mem::take(&mut state.fallbacks);
        let endorsements = std::mem::take(&mut state.endorsements);
        drop(state);
        for scope in lock(&task.validation_endorsements).iter_mut() {
            scope.identities.extend(endorsements.iter().copied());
            for fallback in &fallbacks {
                if !scope
                    .fallbacks
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, fallback))
                {
                    scope.fallbacks.push(fallback.clone());
                }
            }
        }
    }
}

impl Drop for BatchValidationAuthority {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.core
            .metrics
            .task_leases_released(state.leases.held.len());
    }
}

/// Two-phase batched release of a heterogeneous pin set. First decrement every
/// held pin (`release_deferred`), which never enforces — a decrement is a pure
/// narrowing of protection, so ordering among released pins is free and no
/// still-leased terminal is left unprotected. Only releases which remove a
/// terminal's last pin yield a [`FamilyEnforcer`]; they are keyed by stable
/// family identity and deduplicated to one enforcer per family.
/// Second, run each distinct family's enforcement exactly once — after all of
/// that family's decrements are visible. The result is O(pins) decrements plus
/// O(distinct families) enforcement passes. Shared by task-scoped
/// [`TaskLeases`] teardown and the session-held [`RetainedPinSet`].
pub(crate) fn batched_release(held: &mut Vec<Box<dyn ObservedLease>>) {
    if held.is_empty() {
        return;
    }
    let mut deferred = DeferredEnforcements::default();
    batched_release_into(held, &mut deferred);
    deferred.enforce();
}

pub(crate) fn batched_release_into(
    held: &mut Vec<Box<dyn ObservedLease>>,
    deferred: &mut DeferredEnforcements,
) {
    for lease in held.drain(..) {
        deferred.release(lease);
    }
}

/// A session-held set of request-scoped observation pins promoted out of a
/// completed rooted request and retained above the request's task.
///
/// [`TaskLeases`] retains a rooted request's observed terminals only for the
/// lifetime of its task; when the task drops the pins release. A caller that
/// wants a published root's exact observed terminals to stay retained *past*
/// the request — a session/revision selection root for a set of terminals
/// rather than a single one — acquires each pin while the request lease is
/// still live (so the terminal is continuously protected: the pin-under-lock
/// discipline that leaves no birth-eviction window) and transfers it here.
///
/// The set deduplicates by exact terminal identity `(node incarnation, red/green
/// stamp, terminal revision)`, so re-leasing the same terminal never double-pins
/// it. Release is the same two-phase batched teardown as [`TaskLeases`]: on drop
/// every held pin is decrement-released first, then each distinct family enforces
/// its retention bound exactly once — linear in the pin count, not quadratic,
/// which matters when a superseded root releases thousands of pins in one family.
///
/// This is the substrate for an atomic handoff between a superseded published
/// root and its successor: install the successor set (already holding every pin)
/// first, then drop the predecessor set, so no shared terminal is ever left
/// unprotected across the swap.
#[derive(Default)]
pub struct RetainedPinSet {
    /// `(node incarnation, stamp, terminal revision)` of every terminal already
    /// leased here, mirroring [`TaskLeases::observed`] so a redundant re-lease is
    /// dropped rather than double-held.
    pub(crate) observed: HashSet<(u64, u64, Revision), BuildHasherDefault<RetainedIdentityHasher>>,
    /// Node-incarnation/stamp identities retained by at least one exact
    /// terminal revision in this set. Query dependency edges use this same
    /// semantic identity, so any complete retained cone carrying the stamp can
    /// supply its representative terminal during final promotion.
    stamp_identities: HashSet<(u64, u64), BuildHasherDefault<RetainedIdentityHasher>>,
    /// Runtime identities represented by held pins. This makes same-runtime
    /// authority checks proportional to the number of fallback sets, not pins.
    runtime_identities: HashSet<u64, BuildHasherDefault<IncarnationHasher>>,
    /// Live pins, type-erased across families. Dropping the set drops these
    /// through the batched two-phase release.
    pub(crate) held: Vec<Box<dyn ObservedLease>>,
}

impl fmt::Debug for RetainedPinSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedPinSet")
            .field("observed", &self.observed.len())
            .field("stamp_identities", &self.stamp_identities.len())
            .field("runtime_identities", &self.runtime_identities)
            .field("held", &self.held.len())
            .finish()
    }
}

impl RetainedPinSet {
    /// An empty set holding no pins.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of distinct terminals currently held.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Whether the set holds no pins.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Transfer an already-acquired [`TerminalPin`] into the set, deduplicating
    /// by exact terminal identity. Returns whether the pin was newly retained; a
    /// redundant pin (same node incarnation, stamp, and revision already held) is
    /// dropped here rather than double-held, releasing it through the ordinary
    /// per-pin path. The caller must have acquired `pin` while the terminal was
    /// still protected — under the node lock at publication, or before the
    /// request lease that observed it releases — so there is no instant in which
    /// the terminal is exposed and evictable.
    pub fn lease<K, V>(&mut self, pin: TerminalPin<K, V>) -> bool
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let identity = (
            pin.terminal.node_incarnation,
            pin.terminal.stamp,
            pin.terminal.revision,
        );
        if self.observed.insert(identity) {
            self.stamp_identities.insert((identity.0, identity.1));
            self.runtime_identities.insert(pin.family.core.identity);
            pin.family.core.metrics.retained_pin_acquired();
            self.held.push(Box::new(pin));
            true
        } else {
            false
        }
    }

    /// Move every lease of `other` into this set, deduplicating by exact
    /// terminal identity. Metrics stay balanced: each moved lease releases the
    /// acquire its source set counted, then re-acquires only if this set
    /// actually retains it; a rejected duplicate simply drops its pin.
    pub fn absorb(&mut self, mut other: RetainedPinSet) {
        for lease in std::mem::take(&mut other.held) {
            lease.metrics().retained_pins_released(1);
            self.lease_erased(lease);
        }
    }

    pub(crate) fn lease_erased(&mut self, lease: Box<dyn ObservedLease>) -> bool {
        let identity = lease.identity();
        if self.observed.insert(identity) {
            self.stamp_identities.insert((identity.0, identity.1));
            self.runtime_identities.insert(lease.runtime_identity());
            lease.metrics().retained_pin_acquired();
            self.held.push(lease);
            true
        } else {
            false
        }
    }

    fn retains_stamp(&self, incarnation: u64, stamp: u64) -> bool {
        self.stamp_identities.contains(&(incarnation, stamp))
    }

    pub(crate) fn belongs_to_runtime(&self, runtime_identity: u64) -> bool {
        self.runtime_identities.is_empty()
            || (self.runtime_identities.len() == 1
                && self.runtime_identities.contains(&runtime_identity))
    }
}

impl Drop for RetainedPinSet {
    fn drop(&mut self) {
        for lease in &self.held {
            lease.metrics().retained_pins_released(1);
        }
        batched_release(&mut self.held);
    }
}

/// A request-scoped retention lease. The concrete implementor is a
/// [`TerminalPin`], which releases the pinned root on drop. Erasing the family
/// type parameters lets one task hold observation leases across every family it
/// touches without a second retention structure.
pub(crate) trait ObservedLease: Send + Sync {
    fn metrics(&self) -> &Metrics;
    fn runtime_identity(&self) -> u64;
    fn family_token(&self) -> FamilyToken;
    fn identity(&self) -> (u64, u64, Revision);

    fn dependencies(&self) -> &[Observation];

    fn duplicate(&self) -> Box<dyn ObservedLease>;

    /// Decrement-only release for batched teardown. Consumes the lease and
    /// decrements its terminal's pin count immediately. When that was the last
    /// pin, it defers the owning family's `enforce_retention` into the returned
    /// [`FamilyEnforcer`] rather than running it inline; an overlapping pin
    /// returns `None`. Callers dropping many pins at once decrement all of them
    /// first, then run one pass per affected family and one aggregate pass per
    /// runtime, keeping release linear instead of quadratic.
    fn release_deferred(self: Box<Self>) -> Option<FamilyEnforcer>;
}

impl<K, V> ObservedLease for TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn metrics(&self) -> &Metrics {
        &self.family.core.metrics
    }

    fn runtime_identity(&self) -> u64 {
        self.family.core.identity
    }

    fn family_token(&self) -> FamilyToken {
        self.family.inner.token
    }

    fn identity(&self) -> (u64, u64, Revision) {
        (
            self.terminal.node_incarnation,
            self.terminal.stamp,
            self.terminal.revision,
        )
    }

    fn dependencies(&self) -> &[Observation] {
        self.terminal.dependencies()
    }

    fn duplicate(&self) -> Box<dyn ObservedLease> {
        Box::new(
            self.family
                .pin_terminal(&self.terminal)
                .expect("a live terminal pin can be duplicated"),
        )
    }

    fn release_deferred(self: Box<Self>) -> Option<FamilyEnforcer> {
        // Narrow protection now: this pin no longer holds the terminal. This is
        // the same decrement `Drop` would perform, minus the enforcement pass.
        let previous = self.terminal.pins.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous > 0,
            "a deferred terminal pin releases exactly once"
        );
        // Suppress the `Drop` decrement/enforce so the boxed pin can free its
        // Arcs normally without double-releasing or scanning.
        self.deferred.store(true, Ordering::Relaxed);
        (previous == 1).then(|| self.family.retention_enforcer())
    }
}

/// A type-erased, per-family retention enforcement deferred out of a batched
/// lease release. Heterogeneous families produce heterogeneous enforcers; they
/// deduplicate by [`family_id`](Self::family_id) (the stable `FamilyInner`
/// address) so one enforcement runs per distinct family after every pin in that
/// family has been decrement-released.
pub(crate) struct FamilyEnforcer {
    pub(crate) family_id: usize,
    pub(crate) core: Arc<RuntimeCore>,
    pub(crate) enforce: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
pub(crate) struct DeferredEnforcements {
    enforcers: BTreeMap<usize, FamilyEnforcer>,
    runtimes: BTreeMap<u64, Arc<RuntimeCore>>,
}

impl DeferredEnforcements {
    fn insert(&mut self, enforcer: FamilyEnforcer) {
        self.runtimes
            .entry(enforcer.core.identity)
            .or_insert_with(|| enforcer.core.clone());
        self.enforcers.entry(enforcer.family_id).or_insert(enforcer);
    }

    fn release(&mut self, lease: Box<dyn ObservedLease>) {
        let Some(enforcer) = lease.release_deferred() else {
            return;
        };
        self.insert(enforcer);
    }

    fn enforce(self) {
        for (_family_id, enforcer) in self.enforcers {
            enforcer.enforce();
        }
        for (_runtime_id, runtime) in self.runtimes {
            runtime.enforce_runtime_retention();
        }
    }
}

impl FamilyEnforcer {
    /// Runs the deferred single-family enforcement pass exactly once.
    fn enforce(self) {
        (self.enforce)();
    }
}

pub(crate) type TaskDependencyEntry = (u64, NodeIdentity, u64);

#[derive(Debug)]
pub(crate) enum TaskDependencies {
    Empty,
    /// The common single-edge frame needs no side allocation. A second distinct
    /// edge promotes to expected-O(1) hashed membership so wide frames cannot
    /// turn repeated observation into quadratic work. Boxing the uncommon map
    /// keeps every task's frame compact, including zero- and one-edge frames.
    One(TaskDependencyEntry),
    Hashed(Box<AHashMap<u64, (NodeIdentity, u64)>>),
}

impl Default for TaskDependencies {
    fn default() -> Self {
        Self::Empty
    }
}

impl TaskDependencies {
    fn observe(&mut self, node: &NodeIdentity, incarnation: u64, stamp: u64) {
        match self {
            Self::Empty => *self = Self::One((incarnation, node.clone(), stamp)),
            Self::One((previous_incarnation, previous_node, previous_stamp)) => {
                if *previous_incarnation == incarnation {
                    assert_eq!(
                        &*previous_node, node,
                        "one runtime node incarnation must name exactly one display identity"
                    );
                    *previous_stamp = stamp;
                    return;
                }
                let mut hashed = Box::new(AHashMap::with_capacity(2));
                hashed.insert(
                    *previous_incarnation,
                    (previous_node.clone(), *previous_stamp),
                );
                hashed.insert(incarnation, (node.clone(), stamp));
                *self = Self::Hashed(hashed);
            }
            Self::Hashed(entries) => {
                if let Some((previous_node, _)) = entries.insert(incarnation, (node.clone(), stamp))
                {
                    assert_eq!(
                        &previous_node, node,
                        "one runtime node incarnation must name exactly one display identity"
                    );
                }
            }
        }
    }

    /// Publishes this frame's dependencies in the canonical order.
    ///
    /// RUE-1381 fixed a canonical order so two runs of the same compilation
    /// publish the same array; ADR-0074 redefines that order structurally as
    /// `(family, stable_hash, structural_collision_witness, incarnation)`. The
    /// two leading terms are integer comparisons over already-computed data,
    /// so completing a frame names no node; the witness is absent from that
    /// fast path and is computed only when two digests tie, which keeps a
    /// collision's relative order content-derived rather than
    /// allocation-ordered. The trailing incarnation is what separates two
    /// incarnations of one key, and is reached only for them.
    fn into_observations(self) -> Vec<Observation> {
        let mut observations: Vec<Observation> = match self {
            Self::Empty => Vec::new(),
            Self::One((incarnation, node, stamp)) => vec![Observation {
                node,
                incarnation,
                stamp,
            }],
            Self::Hashed(entries) => entries
                .into_iter()
                .map(|(incarnation, (node, stamp))| Observation {
                    node,
                    incarnation,
                    stamp,
                })
                .collect(),
        };
        observations.sort_unstable_by(|left, right| {
            left.node
                .cmp(&right.node)
                .then_with(|| left.incarnation.cmp(&right.incarnation))
        });
        observations
    }
}

#[derive(Debug)]
pub(crate) enum InlineOrderedMap<K, V> {
    Empty,
    /// Request-frame bookkeeping is commonly empty or has one identity. Keep
    /// that entry inline and allocate the ordered map only for a second key.
    One(K, V),
    Ordered(BTreeMap<K, V>),
}

impl<K, V> Default for InlineOrderedMap<K, V> {
    fn default() -> Self {
        Self::Empty
    }
}

impl<K: Ord, V> InlineOrderedMap<K, V> {
    pub(crate) fn insert_with(&mut self, key: K, value: V, merge: impl FnOnce(&mut V, V)) {
        match self {
            Self::Empty => *self = Self::One(key, value),
            Self::One(previous_key, previous_value) if previous_key == &key => {
                merge(previous_value, value);
            }
            Self::One(_, _) => {
                let Self::One(previous_key, previous_value) = std::mem::replace(self, Self::Empty)
                else {
                    unreachable!("the inline ordered map was matched as inline-one")
                };
                let mut ordered = BTreeMap::new();
                ordered.insert(previous_key, previous_value);
                ordered.insert(key, value);
                *self = Self::Ordered(ordered);
            }
            Self::Ordered(entries) => match entries.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(value);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge(entry.get_mut(), value);
                }
            },
        }
    }

    pub(crate) fn into_entries(self) -> Vec<(K, V)> {
        match self {
            Self::Empty => Vec::new(),
            Self::One(key, value) => vec![(key, value)],
            Self::Ordered(entries) => entries.into_iter().collect(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct TaskFrame {
    node: ExactNodeIdentity,
    dependencies: TaskDependencies,
    inputs: InlineOrderedMap<InputIdentity, u64>,
    work: InlineOrderedMap<Arc<str>, u64>,
    handoffs: Vec<Box<dyn QueryAttemptHandoff>>,
    pub(crate) observed_handoffs: Vec<Arc<AttemptHandoffLifecycle>>,
    /// Whether any observed dependency's cone recorded a missing leaf
    /// (ADR-0073). The frame's own missing-leaf inputs are derived from
    /// `inputs` at publication; this bit carries the transitive part.
    dependency_cone_missing: bool,
}

impl TaskFrame {
    fn observe_dependency(&mut self, node: &NodeIdentity, incarnation: u64, stamp: u64) {
        self.dependencies.observe(node, incarnation, stamp);
    }
}

pub(crate) struct TaskFrameOutput {
    pub(crate) dependencies: Vec<Observation>,
    pub(crate) inputs: Vec<InputObservation>,
    pub(crate) work: Vec<(Arc<str>, u64)>,
    pub(crate) handoffs: AttemptHandoffs,
    /// Whether the produced terminal's transitive cone observed a missing
    /// leaf: the frame's own stamp-0 inputs or any dependency's carried bit.
    pub(crate) cone_missing_observation: bool,
}

pub(crate) fn commit_handoff(
    handoff: &mut dyn QueryAttemptHandoff,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    catch_unwind(AssertUnwindSafe(|| {
        let _phase = HandoffCallbackGuard::enter(HandoffCallbackPhase::Commit);
        handoff.commit();
    }))
}

/// Positions a linear membership scan examined: everything up to and including
/// a hit, or the whole list on a miss. Asking at all costs the one question, so
/// this is never zero — which anchors a visits-per-lookup ratio at one instead
/// of at zero, where a scope that stopped being consulted would read as an
/// improvement.
pub(crate) fn scan_visits(found: Option<usize>, len: usize) -> u64 {
    found.map_or(len, |index| index + 1).max(1) as u64
}

pub(crate) fn abort_handoff(
    handoff: &mut dyn QueryAttemptHandoff,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    catch_unwind(AssertUnwindSafe(|| {
        let _phase = HandoffCallbackGuard::enter(HandoffCallbackPhase::Abort);
        handoff.abort();
    }))
}

impl TaskFrameOutput {
    pub(crate) fn abort_handoffs(self) {
        self.handoffs.abort();
    }
}

pub(crate) struct AttemptHandoffs {
    pub(crate) pending: Vec<Box<dyn QueryAttemptHandoff>>,
    pub(crate) observed: Vec<Arc<AttemptHandoffLifecycle>>,
}

impl AttemptHandoffs {
    pub(crate) fn into_lifecycle(mut self) -> Arc<AttemptHandoffLifecycle> {
        if self.pending.is_empty()
            && self
                .observed
                .iter()
                .all(|lifecycle| lifecycle.is_committed())
        {
            return AttemptHandoffLifecycle::shared_committed();
        }
        Arc::new(AttemptHandoffLifecycle::new(
            std::mem::take(&mut self.pending),
            std::mem::take(&mut self.observed),
        ))
    }

    pub(crate) fn abort(mut self) {
        self.abort_pending();
    }

    fn abort_pending(&mut self) {
        for handoff in self.pending.iter_mut().rev() {
            let _ = abort_handoff(&mut **handoff);
        }
        self.pending.clear();
    }
}

impl Drop for AttemptHandoffs {
    fn drop(&mut self) {
        self.abort_pending();
    }
}

#[derive(Debug)]
pub(crate) struct AttemptHandoffLifecycle {
    pub(crate) observed: Arc<[Arc<AttemptHandoffLifecycle>]>,
    state: Mutex<AttemptHandoffState>,
    completed: Arc<WaitCell>,
}

#[derive(Debug)]
pub(crate) enum AttemptHandoffState {
    Pending(Vec<Box<dyn QueryAttemptHandoff>>),
    Committing { owner: TaskId },
    Committed,
    Aborted,
}

pub(crate) enum AttemptHandoffCommit {
    Claimed(Vec<Box<dyn QueryAttemptHandoff>>),
    Committed,
    Aborted,
    Canceled,
}

pub(crate) enum RootHandoffCommitFailure {
    Canceled,
    Invalidated,
    Panicked(Box<dyn std::any::Any + Send>),
}

pub(crate) type HandoffCommitBatch = (
    Arc<AttemptHandoffLifecycle>,
    Vec<Box<dyn QueryAttemptHandoff>>,
);

pub(crate) fn rollback_handoff_batches(
    owner: TaskId,
    mut batches: Vec<HandoffCommitBatch>,
    attempted_callbacks: Vec<usize>,
) {
    assert_eq!(
        batches.len(),
        attempted_callbacks.len(),
        "root rollback records one attempted prefix per claimed lifecycle"
    );
    let mut rollback_safe = vec![true; batches.len()];
    for (index, (_, handoffs)) in batches.iter_mut().enumerate().rev() {
        let attempted = attempted_callbacks[index];
        assert!(
            attempted <= handoffs.len(),
            "an attempted callback prefix cannot exceed its lifecycle"
        );
        for handoff in handoffs[..attempted].iter_mut().rev() {
            if abort_handoff(&mut **handoff).is_err() {
                rollback_safe[index] = false;
            }
        }
        if !rollback_safe[index] {
            // This lifecycle can no longer be retried. Abort its untouched
            // suffix as fail-closed cleanup, but preserve untouched callbacks
            // in every lifecycle whose attempted prefix rolled back cleanly.
            for handoff in handoffs[attempted..].iter_mut().rev() {
                let _ = abort_handoff(&mut **handoff);
            }
        }
    }
    for ((lifecycle, handoffs), rollback_safe) in batches.into_iter().zip(rollback_safe) {
        if rollback_safe {
            lifecycle.rollback_commit(owner, handoffs);
        } else {
            lifecycle.abort_failed_commit(owner, handoffs);
        }
    }
}

impl AttemptHandoffLifecycle {
    pub(crate) fn new(
        handoffs: Vec<Box<dyn QueryAttemptHandoff>>,
        observed: Vec<Arc<AttemptHandoffLifecycle>>,
    ) -> Self {
        Self {
            observed: observed.into(),
            state: Mutex::new(AttemptHandoffState::Pending(handoffs)),
            completed: Arc::new(WaitCell {
                cv: Condvar::new(),
                generation: Mutex::new(0),
            }),
        }
    }

    pub(crate) fn committed() -> Self {
        Self {
            observed: Arc::from([]),
            state: Mutex::new(AttemptHandoffState::Committed),
            completed: Arc::new(WaitCell {
                cv: Condvar::new(),
                generation: Mutex::new(0),
            }),
        }
    }

    pub(crate) fn shared_committed() -> Arc<Self> {
        Self::shared_committed_ref().clone()
    }

    fn shared_committed_ref() -> &'static Arc<Self> {
        static COMMITTED: std::sync::OnceLock<Arc<AttemptHandoffLifecycle>> =
            std::sync::OnceLock::new();
        COMMITTED.get_or_init(|| Arc::new(AttemptHandoffLifecycle::committed()))
    }

    pub(crate) fn is_committed(&self) -> bool {
        matches!(*lock(&self.state), AttemptHandoffState::Committed)
    }

    #[cfg(test)]
    pub(crate) fn collect_observed(lifecycle: &Arc<Self>, observed: &mut Vec<Arc<Self>>) -> bool {
        let mut seen = observed
            .iter()
            .map(|lifecycle| Arc::as_ptr(lifecycle) as usize)
            .collect::<HashSet<_>>();
        Self::collect_observed_once(lifecycle, observed, &mut seen)
    }

    fn collect_observed_once(
        lifecycle: &Arc<Self>,
        observed: &mut Vec<Arc<Self>>,
        seen: &mut HashSet<usize>,
    ) -> bool {
        {
            let state = lock(&lifecycle.state);
            match &*state {
                AttemptHandoffState::Committed => return true,
                AttemptHandoffState::Aborted => return false,
                AttemptHandoffState::Pending(_) | AttemptHandoffState::Committing { .. } => {}
            }
        }
        let identity = Arc::as_ptr(lifecycle) as usize;
        if !seen.insert(identity) {
            return true;
        }
        for dependency in lifecycle.observed.iter() {
            if !Self::collect_observed_once(dependency, observed, seen) {
                return false;
            }
        }
        observed.push(lifecycle.clone());
        true
    }

    pub(crate) fn begin_commit(
        &self,
        owner: TaskId,
        cancellation: &CancellationToken,
        _core: &RuntimeCore,
    ) -> AttemptHandoffCommit {
        let cancellation_watch = cancellation.watch(&self.completed);
        let result = loop {
            if cancellation.is_canceled() {
                break AttemptHandoffCommit::Canceled;
            }
            let mut state = lock(&self.state);
            match &mut *state {
                AttemptHandoffState::Committed => break AttemptHandoffCommit::Committed,
                AttemptHandoffState::Aborted => break AttemptHandoffCommit::Aborted,
                AttemptHandoffState::Committing { .. } => {
                    drop(state);
                    self.completed.wait_until(
                        || {
                            cancellation.is_canceled()
                                || !matches!(
                                    *lock(&self.state),
                                    AttemptHandoffState::Committing { .. }
                                )
                        },
                        #[cfg(test)]
                        || _core.interpose(InterposeSite::HandoffCommitPark),
                    );
                }
                AttemptHandoffState::Pending(_) => {
                    let AttemptHandoffState::Pending(handoffs) =
                        std::mem::replace(&mut *state, AttemptHandoffState::Aborted)
                    else {
                        unreachable!("pending handoffs were selected above")
                    };
                    *state = AttemptHandoffState::Committing { owner };
                    break AttemptHandoffCommit::Claimed(handoffs);
                }
            }
        };
        cancellation.unwatch(cancellation_watch);
        result
    }

    pub(crate) fn finish_commit(&self, owner: TaskId) {
        let mut state = lock(&self.state);
        let AttemptHandoffState::Committing { owner: current } = &*state else {
            panic!("only a committing attempt handoff may finish root commit")
        };
        assert_eq!(*current, owner);
        *state = AttemptHandoffState::Committed;
        drop(state);
        self.completed.notify_all();
    }

    pub(crate) fn rollback_commit(
        &self,
        owner: TaskId,
        handoffs: Vec<Box<dyn QueryAttemptHandoff>>,
    ) {
        let mut state = lock(&self.state);
        let AttemptHandoffState::Committing { owner: current } = &*state else {
            panic!("only a committing attempt handoff may roll back")
        };
        assert_eq!(*current, owner);
        *state = AttemptHandoffState::Pending(handoffs);
        drop(state);
        self.completed.notify_all();
    }

    pub(crate) fn abort_failed_commit(
        &self,
        owner: TaskId,
        handoffs: Vec<Box<dyn QueryAttemptHandoff>>,
    ) {
        let mut state = lock(&self.state);
        let AttemptHandoffState::Committing { owner: current } = &*state else {
            panic!("only a committing attempt handoff may fail rollback")
        };
        assert_eq!(*current, owner);
        *state = AttemptHandoffState::Aborted;
        drop(state);
        drop(handoffs);
        self.completed.notify_all();
    }

    pub(crate) fn abort(&self) {
        let handoffs = loop {
            let mut state = lock(&self.state);
            match &*state {
                AttemptHandoffState::Committed | AttemptHandoffState::Aborted => return,
                AttemptHandoffState::Committing { .. } => {
                    drop(state);
                    self.completed.wait_until(
                        || !matches!(*lock(&self.state), AttemptHandoffState::Committing { .. }),
                        #[cfg(test)]
                        || {},
                    );
                }
                AttemptHandoffState::Pending(_) => {
                    let AttemptHandoffState::Pending(handoffs) =
                        std::mem::replace(&mut *state, AttemptHandoffState::Aborted)
                    else {
                        unreachable!("pending handoffs were selected above")
                    };
                    break handoffs;
                }
            }
        };
        for mut handoff in handoffs.into_iter().rev() {
            let _ = abort_handoff(&mut *handoff);
        }
        self.completed.notify_all();
    }
}

impl Drop for AttemptHandoffLifecycle {
    fn drop(&mut self) {
        self.abort();
    }
}

impl Task {
    pub(crate) fn cached_query<K, V>(
        &self,
        family: FamilyToken,
        key: &K,
    ) -> Option<Arc<QueryTerminal<V>>>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let cache = lock(&self.query_cache);
        let family_cache = cache.families.get(&family.family)?;
        family_cache
            .downcast_ref::<AHashMap<K, Arc<QueryTerminal<V>>>>()
            .expect("a family token has one typed task-cache representation")
            .get(key)
            .cloned()
    }

    pub(crate) fn cache_query<K, V>(
        &self,
        family: FamilyToken,
        key: &K,
        terminal: &Arc<QueryTerminal<V>>,
    ) where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let mut cache = lock(&self.query_cache);
        let family_cache = cache
            .families
            .entry(family.family)
            .or_insert_with(|| Box::new(AHashMap::<K, Arc<QueryTerminal<V>>>::new()));
        let family_cache = family_cache
            .downcast_mut::<AHashMap<K, Arc<QueryTerminal<V>>>>()
            .expect("a family token has one typed task-cache representation");
        match family_cache.entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(terminal.clone());
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                let previous = entry.get();
                assert_eq!(previous.node_incarnation, terminal.node_incarnation);
                assert_eq!(previous.stamp, terminal.stamp);
                assert_eq!(previous.revision, terminal.revision);
            }
        }
    }

    pub(crate) fn defer_pin_release<K, V>(&self, pin: TerminalPin<K, V>)
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        lock(&self.leases).deferred.release(Box::new(pin));
    }

    pub(crate) fn defer_family_enforcement(&self, enforcer: FamilyEnforcer) {
        lock(&self.leases).deferred.insert(enforcer);
    }

    pub(crate) fn batch_child(
        self: &Arc<Self>,
        id: u64,
        authority: Arc<BatchValidationAuthority>,
    ) -> Arc<Self> {
        let inherited_filter = lock(&self.nested_attempt_filters).last().cloned();
        let inherited_validation_fallbacks = lock(&self.validation_endorsements)
            .first()
            .map(|scope| scope.fallbacks.clone());
        // The child evaluates underneath every node this task is currently
        // inside, so those frames enclose it exactly as if they were its own.
        let inherited_ancestry: Arc<[ExactNodeIdentity]> = self
            .ancestry
            .iter()
            .cloned()
            .chain(lock(&self.stack).iter().map(|frame| frame.node.clone()))
            .collect();
        Arc::new(Self {
            id: TaskId(id),
            core: self.core.clone(),
            revision: self.revision,
            revision_epoch: self.revision_epoch,
            cancellation: self.cancellation.clone(),
            owns_permit: AtomicBool::new(false),
            permit_timing: Mutex::new(PermitTiming::default()),
            longest_query_dependency_chain: AtomicU64::new(0),
            publish_critical_path: false,
            stack: Mutex::new(Vec::new()),
            ancestry: inherited_ancestry,
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(inherited_filter.into_iter().collect()),
            // A batch child is a structured descendant of the lexical proof
            // scope. It starts with no task-local endorsements, but keeps the
            // same borrowed published authority live while it validates its
            // own selected cone.
            validation_endorsements: Mutex::new(
                inherited_validation_fallbacks
                    .map(|fallbacks| ValidationEndorsementScope {
                        identities: AHashSet::new(),
                        fallbacks,
                    })
                    .into_iter()
                    .collect(),
            ),
            batch_validation_authority: Some(authority),
            validation_proof_parent: Some(Arc::downgrade(self)),
            validation_proofs: Mutex::new(ValidationProofStack::new()),
            validation_work: AtomicValidationWork::default(),
            leases: Mutex::new(TaskLeases::default()),
            query_cache: Mutex::new(TaskQueryCache::default()),
            observed_handoffs: Mutex::new(Vec::new()),
            checked_handoffs: Mutex::new(AHashSet::new()),
            #[cfg(test)]
            handoff_validation_visits: AtomicUsize::new(0),
            #[cfg(test)]
            validation_endorsement_index_probes: AtomicUsize::new(0),
        })
    }

    pub(crate) fn absorb_batch_child(&self, child: &Arc<Self>, transfer_handoffs: bool) {
        self.validation_work.add(child.validation_work.take());

        let mut child_leases = lock(&child.leases);
        let mut parent_leases = lock(&self.leases);
        for lease in child_leases.held.drain(..) {
            let identity = lease.identity();
            if parent_leases.observed.insert(identity) {
                parent_leases.held.push(lease);
            } else {
                self.core.metrics.task_leases_released(1);
            }
        }
        child_leases.observed.clear();
        drop(parent_leases);
        drop(child_leases);

        let child_endorsements = std::mem::take(&mut *lock(&child.validation_endorsements));
        if let Some(child_endorsements) = child_endorsements.last() {
            for scope in lock(&self.validation_endorsements).iter_mut() {
                scope
                    .identities
                    .extend(child_endorsements.identities.iter().copied());
                for fallback in &child_endorsements.fallbacks {
                    if !scope
                        .fallbacks
                        .iter()
                        .any(|retained| Arc::ptr_eq(retained, fallback))
                    {
                        scope.fallbacks.push(fallback.clone());
                    }
                }
            }
        }

        let child_attempts = std::mem::take(&mut *lock(&child.nested_attempts));
        lock(&self.nested_attempts).extend(child_attempts);

        let child_handoffs = std::mem::take(&mut *lock(&child.observed_handoffs));
        if !transfer_handoffs {
            return;
        }
        lock(&self.checked_handoffs).extend(std::mem::take(&mut *lock(&child.checked_handoffs)));
        assert!(
            child_handoffs.len() <= 1,
            "a structured child returns at most one encapsulating root lifecycle"
        );
        for handoff in child_handoffs {
            assert!(
                self.observe_handoff(handoff),
                "a successful structured child returns a live handoff lifecycle"
            );
        }
    }

    pub(crate) fn next_nested_request(&self) -> u64 {
        self.core.next_task.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn push_nested_attempt_filter(
        self: &Arc<Self>,
        families: &[&str],
    ) -> NestedAttemptFilterGuard {
        let mut selection = families
            .iter()
            .copied()
            .map(Arc::<str>::from)
            .collect::<Vec<_>>();
        selection.sort();
        selection.dedup();
        let mut filters = lock(&self.nested_attempt_filters);
        if let Some(parent) = filters.last() {
            selection.retain(|family| parent.binary_search(family).is_ok());
        }
        let selection: Arc<[Arc<str>]> = selection.into();
        filters.push(selection.clone());
        NestedAttemptFilterGuard {
            task: self.clone(),
            selection,
            not_send_or_sync: PhantomData,
        }
    }

    fn records_nested_attempt(&self, family: &str) -> bool {
        lock(&self.nested_attempt_filters)
            .last()
            .is_none_or(|selection| {
                selection
                    .binary_search_by(|candidate| candidate.as_ref().cmp(family))
                    .is_ok()
            })
    }

    pub(crate) fn push_validation_endorsement_scope(
        self: &Arc<Self>,
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<ValidationEndorsementGuard, RetainTerminalConeError> {
        if fallbacks
            .iter()
            .any(|fallback| !fallback.belongs_to_runtime(self.core.identity))
        {
            return Err(RetainTerminalConeError::ForeignRuntime);
        }
        let mut scopes = lock(&self.validation_endorsements);
        let scope = scopes.len();
        let mut retained_fallbacks = scopes
            .first()
            .map(|scope| scope.fallbacks.clone())
            .unwrap_or_default();
        for fallback in fallbacks {
            if !retained_fallbacks
                .iter()
                .any(|retained| Arc::ptr_eq(retained, fallback))
            {
                retained_fallbacks.push(fallback.clone());
            }
        }
        // A fallback introduced by a nested scope remains live until the
        // oldest enclosing scope drops. Endorsements proven while nested are
        // inserted into every scope, so their borrowed authority must have the
        // same enclosing lifetime.
        for active in &mut *scopes {
            for fallback in &retained_fallbacks {
                if !active
                    .fallbacks
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, fallback))
                {
                    active.fallbacks.push(fallback.clone());
                }
            }
        }
        scopes.push(ValidationEndorsementScope {
            identities: AHashSet::new(),
            fallbacks: retained_fallbacks,
        });
        Ok(ValidationEndorsementGuard {
            task: self.clone(),
            scope,
            not_send_or_sync: PhantomData,
        })
    }

    #[cfg(test)]
    pub(crate) fn validation_endorsement_authority_for_terminal<V>(
        &self,
        terminal: &QueryTerminal<V>,
    ) -> ValidationEndorsementAuthority {
        let authority = self.validation_endorsement_authority_at_raw(
            terminal.node_incarnation,
            terminal.stamp,
            terminal.revision,
        );
        if authority != ValidationEndorsementAuthority::Inactive {
            // This helper inspects authority in tests; it does not bypass
            // validation, so preserve the production counter's meaning.
            self.record_validation_endorsement_outcome(false);
        }
        authority
    }

    /// Tests the only retention authority which may bypass validation of a
    /// candidate root. Published fallback sets retain equal dependency-cone
    /// representatives, but deliberately do not prove the candidate root's
    /// own direct observations current. The candidate path therefore needs to
    /// distinguish exact task-local authority only; scanning fallback indexes
    /// would produce the same ordinary-validation decision after extra work.
    pub(crate) fn validation_candidate_endorsement_authority_for_terminal<V>(
        &self,
        terminal: &QueryTerminal<V>,
    ) -> ValidationEndorsementAuthority {
        let scopes = lock(&self.validation_endorsements);
        let Some(scope) = scopes.first() else {
            return ValidationEndorsementAuthority::Inactive;
        };
        #[cfg(test)]
        self.validation_endorsement_index_probes
            .fetch_add(1, Ordering::Relaxed);
        let authority = if scope.identities.contains(&(
            terminal.node_incarnation,
            terminal.stamp,
            terminal.revision,
        )) {
            ValidationEndorsementAuthority::TaskLocal
        } else {
            ValidationEndorsementAuthority::Missing
        };
        drop(scopes);
        self.record_validation_endorsement_outcome(
            authority == ValidationEndorsementAuthority::TaskLocal,
        );
        authority
    }

    pub(crate) fn validation_endorsement_authority_at(
        &self,
        incarnation: u64,
        stamp: u64,
        exact_revision: Revision,
    ) -> ValidationEndorsementAuthority {
        let authority =
            self.validation_endorsement_authority_at_raw(incarnation, stamp, exact_revision);
        if authority != ValidationEndorsementAuthority::Inactive {
            self.record_validation_endorsement_outcome(matches!(
                authority,
                ValidationEndorsementAuthority::TaskLocal
                    | ValidationEndorsementAuthority::Borrowed
            ));
        }
        authority
    }

    fn validation_endorsement_authority_at_raw(
        &self,
        incarnation: u64,
        stamp: u64,
        exact_revision: Revision,
    ) -> ValidationEndorsementAuthority {
        let scopes = lock(&self.validation_endorsements);
        let Some(scope) = scopes.first() else {
            return ValidationEndorsementAuthority::Inactive;
        };
        #[cfg(test)]
        self.validation_endorsement_index_probes
            .fetch_add(1, Ordering::Relaxed);
        let task_local = scope
            .identities
            .contains(&(incarnation, stamp, exact_revision));
        let authority = if task_local {
            ValidationEndorsementAuthority::TaskLocal
        } else if scope
            .fallbacks
            .iter()
            .any(|fallback| fallback.retains_stamp(incarnation, stamp))
            || self
                .batch_validation_authority
                .as_ref()
                .is_some_and(|authority| {
                    authority.retains_endorsement(incarnation, stamp, exact_revision)
                })
        {
            ValidationEndorsementAuthority::Borrowed
        } else {
            ValidationEndorsementAuthority::Missing
        };
        authority
    }

    fn record_validation_endorsement_outcome(&self, hit: bool) {
        if hit {
            self.validation_work
                .endorsement_hits
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.validation_work
                .endorsement_misses
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub(crate) fn validation_endorsed_identity(
        &self,
        incarnation: u64,
        stamp: u64,
        exact_revision: Revision,
    ) -> bool {
        let scopes = lock(&self.validation_endorsements);
        let Some(scope) = scopes.first() else {
            return false;
        };
        self.validation_work
            .endorsement_misses
            .fetch_add(1, Ordering::Relaxed);
        self.validation_endorsement_index_probes
            .fetch_add(1, Ordering::Relaxed);
        scope
            .identities
            .contains(&(incarnation, stamp, exact_revision))
    }

    #[cfg(test)]
    pub(crate) fn validation_endorsed<V>(&self, terminal: &QueryTerminal<V>) -> bool {
        self.validation_endorsement_authority_for_terminal(terminal)
            == ValidationEndorsementAuthority::TaskLocal
    }

    pub(crate) fn endorse_validation<V>(&self, terminal: &QueryTerminal<V>) {
        let identity = (terminal.node_incarnation, terminal.stamp, terminal.revision);
        for scope in lock(&self.validation_endorsements).iter_mut() {
            scope.identities.insert(identity);
        }
    }

    pub(crate) fn begin_validation(self: &Arc<Self>) -> ValidationProofGuard {
        let mut proofs = lock(&self.validation_proofs);
        let depth = proofs.len();
        proofs.push(VALIDATION_PROOF_REGISTERED);
        drop(proofs);
        ValidationProofGuard {
            task: self.clone(),
            depth,
        }
    }

    pub(crate) fn taint_validation_proofs(&self) {
        for proof in lock(&self.validation_proofs).iter_mut() {
            *proof = VALIDATION_PROOF_UNREGISTERED;
        }
        let mut parent = self
            .validation_proof_parent
            .as_ref()
            .and_then(Weak::upgrade);
        while let Some(task) = parent {
            for proof in lock(&task.validation_proofs).iter_mut() {
                *proof = VALIDATION_PROOF_UNREGISTERED;
            }
            parent = task
                .validation_proof_parent
                .as_ref()
                .and_then(Weak::upgrade);
        }
    }

    pub(crate) fn defer_validation_proofs(&self) {
        for proof in lock(&self.validation_proofs).iter_mut() {
            if *proof == VALIDATION_PROOF_REGISTERED {
                *proof = VALIDATION_PROOF_RETRYABLE;
            }
        }
        let mut parent = self
            .validation_proof_parent
            .as_ref()
            .and_then(Weak::upgrade);
        while let Some(task) = parent {
            for proof in lock(&task.validation_proofs).iter_mut() {
                if *proof == VALIDATION_PROOF_REGISTERED {
                    *proof = VALIDATION_PROOF_RETRYABLE;
                }
            }
            parent = task
                .validation_proof_parent
                .as_ref()
                .and_then(Weak::upgrade);
        }
    }

    /// Records a nested request's lifecycle. The display identity is materialized
    /// lazily: the hot memo-hit/compute path reuses the terminal's already-built
    /// `NodeIdentity`, so no `stable_identity()` is formatted per request. Only
    /// the cold abort branch invokes `fallback_node`, which formats the key.
    pub(crate) fn record_nested<V>(
        &self,
        id: u64,
        fallback_node: impl FnOnce() -> NodeIdentity,
        result: &TaskQueryResult<V>,
    ) {
        let family = match result {
            TaskQueryResult::Terminal { terminal, .. } => terminal.node.family(),
            TaskQueryResult::Aborted { .. } => {
                let node = fallback_node();
                self.core
                    .metrics
                    .record_abort_fallback_identity(node.key().len());
                if !self.records_nested_attempt(node.family()) {
                    return;
                }
                let attempt = match result {
                    TaskQueryResult::Aborted {
                        abort,
                        dependencies,
                        inputs,
                        work,
                    } => NestedQueryAttempt {
                        id,
                        node,
                        node_incarnation: None,
                        origin_request: id,
                        execution: RequestExecution::Aborted,
                        terminal_revision: None,
                        terminal_stamp: None,
                        terminal_kind: None,
                        abort: Some(abort.clone()),
                        dependencies: dependencies.clone().into(),
                        inputs: inputs.clone().into(),
                        work: work.clone().into(),
                    },
                    TaskQueryResult::Terminal { .. } => unreachable!(),
                };
                lock(&self.nested_attempts).push(attempt);
                return;
            }
        };
        if !self.records_nested_attempt(family) {
            return;
        }
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
                terminal_kind: Some(terminal.kind()),
                abort: None,
                dependencies: terminal.dependencies.clone(),
                inputs: terminal.inputs.clone(),
                work: work.clone().into(),
            },
            TaskQueryResult::Aborted { .. } => unreachable!(),
        };
        lock(&self.nested_attempts).push(attempt);
    }

    pub(crate) fn acquire_permit(&self, core: &Arc<RuntimeCore>) -> bool {
        if self.owns_permit.load(Ordering::Acquire) {
            return false;
        }
        core.permits.acquire();
        assert!(!self.owns_permit.swap(true, Ordering::AcqRel));
        lock(&self.permit_timing).acquired();
        core.metrics.permit_acquired();
        true
    }

    pub(crate) fn release_permit(&self, core: &Arc<RuntimeCore>) -> bool {
        if !self.owns_permit.swap(false, Ordering::AcqRel) {
            return false;
        }
        lock(&self.permit_timing).released();
        core.metrics.permit_released();
        core.permits.release();
        true
    }

    /// Record `handoff` in the innermost live observation scope.
    ///
    /// A scope holds only lifecycles that are still live: an already committed
    /// one carries no obligation and is answered without being recorded. What
    /// remains is deduplicated by pointer identity, which is a scan over what
    /// the scope already holds — so this site is counted as a lookup/visit
    /// pair. See [`RuntimeMetrics::handoff_observations`].
    pub(crate) fn observe_handoff(&self, handoff: Arc<AttemptHandoffLifecycle>) -> bool {
        if Arc::ptr_eq(&handoff, AttemptHandoffLifecycle::shared_committed_ref())
            || handoff.is_committed()
        {
            self.core.metrics.handoff_observed(1);
            return true;
        }
        if !self.validate_handoff(&handoff) {
            self.core.metrics.handoff_observed(1);
            return false;
        }
        let mut stack = lock(&self.stack);
        if let Some(frame) = stack.last_mut() {
            let recorded = frame
                .observed_handoffs
                .iter()
                .position(|current| Arc::ptr_eq(current, &handoff));
            self.core
                .metrics
                .handoff_observed(scan_visits(recorded, frame.observed_handoffs.len()));
            if recorded.is_none() {
                frame.observed_handoffs.push(handoff);
            }
            return true;
        }
        drop(stack);
        let mut observed = lock(&self.observed_handoffs);
        let recorded = observed
            .iter()
            .position(|current| Arc::ptr_eq(current, &handoff));
        self.core
            .metrics
            .handoff_observed(scan_visits(recorded, observed.len()));
        if recorded.is_none() {
            // Keep only the returned root. It owns its dependency lifecycle
            // DAG, which the commit barrier expands once in dependency order.
            observed.push(handoff);
        }
        true
    }

    fn validate_handoff(&self, handoff: &Arc<AttemptHandoffLifecycle>) -> bool {
        let mut checked = lock(&self.checked_handoffs);
        let mut newly_checked = AHashSet::new();
        if !self.validate_handoff_once(handoff, &checked, &mut newly_checked) {
            return false;
        }
        checked.extend(newly_checked);
        true
    }

    fn validate_handoff_once(
        &self,
        lifecycle: &Arc<AttemptHandoffLifecycle>,
        checked: &AHashSet<usize>,
        newly_checked: &mut AHashSet<usize>,
    ) -> bool {
        let identity = Arc::as_ptr(lifecycle) as usize;
        if checked.contains(&identity) || !newly_checked.insert(identity) {
            return true;
        }
        #[cfg(test)]
        self.handoff_validation_visits
            .fetch_add(1, Ordering::Relaxed);
        {
            let state = lock(&lifecycle.state);
            match &*state {
                AttemptHandoffState::Committed => return true,
                AttemptHandoffState::Aborted => return false,
                AttemptHandoffState::Pending(_) | AttemptHandoffState::Committing { .. } => {}
            }
        }
        lifecycle
            .observed
            .iter()
            .all(|dependency| self.validate_handoff_once(dependency, checked, newly_checked))
    }

    pub(crate) fn commit_handoffs(&self) -> Result<(), RootHandoffCommitFailure> {
        assert!(
            !self.owns_permit.load(Ordering::Acquire),
            "root handoffs commit only after releasing the execution permit"
        );
        let roots = std::mem::take(&mut *lock(&self.observed_handoffs));
        let mut observed = Vec::new();
        let mut seen = HashSet::new();
        for lifecycle in roots {
            if !AttemptHandoffLifecycle::collect_observed_once(&lifecycle, &mut observed, &mut seen)
            {
                return Err(RootHandoffCommitFailure::Invalidated);
            }
        }
        let mut acquisition_order = observed.into_iter().enumerate().collect::<Vec<_>>();
        acquisition_order.sort_unstable_by_key(|(_, lifecycle)| Arc::as_ptr(lifecycle));
        let mut ordered_batches = Vec::with_capacity(acquisition_order.len());
        for (observation_order, lifecycle) in acquisition_order {
            match lifecycle.begin_commit(self.id, &self.cancellation, &self.core) {
                AttemptHandoffCommit::Claimed(handoffs) => {
                    ordered_batches.push((observation_order, lifecycle, handoffs));
                }
                AttemptHandoffCommit::Committed => {}
                AttemptHandoffCommit::Aborted => {
                    let batches = ordered_batches
                        .into_iter()
                        .map(|(_, lifecycle, handoffs)| (lifecycle, handoffs))
                        .collect::<Vec<_>>();
                    let attempted_callbacks = vec![0; batches.len()];
                    rollback_handoff_batches(self.id, batches, attempted_callbacks);
                    return Err(RootHandoffCommitFailure::Invalidated);
                }
                AttemptHandoffCommit::Canceled => {
                    let batches = ordered_batches
                        .into_iter()
                        .map(|(_, lifecycle, handoffs)| (lifecycle, handoffs))
                        .collect::<Vec<_>>();
                    let attempted_callbacks = vec![0; batches.len()];
                    rollback_handoff_batches(self.id, batches, attempted_callbacks);
                    return Err(RootHandoffCommitFailure::Canceled);
                }
            }
        }
        ordered_batches.sort_unstable_by_key(|(order, _, _)| *order);
        let mut batches = ordered_batches
            .into_iter()
            .map(|(_, lifecycle, handoffs)| (lifecycle, handoffs))
            .collect::<Vec<_>>();

        let mut failure = None;
        let mut attempted_callbacks = vec![0; batches.len()];
        'commit: for (batch_index, (_, handoffs)) in batches.iter_mut().enumerate() {
            for handoff in handoffs {
                if self.cancellation.is_canceled() {
                    failure = Some(RootHandoffCommitFailure::Canceled);
                    break 'commit;
                }
                // Count the callback before entering user code: a panicking
                // commit may already have installed state that abort must undo.
                attempted_callbacks[batch_index] += 1;
                if let Err(payload) = commit_handoff(&mut **handoff) {
                    failure = Some(RootHandoffCommitFailure::Panicked(payload));
                    break 'commit;
                }
            }
        }
        if failure.is_none() && self.cancellation.is_canceled() {
            failure = Some(RootHandoffCommitFailure::Canceled);
        }
        if let Some(failure) = failure {
            // Root publication is one transaction. Roll back every handoff,
            // in the attempted prefix, including earlier successful callbacks,
            // before making any of the terminals claimable by another root.
            // Unattempted callbacks retain their pending state for retry.
            rollback_handoff_batches(self.id, batches, attempted_callbacks);
            return Err(failure);
        }

        for (lifecycle, _) in batches {
            lifecycle.finish_commit(self.id);
        }
        Ok(())
    }

    pub(crate) fn discard_observed_handoffs(&self) {
        lock(&self.observed_handoffs).clear();
    }

    pub(crate) fn push(&self, node: ExactNodeIdentity) {
        let mut stack = lock(&self.stack);
        stack.push(TaskFrame {
            node,
            dependencies: TaskDependencies::default(),
            inputs: InlineOrderedMap::default(),
            work: InlineOrderedMap::default(),
            handoffs: Vec::new(),
            observed_handoffs: Vec::new(),
            dependency_cone_missing: false,
        });
        let depth = self.ancestry.len().saturating_add(stack.len()) as u64;
        self.longest_query_dependency_chain
            .fetch_max(depth, Ordering::Relaxed);
    }

    pub(crate) fn pop(&self, expected: &ExactNodeIdentity) -> TaskFrameOutput {
        let frame = lock(&self.stack)
            .pop()
            .expect("query computation owns one dependency frame");
        assert_eq!(&frame.node, expected);
        let dependencies = frame.dependencies.into_observations();
        let inputs: Vec<InputObservation> = frame
            .inputs
            .into_entries()
            .into_iter()
            .map(|(input, stamp)| InputObservation { input, stamp })
            .collect();
        let cone_missing_observation = frame.dependency_cone_missing
            || inputs.iter().any(|observation| observation.stamp == 0);
        TaskFrameOutput {
            dependencies,
            inputs,
            work: frame.work.into_entries(),
            handoffs: AttemptHandoffs {
                pending: frame.handoffs,
                observed: frame.observed_handoffs,
            },
            cone_missing_observation,
        }
    }

    pub(crate) fn register_attempt_handoff(&self, handoff: Box<dyn QueryAttemptHandoff>) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("attempt handoffs may be registered only inside a query evaluator");
        frame.handoffs.push(handoff);
    }

    pub(crate) fn observe<V>(&self, terminal: &QueryTerminal<V>) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            frame.observe_dependency(&terminal.node, terminal.node_incarnation, terminal.stamp);
            frame.dependency_cone_missing |= terminal.cone_missing_observation;
        }
    }

    pub(crate) fn observe_work(&self, work: &[(Arc<str>, u64)]) {
        if let Some(frame) = lock(&self.stack).last_mut() {
            for (identity, amount) in work {
                frame
                    .work
                    .insert_with(identity.clone(), *amount, |previous, current| {
                        *previous += current;
                    });
            }
        }
    }

    pub(crate) fn record_work(&self, item: WorkItem) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("work recording occurs only inside a query computation");
        frame
            .work
            .insert_with(item.identity, item.amount, |previous, current| {
                *previous += current;
            });
    }

    pub(crate) fn observe_abort_prefix(
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
            frame.observe_dependency(&dependency.node, dependency.incarnation, dependency.stamp);
            // A replayed abort-prefix dependency carries no terminal, so its
            // cone purity is unknown; the enclosing frame stays conservative
            // (ADR-0073) rather than risking a wrongly carried certificate.
            frame.dependency_cone_missing = true;
        }
        for input in inputs {
            frame
                .inputs
                .insert_with(input.input.clone(), input.stamp, |previous, current| {
                    assert_eq!(*previous, current);
                });
        }
        for (identity, amount) in work {
            frame
                .work
                .insert_with(identity.clone(), *amount, |previous, current| {
                    *previous += current;
                });
        }
    }

    pub(crate) fn observe_input(&self, input: InputIdentity, stamp: u64) {
        let mut stack = lock(&self.stack);
        let frame = stack
            .last_mut()
            .expect("input reads occur only inside a query computation");
        frame.inputs.insert_with(input, stamp, |previous, current| {
            assert_eq!(*previous, current);
        });
    }

    pub(crate) fn stack_cycle(&self, node: &ExactNodeIdentity) -> Option<Arc<[NodeIdentity]>> {
        let stack = lock(&self.stack);
        let enclosing = self
            .ancestry
            .iter()
            .chain(stack.iter().map(|frame| &frame.node));
        let start = enclosing.clone().position(|frame| frame == node)?;
        Some(canonical_cycle(
            enclosing
                .skip(start)
                .map(|frame| frame.display.clone())
                .chain(std::iter::once(node.display.clone())),
        ))
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.discard_observed_handoffs();
        self.core
            .metrics
            .validation
            .add(self.validation_work.take());
        self.core
            .metrics
            .task_leases_released(lock(&self.leases).held.len());
        let permit_timing = lock(&self.permit_timing);
        assert!(
            permit_timing.active_since.is_none(),
            "a query task must release its execution permit before it is dropped"
        );
        if self.publish_critical_path {
            self.core
                .metrics
                .query_worker_active_ns
                .fetch_add(permit_timing.accumulated_ns, Ordering::Relaxed);
            self.core.metrics.longest_query_dependency_chain.fetch_max(
                self.longest_query_dependency_chain.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
    }
}
