//! Query families, node/attempt lifecycle, and the join protocol.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

use ahash::{AHashMap, AHashSet, RandomState};

use crate::*;

/// A typed memo table sharing its runtime's scheduler and wait graph.
pub struct QueryFamily<K: QueryKey, V: Clone + Send + Sync + 'static> {
    pub(crate) core: Arc<RuntimeCore>,
    pub(crate) inner: Arc<FamilyInner<K, V>>,
    pub(crate) retention_driver: Arc<dyn RetentionFamily>,
}

/// Non-owning handle for evaluator graphs with cross-family back edges.
pub struct WeakQueryFamily<K: QueryKey, V: Clone + Send + Sync + 'static> {
    core: Weak<RuntimeCore>,
    inner: Weak<FamilyInner<K, V>>,
    retention_driver: Weak<dyn RetentionFamily>,
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
            retention_driver: self.retention_driver.clone(),
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
            retention_driver: self.retention_driver.upgrade()?,
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
            retention_driver: self.retention_driver.clone(),
        }
    }
}

/// Number of memo-index shards per family. A power of two so shard selection
/// is a mask; fixed rather than host-derived so the index's shape is identical
/// on every machine. 32 shards keep expected same-shard collisions rare for
/// the worker counts Rue schedules (currently ≤ 16) while costing only a few
/// kilobytes per family.
pub(crate) const NODE_INDEX_SHARDS: usize = 32;

/// Sharded typed-key memo index (RUE-1241).
///
/// Every memo hit needs only to clone an existing `Arc`, yet a single-mutex
/// index serializes all hits in the family — the dominant measured lock convoy
/// at high worker counts after the registry and revision stores became
/// concurrent. A reader-writer lock was prototyped and measured flat: its
/// read-side acquisition is dearer than an uncontended mutex, which taxed the
/// serial path without repaying at `-j10`. Sharding instead keeps the cheap
/// mutex acquisition and removes cross-key contention by splitting the key
/// space; hits on the *same* key still serialize briefly on one shard, which
/// preserves the hit/removal race rules unchanged per shard.
///
/// Shard selection hashes with this index's own keyed `RandomState`, and each
/// shard map keeps its own keyed `RandomState` — the
/// adversarial-resistance property of the previous single map is preserved,
/// never weakened to a fixed or truncated hash.
///
/// Lock-order contract: a shard guard may be held while acquiring the global
/// node-incarnation registry or a node's state lock (mirroring the previous
/// whole-index mutex); nothing acquires a shard guard while holding either of
/// those, and no path holds two shard guards at once.
pub(crate) struct ShardedNodeIndex<K: QueryKey, V: Clone + Send + Sync + 'static> {
    selector: RandomState,
    shards: [Mutex<AHashMap<K, Arc<Node<K, V>>>>; NODE_INDEX_SHARDS],
}

impl<K, V> ShardedNodeIndex<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    pub(crate) fn new() -> Self {
        Self {
            selector: RandomState::new(),
            shards: std::array::from_fn(|_| Mutex::new(AHashMap::new())),
        }
    }

    pub(crate) fn shard_index(&self, key: &K) -> usize {
        self.selector.hash_one(key) as usize & (NODE_INDEX_SHARDS - 1)
    }

    /// Locks and returns the one shard that can own `key`. Exclusive per
    /// shard: get-miss-insert sequences and the removal re-checks (`users`,
    /// `attempts`, pointer identity) stay atomic under this guard exactly as
    /// they were under the whole-index mutex.
    pub(crate) fn shard(&self, key: &K) -> MutexGuard<'_, AHashMap<K, Arc<Node<K, V>>>> {
        lock(&self.shards[self.shard_index(key)])
    }
}

pub(crate) struct FamilyInner<K: QueryKey, V: Clone + Send + Sync + 'static> {
    pub(crate) core: Weak<RuntimeCore>,
    pub(crate) name: Arc<str>,
    pub(crate) token: FamilyToken,
    /// Registration policy: the family asserts every record is a pure function
    /// of its key alone, so no revision leaf can change the value behind an
    /// unchanged key. This registration is the SOLE minting authority for
    /// [`AdoptableTerminal`] — an ordinary input-dependent family can never
    /// endorse a stale value through adoption.
    pub(crate) content_addressed: bool,
    pub(crate) retention_limit: usize,
    pub(crate) value_equal: fn(&V, &V) -> bool,
    pub(crate) retained_value_charge: fn(&V) -> u64,
    pub(crate) evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
    // Hashed typed-key memo index, sharded so hits on unrelated keys do not
    // convoy on one lock (RUE-1241). Exact `K` equality is authoritative: each
    // shard map is keyed by the typed key itself, so hash collisions resolve
    // through `Eq` and never conflate distinct keys. The maps are unordered:
    // eviction order lives in `retention` below (the memo index never encoded
    // eviction order), so no companion order structure is required.
    pub(crate) nodes: ShardedNodeIndex<K, V>,
    pub(crate) retention: Mutex<FamilyRetentionQueue<K, V>>,
    pub(crate) retained_count: AtomicUsize,
    /// Retained-count watermark for the next publish-side sweep. A pass that
    /// finds only protected entries doubles this watermark, so growing a live
    /// closure examines O(N) entries in total rather than rescanning every
    /// prefix. Releases still force an immediate pass because they can make an
    /// existing terminal newly evictable.
    pub(crate) next_publish_sweep: AtomicUsize,
    pub(crate) retained_nodes: AtomicUsize,
    pub(crate) retained_revisions: Mutex<BTreeMap<Revision, usize>>,
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

impl<K, V> Drop for FamilyInner<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let Some(core) = self.core.upgrade() else {
            return;
        };
        let mut terminals = 0_u64;
        for shard in &self.nodes.shards {
            for node in lock(shard).values() {
                for attempt in &lock(&node.state).attempts {
                    if let AttemptState::Terminal { .. } = &attempt.state {
                        terminals += 1;
                    }
                }
            }
        }
        if terminals > 0 {
            core.metrics
                .retained_terminals
                .fetch_sub(terminals, Ordering::Relaxed);
        }
    }
}

pub(crate) type FamilyEvaluator<K, V> = dyn Fn(&QueryContext, &QueryFamily<K, V>, &K) -> Result<QueryOutput<V>, QueryAbort>
    + Send
    + Sync;

pub(crate) struct Node<K, V> {
    /// Typed key owning this node, retained so eviction can locate the node in
    /// the hashed memo index without a linear scan. It is shared with this
    /// node's identity rather than copied into it, so deferring the display
    /// format costs one `Arc` header per node and not a second typed key.
    key_source: Arc<TypedKeySource<K>>,
    pub(crate) identity: NodeIdentity,
    pub(crate) incarnation: u64,
    // Both links are weak: the runtime and node therefore remain free to die
    // independently, while the destructor can still find its registry.
    registry_core: Weak<RuntimeCore>,
    // Allocation identity makes removal ABA-safe even if an incarnation slot
    // were ever occupied by a different node.
    pub(crate) registry_self: Weak<dyn ErasedNode>,
    pub(crate) users: AtomicUsize,
    wait: Arc<WaitCell>,
    demand: Option<Arc<dyn Fn(Arc<Task>, u64) -> ValidationDemand<V> + Send + Sync>>,
    pub(crate) state: Mutex<NodeState<V>>,
}

impl<K, V> Node<K, V> {
    /// The typed key this node memoizes.
    pub(crate) fn key(&self) -> &K {
        &self.key_source.key
    }
}

