//! Revision store, runtime core, and the public runtime surface.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};
use std::panic::resume_unwind;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};

use ahash::AHashSet;

use crate::*;

pub(crate) static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// Shared execution substrate for all query families in one database.
#[derive(Debug, Clone)]
pub struct QueryRuntime {
    pub(crate) core: Arc<RuntimeCore>,
}

#[derive(Debug)]
pub(crate) struct RuntimeCore {
    pub(crate) identity: u64,
    pub(crate) permits: PermitBudget,
    /// Active task-to-task waits. Ordinary joins reuse their memo node's
    /// materialized identity. Structured edges retain only a shared batch table
    /// plus an item index, formatting the typed key only if the edge participates
    /// in a cycle which must be rendered.
    pub(crate) wait_graph: Mutex<BTreeMap<TaskId, BTreeMap<TaskId, WaitEdgeLabel>>>,
    family_names: Mutex<BTreeSet<Arc<str>>>,
    pub(crate) revisions: RwLock<RevisionStore>,
    pub(crate) nodes: RwLock<NodeRegistry>,
    retention_families: Mutex<BTreeMap<u64, Weak<dyn RetentionFamily>>>,
    retention_budgets: RetentionBudgets,
    /// Aggregate thresholds are consulted only after a family-local probe, not
    /// by every publication.
    next_retained_byte_sweep: AtomicU64,
    next_dependency_pin_sweep: AtomicU64,
    /// Stable family token at which the next pressure pass begins. Only the
    /// single claimed sweep owner mutates this cursor, so the runtime can carry
    /// round-robin fairness across separate pressure events without putting a
    /// lock on ordinary publication.
    retention_sweep_cursor: AtomicU64,
    retention_sweep_claimed: AtomicBool,
    retention_sweep_pending: AtomicBool,
    /// Spawned structured-batch workers currently alive across every root and
    /// nesting depth. The caller always executes one child inline; this global
    /// counter caps additional OS threads at `permits.maximum - 1`.
    pub(crate) batch_workers: AtomicUsize,
    pub(crate) next_task: AtomicU64,
    next_family: AtomicU64,
    pub(crate) next_node: AtomicU64,
    pub(crate) metrics: Metrics,
    #[cfg(test)]
    test_events: TestEvents,
    /// Deterministic interposition hook for concurrency tests. When installed it
    /// is invoked at the retention-handoff sites (publication exposure, join
    /// waiter→pin handoff, reuse candidate discovery) so a test can drive a
    /// concurrent enforcer into the exact window the atomic handoff is meant to
    /// close. Never present in non-test builds and free of cost otherwise.
    #[cfg(test)]
    interpose: InterposeSlot,
}

#[derive(Debug, Default)]
pub(crate) struct NodeRegistry {
    // The index never owns a node. Its entries are removed by the matching
    // node's destructor, so registration and cleanup each address one
    // incarnation rather than scanning the live population.
    entries: HashMap<u64, RegisteredNode, BuildHasherDefault<IncarnationHasher>>,
    // Deterministic structural work: every inspection of a stored registry
    // value charges this counter. The counter is shared with the values so a
    // retain-style population traversal cannot hide behind one API call.
    #[cfg(test)]
    pub(crate) entry_visits: Arc<AtomicUsize>,
}

/// Identity hashing for runtime-owned, monotonically assigned incarnation IDs.
///
/// These keys are never caller-controlled, and the private runtime-ID indexes
/// do not expose iteration order. Caller-controlled typed family keys keep
/// their randomized hashers. Using the ID directly gives exact runtime-index
/// operations expected O(1) lookup without paying a general-purpose
/// string-resistant hashing cost.
#[derive(Debug, Default)]
pub(crate) struct IncarnationHasher(pub(crate) u64);

impl Hasher for IncarnationHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // `Hash for u64` uses `write_u64`; this fallback only keeps the Hasher
        // implementation total if the standard hashing route ever changes.
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.0 = hash;
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

/// Fast hashing for retained terminal and semantic-stamp identities.
///
/// The complete keys are runtime-owned: they combine a monotonically assigned,
/// runtime-unique node incarnation with one of that node's semantic stamps and,
/// for exact terminal identity, a runtime revision. Retention bounds also limit
/// the number of historical stamps and revisions per node. Mix these fixed-width
/// components without paying SipHash on the retention, validation, and cone
/// promotion hot paths; caller-controlled query-key maps keep randomized
/// hashing.
#[derive(Debug, Default)]
pub(crate) struct RetainedIdentityHasher(pub(crate) u64);

impl Hasher for RetainedIdentityHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0 ^ 0x9e3779b97f4a7c15;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.0 = hash;
    }

    fn write_u64(&mut self, value: u64) {
        let mut mixed = value.wrapping_add(0x9e3779b97f4a7c15);
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d049bb133111eb);
        mixed ^= mixed >> 31;
        self.0 = self.0.rotate_left(27) ^ mixed;
    }
}

pub(crate) type RetainedIdentityMap<K, V> =
    HashMap<K, V, BuildHasherDefault<RetainedIdentityHasher>>;
pub(crate) type RetainedIdentitySet<K> = HashSet<K, BuildHasherDefault<RetainedIdentityHasher>>;

#[derive(Debug)]
pub(crate) struct RegisteredNode {
    node: Weak<dyn ErasedNode>,
    #[cfg(test)]
    entry_visits: Arc<AtomicUsize>,
}

/// One family-local FIFO exposed to the runtime only while aggregate retention
/// is under pressure. Ordinary publication never consults this registry.
pub(crate) trait RetentionFamily: fmt::Debug + Send + Sync {
    /// Evicts the oldest currently unprotected terminal in this family. Stale
    /// FIFO entries are discarded as they are encountered.
    fn evict_one(&self) -> bool;

    /// Exact family-local byte/pin gauges without walking terminal nodes.
    fn charge_snapshot(&self) -> FamilyChargeSnapshot;
}

pub(crate) struct RetentionFamilyDriver {
    name: Arc<str>,
    evict_one: Box<dyn Fn() -> bool + Send + Sync>,
    charge_snapshot: Box<dyn Fn() -> FamilyChargeSnapshot + Send + Sync>,
}

