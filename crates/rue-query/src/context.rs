//! Query context and registered-batch execution.

use std::collections::VecDeque;
use std::fmt;
use std::hash::BuildHasherDefault;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::*;

/// Registered evaluators historically ran on the requesting compiler thread,
/// whose platform stack is materially larger than Rust's default spawned-thread
/// stack. Keep structured batch children at that established floor so moving a
/// valid deeply nested query onto a worker cannot create a stack overflow.
pub(crate) const REGISTERED_BATCH_WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

fn batch_completion_coordinator_residual_ns(
    wait_started: Instant,
    worker_finished_at: Instant,
    wait_returned_at: Instant,
) -> u64 {
    duration_ns(wait_returned_at.duration_since(worker_finished_at.max(wait_started)))
}

/// One contention-free publication for a registered batch, including unwind.
struct BatchCoordinatorMeasurement<'a> {
    metrics: &'a Metrics,
    thread_births: u64,
    coordinator_residual_ns: u64,
}

impl<'a> BatchCoordinatorMeasurement<'a> {
    fn new(metrics: &'a Metrics) -> Self {
        Self {
            metrics,
            thread_births: 0,
            coordinator_residual_ns: 0,
        }
    }

    fn record_submission(&mut self, thread_births: u64, coordinator_residual_ns: u64) {
        self.thread_births = self.thread_births.saturating_add(thread_births);
        self.coordinator_residual_ns = self
            .coordinator_residual_ns
            .saturating_add(coordinator_residual_ns);
    }

    fn record_completion_residual(&mut self, coordinator_residual_ns: u64) {
        self.coordinator_residual_ns = self
            .coordinator_residual_ns
            .saturating_add(coordinator_residual_ns);
    }
}

impl Drop for BatchCoordinatorMeasurement<'_> {
    fn drop(&mut self) {
        self.metrics
            .batch_worker_thread_births
            .fetch_add(self.thread_births, Ordering::Relaxed);
        self.metrics
            .batch_worker_coordinator_residual_ns
            .fetch_add(self.coordinator_residual_ns, Ordering::Relaxed);
    }
}

/// Task-scoped access to nested dependency queries.
#[derive(Debug)]
pub struct QueryContext {
    pub(crate) task: Arc<Task>,
    pub(crate) not_send_or_sync: PhantomData<Rc<()>>,
}

pub(crate) struct RegisteredBatchItem<K> {
    pub(crate) request_id: u64,
    pub(crate) key: K,
    pub(crate) ready_at: Instant,
}

pub(crate) struct RegisteredBatchItems<K> {
    pub(crate) family: Arc<str>,
    pub(crate) items: Vec<RegisteredBatchItem<K>>,
}

impl<K> fmt::Debug for RegisteredBatchItems<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredBatchItems")
            .field("family", &self.family)
            .field("items", &self.items.len())
            .finish()
    }
}

/// Type-erased view over the one typed key table already owned by a registered
/// batch. Wait edges keep this table alive through cancellation and completed
/// children without allocating or formatting one label per edge.
pub(crate) trait StructuredWaitLabels: fmt::Debug + Send + Sync {
    fn node_identity(&self, index: usize) -> NodeIdentity;
}