/// Outcome of re-demanding a retained dependency from family-owned authority.
///
/// Re-demand resolves the family's memo by KEY, while a retained dependency
/// observation names one exact node INCARNATION. Retention removes a node from
/// its family's key memo once the node holds no terminals and no live users
/// (`NodeLease::drop` and terminal eviction), after which the next request for
/// that key builds a fresh incarnation whose stamps restart at one. A recursive
/// validation walk holds the old incarnation alive through the incarnation
/// registry, so both nodes can be live at once — and the fresh incarnation's
/// first stamp collides numerically with the retained observation's stamp
/// without describing the same computation.
pub(crate) enum ValidationDemand<V> {
    /// The family still memoizes this key at the demanded incarnation, so the
    /// result proves whether the retained observation is still current.
    Current(TaskQueryResult<V>),
    /// The family's memo for this key is a different incarnation. Nothing the
    /// fresh node publishes can witness the retained observation, so the
    /// dependent is dirty and recomputes against the current graph.
    Superseded,
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

impl<K, V> Drop for Node<K, V> {
    fn drop(&mut self) {
        let Some(core) = self.registry_core.upgrade() else {
            return;
        };
        write(&core.nodes).remove(self.incarnation, &self.registry_self);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidationCertificate {
    pub(crate) revision: Revision,
    pub(crate) stamp: u64,
    pub(crate) terminal_revision: Revision,
    pub(crate) registered_only: bool,
    /// Validation epoch of `revision` when this certificate was minted.
    /// Within one epoch the certificate-eligible history is a single
    /// strictly-additive extension chain, so the ADR-0073 gate may accept
    /// this certificate at a later revision of the same epoch.
    pub(crate) epoch: u64,
    /// Whether the certified terminal's cone observed a missing leaf. Such
    /// a certificate never crosses revisions: an added leaf in a later
    /// strictly-additive revision would change what that cone observed.
    pub(crate) cone_missing_observation: bool,
}

pub(crate) const INLINE_ACTIVE_VALIDATIONS: usize = 8;

/// Runtime incarnations on the current validation recursion path.
///
/// Validation cones are normally shallow, so retaining the first few entries
/// inline avoids constructing a tree for every traversal. Unusually deep cones
/// promote once to a hash set, preserving bounded membership checks instead of
/// making adversarial dependency depth quadratic.
#[derive(Debug, Default)]
pub(crate) enum ActiveValidations {
    #[default]
    Empty,
    Inline {
        entries: [u64; INLINE_ACTIVE_VALIDATIONS],
        len: u8,
    },
    Hashed(AHashSet<u64>),
}

impl ActiveValidations {
    pub(crate) fn insert(&mut self, incarnation: u64) -> bool {
        match self {
            Self::Empty => {
                let mut entries = [0; INLINE_ACTIVE_VALIDATIONS];
                entries[0] = incarnation;
                *self = Self::Inline { entries, len: 1 };
                true
            }
            Self::Inline { entries, len } => {
                let occupied = &entries[..usize::from(*len)];
                if occupied.contains(&incarnation) {
                    return false;
                }
                if usize::from(*len) < INLINE_ACTIVE_VALIDATIONS {
                    entries[usize::from(*len)] = incarnation;
                    *len += 1;
                    return true;
                }

                let mut promoted = AHashSet::with_capacity(INLINE_ACTIVE_VALIDATIONS + 1);
                promoted.extend(occupied.iter().copied());
                let inserted = promoted.insert(incarnation);
                assert!(
                    inserted,
                    "a distinct incarnation must remain distinct during cycle-set promotion"
                );
                *self = Self::Hashed(promoted);
                true
            }
            Self::Hashed(entries) => entries.insert(incarnation),
        }
    }

    pub(crate) fn remove(&mut self, incarnation: &u64) -> bool {
        match self {
            Self::Empty => false,
            Self::Inline { entries, len } => {
                let Some(position) = entries[..usize::from(*len)]
                    .iter()
                    .position(|entry| entry == incarnation)
                else {
                    return false;
                };
                *len -= 1;
                entries[position] = entries[usize::from(*len)];
                if *len == 0 {
                    *self = Self::Empty;
                }
                true
            }
            Self::Hashed(entries) => entries.remove(incarnation),
        }
    }
}

pub(crate) trait ErasedNode: fmt::Debug + Send + Sync {
    /// Exact runtime-local incarnation represented by this erased handle.
    fn incarnation(&self) -> u64;

    fn validated_stamp(
        &self,
        _core: &RuntimeCore,
        task: &Arc<Task>,
        active: &mut ActiveValidations,
    ) -> Result<Option<u64>, QueryAbort>;

    /// Records one exact validation certificate and reports whether it was new.
    fn mark_validated(&self, certificate: ValidationCertificate) -> bool;

    /// The typed node behind this erased handle, for the family-owned
    /// exact-terminal adoption path ([`QueryFamily::observe_adopted_terminal`])
    /// to recover its own `Node<K, V>` from the incarnation index without a
    /// key lookup.
    fn as_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;
}

impl<K, V> ErasedNode for Node<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn incarnation(&self) -> u64 {
        self.incarnation
    }

    fn validated_stamp(
        &self,
        core: &RuntimeCore,
        task: &Arc<Task>,
        active: &mut ActiveValidations,
    ) -> Result<Option<u64>, QueryAbort> {
        if !active.insert(self.incarnation) {
            task.validation_work
                .active_cycle_prunes
                .fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        let mut proof_reacquisition_miss = false;
        {
            let state = lock(&self.state);
            // ADR-0073 `extends_for_certificate`: a certificate is accepted at
            // the exact revision it was minted for, or forward along the same
            // certificate-eligible extension chain — equal validation epoch
            // (same-epoch history is one strictly-additive linear chain, so
            // equal epoch plus directional id order proves ancestry), same
            // compatibility namespace, certificate not newer than the request,
            // and only when the certified cone observed no missing leaf (an
            // added leaf in a later additive revision would change what such
            // a cone observed).
            if let Some(certificate) = &state.validated_at
                && (certificate.revision == task.revision
                    || (!certificate.cone_missing_observation
                        && certificate.epoch == task.revision_epoch
                        && certificate.revision.is_compatible_with(task.revision)
                        && certificate.revision.id < task.revision.id))
            {
                // A registered-cone endorsement is also a retention proof. A
                // memo may skip validation in that scope only when this task
                // has already leased the exact terminal or a live fallback owns
                // an equal node/stamp representative. Final promotion walks the
                // selected representative's own transitive cone.
                let endorsement_authority = task.validation_endorsement_authority_at(
                    self.incarnation,
                    certificate.stamp,
                    certificate.terminal_revision,
                );
                if matches!(
                    endorsement_authority,
                    ValidationEndorsementAuthority::Inactive
                        | ValidationEndorsementAuthority::TaskLocal
                        | ValidationEndorsementAuthority::Borrowed
                ) {
                    if !certificate.registered_only {
                        task.taint_validation_proofs();
                    }
                    task.validation_work
                        .memo_hits
                        .fetch_add(1, Ordering::Relaxed);
                    active.remove(&self.incarnation);
                    return Ok(Some(certificate.stamp));
                }
                proof_reacquisition_miss = true;
            }
        }
        if proof_reacquisition_miss {
            task.validation_work
                .proof_reacquisition_misses
                .fetch_add(1, Ordering::Relaxed);
        } else {
            task.validation_work
                .certificate_misses
                .fetch_add(1, Ordering::Relaxed);
        }
        if let Some(demand) = &self.demand {
            let demand_work = ValidationDemandWork::new(&task.validation_work);
            #[cfg(test)]
            core.interpose(InterposeSite::RetainedDependencyDemand);
            let request_id = task.next_nested_request();
            let ValidationDemand::Current(result) = demand(task.clone(), request_id) else {
                // Retention retired this incarnation from its family's key memo.
                // The key is still computable, but only as a fresh incarnation
                // whose stamps are unrelated to the retained observation, so the
                // dependent is dirty rather than provably green.
                task.validation_work
                    .superseded
                    .fetch_add(1, Ordering::Relaxed);
                active.remove(&self.incarnation);
                return Ok(None);
            };
            task.record_nested(request_id, || self.identity.clone(), &result);
            active.remove(&self.incarnation);
            return match result {
                TaskQueryResult::Terminal {
                    terminal,
                    execution,
                    ..
                } => {
                    match execution {
                        RequestExecution::Reused => &task.validation_work.demand_reuses,
                        RequestExecution::Computed => &task.validation_work.demand_computes,
                        RequestExecution::Joined => &task.validation_work.demand_joins,
                        RequestExecution::Aborted => &task.validation_work.demand_aborts,
                    }
                    .fetch_add(1, Ordering::Relaxed);
                    demand_work.finish();
                    // Retention can retire this incarnation between the memo
                    // check above and the request itself. The answering
                    // incarnation is authoritative: a stamp published by any
                    // other node is a different counter and never witnesses this
                    // observation, however numerically equal it looks.
                    if terminal.node_incarnation != self.incarnation {
                        task.validation_work
                            .superseded
                            .fetch_add(1, Ordering::Relaxed);
                        return Ok(None);
                    }
                    // A validation-only computation or join returns an exact
                    // stamp but does not transfer its candidate pin into the
                    // task's proof cone. Mark every enclosing certificate for
                    // one repair traversal: now that the terminal is published,
                    // an ordinary Reused validation can capture it.
                    if execution != RequestExecution::Reused {
                        task.defer_validation_proofs();
                    }
                    Ok(Some(terminal.stamp))
                }
                // A dependency that cannot be produced under this revision
                // because one of its inputs is gone does not abort the
                // dependent: it makes the dependent's retained terminal
                // invalid, so the dependent recomputes against the current
                // graph (RUE-1137 item 8).
                //
                // The distinction is which side of the edge the demand is on.
                // Demanding an input a *computation* has not yet discovered is
                // the external-input protocol asking the host to supply it.
                // Re-demanding an input a *retained terminal already observed*,
                // and finding it absent, is an ordinary red edge — the removal
                // is the change. Without this, editing an import so a module
                // leaves the graph aborts every dependent instead of
                // recomputing it.
                //
                // Only missing inputs are absorbed. Cancellation, dependency
                // cycles, and engine invariant violations still propagate: they
                // say nothing about whether the retained terminal is stale.
                TaskQueryResult::Aborted {
                    abort: QueryAbort::MissingInput(_),
                    ..
                } => {
                    task.validation_work
                        .demand_aborts
                        .fetch_add(1, Ordering::Relaxed);
                    demand_work.finish();
                    Ok(None)
                }
                TaskQueryResult::Aborted { abort, .. } => {
                    task.validation_work
                        .demand_aborts
                        .fetch_add(1, Ordering::Relaxed);
                    demand_work.finish();
                    Err(abort)
                }
            };
        }
        // An externally supplied evaluator cannot participate in a reusable
        // registered-only proof: the runtime cannot re-demand and pin its
        // complete dependency cone from family-owned authority.
        task.taint_validation_proofs();
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

    fn as_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn mark_validated(&self, certificate: ValidationCertificate) -> bool {
        let mut state = lock(&self.state);
        if state.attempts.iter().any(|attempt| {
            matches!(
                &attempt.state,
                AttemptState::Terminal { terminal, .. }
                    if terminal.stamp == certificate.stamp
                        && terminal.revision == certificate.terminal_revision
            )
        }) {
            if state.validated_at.as_ref() == Some(&certificate) {
                return false;
            }
            state.validated_at = Some(certificate);
            return true;
        }
        false
    }
}

#[derive(Debug)]
pub(crate) struct WaitCell {
    pub(crate) cv: Condvar,
    pub(crate) generation: Mutex<u64>,
}

impl WaitCell {
    pub(crate) fn notify_all(&self) {
        *lock(&self.generation) += 1;
        self.cv.notify_all();
    }

    pub(crate) fn wait_until(
        &self,
        mut predicate: impl FnMut() -> bool,
        #[cfg(test)] mut before_park: impl FnMut(),
    ) {
        let mut generation = lock(&self.generation);
        while !predicate() {
            #[cfg(test)]
            before_park();
            generation = wait(&self.cv, generation);
        }
    }
}

#[derive(Debug)]
pub(crate) struct NodeState<V> {
    next_attempt: u64,
    next_stamp: u64,
    pub(crate) attempts: VecDeque<Attempt<V>>,
    /// The exact terminal revision and stamp already proven against this
    /// immutable request revision.
    ///
    /// This is a verification skip only. The matching terminal must still be
    /// retained, and the first visit in every revision continues to validate
    /// direct inputs and the complete dependency cone authoritatively.
    pub(crate) validated_at: Option<ValidationCertificate>,
}

impl<V> NodeState<V> {
    /// Remove one attempt and invalidate a certificate backed by its terminal.
    ///
    /// Keeping this invariant at the three removal sites makes certificate
    /// lookup O(1): a present certificate always names a terminal still held by
    /// this node, so the validation hot path does not rescan retained attempts.
    pub(crate) fn remove_attempt(&mut self, index: usize) -> Option<Attempt<V>> {
        let removed = self.attempts.remove(index)?;
        if let AttemptState::Terminal { terminal, .. } = &removed.state
            && self.validated_at.as_ref().is_some_and(|certificate| {
                certificate.stamp == terminal.stamp
                    && certificate.terminal_revision == terminal.revision
            })
        {
            self.validated_at = None;
        }
        Some(removed)
    }
}

#[derive(Debug)]
pub(crate) struct Attempt<V> {
    pub(crate) id: u64,
    pub(crate) revision: Revision,
    pub(crate) state: AttemptState<V>,
}

#[derive(Debug)]
pub(crate) enum AttemptState<V> {
    Computing {
        owner: TaskId,
        waiters: usize,
    },
    Terminal {
        terminal: Arc<QueryTerminal<V>>,
        waiters: usize,
        handoffs: Arc<AttemptHandoffLifecycle>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FamilyToken {
    pub(crate) runtime: u64,
    pub(crate) family: u64,
}

/// Result of attempting to share another task's in-flight attempt.
pub(crate) enum JoinOutcome<K: QueryKey, V: Clone + Send + Sync + 'static> {
    /// The attempt reached a terminal and its protection was handed to this pin.
    Joined(u64, Arc<AttemptHandoffLifecycle>, TerminalPin<K, V>),
    /// The attempt disappeared (detached or evicted); rediscover from scratch.
    Retry,
    /// Waiting for this attempt's owner would close a wait-graph loop. The
    /// caller claims a private attempt rather than failing the request.
    Contended,
}

pub(crate) enum TaskQueryResult<V> {
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
    pub(crate) fn into_result(self) -> Result<Arc<QueryTerminal<V>>, QueryAbort> {
        match self {
            Self::Terminal { terminal, .. } => Ok(terminal),
            Self::Aborted { abort, .. } => Err(abort),
        }
    }
}

pub(crate) struct NodeLease<K: QueryKey, V: Clone + Send + Sync + 'static> {
    family: Weak<FamilyInner<K, V>>,
    key: K,
    pub(crate) node: Arc<Node<K, V>>,
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
        let mut nodes = family.nodes.shard(&self.key);
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
            retention_driver: Arc::downgrade(&self.retention_driver),
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
        // Shards are visited one at a time; the probe was never a cross-key
        // atomic snapshot (its answer could go stale the moment the old
        // whole-index lock released), so per-shard consistency is unchanged.
        self.inner
            .nodes
            .shards
            .iter()
            .any(|shard| lock(shard).keys().any(&mut predicate))
    }

    /// Whether the exact key currently owns a live memo node in this family.
    ///
    /// "Retained" means that the key is still present in the family's memo
    /// index. It does not mean that a terminal for the current revision is
    /// valid, selected, or protected from eviction; those properties are
    /// decided by the ordinary query request. This exact-key probe preserves
    /// the memo node's lifetime semantics while using the index's O(1)
    /// average-case lookup instead of enumerating every retained key.
    pub fn contains_retained_key(&self, key: &K) -> bool {
        self.inner.nodes.shard(key).contains_key(key)
    }

    /// Caller-owned provenance identities for every retained reusable terminal.
    pub fn retained_origin_request_ids(&self) -> BTreeSet<u64> {
        let mut nodes = Vec::new();
        for shard in &self.inner.nodes.shards {
            nodes.extend(lock(shard).values().cloned());
        }
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

    pub(crate) fn node(&self, key: K) -> Result<NodeLease<K, V>, QueryAbort> {
        // ADR-0063 Phase 7 hashed memo index. Lookup is O(1) expected on the
        // typed key's `Hash`, but exact `K` equality remains authoritative:
        // `HashMap<K, _>` resolves any hash collision through `Eq`, so distinct
        // keys that hash alike still map to distinct nodes. Display identity is
        // never consulted for lookup. The guard covers the whole hit/miss
        // sequence including the `users` increment below, so removal's
        // `users == 0` re-check under the same shard guard cannot race a hit.
        let mut nodes = self.inner.nodes.shard(&key);
        let node = if let Some(node) = nodes.get(&key) {
            node.clone()
        } else {
            // ADR-0074: minting a node digests its typed key and formats
            // nothing. The presentation text is produced later, if a
            // diagnostic, cycle render, abort, or `Debug` dump asks for a name.
            let stable_hash = stable_key_hash(&key);
            let incarnation = self.core.next_node.fetch_add(1, Ordering::Relaxed);
            let demand = self.inner.evaluator.as_ref().map(|_| {
                let core = self.core.clone();
                let family = Arc::downgrade(&self.inner);
                let retention_driver = self.retention_driver.clone();
                let key = key.clone();
                Arc::new(move |task: Arc<Task>, origin_request: u64| {
                    let Some(inner) = family.upgrade() else {
                        return ValidationDemand::Current(TaskQueryResult::Aborted {
                            abort: QueryAbort::ForeignRuntime,
                            dependencies: Vec::new(),
                            inputs: Vec::new(),
                            work: Vec::new(),
                        });
                    };
                    // Re-demand answers by key, so it can only witness this
                    // observation while the family's memo for the key is still
                    // this incarnation. Checking first keeps a superseded node
                    // from resurrecting its key speculatively — which would both
                    // charge retention for work the dependent must redo anyway
                    // and park this task behind whichever request is already
                    // computing the fresh incarnation.
                    let superseded = {
                        let nodes = inner.nodes.shard(&key);
                        nodes
                            .get(&key)
                            .is_none_or(|current| current.incarnation != incarnation)
                    };
                    if superseded {
                        return ValidationDemand::Superseded;
                    }
                    ValidationDemand::Current(
                        QueryFamily {
                            core: core.clone(),
                            inner,
                            retention_driver: retention_driver.clone(),
                        }
                        .query_task_registered_for_validation(
                            task,
                            key.clone(),
                            origin_request,
                        ),
                    )
                })
                    as Arc<dyn Fn(Arc<Task>, u64) -> ValidationDemand<V> + Send + Sync>
            });
            let registry_core = Arc::downgrade(&self.core);
            // One typed-key allocation serves both the node's own eviction
            // lookup and its identity's deferred presentation, so the identity
            // owns no second copy of the key.
            let key_source = Arc::new(TypedKeySource {
                key: key.clone(),
                core: registry_core.clone(),
            });
            let node: Arc<Node<K, V>> = Arc::new_cyclic(|registry_self: &Weak<Node<K, V>>| {
                let registry_self: Weak<dyn ErasedNode> = registry_self.clone();
                Node {
                    identity: NodeIdentity::registered(
                        self.inner.name.clone(),
                        key_source.clone(),
                        stable_hash,
                        self.core.identity,
                        registry_self.clone(),
                    ),
                    key_source,
                    incarnation,
                    registry_core,
                    registry_self,
                    users: AtomicUsize::new(0),
                    wait: Arc::new(WaitCell {
                        cv: Condvar::new(),
                        generation: Mutex::new(0),
                    }),
                    demand,
                    state: Mutex::new(NodeState {
                        next_attempt: 1,
                        next_stamp: 1,
                        attempts: VecDeque::new(),
                        validated_at: None,
                    }),
                }
            });
            let erased: Arc<dyn ErasedNode> = node.clone();
            let mut registry = write(&self.core.nodes);
            let inserted = registry.insert(incarnation, Arc::downgrade(&erased));
            drop(registry);
            assert!(inserted, "node incarnations are unique within a runtime");
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

    /// Lease only an already-indexed node. Unlike `node`, this path never
    /// mints an incarnation, so a ready-only observation cannot create a
    /// competing admission authority on a cold key. The shard guard covers
    /// lookup and the users increment together with retirement's re-check.
    pub(crate) fn existing_node(&self, key: &K) -> Option<NodeLease<K, V>> {
        let nodes = self.inner.nodes.shard(key);
        let node = nodes.get(key)?.clone();
        node.users.fetch_add(1, Ordering::AcqRel);
        Some(NodeLease {
            family: Arc::downgrade(&self.inner),
            key: key.clone(),
            node,
        })
    }

    pub(crate) fn query_task<F>(
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

    pub(crate) fn query_task_registered(
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

    pub(crate) fn probe_task_registered_ready(
        &self,
        task: &Arc<Task>,
        key: &K,
        origin_request: u64,
    ) -> Result<ReadyQueryProbe<V>, QueryAbort> {
        if task.cancellation.is_canceled() {
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Canceled);
        }

        if let Some(terminal) = task.cached_query(self.inner.token, key) {
            let exact_node = ExactNodeIdentity {
                display: terminal.node.clone(),
                incarnation: terminal.node_incarnation,
            };
            if let Some(cycle) = task.stack_cycle(&exact_node) {
                self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Cycle(cycle));
            }
            if task.cancellation.is_canceled() {
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Canceled);
            }
            self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
            task.observe(&terminal);
            let result = TaskQueryResult::Terminal {
                terminal: terminal.clone(),
                execution: RequestExecution::Reused,
                work: Vec::new(),
            };
            task.record_nested(origin_request, || terminal.node.clone(), &result);
            return Ok(ReadyQueryProbe::Ready(terminal));
        }

        // Probe the existing memo index directly. In particular, do not call
        // `node`: a miss must not mint a node or enter the evaluator path.
        // The lease pins the exact incarnation across state inspection and
        // terminal observation, preventing ABA retirement/recreation races.
        let Some(node_lease) = self.existing_node(key) else {
            return Ok(ReadyQueryProbe::Miss);
        };
        let node = &node_lease.node;

        let candidate = {
            let state = lock(&node.state);
            let mut current = None;
            let mut in_progress = false;
            for attempt in state.attempts.iter().rev() {
                if attempt.revision != task.revision {
                    continue;
                }
                match &attempt.state {
                    AttemptState::Computing { .. } => in_progress = true,
                    AttemptState::Terminal {
                        terminal, handoffs, ..
                    } => {
                        if current.is_none() {
                            current = Some((terminal.clone(), handoffs.clone()));
                        }
                    }
                }
            }
            if in_progress {
                None
            } else {
                current.map(|(terminal, handoffs)| {
                    let pin = self
                        .pin_terminal(&terminal)
                        .expect("a family pins its own retained terminal");
                    (terminal, handoffs, pin)
                })
            }
        };
        let Some((terminal, handoffs, pin)) = candidate else {
            let state = lock(&node.state);
            let current_revision_exists = state.attempts.iter().any(|attempt| {
                attempt.revision == task.revision
                    && matches!(
                        &attempt.state,
                        AttemptState::Computing { .. } | AttemptState::Terminal { .. }
                    )
            });
            return Ok(if current_revision_exists {
                ReadyQueryProbe::NotReady
            } else {
                ReadyQueryProbe::Miss
            });
        };

        // A terminal with a pending/invalid handoff is not safe to expose yet.
        // Leave it untouched so the ordinary request path can retry and own
        // the handoff lifecycle later.
        if !task.observe_handoff(handoffs) {
            drop(pin);
            return Ok(ReadyQueryProbe::NotReady);
        }
        if task.cancellation.is_canceled() {
            drop(pin);
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Canceled);
        }
        self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
        task.observe(&terminal);
        self.lease_observed_pin(task, pin);
        task.cache_query(self.inner.token, key, &terminal);
        let result = TaskQueryResult::Terminal {
            terminal: terminal.clone(),
            execution: RequestExecution::Reused,
            work: Vec::new(),
        };
        task.record_nested(origin_request, || terminal.node.clone(), &result);
        Ok(ReadyQueryProbe::Ready(terminal))
    }

    /// Observe a registered query without ever creating an incarnation or
    /// claiming an attempt. A current-revision computation may be joined, but
    /// a cold key, stale-only key, wait-graph contention, or an owner that
    /// aborts remains a non-ready result for this observer.
    pub(crate) fn join_task_registered_noncomputing(
        &self,
        task: &Arc<Task>,
        key: &K,
        origin_request: u64,
    ) -> Result<ReadyQueryProbe<V>, QueryAbort> {
        if task.cancellation.is_canceled() {
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Canceled);
        }

        if let Some(terminal) = task.cached_query(self.inner.token, key) {
            let exact_node = ExactNodeIdentity {
                display: terminal.node.clone(),
                incarnation: terminal.node_incarnation,
            };
            if let Some(cycle) = task.stack_cycle(&exact_node) {
                self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Cycle(cycle));
            }
            if task.cancellation.is_canceled() {
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Canceled);
            }
            self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
            task.observe(&terminal);
            let result = TaskQueryResult::Terminal {
                terminal: terminal.clone(),
                execution: RequestExecution::Reused,
                work: Vec::new(),
            };
            task.record_nested(origin_request, || terminal.node.clone(), &result);
            return Ok(ReadyQueryProbe::Ready(terminal));
        }

        let Some(node_lease) = self.existing_node(key) else {
            return Ok(ReadyQueryProbe::Miss);
        };
        let node = &node_lease.node;
        let exact_node = ExactNodeIdentity {
            display: node.identity.clone(),
            incarnation: node.incarnation,
        };
        if let Some(cycle) = task.stack_cycle(&exact_node) {
            self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Cycle(cycle));
        }