impl fmt::Debug for RetentionFamilyDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetentionFamilyDriver")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl RetentionFamily for RetentionFamilyDriver {
    fn evict_one(&self) -> bool {
        (self.evict_one)()
    }

    fn charge_snapshot(&self) -> FamilyChargeSnapshot {
        (self.charge_snapshot)()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FamilyChargeSnapshot {
    pub(crate) retained_bytes: u64,
    pub(crate) dependency_pins: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RuntimeRetentionSnapshot {
    pub(crate) retained_bytes: u64,
    pub(crate) dependency_pins: u64,
    pub(crate) live_families: u64,
}

impl RegisteredNode {
    fn ptr_eq(&self, node: &Weak<dyn ErasedNode>) -> bool {
        #[cfg(test)]
        self.entry_visits.fetch_add(1, Ordering::Relaxed);
        Weak::ptr_eq(&self.node, node)
    }

    // This is the operation used by the former scan-on-insert implementation.
    // Keeping its structural charge on the stored value makes that regression
    // deterministic without adding production telemetry.
    #[cfg(test)]
    #[allow(dead_code)]
    fn strong_count(&self) -> usize {
        self.entry_visits.fetch_add(1, Ordering::Relaxed);
        self.node.strong_count()
    }
}

impl NodeRegistry {
    pub(crate) fn get(&self, incarnation: &u64) -> Option<&Weak<dyn ErasedNode>> {
        self.entries.get(incarnation).map(|entry| &entry.node)
    }

    pub(crate) fn insert(&mut self, incarnation: u64, node: Weak<dyn ErasedNode>) -> bool {
        let node = RegisteredNode {
            node,
            #[cfg(test)]
            entry_visits: self.entry_visits.clone(),
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(incarnation) {
            entry.insert(node);
            true
        } else {
            false
        }
    }

    pub(crate) fn remove(&mut self, incarnation: u64, node: &Weak<dyn ErasedNode>) {
        if self
            .entries
            .get(&incarnation)
            .is_some_and(|registered| registered.ptr_eq(node))
        {
            self.entries.remove(&incarnation);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Test-only holder for the interposition hook, with a `Debug` impl so the
/// enclosing `RuntimeCore` can still derive `Debug`.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct InterposeSlot(pub(crate) Mutex<Option<Arc<dyn Fn(InterposeSite) + Send + Sync>>>);

#[cfg(test)]
impl fmt::Debug for InterposeSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterposeSlot")
            .finish_non_exhaustive()
    }
}

/// The retention-handoff sites a concurrency test may interpose on.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterposeSite {
    /// A freshly published terminal has just been exposed and enqueued; with the
    /// fix its request lease pin is already held.
    PublishExposed,
    /// A joiner has transferred waiter protection into a pin and decremented the
    /// waiter count.
    JoinHandoff,
    /// The first reuse candidate has been discovered and pinned under the node
    /// lock, before recursive validation releases the lock.
    ReuseDiscovered,
    /// A node joiner observed a still-computing attempt and live cancellation
    /// token while holding the predicate lock, immediately before parking.
    NodeJoinPark,
    /// A lifecycle waiter observed neither completion nor cancellation while
    /// holding the predicate lock, immediately before atomically parking.
    HandoffCommitPark,
    /// Aggregate retention has finished a pass and is about to hand sweep
    /// ownership to any publisher which arrived concurrently.
    RetentionSweepRelease,
    /// A retained dependency's exact node has been upgraded from the
    /// incarnation registry and missed its validation memo, immediately before
    /// re-demanding the key from family-owned authority.
    RetainedDependencyDemand,
    /// A concurrent batch child has just published one freshly proved
    /// endorsement (with its backing lease) into the batch's shared
    /// authority, outside every lock.
    BatchProofPublished,
}

pub(crate) const REVISION_RETENTION_LIMIT: usize = 64;

#[derive(Debug)]
pub(crate) struct RevisionStore {
    entries: BTreeMap<u64, RevisionEntry>,
    retired_through: u64,
    /// Next validation epoch to assign. Epochs are runtime-global and
    /// monotone; equality is only ever compared, never ordered.
    next_epoch: u64,
    /// Per compatibility-namespace head of the current certificate-eligible
    /// extension chain (ADR-0073). An overlay publication preserves its
    /// parent's epoch only when the parent is this head and the delta is
    /// strictly additive; every other publication starts a fresh epoch, so
    /// same-epoch history is one linear chain by construction. Head ids may
    /// name retired entries; they are compared, never dereferenced.
    epoch_heads: BTreeMap<u64, EpochHead>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EpochHead {
    epoch: u64,
    head_id: u64,
}

impl RevisionStore {
    fn fresh_epoch(&mut self) -> u64 {
        let epoch = self.next_epoch;
        self.next_epoch += 1;
        epoch
    }

    pub(crate) fn epoch_of(&self, revision_id: u64) -> Option<u64> {
        self.entries.get(&revision_id).map(|entry| entry.epoch)
    }

    /// The stamp of `input` in `revision_id`, resolving through the revision's
    /// overlay chain. `None` means the leaf is absent (a recorded-absent optional
    /// leaf reads as absent here).
    pub(crate) fn input_stamp(&self, revision_id: u64, input: &InputIdentity) -> Option<u64> {
        self.entries.get(&revision_id)?.inputs.stamp(input)
    }

    /// Whether `input` is present in `revision_id` (through the overlay chain).
    fn input_present(&self, revision_id: u64, input: &InputIdentity) -> bool {
        self.input_stamp(revision_id, input).is_some()
    }
}

#[derive(Debug)]
pub(crate) struct RevisionEntry {
    revision: Revision,
    inputs: Arc<RevisionInputs>,
    active_requests: usize,
    /// Validation epoch this revision belongs to (ADR-0073).
    epoch: u64,
}

/// Overlay chains longer than this are compacted into one complete map at the
/// next successor publication, so lookup depth stays bounded even if a caller
/// publishes an unexpectedly long chain.
pub(crate) const OVERLAY_COMPACTION_DEPTH: usize = 16;

/// The immutable leaf view of one revision: either a complete map, or a sparse
/// successor overlay whose unresolved leaves are STRUCTURALLY INHERITED from the
/// parent's input node by `Arc` (RUE-1112). The parent input node is owned by the
/// overlay itself, so the parent's revision-store ENTRY may retire while every
/// child's logical view stays complete — retention never has to pin ancestors.
#[derive(Debug)]
pub(crate) enum RevisionInputs {
    Full(Arc<BTreeMap<InputIdentity, u64>>),
    Overlay {
        parent: Arc<RevisionInputs>,
        delta: Arc<BTreeMap<InputIdentity, u64>>,
        depth: usize,
    },
}

impl RevisionInputs {
    /// Resolve one leaf: the newest overlay delta wins, then ancestors in order.
    fn stamp(&self, input: &InputIdentity) -> Option<u64> {
        match self {
            Self::Full(map) => map.get(input).copied(),
            Self::Overlay { parent, delta, .. } => {
                delta.get(input).copied().or_else(|| parent.stamp(input))
            }
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Full(_) => 0,
            Self::Overlay { depth, .. } => *depth,
        }
    }

    /// Materialize the complete logical leaf map (used only for compaction).
    fn flatten(&self) -> BTreeMap<InputIdentity, u64> {
        match self {
            Self::Full(map) => map.as_ref().clone(),
            Self::Overlay { parent, delta, .. } => {
                let mut merged = parent.flatten();
                for (input, stamp) in delta.iter() {
                    merged.insert(input.clone(), *stamp);
                }
                merged
            }
        }
    }
}

pub(crate) struct RevisionLease {
    core: Arc<RuntimeCore>,
    revision: Revision,
}

impl Drop for RevisionLease {
    fn drop(&mut self) {
        let mut revisions = write(&self.core.revisions);
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
pub(crate) struct TestEvents {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl QueryRuntime {
    /// Creates a runtime with one shared structured concurrency budget.
    pub fn new(max_concurrency: usize) -> Self {
        Self::with_retention_budgets(max_concurrency, RetentionBudgets::default())
    }

    /// Creates a runtime with explicit runtime-wide soft retention budgets.
    ///
    /// This constructor is primarily useful for deterministic policy tests and
    /// benchmark calibration. A zero budget is valid: newly published work is
    /// reclaimed immediately when it has no live protection.
    pub fn with_retention_budgets(
        max_concurrency: usize,
        retention_budgets: RetentionBudgets,
    ) -> Self {
        assert!(max_concurrency > 0, "query concurrency must be nonzero");
        Self {
            core: Arc::new(RuntimeCore {
                identity: NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed),
                permits: PermitBudget::new(max_concurrency),
                wait_graph: Mutex::new(BTreeMap::new()),
                family_names: Mutex::new(BTreeSet::new()),
                revisions: RwLock::new(RevisionStore {
                    entries: BTreeMap::new(),
                    retired_through: 0,
                    next_epoch: 1,
                    epoch_heads: BTreeMap::new(),
                }),
                nodes: RwLock::new(NodeRegistry::default()),
                retention_families: Mutex::new(BTreeMap::new()),
                retention_budgets,
                next_retained_byte_sweep: AtomicU64::new(
                    retention_budgets.retained_bytes.saturating_add(1),
                ),
                next_dependency_pin_sweep: AtomicU64::new(
                    retention_budgets.dependency_pins.saturating_add(1),
                ),
                retention_sweep_cursor: AtomicU64::new(1),
                retention_sweep_claimed: AtomicBool::new(false),
                retention_sweep_pending: AtomicBool::new(false),
                batch_workers: AtomicUsize::new(0),
                next_task: AtomicU64::new(1),
                next_family: AtomicU64::new(1),
                next_node: AtomicU64::new(1),
                metrics: Metrics::default(),
                #[cfg(test)]
                test_events: TestEvents::default(),
                #[cfg(test)]
                interpose: InterposeSlot::default(),
            }),
        }
    }

    /// Creates a typed family with deterministic FIFO terminal retention.
    ///
    /// This convenience form assigns no heap-owned success-value charge. A
    /// caller retaining heap-backed values should use the corresponding
    /// `*_and_retained_charge` constructor or set an exact charge on each
    /// [`QueryOutput`]. The terminal envelope, diagnostics, work, and
    /// observations are always charged by the runtime.
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
    /// in red/green publication. This convenience form assigns no heap-owned
    /// success-value charge; see [`Self::family_with_equality_and_retained_charge`].
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
        self.family_with_optional_evaluator(stable_name, retention_limit, value_equal, |_| 0, None)
    }

    /// Creates an unregistered family with an estimator for heap-owned success
    /// value data.
    pub fn family_with_equality_and_retained_charge<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_with_optional_evaluator(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            None,
        )
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
            |_| 0,
            Some(Arc::new(evaluator)),
        )
    }

    /// Creates a registered family with an allocator-independent estimator for
    /// heap-owned success-value data. The estimator excludes inline `V` storage,
    /// which is already part of the terminal envelope.
    pub fn family_with_equality_and_evaluator_and_retained_charge<K, V, E>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
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
            retained_value_charge,
            Some(Arc::new(evaluator)),
        )
    }

    fn family_with_optional_evaluator<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
        evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_inner(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            evaluator,
            false,
        )
    }

    /// Creates a typed family REGISTERED CONTENT-ADDRESSED: the family asserts
    /// every record is a pure function of its key alone, so no revision leaf
    /// can change the value behind an unchanged key. This registration is the
    /// SOLE minting authority for [`AdoptableTerminal`]
    /// ([`QueryFamily::adoptable_terminal`]); an ordinary input-dependent
    /// family can never endorse a stale value through terminal adoption.
    pub fn content_addressed_family_with_equality<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_inner(stable_name, retention_limit, value_equal, |_| 0, None, true)
    }

    /// Creates a content-addressed family with an estimator for heap-owned
    /// success-value data.
    pub fn content_addressed_family_with_equality_and_retained_charge<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
    ) -> Result<QueryFamily<K, V>, FamilyError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        self.family_inner(
            stable_name,
            retention_limit,
            value_equal,
            retained_value_charge,
            None,
            true,
        )
    }

    fn family_inner<K, V>(
        &self,
        stable_name: impl Into<Arc<str>>,
        retention_limit: usize,
        value_equal: fn(&V, &V) -> bool,
        retained_value_charge: fn(&V) -> u64,
        evaluator: Option<Arc<FamilyEvaluator<K, V>>>,
        content_addressed: bool,
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
        let family_number = self.core.next_family.fetch_add(1, Ordering::Relaxed);
        let inner = Arc::new(FamilyInner {
            core: Arc::downgrade(&self.core),
            name: name.clone(),
            token: FamilyToken {
                runtime: self.core.identity,
                family: family_number,
            },
            content_addressed,
            retention_limit,
            value_equal,
            retained_value_charge,
            evaluator,
            nodes: ShardedNodeIndex::new(),
            retention: Mutex::new(FamilyRetentionQueue::new(self.core.retention_budgets)),
            retained_count: AtomicUsize::new(0),
            next_publish_sweep: AtomicUsize::new(retention_limit.saturating_add(1)),
            retained_nodes: AtomicUsize::new(0),
            retained_revisions: Mutex::new(BTreeMap::new()),
        });
        let weak_core = Arc::downgrade(&self.core);
        let weak_inner = Arc::downgrade(&inner);
        let charge_inner = Arc::downgrade(&inner);
        let retention_driver: Arc<dyn RetentionFamily> = Arc::new(RetentionFamilyDriver {
            name,
            evict_one: Box::new(move || {
                let Some(core) = weak_core.upgrade() else {
                    return false;
                };
                let Some(inner) = weak_inner.upgrade() else {
                    return false;
                };
                evict_one_from_family(&core, &inner)
            }),
            charge_snapshot: Box::new(move || {
                charge_inner
                    .upgrade()
                    .map_or_else(FamilyChargeSnapshot::default, |inner| {
                        family_charge_snapshot(&inner)
                    })
            }),
        });
        lock(&self.core.retention_families)
            .insert(family_number, Arc::downgrade(&retention_driver));
        Ok(QueryFamily {
            core: self.core.clone(),
            inner,
            retention_driver,
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
        let mut revisions = write(&self.core.revisions);
        match revisions.entries.get(&revision.id) {
            Some(previous) if previous.revision == revision => match previous.inputs.as_ref() {
                RevisionInputs::Full(previous_inputs)
                    if previous_inputs.as_ref() == inputs.as_ref() =>
                {
                    Ok(())
                }
                _ => Err(RevisionError::AlreadyPublished(revision)),
            },
            Some(_) => Err(RevisionError::AlreadyPublished(revision)),
            None => {
                if revision.id <= revisions.retired_through {
                    return Err(RevisionError::Retired(revision));
                }
                // An independent full view starts a fresh certificate epoch
                // and becomes its namespace's extension head (ADR-0073): it
                // may re-stamp or drop any leaf, so no prior certificate can
                // survive it, and no prior chain may extend past it.
                let epoch = revisions.fresh_epoch();
                revisions.epoch_heads.insert(
                    revision.compatibility,
                    EpochHead {
                        epoch,
                        head_id: revision.id,
                    },
                );
                revisions.entries.insert(
                    revision.id,
                    RevisionEntry {
                        revision,
                        inputs: Arc::new(RevisionInputs::Full(inputs)),
                        active_requests: 0,
                        epoch,
                    },
                );
                self.core.enforce_revision_retention(&mut revisions);
                Ok(())
            }
        }
    }

    /// Publishes a sparse immutable successor overlay revision (RUE-1112).
    ///
    /// The successor is a logically complete immutable input view: `delta` leaves
    /// resolve from the overlay, and every other leaf is STRUCTURALLY INHERITED
    /// from `parent`'s input node by `Arc` — only the delta is materialized. The
    /// delta may ADD new leaves and may RE-STAMP an existing leaf identity (the
    /// ordinary sparse representation of a changed derived input in a new
    /// immutable revision — the parent view itself is never mutated, and
    /// observers of a re-stamped identity are dirtied by red/green validation
    /// exactly as under a full publication). Removal is inexpressible.
    ///
    /// `parent` must be a currently retained revision of this runtime with the
    /// SAME compatibility token (an overlay never crosses a fresh observation
    /// generation), and `revision.id` must be strictly newer than `parent.id`
    /// (acyclic, monotonic lineage). The overlay owns the parent's input node, so
    /// the parent's revision-store entry may retire without breaking any child;
    /// chains deeper than [`OVERLAY_COMPACTION_DEPTH`] are compacted at
    /// publication. This layer does NOT verify domain additivity — a caller such
    /// as the compiler's trusted-toolchain lineage enforces its own
    /// strictly-additive source contract before publishing. Fresh generations
    /// keep using the complete [`Self::publish_revision`] path.
    pub fn publish_revision_overlay(
        &self,
        revision: Revision,
        parent: Revision,
        delta: impl IntoIterator<Item = (InputIdentity, u64)>,
    ) -> Result<(), RevisionError> {
        let mut delta_map = BTreeMap::new();
        for (input, stamp) in delta {
            if stamp == 0 {
                return Err(RevisionError::ReservedInputStamp(input));
            }
            if let Some(previous) = delta_map.insert(input.clone(), stamp)
                && previous != stamp
            {
                return Err(RevisionError::ConflictingInput(input));
            }
        }
        let delta_map = Arc::new(delta_map);
        let mut revisions = write(&self.core.revisions);
        let Some(parent_entry) = revisions
            .entries
            .get(&parent.id)
            .filter(|entry| entry.revision == parent)
        else {
            return Err(RevisionError::OverlayParentUnavailable(parent));
        };
        if revision.compatibility != parent.compatibility {
            return Err(RevisionError::IncompatibleOverlayParent(parent));
        }
        if revision.id <= parent.id {
            return Err(RevisionError::NonMonotonicOverlay(revision));
        }
        let parent_epoch = parent_entry.epoch;
        let parent_inputs = parent_entry.inputs.clone();
        // ADR-0073 epoch rule, decided mechanically at this boundary: the
        // child continues its parent's certificate epoch only when the parent
        // is the namespace's current extension head AND the delta strictly
        // adds new leaf identities. A re-stamped identity or a non-head
        // parent (an independent view or a sibling child) starts a fresh
        // epoch, so certificate-eligible same-epoch history stays one linear
        // additive chain by construction. The membership probes are bounded
        // by OVERLAY_COMPACTION_DEPTH.
        let parent_is_head = revisions
            .epoch_heads
            .get(&parent.compatibility)
            .is_some_and(|head| head.epoch == parent_epoch && head.head_id == parent.id);
        let strictly_additive = delta_map
            .keys()
            .all(|input| parent_inputs.stamp(input).is_none());
        let extends_head = parent_is_head && strictly_additive;
        let inputs = if parent_inputs.depth() >= OVERLAY_COMPACTION_DEPTH {
            // Compact a deep chain into one complete map so lookup depth stays
            // bounded; the merged map is the same logical view.
            let mut merged = parent_inputs.flatten();
            for (input, stamp) in delta_map.iter() {
                merged.insert(input.clone(), *stamp);
            }
            Arc::new(RevisionInputs::Full(Arc::new(merged)))
        } else {
            Arc::new(RevisionInputs::Overlay {
                depth: parent_inputs.depth() + 1,
                parent: parent_inputs,
                delta: delta_map.clone(),
            })
        };
        match revisions.entries.get(&revision.id) {
            Some(previous) if previous.revision == revision => {
                // Idempotent only for the identical logical view: same parent
                // input node and identical delta (or the identical compaction).
                let identical = match (previous.inputs.as_ref(), inputs.as_ref()) {
                    (
                        RevisionInputs::Overlay {
                            parent: previous_parent,
                            delta: previous_delta,
                            ..
                        },
                        RevisionInputs::Overlay { parent, delta, .. },
                    ) => Arc::ptr_eq(previous_parent, parent) && previous_delta == delta,
                    (RevisionInputs::Full(previous_map), RevisionInputs::Full(map)) => {
                        previous_map == map
                    }
                    _ => false,
                };
                if identical {
                    Ok(())
                } else {
                    Err(RevisionError::AlreadyPublished(revision))
                }
            }
            Some(_) => Err(RevisionError::AlreadyPublished(revision)),
            None => {
                if revision.id <= revisions.retired_through {
                    return Err(RevisionError::Retired(revision));
                }
                let epoch = if extends_head {
                    revisions.epoch_heads.insert(
                        revision.compatibility,
                        EpochHead {
                            epoch: parent_epoch,
                            head_id: revision.id,
                        },
                    );
                    parent_epoch
                } else {
                    let epoch = revisions.fresh_epoch();
                    revisions.epoch_heads.insert(
                        revision.compatibility,
                        EpochHead {
                            epoch,
                            head_id: revision.id,
                        },
                    );
                    epoch
                };
                revisions.entries.insert(
                    revision.id,
                    RevisionEntry {
                        revision,
                        inputs,
                        active_requests: 0,
                        epoch,
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
        if HandoffCallbackGuard::active() {
            return QueryRequestAttempt {
                id: 0,
                origin_request: origin_request.unwrap_or(0),
                execution: RequestExecution::Aborted,
                terminal: None,
                abort: Some(QueryAbort::Canceled),
                dependencies: Arc::from([]),
                inputs: Arc::from([]),
                work: Arc::from([]),
                nested_attempts: Arc::from([]),
                result_lease: Mutex::new(None),
            };
        }
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
                result_lease: Mutex::new(None),
            };
        }
        let id = self.core.next_task.fetch_add(1, Ordering::Relaxed);
        let origin_request = origin_request.unwrap_or(id);
        let Some((_revision_lease, revision_epoch)) = self.core.pin_revision(revision) else {
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
                result_lease: Mutex::new(None),
            };
        };
        let task = Arc::new(Task {
            id: TaskId(id),
            core: self.core.clone(),
            revision,
            revision_epoch,
            cancellation,
            owns_permit: AtomicBool::new(false),
            permit_timing: Mutex::new(PermitTiming::default()),
            longest_query_dependency_chain: AtomicU64::new(0),
            publish_critical_path: true,
            stack: Mutex::new(Vec::new()),
            ancestry: Arc::from([]),
            nested_attempts: Mutex::new(Vec::new()),
            nested_attempt_filters: Mutex::new(Vec::new()),
            validation_endorsements: Mutex::new(Vec::new()),
            batch_validation_authority: None,
            validation_proof_parent: None,
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
        });
        let result = match compute {
            Some(compute) => family.query_task(task.clone(), key, origin_request, compute),
            None => family.query_task_registered(task.clone(), key, origin_request),
        };
        let result = match result {
            TaskQueryResult::Terminal { terminal, work, .. } if task.cancellation.is_canceled() => {
                task.discard_observed_handoffs();
                TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: terminal.dependencies().to_vec(),
                    inputs: terminal.inputs().to_vec(),
                    work,
                }
            }
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => match task.commit_handoffs() {
                Ok(()) => TaskQueryResult::Terminal {
                    terminal,
                    execution,
                    work,
                },
                Err(RootHandoffCommitFailure::Canceled) => TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: terminal.dependencies().to_vec(),
                    inputs: terminal.inputs().to_vec(),
                    work,
                },
                Err(RootHandoffCommitFailure::Invalidated) => TaskQueryResult::Aborted {
                    abort: QueryAbort::Canceled,
                    dependencies: terminal.dependencies().to_vec(),
                    inputs: terminal.inputs().to_vec(),
                    work,
                },
                Err(RootHandoffCommitFailure::Panicked(payload)) => resume_unwind(payload),
            },
            aborted @ TaskQueryResult::Aborted { .. } => {
                task.discard_observed_handoffs();
                aborted
            }
        };
        let nested_attempts: Arc<[NestedQueryAttempt]> = lock(&task.nested_attempts).clone().into();
        match result {
            TaskQueryResult::Terminal {
                terminal,
                execution,
                work,
            } => {
                let origin_request = terminal.origin_request_id();
                // Carry a live result lease out of the request. The producing task
                // is still alive here (it drops at the end of this function), and
                // it still holds its own request-scoped lease on `terminal`, so
                // `terminal` is currently retained. Pinning it now hands protection
                // from the task's about-to-drop lease to the returned attempt with
                // no gap. The caller keeps the attempt alive until after it
                // registers a successor protection (selection root), closing the
                // promotion window entirely.
                let result_lease = family
                    .pin_terminal(&terminal)
                    .ok()
                    .map(|pin| Box::new(pin) as Box<dyn ObservedLease>);
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
                    result_lease: Mutex::new(result_lease),
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
                result_lease: Mutex::new(None),
            },
        }
    }

    /// Returns a point-in-time structural metrics snapshot.
    pub fn metrics(&self) -> RuntimeMetrics {
        let retention = self.core.retention_snapshot();
        self.core.record_retention_peaks(retention);
        let mut metrics = self
            .core
            .metrics
            .snapshot(self.core.retention_budgets, retention);
        metrics.retained_revisions = read(&self.core.revisions).entries.len() as u64;
        metrics
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn wait_for_metrics(&self, predicate: impl Fn(RuntimeMetrics) -> bool) {
        let mut generation = lock(&self.core.test_events.generation);
        while !predicate(self.metrics()) {
            generation = wait(&self.core.test_events.changed, generation);
        }
    }

    /// Installs a deterministic interposition hook for concurrency tests.
    #[cfg(test)]
    pub(crate) fn set_interpose(&self, hook: Arc<dyn Fn(InterposeSite) + Send + Sync>) {
        *lock(&self.core.interpose.0) = Some(hook);
    }

    /// Removes any installed interposition hook.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn clear_interpose(&self) {
        *lock(&self.core.interpose.0) = None;
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
    /// A successor overlay named a parent revision that is not currently a
    /// retained revision of this runtime lineage.
    OverlayParentUnavailable(Revision),
    /// A successor overlay named a parent from a different compatibility
    /// generation; an overlay never crosses a fresh observation generation.
    IncompatibleOverlayParent(Revision),
    /// A successor overlay's revision id is not strictly newer than its
    /// parent's; the overlay lineage is acyclic and monotonic.
    NonMonotonicOverlay(Revision),
}

/// Invalid family declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyError {
    /// Family names are stable identities and cannot be empty.
    EmptyName,
    /// A runtime already owns this stable family name.
    DuplicateName(Arc<str>),
}