impl<K: QueryKey> StructuredWaitLabels for RegisteredBatchItems<K> {
    fn node_identity(&self, index: usize) -> NodeIdentity {
        let item = self
            .items
            .get(index)
            .expect("a structured wait edge names one live batch item");
        NodeIdentity::from_key(self.family.clone(), &item.key)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum WaitEdgeLabel {
    Materialized(NodeIdentity),
    Structured {
        labels: Arc<dyn StructuredWaitLabels>,
        index: usize,
    },
}

impl WaitEdgeLabel {
    pub(crate) fn node_identity(&self, metrics: &Metrics) -> NodeIdentity {
        match self {
            Self::Materialized(node) => node.clone(),
            Self::Structured { labels, index } => {
                let node = labels.node_identity(*index);
                metrics.record_structured_wait_identity(node.key().len());
                node
            }
        }
    }
}

pub(crate) type BatchCompletion<V> = (usize, Arc<Task>, TaskQueryResult<V>);

pub(crate) fn run_registered_batch_worker<K, V>(
    queue: Arc<Mutex<VecDeque<usize>>>,
    items: Arc<RegisteredBatchItems<K>>,
    family: QueryFamily<K, V>,
    parent: Arc<Task>,
    authority: Arc<BatchValidationAuthority>,
    tracing_dispatch: tracing::Dispatch,
    tracing_parent: tracing::Span,
) -> std::thread::Result<Vec<BatchCompletion<V>>>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    tracing::dispatcher::with_default(&tracing_dispatch, || {
        parent
            .core
            .metrics
            .batch_worker_lanes_entered
            .fetch_add(1, Ordering::Relaxed);
        let parent_span = tracing_parent.enter();
        let mut ready_items = 0u64;
        let mut ready_wait_ns = 0u64;
        let mut max_ready_wait_ns = 0u64;
        let mut query_worker_active_ns = 0u64;
        let mut longest_query_dependency_chain = 0u64;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut completed = Vec::new();
            loop {
                let Some(index) = lock(&queue).pop_front() else {
                    break;
                };
                let item = &items.items[index];
                let wait_ns = duration_ns(item.ready_at.elapsed());
                ready_items = ready_items.saturating_add(1);
                ready_wait_ns = ready_wait_ns.saturating_add(wait_ns);
                max_ready_wait_ns = max_ready_wait_ns.max(wait_ns);
                let child = parent.batch_child(item.request_id, authority.clone());
                let result =
                    family.query_task_registered(child.clone(), item.key.clone(), item.request_id);
                if matches!(result, TaskQueryResult::Terminal { .. }) {
                    authority.publish_child(&child);
                }
                query_worker_active_ns = query_worker_active_ns
                    .saturating_add(lock(&child.permit_timing).accumulated_ns);
                longest_query_dependency_chain = longest_query_dependency_chain
                    .max(child.longest_query_dependency_chain.load(Ordering::Relaxed));
                completed.push((index, child, result));
            }
            completed
        }));
        parent
            .core
            .metrics
            .ready_items
            .fetch_add(ready_items, Ordering::Relaxed);
        parent
            .core
            .metrics
            .ready_wait_ns
            .fetch_add(ready_wait_ns, Ordering::Relaxed);
        parent
            .core
            .metrics
            .max_ready_wait_ns
            .fetch_max(max_ready_wait_ns, Ordering::Relaxed);
        parent
            .core
            .metrics
            .query_worker_active_ns
            .fetch_add(query_worker_active_ns, Ordering::Relaxed);
        parent
            .core
            .metrics
            .longest_query_dependency_chain
            .fetch_max(longest_query_dependency_chain, Ordering::Relaxed);
        drop(parent_span);
        // A tracing subscriber may buffer observations locally to avoid
        // perturbing parallel query execution. This generic lifecycle marker
        // lets it publish once at the bounded worker-completion boundary; the
        // runtime knows nothing about compiler phases or measurement schemas.
        tracing::trace!(timing_flush = true, "registered query worker complete");
        result
    })
}

impl QueryContext {
    /// Exact immutable revision pinned by this task.
    pub fn revision(&self) -> Revision {
        self.task.revision
    }