        loop {
            if task.cancellation.is_canceled() {
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Canceled);
            }

            let mut join_candidate = None;
            let mut ready_candidate = None;
            let mut ready_pin = None;
            {
                let mut state = lock(&node.state);
                for attempt in state.attempts.iter_mut().rev() {
                    if attempt.revision != task.revision {
                        continue;
                    }
                    match &mut attempt.state {
                        AttemptState::Computing { owner, waiters } if join_candidate.is_none() => {
                            *waiters += 1;
                            join_candidate = Some((attempt.id, *owner));
                        }
                        AttemptState::Terminal {
                            terminal, handoffs, ..
                        } if ready_candidate.is_none() => {
                            ready_candidate = Some((terminal.clone(), handoffs.clone()));
                        }
                        _ => {}
                    }
                }

                if join_candidate.is_none() {
                    if let Some((terminal, handoffs)) = ready_candidate.take() {
                        let pin = self
                            .pin_terminal(&terminal)
                            .expect("a family pins its own retained terminal");
                        ready_pin = Some((terminal, handoffs, pin));
                    }
                }
            }

            if let Some((attempt, owner)) = join_candidate {
                self.core.metrics.joins.fetch_add(1, Ordering::Relaxed);
                #[cfg(test)]
                self.core.test_changed();
                match self.join(task, node, attempt, owner) {
                    Err(abort) => return Err(abort),
                    Ok(JoinOutcome::Retry) => continue,
                    Ok(JoinOutcome::Contended) => return Ok(ReadyQueryProbe::NotReady),
                    Ok(JoinOutcome::Joined(_joined_attempt, handoffs, pin)) => {
                        let terminal = pin.terminal().clone();
                        if !task.observe_handoff(handoffs) {
                            drop(pin);
                            return Ok(ReadyQueryProbe::NotReady);
                        }
                        if task.cancellation.is_canceled() {
                            drop(pin);
                            self.core
                                .metrics
                                .cancellations
                                .fetch_add(1, Ordering::Relaxed);
                            return Err(QueryAbort::Canceled);
                        }
                        task.observe(&terminal);
                        self.lease_observed_pin(task, pin);
                        task.cache_query(self.inner.token, key, &terminal);
                        let result = TaskQueryResult::Terminal {
                            terminal: terminal.clone(),
                            execution: RequestExecution::Joined,
                            work: Vec::new(),
                        };
                        task.record_nested(origin_request, || terminal.node.clone(), &result);
                        return Ok(ReadyQueryProbe::Ready(terminal));
                    }
                }
            }