impl RuntimeCore {
    /// Resolves an exact node for validation, preferring the weak handle minted
    /// with its display identity. Display-only or expired identities fall back
    /// to the shared incarnation index and charge that otherwise avoidable work.
    pub(crate) fn validation_node(
        &self,
        identity: &NodeIdentity,
        incarnation: u64,
        work: &AtomicValidationWork,
    ) -> Option<Arc<dyn ErasedNode>> {
        if let Some(node) = identity.registered_node(self.identity, incarnation) {
            return Some(node);
        }
        work.registry_index_lookups.fetch_add(1, Ordering::Relaxed);
        self.registered_node(incarnation)
    }

    /// Upgrades one exact registry entry while holding the registry read guard,
    /// then releases that guard before the erased node can run callbacks or
    /// recurse through validation.
    pub(crate) fn registered_node(&self, incarnation: u64) -> Option<Arc<dyn ErasedNode>> {
        let registry = read(&self.nodes);
        let node = registry.get(&incarnation).and_then(Weak::upgrade);
        drop(registry);
        node
    }

    fn live_retention_families(&self) -> Vec<(u64, Arc<dyn RetentionFamily>)> {
        let mut registered = lock(&self.retention_families);
        let mut live = Vec::with_capacity(registered.len());
        registered.retain(|token, family| {
            if let Some(family) = family.upgrade() {
                live.push((*token, family));
                true
            } else {
                false
            }
        });
        live
    }