    /// Maximum number of query evaluators this runtime may execute concurrently.
    ///
    /// A scheduler can use this immutable construction-time limit to avoid
    /// creating structured child tasks when the runtime has only one permit and
    /// same-task proof sharing is therefore strictly cheaper.
    pub fn max_concurrency(&self) -> usize {
        self.task.core.permits.maximum
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

    /// Registers one non-semantic resource for atomic attempt completion.
    ///
    /// The handoff belongs to the current evaluator frame and, after successful
    /// publication, to that internal terminal attempt. The whole top-level root
    /// commits all handoffs it observed, including through nested query, reuse,
    /// and join. A root abort leaves published handoffs pending; speculative
    /// validation never claims them; terminal eviction aborts them.
    pub fn register_attempt_handoff(&self, handoff: impl QueryAttemptHandoff) {
        self.task.register_attempt_handoff(Box::new(handoff));
    }

    /// Restricts operational nested-attempt ledger materialization for this
    /// scope and every nested query it evaluates.
    ///
    /// Query execution and semantic bookkeeping are unchanged: dependency
    /// observations, validation, request leases, cancellation, work,
    /// handoffs, memoization, and terminal publication all still run exactly
    /// as they do without this scope. Only [`QueryRequestAttempt::nested_attempts`]
    /// is filtered. Nested scopes intersect with their parent selection, so an
    /// inner evaluator cannot restore rows its caller chose not to retain.
    ///
    /// The returned guard restores the preceding selection when dropped,
    /// including during unwinding.
    pub fn retain_nested_attempts_for(&self, families: &[&str]) -> NestedAttemptFilterGuard {
        self.task.push_nested_attempt_filter(families)
    }

    /// Enables task-local reuse of full registered-only validation proofs.
    ///
    /// A proof is endorsed only after recursively validating the exact
    /// terminal and every dependency without crossing an unregistered node.
    /// Every terminal in that registered cone remains pinned by the task.
    /// Cache hits still follow the ordinary request path for cancellation,
    /// direct dependency observation, handoffs, work, and request-ledger
    /// classification; only repeated recursive validation is skipped.
    pub fn endorse_registered_validations(&self) -> ValidationEndorsementGuard {
        self.task
            .push_validation_endorsement_scope(&[])
            .expect("an empty validation authority cannot name a foreign runtime")
    }

    /// Enables registered-proof reuse backed by live published pin sets.
    ///
    /// A current revision certificate may skip recursive validation when this
    /// task has already endorsed its exact terminal identity or one of
    /// `fallbacks` retains the same node incarnation and semantic stamp. Query
    /// dependency edges use that same identity: final promotion selects the
    /// retained representative and walks its own complete cone, failing closed
    /// if any edge is absent. Borrowed authority never bypasses validation of a
    /// requested root itself. The returned guard owns the fallback Arcs for the
    /// complete lexical scope, so borrowed authority cannot outlive its pins.
    /// Empty sets are accepted; a set containing any foreign-runtime pin fails
    /// closed before the scope becomes active. Nested scopes form one safe
    /// authority union: a fallback first introduced by an inner scope remains
    /// pinned and usable until the oldest enclosing endorsement guard drops,
    /// because endorsements proven inside that scope are promoted outward too.
    pub fn endorse_registered_validations_from(
        &self,
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<ValidationEndorsementGuard, RetainTerminalConeError> {
        self.task.push_validation_endorsement_scope(fallbacks)
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
        // Caller-supplied evaluators have no family-owned re-demand authority.
        // Crossing this boundary anywhere inside a registered-only validation
        // walk must fail the certificate closed, including when a structured
        // batch child computes the unregistered node rather than validating a
        // retained one.
        self.task.taint_validation_proofs();
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
            move || NodeIdentity::from_key(family.inner.name.clone(), &key),
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
            move || NodeIdentity::from_key(family.inner.name.clone(), &key),
            &result,
        );
        result.into_result()
    }

    /// Observes an already-published exact-current-revision terminal without
    /// creating a node, evaluating, joining, or waiting. A miss or not-ready
    /// result is intentionally typed so callers can materialize a separate
    /// plan without accidentally demanding this registered family.
    pub fn probe_registered_ready<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
    ) -> Result<ReadyQueryProbe<V>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "ready probes require a registered evaluator"
        );
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        let request_id = self.task.next_nested_request();
        family.probe_task_registered_ready(&self.task, &key, request_id)
    }

    /// Reuses or joins an exact-current-revision registered query without
    /// creating an incarnation or invoking its evaluator. A cold or stale
    /// key is [`ReadyQueryProbe::Miss`]; an in-flight handoff that cannot be
    /// safely reused is [`ReadyQueryProbe::NotReady`]. If the owner aborts,
    /// this observer does not claim the key and rescans for a current attempt.
    pub fn join_registered_noncomputing<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        key: K,
    ) -> Result<ReadyQueryProbe<V>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "non-computing joins require a registered evaluator"
        );
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        let request_id = self.task.next_nested_request();
        family.join_task_registered_noncomputing(&self.task, &key, request_id)
    }

    fn query_registered_ref<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        key: &K,
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
        self.task.record_nested(
            request_id,
            move || NodeIdentity::from_key(family.inner.name.clone(), key),
            &result,
        );
        result.into_result()
    }

    /// Requests stable-ordered registered dependencies with concurrency-aware
    /// scheduling.
    ///
    /// When an enclosing batch has already reserved every nested worker slot,
    /// this keeps every request in the current task, preserving the same
    /// dependency observations and result order without allocating structured
    /// children or donating a permit. Otherwise independent evaluators use the
    /// ordinary structured batch and can run in parallel.
    pub fn query_registered_adaptive_batch<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<Vec<Arc<QueryTerminal<V>>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free dependency requests require a registered evaluator"
        );
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        if self.max_concurrency() == 1 {
            return keys
                .into_iter()
                .map(|key| self.query_registered_ref(family, &key))
                .collect();
        }
        let keys = keys.into_iter().collect::<Vec<_>>();
        if keys.len() <= 1 {
            return keys
                .into_iter()
                .map(|key| self.query_registered(family, key))
                .collect();
        }

        // Reserve the nested batch worker capacity before constructing any
        // structured-batch state. An enclosing batch owns all of the
        // `maximum - 1` worker slots while its children run, so a nested
        // adaptive request would otherwise allocate wait-graph edges, an
        // authority, and one child task per item despite having no worker to
        // execute in parallel. A zero-count claim is a race-safe signal to
        // preserve the same ordered dependency observations in this task.
        let worker_claim =
            BatchWorkerClaim::new(self.task.core.clone(), keys.len().saturating_sub(1));
        if worker_claim.count == 0 {
            return keys
                .into_iter()
                .map(|key| self.query_registered(family, key))
                .collect();
        }
        let items = Arc::new(RegisteredBatchItems {
            family: family.inner.name.clone(),
            items: keys
                .into_iter()
                .map(|key| RegisteredBatchItem {
                    request_id: self.task.next_nested_request(),
                    key,
                    ready_at: Instant::now(),
                })
                .collect(),
        });
        self.query_registered_batch_with_claim(family, items, worker_claim)
    }

    /// Requests stable-ordered registered dependencies from borrowed keys.
    ///
    /// This is the one-worker counterpart to [`Self::query_registered_adaptive_batch`]
    /// for callers whose keys already live in an immutable batch key. It keeps
    /// the same request accounting and ordered results, but avoids cloning each
    /// key merely to hand ownership to the adaptive scheduler before the
    /// scheduler clones it for the actual query.
    pub fn query_registered_adaptive_batch_refs<'a, K, V, I>(
        &self,
        family: &QueryFamily<K, V>,
        keys: I,
    ) -> Result<Vec<Arc<QueryTerminal<V>>>, QueryAbort>
    where
        K: QueryKey + 'a,
        V: Clone + Send + Sync + 'static,
        I: IntoIterator<Item = &'a K>,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free dependency requests require a registered evaluator"
        );
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        if self.max_concurrency() == 1 {
            return keys
                .into_iter()
                .map(|key| self.query_registered_ref(family, key))
                .collect();
        }
        self.query_registered_adaptive_batch(family, keys.into_iter().cloned())
    }

    /// Requests a stable-ordered batch of dependencies through one registered
    /// family.
    ///
    /// Every item executes in a distinct child task, inheriting this task's
    /// immutable revision and cancellation token while owning its own
    /// wait-graph identity and execution permit. The parent donates its permit
    /// for the complete join interval, which makes the same interface
    /// progress-guaranteed when the runtime has one permit.
    ///
    /// Results are reduced in input order, independent of worker completion
    /// order. Dependency observations, request leases, handoffs, work, and the
    /// operational nested-attempt ledger are transferred into the parent before
    /// a child task can release them. Callers must therefore supply keys in
    /// their semantic stable order.
    ///
    /// When the parent has an active registered-validation scope, a child which
    /// reaches a terminal atomically publishes its registered-only proof and
    /// backing leases to this batch. Later siblings may borrow that authority
    /// without repeating the same recursive validation. The authority is
    /// lexical and moves into the parent before this method returns; unrelated
    /// batches, requests, and revisions share no mutable proof state.
    ///
    /// The wait graph stores each child as a compact index into this batch's
    /// typed key table. The table outlives every registered edge, including
    /// queued and already-completed children, and produces a display identity
    /// only if a scheduling cycle needs a diagnostic path.
    pub fn query_registered_batch<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<Vec<Arc<QueryTerminal<V>>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        assert!(
            family.inner.evaluator.is_some(),
            "closure-free dependency requests require a registered evaluator"
        );
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(QueryAbort::ForeignRuntime);
        }
        let items = Arc::new(RegisteredBatchItems {
            family: family.inner.name.clone(),
            items: keys
                .into_iter()
                .map(|key| RegisteredBatchItem {
                    request_id: self.task.next_nested_request(),
                    key,
                    ready_at: Instant::now(),
                })
                .collect(),
        });
        if items.items.is_empty() {
            return Ok(Vec::new());
        }
        let worker_claim =
            BatchWorkerClaim::new(self.task.core.clone(), items.items.len().saturating_sub(1));
        self.query_registered_batch_with_claim(family, items, worker_claim)
    }

    fn query_registered_batch_with_claim<K, V>(
        &self,
        family: &QueryFamily<K, V>,
        items: Arc<RegisteredBatchItems<K>>,
        worker_claim: BatchWorkerClaim,
    ) -> Result<Vec<Arc<QueryTerminal<V>>>, QueryAbort>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        if items.items.is_empty() {
            return Ok(Vec::new());
        }

        let wait_labels: Arc<dyn StructuredWaitLabels> = items.clone();
        let structured_waits = StructuredWaitGuard::new(
            self.task.core.clone(),
            self.task.id,
            wait_labels,
            items
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| (TaskId(item.request_id), index)),
        )
        .map_err(QueryAbort::Cycle)?;
        let queue = Arc::new(Mutex::new(VecDeque::from_iter(0..items.items.len())));
        let batch_authority = Arc::new(BatchValidationAuthority::new(
            self.task.core.clone(),
            self.task.batch_validation_authority.clone(),
            worker_claim.count > 0,
        ));
        batch_authority.seed_from_task(&self.task);
        // `tracing` dispatch is thread-local. Carry the caller's subscriber into
        // each child so scheduled evaluation keeps compiler timing/log events.
        let tracing_dispatch = tracing::dispatcher::get_default(Clone::clone);
        let tracing_parent = tracing::Span::current();
        let donation = ParentPermitDonation::new(self.task.clone());
        if donation.donated {
            self.task
                .core
                .metrics
                .donated_permits
                .fetch_add(1, Ordering::Relaxed);
        }
        let mut workers = Vec::with_capacity(worker_claim.count);
        let mut coordinator_measurement = BatchCoordinatorMeasurement::new(&self.task.core.metrics);
        for _ in 0..worker_claim.count {
            let queue = queue.clone();
            let items = items.clone();
            let family = (*family).clone();
            let parent = self.task.clone();
            let authority = batch_authority.clone();
            let tracing_dispatch = tracing_dispatch.clone();
            let tracing_parent = tracing_parent.clone();
            let dispatch_started = Instant::now();
            let (worker, born) = self.task.core.batch_executor.submit(move || {
                run_registered_batch_worker(
                    queue,
                    items,
                    family,
                    parent,
                    authority,
                    tracing_dispatch,
                    tracing_parent,
                )
            });
            coordinator_measurement
                .record_submission(born, duration_ns(dispatch_started.elapsed()));
            workers.push(worker);
        }
        let inline = run_registered_batch_worker(
            queue.clone(),
            items.clone(),
            (*family).clone(),
            self.task.clone(),
            batch_authority.clone(),
            tracing_dispatch.clone(),
            tracing_parent.clone(),
        );
        let mut completed = Vec::new();
        let mut panic = None;
        match inline {
            Ok(inline_completed) => completed.extend(inline_completed),
            Err(payload) => panic = Some(payload),
        }
        for worker in workers {
            let wait_started = Instant::now();
            let (submitted_result, worker_finished_at) = worker.join();
            // The worker timestamp separates useful-execution wait from
            // completion delivery without changing the structured wait. If
            // the worker was already done, the whole receive is coordinator
            // residual; otherwise only the completion-to-return tail is.
            coordinator_measurement.record_completion_residual(
                batch_completion_coordinator_residual_ns(
                    wait_started,
                    worker_finished_at,
                    Instant::now(),
                ),
            );
            match submitted_result {
                Ok(Ok(worker_completed)) => completed.extend(worker_completed),
                Ok(Err(payload)) | Err(payload) if panic.is_none() => panic = Some(payload),
                Ok(Err(_)) | Err(_) => {}
            }
        }
        drop(coordinator_measurement);
        drop(structured_waits);
        drop(donation);
        if let Some(payload) = panic {
            resume_unwind(payload);
        }
        batch_authority.absorb_into_task(&self.task);

        completed.sort_unstable_by_key(|(index, ..)| *index);
        let mut terminals = Vec::with_capacity(completed.len());
        for (index, child, result) in completed {
            let item = &items.items[index];
            self.task
                .absorb_batch_child(&child, matches!(&result, TaskQueryResult::Terminal { .. }));
            match &result {
                TaskQueryResult::Terminal { terminal, work, .. } => {
                    self.task.observe(terminal);
                    self.task.observe_work(work);
                    self.task
                        .cache_query(family.inner.token, &item.key, terminal);
                }
                TaskQueryResult::Aborted {
                    dependencies,
                    inputs,
                    work,
                    ..
                } => self.task.observe_abort_prefix(dependencies, inputs, work),
            }
            self.task.record_nested(
                item.request_id,
                || NodeIdentity::from_key(items.family.clone(), &item.key),
                &result,
            );
            terminals.push(result.into_result()?);
        }
        Ok(terminals)
    }

    /// Duplicates every terminal lease currently observed by this rooted task.
    ///
    /// The returned set is acquired while the task's original leases remain
    /// live, so a publication handoff can promote the exact dependency cone
    /// past request completion without an eviction window.
    pub fn retain_observed_terminals(&self) -> RetainedPinSet {
        retain_task_observations(&self.task)
    }

    /// RUE-1584: shares one completed retained cone with concurrently running
    /// batch siblings, ahead of the publication root it is also headed for.
    ///
    /// A collection window captures its predecessor roots' leases when it
    /// opens; siblings racing in one concurrent batch capture them before the
    /// first finisher installs its cone, so every one of them re-leases the
    /// shared leaves through demand cascades. Publishing the finished cone
    /// into the batch's shared authority closes that window: sibling probes
    /// and sibling promotion walks both consult authority fallbacks. The
    /// authority retains the Arc until the batch joins, and the join absorbs
    /// it into the parent scope, so the backing outlives every borrower. On a
    /// sequential batch (or outside any batch) this is a no-op: completion
    /// publication already precedes the next child's first probe.
    pub fn publish_batch_retention_fallback(&self, fallback: &Arc<RetainedPinSet>) {
        let Some(authority) = self.task.batch_validation_authority.as_deref() else {
            return;
        };
        let Some(target) = authority.nearest_concurrent() else {
            return;
        };
        target.publish_fallback(fallback);
    }

    /// Duplicates only the live terminals observed from one exact registered
    /// family in this rooted task.
    ///
    /// Staged compiler publications use this narrower bridge when a small
    /// family history would otherwise evict producer payloads between two
    /// top-level requests. Unlike retaining a terminal cone, this does not walk
    /// or pin unrelated ancestors and siblings; a later registered validation
    /// may borrow the selected terminals as ordinary fallback authority and
    /// must still prove every other edge itself.
    pub fn retain_observed_family<K, V>(
        &self,
        family: &QueryFamily<K, V>,
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        if !Arc::ptr_eq(&self.task_runtime(), &family.core) {
            return Err(RetainTerminalConeError::ForeignRuntime);
        }
        Ok(retain_task_family_observations(
            &self.task,
            family.inner.token,
        ))
    }

    /// Duplicates only the observed terminals reachable from one exact root.
    ///
    /// Validation can temporarily observe terminals from a superseded
    /// predecessor before recomputing a successor. This graph walk follows the
    /// immutable dependency observations of `root` through the task's live
    /// request leases, excluding unrelated validation pins while preserving
    /// continuous protection for the complete current cone.
    pub fn retain_observed_terminal_cone<V>(
        &self,
        root: &Arc<QueryTerminal<V>>,
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        V: Clone + Send + Sync + 'static,
    {
        self.retain_observed_terminal_cones_from(std::slice::from_ref(root), &[])
    }

    /// Retain one exact current-task terminal cone, filling validation leaves
    /// omitted by green reuse from prioritized published snapshots.
    pub fn retain_observed_terminal_cone_from<V>(
        &self,
        root: &Arc<QueryTerminal<V>>,
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        V: Clone + Send + Sync + 'static,
    {
        self.retain_observed_terminal_cones_from(std::slice::from_ref(root), fallbacks)
    }

    /// Retain the union of exact current-task terminal cones, filling
    /// validation leaves omitted by green reuse from prioritized published
    /// snapshots. Every root must be observed by this task; fallback snapshots
    /// may satisfy only dependency edges. Promotion uses both the explicitly
    /// supplied snapshots and every fallback promoted into the active lexical
    /// validation scope, so a descendant skipped under borrowed authority keeps
    /// the same backing pins available here. Current observations always
    /// override fallbacks, and within one source an edge selects the greatest
    /// terminal revision with the requested incarnation and stamp.
    ///
    /// The hash-indexed lease universe is built once and exact identities are
    /// hash-deduplicated while the union is walked once. Promotion is therefore
    /// expected O(N + E) in available leases and selected dependency edges,
    /// rather than rescanning or tree-searching leases per edge and per root.
    pub fn retain_observed_terminal_cones_from<V>(
        &self,
        roots: &[Arc<QueryTerminal<V>>],
        fallbacks: &[Arc<RetainedPinSet>],
    ) -> Result<RetainedPinSet, RetainTerminalConeError>
    where
        V: Clone + Send + Sync + 'static,
    {
        let mut batch_authorities = Vec::new();
        let mut next_authority = self.task.batch_validation_authority.clone();
        while let Some(authority) = next_authority {
            next_authority = authority.parent.clone();
            batch_authorities.push(authority);
        }
        let batch_authority_states = batch_authorities
            .iter()
            .map(|authority| read(&authority.state))
            .collect::<Vec<_>>();
        let mut promotion_fallbacks = {
            let scopes = lock(&self.task.validation_endorsements);
            let Some(scope) = scopes.first() else {
                return Err(RetainTerminalConeError::NoRegisteredValidationScope);
            };
            scope.fallbacks.clone()
        };
        for authority in &batch_authority_states {
            for fallback in &authority.fallbacks {
                if !promotion_fallbacks
                    .iter()
                    .any(|retained| Arc::ptr_eq(retained, fallback))
                {
                    promotion_fallbacks.push(fallback.clone());
                }
            }
        }
        for fallback in fallbacks {
            if !promotion_fallbacks
                .iter()
                .any(|retained| Arc::ptr_eq(retained, fallback))
            {
                promotion_fallbacks.push(fallback.clone());
            }
        }
        if promotion_fallbacks
            .iter()
            .any(|fallback| !fallback.belongs_to_runtime(self.task.core.identity))
        {
            return Err(RetainTerminalConeError::ForeignRuntime);
        }
        let leases = lock(&self.task.leases);
        let batch_lease_count = batch_authority_states
            .iter()
            .map(|authority| authority.leases.held.len())
            .sum::<usize>();
        let mut current_roots: RetainedIdentityMap<_, Option<&dyn ObservedLease>> =
            RetainedIdentityMap::with_capacity_and_hasher(
                roots.len(),
                BuildHasherDefault::default(),
            );
        for root in roots {
            current_roots
                .entry((root.node_incarnation, root.stamp, root.revision))
                .or_insert(None);
        }
        let mut selected = RetainedIdentityMap::with_capacity_and_hasher(
            leases.held.len() + batch_lease_count,
            BuildHasherDefault::default(),
        );
        for lease in &leases.held {
            let identity = lease.identity();
            if let Some(root) = current_roots.get_mut(&identity) {
                *root = Some(lease.as_ref());
            }
            let selected_for_stamp = selected
                .entry((identity.0, identity.1))
                .or_insert(lease.as_ref());
            if selected_for_stamp.identity().2 < identity.2 {
                *selected_for_stamp = lease.as_ref();
            }
        }
        for authority in &batch_authority_states {
            for lease in &authority.leases.held {
                let identity = lease.identity();
                let selected_for_stamp = selected
                    .entry((identity.0, identity.1))
                    .or_insert(lease.as_ref());
                if selected_for_stamp.identity().2 < identity.2 {
                    *selected_for_stamp = lease.as_ref();
                }
            }
        }
        for fallback in &promotion_fallbacks {
            let mut fallback_selected = RetainedIdentityMap::with_capacity_and_hasher(
                fallback.held.len(),
                BuildHasherDefault::default(),
            );
            for lease in &fallback.held {
                let identity = lease.identity();
                let selected_for_stamp = fallback_selected
                    .entry((identity.0, identity.1))
                    .or_insert(lease.as_ref());
                if selected_for_stamp.identity().2 < identity.2 {
                    *selected_for_stamp = lease.as_ref();
                }
            }
            for (identity, lease) in fallback_selected {
                selected.entry(identity).or_insert(lease);
            }
        }

        let mut pending = Vec::with_capacity(roots.len());
        for root in roots {
            pending.push(
                current_roots
                    .get(&(root.node_incarnation, root.stamp, root.revision))
                    .copied()
                    .flatten()
                    .ok_or(RetainTerminalConeError::RootNotObserved)?,
            );
        }
        let mut retained = RetainedPinSet::new();
        let mut visited = RetainedIdentitySet::with_capacity_and_hasher(
            selected.len(),
            BuildHasherDefault::default(),
        );
        while let Some(lease) = pending.pop() {
            if !visited.insert(lease.identity()) {
                continue;
            }
            for dependency in lease.dependencies() {
                pending.push(
                    *selected
                        .get(&(dependency.incarnation, dependency.stamp))
                        .ok_or_else(|| {
                            RetainTerminalConeError::DependencyNotObserved(dependency.clone())
                        })?,
                );
            }
            retained.lease_erased(lease.duplicate());
        }
        Ok(retained)
    }

    fn task_runtime(&self) -> Arc<RuntimeCore> {
        self.task.core.clone()
    }
}

#[cfg(test)]
mod coordinator_residual_tests {
    use super::*;
    #[cfg(panic = "unwind")]
    use std::panic::AssertUnwindSafe;
    use std::time::Duration;

    #[test]
    fn completion_residual_excludes_wait_for_useful_worker_execution() {
        let origin = Instant::now();
        assert_eq!(
            batch_completion_coordinator_residual_ns(
                origin,
                origin + Duration::from_nanos(10),
                origin + Duration::from_nanos(13),
            ),
            3
        );
        assert_eq!(
            batch_completion_coordinator_residual_ns(
                origin + Duration::from_nanos(10),
                origin,
                origin + Duration::from_nanos(13),
            ),
            3,
            "an already-finished worker attributes the whole receive to coordinator residual"
        );
    }

    #[test]
    fn batch_measurement_publishes_accumulated_totals_on_drop() {
        let metrics = Metrics::default();
        {
            let mut measurement = BatchCoordinatorMeasurement::new(&metrics);
            measurement.record_submission(1, 7);
            measurement.record_completion_residual(5);
        }
        assert_eq!(
            metrics.batch_worker_thread_births.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .batch_worker_coordinator_residual_ns
                .load(Ordering::Relaxed),
            12
        );
    }

    #[test]
    #[cfg(panic = "unwind")]
    fn batch_measurement_publishes_accumulated_totals_on_unwind() {
        let metrics = Metrics::default();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let mut measurement = BatchCoordinatorMeasurement::new(&metrics);
            measurement.record_submission(1, 7);
            measurement.record_completion_residual(5);
            panic!("later worker submission failed");
        }));
        assert!(panic.is_err());
        assert_eq!(
            metrics.batch_worker_thread_births.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .batch_worker_coordinator_residual_ns
                .load(Ordering::Relaxed),
            12
        );
    }
}