            let Some((terminal, handoffs, pin)) = ready_pin else {
                return Ok(ReadyQueryProbe::Miss);
            };
            if !task.observe_handoff(handoffs) {
                drop(pin);
                return Ok(ReadyQueryProbe::NotReady);
            }
            if task.cancellation.is_canceled() {
                drop(pin);
                self.core
                    .metrics
                    .cancellations
                    .fetch_add(1, Ordering::Relaxed);
                return Err(QueryAbort::Canceled);
            }
            self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
            task.observe(&terminal);
            self.lease_observed_pin(task, pin);
            task.cache_query(self.inner.token, key, &terminal);
            let result = TaskQueryResult::Terminal {
                terminal: terminal.clone(),
                execution: RequestExecution::Reused,
                work: Vec::new(),
            };
            task.record_nested(origin_request, || terminal.node.clone(), &result);
            return Ok(ReadyQueryProbe::Ready(terminal));
        }
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
        if observe_result {
            if let Some(terminal) = task.cached_query(self.inner.token, &key) {
                let exact_node = ExactNodeIdentity {
                    display: terminal.node.clone(),
                    incarnation: terminal.node_incarnation,
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
                self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
                task.observe(&terminal);
                return TaskQueryResult::Terminal {
                    terminal,
                    execution: RequestExecution::Reused,
                    work: Vec::new(),
                };
            }
        }
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
            if observe_result {
                self.core.metrics.cycles.fetch_add(1, Ordering::Relaxed);
            }
            return TaskQueryResult::Aborted {
                abort: QueryAbort::Cycle(cycle),
                dependencies: Vec::new(),
                inputs: Vec::new(),
                work: Vec::new(),
            };
        }
        // Attempts whose owner this task cannot wait for without deadlocking.
        let mut contended = BTreeSet::new();
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