    fn retention_charge_snapshot_from(
        families: &[(u64, Arc<dyn RetentionFamily>)],
    ) -> RuntimeRetentionSnapshot {
        let mut snapshot = families.iter().fold(
            RuntimeRetentionSnapshot::default(),
            |mut total, (_, family)| {
                let family = family.charge_snapshot();
                total.retained_bytes = total.retained_bytes.saturating_add(family.retained_bytes);
                total.dependency_pins =
                    total.dependency_pins.saturating_add(family.dependency_pins);
                total
            },
        );
        snapshot.live_families = u64::try_from(families.len()).unwrap_or(u64::MAX);
        snapshot
    }

    fn retention_snapshot(&self) -> RuntimeRetentionSnapshot {
        Self::retention_charge_snapshot_from(&self.live_retention_families())
    }

    fn record_retention_peaks(&self, snapshot: RuntimeRetentionSnapshot) {
        self.metrics
            .peak_retained_bytes
            .fetch_max(snapshot.retained_bytes, Ordering::Relaxed);
        self.metrics
            .peak_retained_dependency_pins
            .fetch_max(snapshot.dependency_pins, Ordering::Relaxed);
    }

    fn runtime_retention_over_budget(&self, snapshot: RuntimeRetentionSnapshot) -> (bool, bool) {
        (
            snapshot.retained_bytes > self.retention_budgets.retained_bytes,
            snapshot.dependency_pins > self.retention_budgets.dependency_pins,
        )
    }