            // Acquire a protective pin on the reuse candidate about to be
            // validated while it is still retained under the node lock, before
            // releasing the lock to validate. A candidate discovered here
            // therefore cannot be evicted in the window between discovery and
            // either lease transfer (on a validated reuse) or release (on a
            // stale candidate): its pin holds it retained through the recursive
            // validation that follows.
            //
            // Discovery is one candidate at a time, newest first, because the
            // walk below returns on the first validated candidate. Pinning the
            // node's whole retained set up front instead costs one pin per
            // attempt the node has ever accumulated and releases all but one
            // immediately, and every surplus release runs a family and runtime
            // retention enforcement pass. That makes a single request O(retained
            // attempts) and a session which republishes the same keys across
            // revisions quadratic in its revision count (RUE-1262).
            //
            // `cursor` is the id of the last candidate examined; attempt ids
            // ascend with publication, so selecting the greatest terminal id
            // below it walks the same newest-first order the snapshot did. A
            // candidate evicted before the cursor reaches it was unprotected by
            // definition, and missing it costs an ordinary recompute rather
            // than a wrong answer.
            let mut cursor = u64::MAX;
            #[cfg(test)]
            let mut discovered = false;
            loop {
                let candidate = {
                    let state = lock(&node.state);
                    state.attempts.iter().rev().find_map(|attempt| {
                        match (attempt.id < cursor).then_some(&attempt.state) {
                            Some(AttemptState::Terminal {
                                terminal, handoffs, ..
                            }) => Some((
                                attempt.id,
                                handoffs.clone(),
                                self.pin_terminal(terminal)
                                    .expect("a family pins its own retained terminal"),
                            )),
                            Some(AttemptState::Computing { .. }) | None => None,
                        }
                    })
                };
                let Some((attempt_id, handoffs, pin)) = candidate else {
                    break;
                };
                cursor = attempt_id;
                #[cfg(test)]
                if !std::mem::replace(&mut discovered, true) {
                    self.core.interpose(InterposeSite::ReuseDiscovered);
                }
                let terminal = pin.terminal().clone();
                let endorsement_authority = if self.inner.evaluator.is_some() {
                    task.validation_candidate_endorsement_authority_for_terminal(&terminal)
                } else {
                    ValidationEndorsementAuthority::Inactive
                };
                let endorsement_enabled =
                    endorsement_authority != ValidationEndorsementAuthority::Inactive;
                // Only a task-local endorsement proves this candidate's own
                // inputs and dependency edges current. Published fallbacks
                // supply retention authority for current dependency
                // certificates while the ordinary root validation still
                // checks those direct observations.
                let endorsement_hit =
                    endorsement_authority == ValidationEndorsementAuthority::TaskLocal;
                let mut validation = if endorsement_hit {
                    Ok((true, true, false))
                } else {
                    self.core.valid_for_revision(&terminal, &task)
                };
                if matches!(validation, Ok((true, false, true))) {
                    // A registered validation-only compute or join proved the
                    // value current but could not transfer its dependency pin.
                    // The joined/computed terminal is published now, so one
                    // ordinary reuse traversal repairs and leases the complete
                    // registered cone before this root can be endorsed.
                    validation = self.core.valid_for_revision(&terminal, &task);
                }
                match validation {
                    Ok((true, registered_only, _)) => {
                        if observe_result && !task.observe_handoff(handoffs) {
                            task.defer_pin_release(pin);
                            self.detach_terminal_attempt(node, attempt_id);
                            continue;
                        }
                        let endorse = endorsement_enabled && !endorsement_hit && registered_only;
                        if endorse {
                            task.endorse_validation(&terminal);
                        }
                        if endorsement_enabled && !endorsement_hit {
                            self.publish_proof_to_batch(&task, &terminal, endorse);
                        }
                        self.core.metrics.reuses.fetch_add(1, Ordering::Relaxed);
                        if observe_result {
                            // Transfer the temporary discovery pin into the task's
                            // request-scoped lease set, so protection is continuous
                            // from discovery through the lifetime of the request.
                            task.observe(&terminal);
                        }
                        // Even a valid traversal conservatively tainted by an
                        // unregistered descendant can be a dependency of a
                        // promotable registered root. Keep this exact candidate
                        // available to the final cone walk while the scope is
                        // active, without endorsing it or disabling ordinary
                        // green reuse for mixed cones. Promotion still fails
                        // closed if the unregistered edge itself is absent.
                        if observe_result || endorsement_enabled {
                            self.lease_observed_pin(&task, pin);
                        } else {
                            task.defer_pin_release(pin);
                        }
                        if observe_result {
                            task.cache_query(self.inner.token, &lease.key, &terminal);
                        }
                        return TaskQueryResult::Terminal {
                            terminal,
                            execution: RequestExecution::Reused,
                            work: Vec::new(),
                        };
                    }
                    Ok((false, _, _)) => task.defer_pin_release(pin),
                    Err(abort) => {
                        task.defer_pin_release(pin);
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
                            // An attempt this task already proved it cannot wait
                            // for is skipped, so the fallback below claims a
                            // private attempt instead of reselecting the same
                            // unwaitable one forever.
                            if attempt.revision == task.revision && !contended.contains(&attempt.id)
                            {
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
                        Ok(JoinOutcome::Contended) => {
                            contended.insert(attempt);
                            continue;
                        }
                        Ok(JoinOutcome::Joined(joined_attempt, handoffs, pin)) => {
                            // `join` transferred the waiter's protection into this
                            // pin before decrementing the waiter count, so the
                            // joined terminal has been continuously protected. Move
                            // that protection into the request lease (or drop it,
                            // leaving an unobserved validation join speculative).
                            let terminal = pin.terminal().clone();
                            let mut endorse_join = false;
                            if observe_result && !lock(&task.validation_endorsements).is_empty() {
                                // The owner's dependency leases remain in the
                                // owner's task. A waiter that will promote this
                                // joined root must therefore validate it in its
                                // own registered scope, reacquiring the exact
                                // descendant cone before the root becomes
                                // observable here.
                                let mut validation = self.core.valid_for_revision(&terminal, &task);
                                if matches!(validation, Ok((true, false, true))) {
                                    validation = self.core.valid_for_revision(&terminal, &task);
                                }
                                match validation {
                                    Ok((true, registered_only, _)) => {
                                        endorse_join = registered_only;
                                    }
                                    Ok((false, _, _)) => {
                                        task.defer_pin_release(pin);
                                        continue;
                                    }
                                    Err(abort) => {
                                        task.defer_pin_release(pin);
                                        return TaskQueryResult::Aborted {
                                            abort,
                                            dependencies: Vec::new(),
                                            inputs: Vec::new(),
                                            work: Vec::new(),
                                        };
                                    }
                                }
                            }
                            if observe_result && !task.observe_handoff(handoffs) {
                                self.detach_terminal_attempt(node, joined_attempt);
                                continue;
                            }
                            if observe_result {
                                if endorse_join {
                                    task.endorse_validation(&terminal);
                                }
                                self.publish_proof_to_batch(&task, &terminal, endorse_join);
                                task.observe(&terminal);
                                self.lease_observed_pin(&task, pin);
                                task.cache_query(self.inner.token, &lease.key, &terminal);
                            }
                            return TaskQueryResult::Terminal {
                                terminal,
                                execution: RequestExecution::Joined,
                                work: Vec::new(),
                            };
                        }
                        Ok(JoinOutcome::Retry) => continue,
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
                    let frame = task.pop(&exact_node);

                    let result = match body {
                        Ok(result) if !task.cancellation.is_canceled() => result,
                        Ok(_) => Err(QueryAbort::Canceled),
                        Err(payload) => {
                            self.abort_attempt(node, attempt);
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            frame.abort_handoffs();
                            resume_unwind(payload)
                        }
                    };

                    match result {
                        Ok(output) => {
                            let TaskFrameOutput {
                                dependencies,
                                inputs,
                                mut work,
                                handoffs,
                                cone_missing_observation,
                            } = frame;
                            let handoffs = handoffs.into_lifecycle();
                            let terminal = self.publish(
                                node,
                                attempt,
                                task.revision,
                                origin_request,
                                output,
                                dependencies,
                                inputs,
                                cone_missing_observation,
                                &task,
                                observe_result,
                                handoffs.clone(),
                            );
                            if acquired_here {
                                task.release_permit(&self.core);
                            }
                            if observe_result && !task.observe_handoff(handoffs) {
                                self.detach_terminal_attempt(node, attempt);
                                continue;
                            }
                            work.extend(terminal.work().iter().cloned());
                            let work = canonical_reduced_work(work);
                            if observe_result {
                                task.observe(&terminal);
                                task.cache_query(self.inner.token, &lease.key, &terminal);
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
                            let TaskFrameOutput {
                                dependencies,
                                inputs,
                                work,
                                handoffs,
                                cone_missing_observation: _,
                            } = frame;
                            task.observe_abort_prefix(&dependencies, &inputs, &work);
                            handoffs.abort();
                            return TaskQueryResult::Aborted {
                                abort,
                                dependencies,
                                inputs,
                                work,
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
    ) -> Result<JoinOutcome<K, V>, QueryAbort> {
        let mut state = lock(&node.state);
        if task.cancellation.is_canceled() {
            let enforce = decrement_waiter(&mut state, attempt_id);
            drop(state);
            if enforce {
                self.enforce_retention();
                self.core.enforce_runtime_retention();
            }
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
            return Err(QueryAbort::Canceled);
        }
        let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) else {
            return Ok(JoinOutcome::Retry);
        };
        match &mut attempt.state {
            AttemptState::Terminal {
                terminal,
                waiters,
                handoffs,
            } => {
                // Transfer this waiter's protection into a pin *before* dropping
                // the waiter count. Even when this is the last waiter — the count
                // falls to zero here — the pin is already established, so the
                // handoff leaves no instant in which the terminal is unprotected
                // and a concurrent enforcer racing it can never detach it.
                let pin = self
                    .pin_terminal(terminal)
                    .expect("a family pins its own retained terminal");
                let handoffs = handoffs.clone();
                *waiters -= 1;
                drop(state);
                #[cfg(test)]
                self.core.interpose(InterposeSite::JoinHandoff);
                return Ok(JoinOutcome::Joined(attempt_id, handoffs, pin));
            }
            AttemptState::Computing {
                owner: actual_owner,
                ..
            } => assert_eq!(*actual_owner, owner),
        }
        if self
            .core
            .begin_wait(
                task.id,
                owner,
                WaitEdgeLabel::Materialized(node.identity.clone()),
            )
            .is_err()
        {
            // Waiting here would close a wait-graph loop. That loop says two
            // tasks reached an overlapping set of nodes in opposite orders — a
            // scheduling conflict, not a statement about the query graph. A real
            // dependency cycle is a property of the request's structure and is
            // reported exactly by `Task::stack_cycle`, which sees through batch
            // boundaries via the task's ancestry.
            //
            // Failing the request here would make an ordinary interleaving
            // surface as a user-visible cycle, and which task loses the race is
            // nondeterministic. Decline the join instead and let the caller
            // claim a private attempt: query evaluation is deterministic, so a
            // duplicated computation costs work and never an answer, and
            // `publish` folds an equal result back onto the existing stamp.
            decrement_waiter(&mut state, attempt_id);
            self.core
                .metrics
                .declined_joins
                .fetch_add(1, Ordering::Relaxed);
            return Ok(JoinOutcome::Contended);
        }
        let cancellation_watch = task.cancellation.watch(&node.wait);
        let donated = task.release_permit(&self.core);
        if donated {
            self.core
                .metrics
                .donated_permits
                .fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        let mut enforce_after_cancellation = false;
        let result = loop {
            let mut state = lock(&node.state);
            if task.cancellation.is_canceled() {
                enforce_after_cancellation = decrement_waiter(&mut state, attempt_id);
                drop(state);
                break Err(QueryAbort::Canceled);
            }
            let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) else {
                break Ok(JoinOutcome::Retry);
            };
            match &mut attempt.state {
                AttemptState::Computing { .. } => {
                    drop(state);
                    node.wait.wait_until(
                        || {
                            if task.cancellation.is_canceled() {
                                return true;
                            }
                            let state = lock(&node.state);
                            !state.attempts.iter().any(|attempt| {
                                attempt.id == attempt_id
                                    && matches!(attempt.state, AttemptState::Computing { .. })
                            })
                        },
                        #[cfg(test)]
                        || self.core.interpose(InterposeSite::NodeJoinPark),
                    );
                }
                AttemptState::Terminal {
                    terminal,
                    waiters,
                    handoffs,
                } => {
                    // Transfer waiter protection into a pin before decrementing,
                    // as above: no unprotected instant even for the last waiter.
                    let pin = self
                        .pin_terminal(terminal)
                        .expect("a family pins its own retained terminal");
                    *waiters -= 1;
                    break Ok(JoinOutcome::Joined(attempt_id, handoffs.clone(), pin));
                }
            }
        };
        task.cancellation.unwatch(cancellation_watch);
        self.core.end_wait(task.id, owner);
        if donated {
            task.acquire_permit(&self.core);
        }
        if enforce_after_cancellation {
            self.enforce_retention();
            self.core.enforce_runtime_retention();
        }
        if matches!(result, Err(QueryAbort::Canceled)) {
            self.core
                .metrics
                .cancellations
                .fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(test)]
        if matches!(result, Ok(JoinOutcome::Joined(..))) {
            self.core.interpose(InterposeSite::JoinHandoff);
        }
        result
    }

    fn abort_attempt(&self, node: &Arc<Node<K, V>>, attempt_id: u64) {
        let mut state = lock(&node.state);
        if let Some(index) = state.attempts.iter().position(|item| item.id == attempt_id) {
            state.remove_attempt(index);
        }
        drop(state);
        node.wait.notify_all();
    }

    pub(crate) fn detach_terminal_attempt(&self, node: &Arc<Node<K, V>>, attempt_id: u64) {
        let removed = {
            let mut state = lock(&node.state);
            let Some(index) = state.attempts.iter().position(|item| item.id == attempt_id) else {
                return;
            };
            if !matches!(state.attempts[index].state, AttemptState::Terminal { .. }) {
                return;
            }
            let removed = state
                .remove_attempt(index)
                .expect("terminal attempt exists");
            match removed.state {
                AttemptState::Terminal { terminal, .. } => Some(terminal),
                AttemptState::Computing { .. } => unreachable!(),
            }
        };
        if let Some(terminal) = removed {
            self.core
                .metrics
                .retained_terminals
                .fetch_sub(1, Ordering::Relaxed);
            self.inner.retained_count.fetch_sub(1, Ordering::Relaxed);
            lock(&self.inner.retention)
                .remove_charge(terminal.retained_charge, terminal.dependency_pin_charge);
            node.wait.notify_all();
        }
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
        cone_missing_observation: bool,
        task: &Arc<Task>,
        lease: bool,
        handoffs: Arc<AttemptHandoffLifecycle>,
    ) -> Arc<QueryTerminal<V>> {
        let QueryOutput {
            outcome,
            kind,
            diagnostics,
            work,
            retained_value_charge,
        } = output;
        let diagnostics = canonical_diagnostics(diagnostics);
        let work = canonical_work(work);
        let retained_value_charge = match &outcome {
            QueryOutcome::Success(value) => Some(
                retained_value_charge.unwrap_or_else(|| (self.inner.retained_value_charge)(value)),
            ),
            QueryOutcome::Failure(_) => None,
        };
        let (retained_charge, dependency_pin_charge) = retained_terminal_charge(
            &outcome,
            retained_value_charge,
            &diagnostics,
            &work,
            &dependencies,
            &inputs,
        );
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
            terminal.kind == kind
                && outcomes_equal(self.inner.value_equal, &terminal.outcome, &outcome)
                && semantic_diagnostics_equal(&terminal.diagnostics, &diagnostics)
                // Cone purity is part of the red/green identity (ADR-0073): a
                // parent observes only (node, stamp), so a purity transition
                // must change the stamp, or parents would stay certified with
                // stale safety metadata and a later additive revision could
                // wrongly carry their certificates past a satisfied absence.
                && terminal.cone_missing_observation == cone_missing_observation
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
            outcome,
            kind,
            diagnostics: diagnostics.into(),
            work: work.into(),
            dependencies: dependencies.into(),
            inputs: inputs.into(),
            cone_missing_observation,
            retained_charge,
            dependency_pin_charge,
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
            handoffs,
        };
        // Publication proves the value for this revision, but not that its
        // dependency cone was reached exclusively through registered family
        // evaluators. A later authoritative validation may strengthen this
        // conservative bit for the separate retained-cone endorsement API.
        state.validated_at = Some(ValidationCertificate {
            revision,
            stamp,
            terminal_revision: revision,
            registered_only: false,
            epoch: task.revision_epoch,
            cone_missing_observation,
        });
        // Acquire the request lease *under the node lock*, before the terminal is
        // enqueued for retention or made reachable to a concurrent enforcer. The
        // pin's `pins > 0` is thus established atomically with publication: there
        // is no instant in which the just-published terminal is both evictable and
        // unleased. Speculative validation publications (`lease == false`) are
        // intentionally left evictable and take no pin here.
        let lease_pin = if lease {
            Some(
                self.pin_terminal(&terminal)
                    .expect("a family pins its own retained terminal"),
            )
        } else {
            None
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
        let aggregate_probe = lock(&self.inner.retention).publish(
            RetentionEntry {
                node: Arc::downgrade(node),
                attempt: attempt_id,
            },
            retained_charge,
            dependency_pin_charge,
        );
        // The terminal is now exposed and enqueued — reachable to any concurrent
        // enforcer. With `lease_pin` already held (acquired under the node lock
        // above) it is protected before it becomes evictable, so an enforcer that
        // runs at this exact instant cannot detach it.
        #[cfg(test)]
        self.core.interpose(InterposeSite::PublishExposed);
        // Transfer the pin into the task's request-scoped lease set (deduplicated),
        // so a tiny bound cannot evict the terminal at birth while the rooted
        // request that produced it is still running.
        if let Some(pin) = lease_pin {
            self.lease_observed_pin(task, pin);
        }
        node.wait.notify_all();
        if lease {
            self.enforce_retention_after_publish();
        } else {
            // A validation-only publication has no birth lease and can be
            // evicted immediately. Sweep in bounded batches rather than once
            // per publication: when a large protected prefix occupies the
            // queue, evicting one new unpinned terminal per full scan is
            // quadratic in the validation cone. The rooted task schedules one
            // final strict pass, so the family is back at its configured bound
            // when the request completes.
            task.defer_family_enforcement(self.retention_enforcer());
            self.enforce_retention_after_publish_with_margin(VALIDATION_PUBLISH_SWEEP_QUANTUM);
        }
        if aggregate_probe {
            self.core.enforce_runtime_retention_after_probe();
        }
        terminal
    }

    fn enforce_retention_after_publish(&self) {
        self.enforce_retention_after_publish_with_margin(1);
    }

    fn enforce_retention_after_publish_with_margin(&self, sweep_margin: usize) {
        let retained = self.inner.retained_count.load(Ordering::Acquire);
        loop {
            let threshold = self.inner.next_publish_sweep.load(Ordering::Acquire);
            if retained < threshold {
                return;
            }
            // Claim this watermark before taking the retention lock. Concurrent
            // publishers skip while this pass is pending; the pass installs the
            // next watermark from the count it observes at completion.
            if self
                .inner
                .next_publish_sweep
                .compare_exchange(threshold, usize::MAX, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.enforce_retention_with_margin(sweep_margin);
                return;
            }
        }
    }

    pub(crate) fn enforce_retention(&self) {
        // A released pin often asks an already-converged family for a strict
        // pass. Skip only when both the live count and publication watermark
        // are strict; a stale geometric watermark still needs the ordinary
        // pass below to rebase future publish-side enforcement.
        if retention_already_converged(
            &self.inner.retained_count,
            &self.inner.next_publish_sweep,
            self.inner.retention_limit,
        ) {
            return;
        }
        self.enforce_retention_with_margin(1);
    }

    fn enforce_retention_with_margin(&self, sweep_margin: usize) {
        self.core
            .metrics
            .retention_enforcements
            .fetch_add(1, Ordering::Relaxed);
        while self.inner.retained_count.load(Ordering::Relaxed) > self.inner.retention_limit
            && evict_one_from_family(&self.core, &self.inner)
        {}
        // The pass could not reach the configured bound: every remaining
        // candidate was a protected root the live closure still needs. Grow and
        // record the pressure event rather than evict a required terminal.
        let retained = self.inner.retained_count.load(Ordering::Relaxed);
        if retained > self.inner.retention_limit {
            self.core
                .metrics
                .retention_growth
                .fetch_add(1, Ordering::Relaxed);
        }
        let next_publish_sweep = if retained > self.inner.retention_limit {
            retained.saturating_mul(2).max(retained.saturating_add(1))
        } else {
            self.inner.retention_limit.saturating_add(sweep_margin)
        };
        self.inner
            .next_publish_sweep
            .store(next_publish_sweep, Ordering::Release);
    }

    pub(crate) fn retention_enforcer(&self) -> FamilyEnforcer {
        FamilyEnforcer {
            family_id: Arc::as_ptr(&self.inner) as *const () as usize,
            core: self.core.clone(),
            enforce: Box::new({
                let family = self.clone();
                move || family.enforce_retention()
            }),
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
            deferred: AtomicBool::new(false),
        })
    }

    /// Mint the exact-terminal adoption capability for a terminal of THIS
    /// family. Only a family REGISTERED CONTENT-ADDRESSED
    /// ([`QueryRuntime::content_addressed_family_with_equality`]) can mint
    /// one: the registration is the mechanical assertion that the key alone
    /// pins each terminal's value, which is what makes an input-free
    /// endorsement at another revision sound. Any other family is refused.
    pub fn adoptable_terminal(
        &self,
        terminal: &Arc<QueryTerminal<V>>,
    ) -> Result<AdoptableTerminal<V>, AdoptTerminalError> {
        if terminal.family_token != self.inner.token {
            return Err(AdoptTerminalError::ForeignFamily);
        }
        if !self.inner.content_addressed {
            return Err(AdoptTerminalError::NotContentAddressed);
        }
        Ok(AdoptableTerminal {
            terminal: terminal.clone(),
        })
    }

    /// Record an ALREADY-HELD adoptable terminal of this family as a
    /// dependency of the computing task: an exact-terminal capability that
    /// never hashes or compares the terminal's content key (the node is
    /// located by incarnation). The terminal must still be retained on its
    /// node — a stale or evicted terminal is rejected, never silently
    /// re-derived. On success the task observes the exact
    /// `{node, incarnation, stamp}` identity of the held terminal and leases
    /// it for the request's lifetime, and the node is endorsed at the task's
    /// pinned revision — an input-free republication with the SAME stamp,
    /// routed through the family's ordinary retained-publication accounting
    /// (metrics, retention queue, eviction) — so red/green validation of the
    /// recorded observation succeeds at this revision and its compatible
    /// descendants without touching the leaves the original computation
    /// observed. Soundness comes from the capability: only a
    /// content-addressed registration mints [`AdoptableTerminal`].
    pub fn observe_adopted_terminal(
        &self,
        context: &QueryContext,
        adoptable: &AdoptableTerminal<V>,
    ) -> Result<(), AdoptTerminalError> {
        let terminal = &adoptable.terminal;
        if terminal.family_token != self.inner.token {
            return Err(AdoptTerminalError::ForeignFamily);
        }
        if !self.inner.content_addressed {
            return Err(AdoptTerminalError::NotContentAddressed);
        }
        if !Arc::ptr_eq(&self.core, &context.task.core) {
            return Err(AdoptTerminalError::ForeignRuntime);
        }
        // Pin FIRST (mirroring reuse-candidate discovery) so the terminal
        // cannot be evicted between validation and the lease transfer below.
        let pin = self
            .pin_terminal(terminal)
            .map_err(|_| AdoptTerminalError::ForeignFamily)?;
        // Locate the node by INCARNATION — never by key hash or equality —
        // and recover this family's typed node from the erased handle.
        let node = self
            .core
            .registered_node(terminal.node_incarnation)
            .ok_or(AdoptTerminalError::Evicted)?;
        let node = node
            .as_any()
            .downcast::<Node<K, V>>()
            .map_err(|_| AdoptTerminalError::Evicted)?;
        // An incarnation-index anomaly (a recycled slot or foreign node) must
        // never endorse an unrelated node.
        if node.incarnation != terminal.node_incarnation || node.identity != terminal.node {
            return Err(AdoptTerminalError::Evicted);
        }
        let endorsed_pin = self.endorse_adopted_stamp(&node, context.task.revision, terminal)?;
        // Record the exact observation and transfer BOTH pins into the task's
        // request-scoped lease set: the held predecessor and the endorsement
        // itself, whose lease identity differs by its adopting revision. The
        // endorsement pin was acquired under the node lock, so there is no
        // instant in which it is both exposed and evictable.
        context.task.observe(terminal);
        self.lease_adopted_pin(&context.task, pin);
        self.lease_adopted_pin(&context.task, endorsed_pin);
        Ok(())
    }

    /// Endorse a still-retained stamp of `node` at `revision` by an input-free
    /// republication of the held value with the SAME stamp, accounted exactly
    /// like an ordinary retained publication: metered, enqueued for retention,
    /// and evictable — an adoption chain is bounded by the family's retention
    /// limit, and an evicted endorsement simply dirties its dependents through
    /// ordinary red/green validation.
    fn endorse_adopted_stamp(
        &self,
        node: &Arc<Node<K, V>>,
        revision: Revision,
        held: &Arc<QueryTerminal<V>>,
    ) -> Result<TerminalPin<K, V>, AdoptTerminalError> {
        // Resolved before the node lock: the store guard and node locks are
        // never held together on this path. A retired revision entry yields
        // the never-assigned epoch 0, so such a certificate can only satisfy
        // the exact-revision gate, never the cross-revision one.
        let revision_epoch = read(&self.core.revisions)
            .epoch_of(revision.id)
            .unwrap_or(0);
        let (attempt_id, endorsed_pin) = {
            let mut state = lock(&node.state);
            // The endorsed stamp must still be retained on this node; a stale
            // or evicted terminal is rejected, never silently re-derived.
            let retained = state.attempts.iter().any(|attempt| {
                matches!(
                    &attempt.state,
                    AttemptState::Terminal { terminal, .. } if terminal.stamp == held.stamp
                )
            });
            if !retained {
                return Err(AdoptTerminalError::Evicted);
            }
            // Idempotent per (revision, stamp); the scan is bounded by the
            // retention limit because endorsements are evictable entries. The
            // existing endorsement is re-pinned under the lock so THIS
            // adopting attempt protects it too.
            let endorsed = state.attempts.iter().find_map(|attempt| {
                if attempt.revision != revision {
                    return None;
                }
                match &attempt.state {
                    AttemptState::Terminal { terminal, .. }
                        if terminal.stamp == held.stamp && terminal.inputs.is_empty() =>
                    {
                        Some(terminal.clone())
                    }
                    _ => None,
                }
            });
            if let Some(endorsed) = endorsed {
                return Ok(self
                    .pin_terminal(&endorsed)
                    .expect("a family pins its own retained terminal"));
            }
            let adopted = Arc::new(QueryTerminal {
                family_token: held.family_token,
                node: held.node.clone(),
                node_incarnation: held.node_incarnation,
                revision,
                stamp: held.stamp,
                origin_request: held.origin_request,
                outcome: held.outcome.clone(),
                kind: held.kind,
                diagnostics: held.diagnostics.clone(),
                work: held.work.clone(),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                cone_missing_observation: held.cone_missing_observation,
                retained_charge: held.retained_charge,
                dependency_pin_charge: 0,
                pins: AtomicUsize::new(0),
            });
            let id = state.next_attempt;
            state.next_attempt += 1;
            state.attempts.push_back(Attempt {
                id,
                revision,
                state: AttemptState::Terminal {
                    terminal: adopted.clone(),
                    waiters: 0,
                    handoffs: AttemptHandoffLifecycle::shared_committed(),
                },
            });
            state.validated_at = Some(ValidationCertificate {
                revision,
                stamp: held.stamp,
                terminal_revision: revision,
                registered_only: true,
                epoch: revision_epoch,
                cone_missing_observation: held.cone_missing_observation,
            });
            // Pin UNDER the node lock, before the endorsement is enqueued or
            // reachable to a concurrent enforcer: `pins > 0` is established
            // atomically with insertion, so retention pressure (including the
            // enforcement pass below) can never evict it at birth.
            let pin = self
                .pin_terminal(&adopted)
                .expect("a family pins its own retained terminal");
            (id, pin)
        };
        self.core
            .metrics
            .green_publications
            .fetch_add(1, Ordering::Relaxed);
        self.core
            .metrics
            .retained_terminals
            .fetch_add(1, Ordering::Relaxed);
        self.inner.retained_count.fetch_add(1, Ordering::Relaxed);
        let aggregate_probe = lock(&self.inner.retention).publish(
            RetentionEntry {
                node: Arc::downgrade(node),
                attempt: attempt_id,
            },
            held.retained_charge,
            0,
        );
        self.enforce_retention_after_publish();
        if aggregate_probe {
            self.core.enforce_runtime_retention_after_probe();
        }
        Ok(endorsed_pin)
    }

    /// Transfers an already-acquired [`TerminalPin`] into a task's request-scoped
    /// lease set, retaining it for the lifetime of the rooted request.
    ///
    /// The pin is acquired by the caller *while the terminal is still protected*
    /// (under the node lock at publication, before decrementing waiter protection
    /// at a join, or on a candidate still retained under the node lock at reuse),
    /// then handed here. This makes acquisition atomic with retention: the
    /// terminal is continuously protected from the instant it is observed until
    /// the request's task drops, so a terminal the live computation still needs
    /// can never be evicted out from under it. Because nested queries execute in
    /// the same `task`, a nested observation inherits the root request's lease
    /// automatically. The lease releases — leaving no permanent retention — when
    /// the task drops.
    ///
    /// If the task has already leased this exact terminal (same node
    /// incarnation, stamp, and terminal revision), the redundant `pin` is
    /// dropped here rather than double-held. Ordinary validation-only terminals
    /// remain speculative and are dropped by the caller. A live registered
    /// endorsement scope also leases a valid validation-only candidate so final
    /// cone promotion can select the exact proof path; unrelated pins remain
    /// task-scoped and are excluded from the promoted graph walk.
    fn lease_observed_pin(&self, task: &Arc<Task>, pin: TerminalPin<K, V>) {
        let counter = if self.insert_task_lease(task, pin) {
            &task.validation_work.unique_terminal_lease_observations
        } else {
            &task.validation_work.duplicate_terminal_lease_observations
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Transfers exact terminals already held by the adoption capability.
    /// Adoption never resolves a family key or visits its memo index, so it is
    /// deliberately outside the repeated-query opportunity measured by
    /// [`ValidationWork::terminal_lease_observations`].
    fn lease_adopted_pin(&self, task: &Arc<Task>, pin: TerminalPin<K, V>) {
        self.insert_task_lease(task, pin);
    }

    /// RUE-1584: a batch child's freshly proved terminal becomes visible to
    /// concurrently running siblings now, not at this child's completion.
    /// The authority receives a backing pin of its own, minted here, so the
    /// published proof stays leased for the authority's whole lifetime even
    /// if the proving child aborts before its ordinary completion
    /// publication. `endorse` mirrors the task-local decision: endorsable
    /// proofs publish their exact endorsement, while every proof publishes
    /// its lease so siblings skip revalidating the exact terminal.
    /// Sequential batches skip this: their completion publication already
    /// precedes the next child's first probe.
    fn publish_proof_to_batch(&self, task: &Task, terminal: &Arc<QueryTerminal<V>>, endorse: bool) {
        let Some(authority) = task.batch_validation_authority.as_deref() else {
            return;
        };
        let Some(target) = authority.nearest_concurrent() else {
            return;
        };
        let Ok(pin) = self.pin_terminal(terminal) else {
            return;
        };
        target.publish_proof(
            (terminal.node_incarnation, terminal.stamp, terminal.revision),
            Box::new(pin),
            endorse,
        );
    }

    /// Inserts one exact terminal into the existing task lease set and reports
    /// whether this task had not already leased it.
    fn insert_task_lease(&self, task: &Arc<Task>, pin: TerminalPin<K, V>) -> bool {
        let mut leases = lock(&task.leases);
        let identity = (
            pin.terminal.node_incarnation,
            pin.terminal.stamp,
            pin.terminal.revision,
        );
        let inserted = leases.observed.insert(identity);
        if inserted {
            leases.held.push(Box::new(pin));
            task.core.metrics.task_lease_acquired();
        }
        inserted
    }

    /// Pins terminals computed under this exact retained revision.
    pub fn retain_revision(&self, revision: Revision) -> RevisionPin<K, V> {
        *lock(&self.inner.retained_revisions)
            .entry(revision)
            .or_default() += 1;
        RevisionPin {
            family: self.clone(),
            revision,
            view: self.core.pin_revision(revision).map(|(lease, _)| lease),
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