    /// A family-local deterministic charge quantum crossed. This cold probe
    /// performs the cross-family sum and touches aggregate peak/pressure state;
    /// ordinary publications remain confined to their existing family lock.
    pub(crate) fn enforce_runtime_retention_after_probe(&self) {
        self.metrics
            .aggregate_retention_probes
            .fetch_add(1, Ordering::Relaxed);
        let snapshot = Self::retention_charge_snapshot_from(&self.live_retention_families());
        self.record_retention_peaks(snapshot);
        if snapshot.retained_bytes < self.next_retained_byte_sweep.load(Ordering::Acquire)
            && snapshot.dependency_pins < self.next_dependency_pin_sweep.load(Ordering::Acquire)
        {
            return;
        }
        self.enforce_runtime_retention();
    }

    pub(crate) fn enforce_runtime_retention(&self) {
        let snapshot = Self::retention_charge_snapshot_from(&self.live_retention_families());
        self.record_retention_peaks(snapshot);
        let (bytes_over, pins_over) = self.runtime_retention_over_budget(snapshot);
        if !bytes_over && !pins_over {
            return;
        }
        self.retention_sweep_pending.store(true, Ordering::Release);
        if self
            .retention_sweep_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        loop {
            self.retention_sweep_pending.store(false, Ordering::Release);
            self.run_runtime_retention_sweep();
            #[cfg(test)]
            self.interpose(InterposeSite::RetentionSweepRelease);
            if self.retention_sweep_pending.swap(false, Ordering::AcqRel) {
                continue;
            }
            self.retention_sweep_claimed.store(false, Ordering::Release);
            // Close the handoff race: a publisher which observed the old claim
            // sets `pending` before returning. If it arrived between our final
            // pending check and release, reclaim ownership and run its pass.
            if !self.retention_sweep_pending.swap(false, Ordering::AcqRel) {
                return;
            }
            if self
                .retention_sweep_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
        }
    }

    fn run_runtime_retention_sweep(&self) {
        let families = self.live_retention_families();
        let initial = Self::retention_charge_snapshot_from(&families);
        self.record_retention_peaks(initial);
        let (byte_pressure, pin_pressure) = self.runtime_retention_over_budget(initial);
        if !byte_pressure && !pin_pressure {
            return;
        }
        if byte_pressure {
            self.metrics
                .retained_byte_pressure_events
                .fetch_add(1, Ordering::Relaxed);
        }
        if pin_pressure {
            self.metrics
                .dependency_pin_pressure_events
                .fetch_add(1, Ordering::Relaxed);
        }

        // Family registration is cold. Snapshot and prune its weak entries only
        // under actual pressure, then perform all eviction without the registry
        // lock. BTreeMap order is the stable family-token order.
        let mut start = families.partition_point(|(token, _)| {
            *token < self.retention_sweep_cursor.load(Ordering::Relaxed)
        });
        if start == families.len() {
            start = 0;
        }
        loop {
            let snapshot = Self::retention_charge_snapshot_from(&families);
            let (bytes_over, pins_over) = self.runtime_retention_over_budget(snapshot);
            if !bytes_over && !pins_over {
                break;
            }
            let mut progress = false;
            for offset in 0..families.len() {
                let snapshot = Self::retention_charge_snapshot_from(&families);
                let (bytes_over, pins_over) = self.runtime_retention_over_budget(snapshot);
                if !bytes_over && !pins_over {
                    break;
                }
                let index = (start + offset) % families.len();
                let (token, family) = &families[index];
                if family.evict_one() {
                    progress = true;
                    let next = families
                        .get((index + 1) % families.len())
                        .map_or(token.saturating_add(1), |(token, _)| *token);
                    self.retention_sweep_cursor.store(next, Ordering::Relaxed);
                    if bytes_over {
                        self.metrics
                            .retained_byte_evictions
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    if pins_over {
                        self.metrics
                            .dependency_pin_evictions
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if !progress {
                break;
            }
            start = (start + 1) % families.len();
        }

        let retained = Self::retention_charge_snapshot_from(&families);
        let retained_bytes = retained.retained_bytes;
        let retained_pins = retained.dependency_pins;
        let byte_overage = retained_bytes.saturating_sub(self.retention_budgets.retained_bytes);
        let pin_overage = retained_pins.saturating_sub(self.retention_budgets.dependency_pins);
        if byte_overage > 0 {
            self.metrics
                .retained_byte_overflow_events
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .peak_retained_byte_overage
                .fetch_max(byte_overage, Ordering::Relaxed);
        }
        if pin_overage > 0 {
            self.metrics
                .dependency_pin_overflow_events
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .peak_dependency_pin_overage
                .fetch_max(pin_overage, Ordering::Relaxed);
        }
        let next_bytes = if byte_overage > 0 {
            retained_bytes
                .saturating_mul(2)
                .max(retained_bytes.saturating_add(1))
        } else {
            self.retention_budgets.retained_bytes.saturating_add(1)
        };
        let next_pins = if pin_overage > 0 {
            retained_pins
                .saturating_mul(2)
                .max(retained_pins.saturating_add(1))
        } else {
            self.retention_budgets.dependency_pins.saturating_add(1)
        };
        self.next_retained_byte_sweep
            .store(next_bytes, Ordering::Release);
        self.next_dependency_pin_sweep
            .store(next_pins, Ordering::Release);
    }

    pub(crate) fn revision_input(&self, revision: Revision, input: &InputIdentity) -> Option<u64> {
        let revisions = read(&self.revisions);
        if revisions
            .entries
            .get(&revision.id)
            .is_none_or(|entry| entry.revision != revision)
        {
            return None;
        }
        revisions.input_stamp(revision.id, input)
    }

    pub(crate) fn valid_for_revision<V>(
        &self,
        terminal: &QueryTerminal<V>,
        task: &Arc<Task>,
    ) -> Result<(bool, bool, bool), QueryAbort> {
        let traversal_work = ValidationTraversalWork {
            work: &task.validation_work,
            outcome: None,
        };
        let proof = task.begin_validation();
        let valid =
            self.valid_for_revision_inner(terminal, task, &mut ActiveValidations::default())?;
        let registered_only = proof.registered_only();
        let retryable = proof.retryable();
        if valid {
            self.mark_terminal_validated(terminal, task.revision, registered_only, task);
        }
        traversal_work.finish(valid);
        Ok((valid, registered_only, retryable))
    }

    fn mark_terminal_validated<V>(
        &self,
        terminal: &QueryTerminal<V>,
        revision: Revision,
        registered_only: bool,
        task: &Task,
    ) {
        if let Some(node) = self.validation_node(
            &terminal.node,
            terminal.node_incarnation,
            &task.validation_work,
        ) {
            if node.mark_validated(ValidationCertificate {
                revision,
                stamp: terminal.stamp,
                terminal_revision: terminal.revision,
                registered_only,
                epoch: task.revision_epoch,
                cone_missing_observation: terminal.cone_missing_observation,
            }) {
                task.validation_work
                    .certificates_published
                    .fetch_add(1, Ordering::Relaxed);
            }
        } else {
            task.validation_work
                .registry_misses
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn valid_for_revision_inner<V>(
        &self,
        terminal: &QueryTerminal<V>,
        task: &Arc<Task>,
        active: &mut ActiveValidations,
    ) -> Result<bool, QueryAbort> {
        if !terminal.revision.is_compatible_with(task.revision) {
            return Ok(false);
        }
        // Compatibility tokens are only a scheduling hint. Direct inputs are
        // checked exactly, while dependency stamps are validated recursively
        // against the current compatible terminal of the exact child node.
        let revisions = read(&self.revisions);
        if revisions
            .entries
            .get(&task.revision.id)
            .is_none_or(|entry| entry.revision != task.revision)
        {
            return Ok(false);
        }
        task.validation_work
            .input_observations
            .fetch_add(terminal.inputs.len() as u64, Ordering::Relaxed);
        let direct_inputs_valid = terminal.inputs.iter().all(|observed| {
            if observed.stamp == 0 {
                !revisions.input_present(task.revision.id, &observed.input)
            } else {
                revisions.input_stamp(task.revision.id, &observed.input) == Some(observed.stamp)
            }
        });
        // Registered-node validation can reenter input lookup and recursively
        // validate descendants. Never carry the revision guard across that
        // boundary: publication, lease release, and retention must remain able
        // to acquire the exclusive store guard.
        drop(revisions);
        if !direct_inputs_valid {
            return Ok(false);
        }
        // These two counters advance together for every inspected dependency.
        // Accumulate the exact attempted prefix locally so the common complete
        // traversal performs two atomic updates rather than two per edge. The
        // guard also flushes an early return or evaluator unwind.
        let mut dependency_work = DependencyValidationWork {
            work: &task.validation_work,
            observations: 0,
        };
        for observed in terminal.dependencies.iter() {
            dependency_work.observe();
            let node =
                self.validation_node(&observed.node, observed.incarnation, &task.validation_work);
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
                    // Validation follows the predecessor's dependency graph.
                    // A legal edit may reverse an edge: while computing the
                    // current parent, checking the stale child can temporarily
                    // point back to that parent. This is evidence that the
                    // retained candidate is dirty, not that the current graph
                    // is cyclic. Recompute from current inputs; a real cycle is
                    // then rediscovered by the ordinary observed request.
                    Err(QueryAbort::Cycle(_)) => return Ok(false),
                    Err(abort) => return Err(abort),
                },
                None => {
                    task.validation_work
                        .registry_misses
                        .fetch_add(1, Ordering::Relaxed);
                    None
                }
            };
            if stamp != Some(observed.stamp) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Pins `revision` against retention and returns its lease together with
    /// the revision's validation epoch (ADR-0073), resolved under the same
    /// store guard so tasks carry the epoch without a second lookup.
    pub(crate) fn pin_revision(
        self: &Arc<Self>,
        revision: Revision,
    ) -> Option<(RevisionLease, u64)> {
        let mut revisions = write(&self.revisions);
        let entry = revisions
            .entries
            .get_mut(&revision.id)
            .filter(|entry| entry.revision == revision)?;
        entry.active_requests += 1;
        let epoch = entry.epoch;
        Some((
            RevisionLease {
                core: self.clone(),
                revision,
            },
            epoch,
        ))
    }

    fn enforce_revision_retention(&self, revisions: &mut RevisionStore) {
        // An overlay entry owns its parent's INPUT NODE by `Arc`, so retiring a
        // parent's revision-store entry never breaks a child's logical view;
        // retention stays a simple bounded sweep with no ancestor pinning.
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
    pub(crate) fn test_changed(&self) {
        *lock(&self.test_events.generation) += 1;
        self.test_events.changed.notify_all();
    }

    /// Invokes the installed interposition hook, if any, for `site`. The hook is
    /// cloned out and the lock released before calling, so the hook may reenter
    /// the runtime (issue queries, install/clear itself) without deadlocking.
    #[cfg(test)]
    pub(crate) fn interpose(&self, site: InterposeSite) {
        let hook = lock(&self.interpose.0).clone();
        if let Some(hook) = hook {
            hook(site);
        }
    }

    pub(crate) fn begin_wait(
        &self,
        waiter: TaskId,
        owner: TaskId,
        label: WaitEdgeLabel,
    ) -> Result<(), Arc<[NodeIdentity]>> {
        let mut graph = lock(&self.wait_graph);
        let previous = graph.entry(waiter).or_default().insert(owner, label);
        assert!(
            previous.is_none(),
            "one task cannot register the same wait owner twice"
        );
        let mut path = Vec::new();
        let mut visited = BTreeSet::new();
        if wait_path(&graph, owner, waiter, &mut visited, &mut path) {
            let edges = graph.get_mut(&waiter).expect("new wait edge is present");
            path.push(
                edges
                    .remove(&owner)
                    .expect("the newly inserted wait edge is present"),
            );
            if edges.is_empty() {
                graph.remove(&waiter);
            }
            drop(graph);
            return Err(canonical_cycle(
                path.into_iter()
                    .map(|label| label.node_identity(&self.metrics)),
            ));
        }
        Ok(())
    }

    pub(crate) fn end_wait(&self, waiter: TaskId, owner: TaskId) {
        let mut graph = lock(&self.wait_graph);
        let Some(edges) = graph.get_mut(&waiter) else {
            return;
        };
        edges.remove(&owner);
        if edges.is_empty() {
            graph.remove(&waiter);
        }
    }
}

pub(crate) fn wait_path(
    graph: &BTreeMap<TaskId, BTreeMap<TaskId, WaitEdgeLabel>>,
    current: TaskId,
    target: TaskId,
    visited: &mut BTreeSet<TaskId>,
    path: &mut Vec<WaitEdgeLabel>,
) -> bool {
    if !visited.insert(current) {
        return false;
    }
    let Some(edges) = graph.get(&current) else {
        return false;
    };
    for (owner, label) in edges {
        path.push(label.clone());
        if *owner == target || wait_path(graph, *owner, target, visited, path) {
            return true;
        }
        path.pop();
    }
    false
}

#[derive(Debug)]
pub(crate) struct PermitBudget {
    pub(crate) maximum: usize,
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

    pub(crate) fn acquire(&self) {
        let mut used = lock(&self.used);
        while *used == self.maximum {
            used = wait(&self.available, used);
        }
        *used += 1;
    }

    pub(crate) fn release(&self) {
        let mut used = lock(&self.used);
        assert!(*used > 0, "cannot release an unowned query permit");
        *used -= 1;
        drop(used);
        self.available.notify_one();
    }
}

/// Drop one waiter and report whether an already-terminal attempt just lost its
/// final waiter. Callers use the result only after releasing the node-state
/// lock, because retention enforcement may revisit this node.
pub(crate) fn decrement_waiter<V>(state: &mut NodeState<V>, attempt_id: u64) -> bool {
    if let Some(attempt) = state.attempts.iter_mut().find(|item| item.id == attempt_id) {
        match &mut attempt.state {
            AttemptState::Computing { waiters, .. } => {
                assert!(*waiters > 0, "a computing waiter releases exactly once");
                *waiters -= 1;
            }
            AttemptState::Terminal { waiters, .. } => {
                assert!(*waiters > 0, "a terminal waiter releases exactly once");
                *waiters -= 1;
                return *waiters == 0;
            }
        }
    }
    false
}

/// Deterministic retained charge of one published terminal.
///
/// ADR-0074 denominates identity structurally: a node, each observed
/// dependency, and each observed input are charged one fixed
/// [`IDENTITY_CHARGE_BYTES`] rather than the byte length of a formatted
/// family/key pair. Every other term — the terminal header, the retained
/// payload, diagnostics, work items, and the per-observation headers — is
/// unchanged, so the charge still tracks what retention actually holds while
/// no longer requiring a name for anything.
pub(crate) fn retained_terminal_charge<V>(
    outcome: &QueryOutcome<V>,
    retained_value_charge: Option<u64>,
    diagnostics: &[QueryDiagnostic],
    work: &[(Arc<str>, u64)],
    dependencies: &[Observation],
    inputs: &[InputObservation],
) -> (u64, u64) {
    let mut bytes = std::mem::size_of::<QueryTerminal<V>>() as u64;
    bytes = bytes.saturating_add(IDENTITY_CHARGE_BYTES);
    bytes = bytes.saturating_add(match outcome {
        QueryOutcome::Success(_) => retained_value_charge.unwrap_or(0),
        QueryOutcome::Failure(failure) => {
            (failure.code.len() as u64).saturating_add(failure.payload.len() as u64)
        }
    });
    for diagnostic in diagnostics {
        bytes = bytes
            .saturating_add(std::mem::size_of::<QueryDiagnostic>() as u64)
            .saturating_add(diagnostic.identity.len() as u64)
            .saturating_add(diagnostic.payload.len() as u64);
        if let Some(position) = &diagnostic.presentation {
            bytes = bytes
                .saturating_add(std::mem::size_of::<PresentationPosition>() as u64)
                .saturating_add(position.source.len() as u64);
        }
    }
    for (identity, _) in work {
        bytes = bytes
            .saturating_add(std::mem::size_of::<(Arc<str>, u64)>() as u64)
            .saturating_add(identity.len() as u64);
    }
    for _ in dependencies {
        bytes = bytes
            .saturating_add(std::mem::size_of::<Observation>() as u64)
            .saturating_add(IDENTITY_CHARGE_BYTES);
    }
    for _ in inputs {
        bytes = bytes
            .saturating_add(std::mem::size_of::<InputObservation>() as u64)
            .saturating_add(IDENTITY_CHARGE_BYTES);
    }
    let dependency_pins =
        u64::try_from(dependencies.len().saturating_add(inputs.len())).unwrap_or(u64::MAX);
    (bytes, dependency_pins)
}
