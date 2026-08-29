//! Runtime and per-family metrics surfaces.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::*;

/// Display-only query identities materialized by the runtime.
///
/// The byte counters record the UTF-8 length returned by
/// [`QueryKey::stable_identity`]. Family names are shared separately and are
/// not included. Typed keys remain authoritative for memo lookup.
///
/// Since ADR-0074 every counter here reports an actual formatting event.
/// Nothing on the ordinary path needs a node's name: ordering, equality,
/// hashing, the published dependency order, and the retained charge are all
/// defined on `(family, stable_hash)`. A key is formatted when something asks
/// what a node is *called* — a diagnostic, a rendered cycle, an aborted nested
/// request, or a `Debug` dump.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisplayIdentityMetrics {
    /// Memo-node identities formatted on first demand for a name.
    pub memo_node_materializations: u64,
    /// Formatted key bytes retained by materialized memo-node identities.
    pub memo_node_bytes: u64,
    /// Structured batch identities materialized only to render wait cycles.
    pub structured_wait_materializations: u64,
    /// Formatted structured-batch key bytes used to render wait cycles.
    pub structured_wait_bytes: u64,
    /// Identities created lazily when nested requests abort.
    pub abort_fallback_materializations: u64,
    /// Formatted key bytes created lazily when nested requests abort.
    pub abort_fallback_bytes: u64,
}

/// Deterministic structural execution counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    /// New computations claimed.
    pub claims: u64,
    /// Compatible in-flight requests a task elected to share. A join declined
    /// as unwaitable is counted here too and again under `declined_joins`.
    pub joins: u64,
    /// Compatible retained terminals reused.
    pub reuses: u64,
    /// Retained-terminal validation work.
    pub validation: ValidationWork,
    /// Presentation-only query identity materialization.
    pub display_identities: DisplayIdentityMetrics,
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
    /// Joins declined because waiting would have closed a wait-graph loop, each
    /// resolved by computing a private attempt.
    pub declined_joins: u64,
    /// Retained terminal attempts evicted.
    pub evictions: u64,
    /// Current retained terminal attempts.
    pub retained_terminals: u64,
    /// Current deterministic charge for retained terminal artifacts.
    pub retained_bytes: u64,
    /// Peak deterministic charge for retained terminal artifacts.
    pub peak_retained_bytes: u64,
    /// Current retained dependency and input observation edges.
    pub retained_dependency_pins: u64,
    /// Peak retained dependency and input observation edges.
    pub peak_retained_dependency_pins: u64,
    /// Configured runtime-wide soft artifact-charge budget.
    pub retained_byte_budget: u64,
    /// Configured runtime-wide soft dependency-observation budget.
    pub dependency_pin_budget: u64,
    /// Cross-family charge aggregations triggered by family-local watermarks.
    pub aggregate_retention_probes: u64,
    /// Deterministic family-local byte/pin quanta between aggregate probes.
    pub retained_byte_probe_quantum: u64,
    pub dependency_pin_probe_quantum: u64,
    /// Worst-case soft-budget detection overshoot from all currently live
    /// families publishing just below their next probe watermark.
    pub retained_byte_probe_overshoot_bound: u64,
    pub dependency_pin_probe_overshoot_bound: u64,
    /// Enforcement passes which began above the byte budget.
    pub retained_byte_pressure_events: u64,
    /// Enforcement passes which began above the dependency-pin budget.
    pub dependency_pin_pressure_events: u64,
    /// Byte-pressure passes unable to reach budget because all candidates were
    /// protected.
    pub retained_byte_overflow_events: u64,
    /// Dependency-pressure passes unable to reach budget because all candidates
    /// were protected.
    pub dependency_pin_overflow_events: u64,
    /// Largest protected byte overage observed after an enforcement pass.
    pub peak_retained_byte_overage: u64,
    /// Largest protected dependency-pin overage observed after enforcement.
    pub peak_dependency_pin_overage: u64,
    /// Terminals evicted while byte pressure was active.
    pub retained_byte_evictions: u64,
    /// Terminals evicted while dependency-pin pressure was active.
    pub dependency_pin_evictions: u64,
    /// Retention passes forced to grow past the configured terminal bound
    /// because every eviction candidate was a protected root (waiter, pin,
    /// request-scoped observation lease, or retained revision). This is the
    /// bounded-retention pressure marker: under a live closure exceeding the
    /// configured budget the policy is to grow and record the event here, never
    /// to evict a terminal the current computation still needs.
    pub retention_growth: u64,
    /// Retention enforcement passes run. Each family pass which reaches the
    /// eviction loop increments this once; an already-converged family does not.
    /// Batched task-lease teardown deliberately requests one pass per distinct
    /// family involved rather than one per released pin, so releasing N pins in
    /// one family raises this by at most one, not by N. This counter makes the
    /// resulting work reduction observable to tests.
    pub retention_enforcements: u64,
    /// Retention-queue entries examined by enforcement passes.
    ///
    /// Unlike `retention_enforcements`, this measures the work inside a pass.
    /// Publish-side batching keeps this linear when a live protected closure
    /// grows past its configured soft floor.
    pub retention_scan_entries: u64,
    /// Attempt-handoff lifecycles offered to a task's observation scope, and
    /// the scope positions examined answering them.
    ///
    /// These are a pair. Recording a lifecycle deduplicates it by pointer
    /// identity against every lifecycle the same scope already observed, so
    /// the question count is linear in the work a program dispatches while
    /// the visit count is what distinguishes a bounded scope from one that
    /// accumulates. Only their ratio is a scaling property: a scope that
    /// starts holding live lifecycles turns each observation into a walk over
    /// the ones before it, and nothing else this runtime publishes moves.
    pub handoff_observations: u64,
    pub handoff_observation_visits: u64,
    /// Peak simultaneously executing query bodies.
    pub peak_active_bodies: u64,
    /// Peak tasks simultaneously owning execution permits. Nested query bodies
    /// on one task count once, so this is the worker-utilization concurrency.
    pub peak_query_workers: u64,
    /// Terminals currently protected by rooted-request observation leases.
    pub active_task_leases: u64,
    /// Peak terminals simultaneously protected by rooted-request observation
    /// leases. Unlike RSS, this is an allocator-independent ownership gauge.
    pub peak_task_leases: u64,
    /// Terminals currently protected by session-owned retained pin sets.
    pub active_retained_pins: u64,
    /// Peak terminals simultaneously protected by session-owned retained pin
    /// sets. This includes atomic predecessor/successor handoff overlap.
    pub peak_retained_pins: u64,
    /// Times a parked joiner released its permit.
    pub donated_permits: u64,
    /// Nanoseconds tasks held an execution permit, summed across tasks.
    pub query_worker_active_ns: u64,
    /// Registered batch items observed in the ready queue.
    pub ready_items: u64,
    /// Extra registered-batch worker slots requested from the shared scheduler.
    ///
    /// This is structural scheduling evidence: it counts desired logical slots,
    /// not operating-system thread creation. A batch with N items requests
    /// N - 1 extra slots because the donating parent supplies the remaining
    /// scheduler lane.
    pub batch_worker_slots_requested: u64,
    /// Extra registered-batch worker slots granted from the shared scheduler.
    pub batch_worker_slots_granted: u64,
    /// Registered-batch scheduler lanes which entered their worker loop.
    ///
    /// This includes the donating parent's inline lane. It deliberately says
    /// nothing about whether a lane maps to a newly created operating-system
    /// thread; it proves that every granted logical lane reached execution.
    pub batch_worker_lanes_entered: u64,
    /// Sum and maximum of ready-to-start delay for registered batch items.
    pub ready_wait_ns: u64,
    pub max_ready_wait_ns: u64,
    /// Longest nested query/batch ancestry observed by a task.
    pub longest_query_dependency_chain: u64,
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
    /// Configured terminal bound. Protected roots — waiters, explicit pins,
    /// request-scoped observation leases, and retained revisions — may exceed it
    /// temporarily; the excess is reclaimed as those roots release.
    pub terminal_limit: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Metrics {
    pub(crate) claims: AtomicU64,
    pub(crate) joins: AtomicU64,
    pub(crate) reuses: AtomicU64,
    pub(crate) validation: AtomicValidationWork,
    memo_node_identity_materializations: AtomicU64,
    memo_node_identity_bytes: AtomicU64,
    structured_wait_identity_materializations: AtomicU64,
    structured_wait_identity_bytes: AtomicU64,
    abort_fallback_identity_materializations: AtomicU64,
    abort_fallback_identity_bytes: AtomicU64,
    pub(crate) body_completions: AtomicU64,
    pub(crate) red_publications: AtomicU64,
    pub(crate) green_publications: AtomicU64,
    pub(crate) cancellations: AtomicU64,
    pub(crate) cycles: AtomicU64,
    pub(crate) declined_joins: AtomicU64,
    pub(crate) evictions: AtomicU64,
    pub(crate) retained_terminals: AtomicU64,
    pub(crate) peak_retained_bytes: AtomicU64,
    pub(crate) peak_retained_dependency_pins: AtomicU64,
    pub(crate) retained_byte_pressure_events: AtomicU64,
    pub(crate) dependency_pin_pressure_events: AtomicU64,
    pub(crate) aggregate_retention_probes: AtomicU64,
    pub(crate) retained_byte_overflow_events: AtomicU64,
    pub(crate) dependency_pin_overflow_events: AtomicU64,
    pub(crate) peak_retained_byte_overage: AtomicU64,
    pub(crate) peak_dependency_pin_overage: AtomicU64,
    pub(crate) retained_byte_evictions: AtomicU64,
    pub(crate) dependency_pin_evictions: AtomicU64,
    pub(crate) retention_growth: AtomicU64,
    pub(crate) retention_enforcements: AtomicU64,
    pub(crate) retention_scan_entries: AtomicU64,
    handoff_observations: AtomicU64,
    handoff_observation_visits: AtomicU64,
    active_bodies: AtomicU64,
    peak_active_bodies: AtomicU64,
    active_query_workers: AtomicU64,
    peak_query_workers: AtomicU64,
    active_task_leases: AtomicU64,
    peak_task_leases: AtomicU64,
    active_retained_pins: AtomicU64,
    peak_retained_pins: AtomicU64,
    pub(crate) donated_permits: AtomicU64,
    pub(crate) query_worker_active_ns: AtomicU64,
    pub(crate) ready_items: AtomicU64,
    pub(crate) batch_worker_slots_requested: AtomicU64,
    pub(crate) batch_worker_slots_granted: AtomicU64,
    pub(crate) batch_worker_lanes_entered: AtomicU64,
    pub(crate) ready_wait_ns: AtomicU64,
    pub(crate) max_ready_wait_ns: AtomicU64,
    pub(crate) longest_query_dependency_chain: AtomicU64,
}

impl Metrics {
    pub(crate) fn snapshot(
        &self,
        budgets: RetentionBudgets,
        retention: RuntimeRetentionSnapshot,
    ) -> RuntimeMetrics {
        RuntimeMetrics {
            claims: self.claims.load(Ordering::Relaxed),
            joins: self.joins.load(Ordering::Relaxed),
            reuses: self.reuses.load(Ordering::Relaxed),
            validation: self.validation.snapshot(),
            display_identities: DisplayIdentityMetrics {
                memo_node_materializations: self
                    .memo_node_identity_materializations
                    .load(Ordering::Relaxed),
                memo_node_bytes: self.memo_node_identity_bytes.load(Ordering::Relaxed),
                structured_wait_materializations: self
                    .structured_wait_identity_materializations
                    .load(Ordering::Relaxed),
                structured_wait_bytes: self.structured_wait_identity_bytes.load(Ordering::Relaxed),
                abort_fallback_materializations: self
                    .abort_fallback_identity_materializations
                    .load(Ordering::Relaxed),
                abort_fallback_bytes: self.abort_fallback_identity_bytes.load(Ordering::Relaxed),
            },
            body_completions: self.body_completions.load(Ordering::Relaxed),
            red_publications: self.red_publications.load(Ordering::Relaxed),
            green_publications: self.green_publications.load(Ordering::Relaxed),
            cancellations: self.cancellations.load(Ordering::Relaxed),
            cycles: self.cycles.load(Ordering::Relaxed),
            declined_joins: self.declined_joins.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            retained_terminals: self.retained_terminals.load(Ordering::Relaxed),
            retained_bytes: retention.retained_bytes,
            peak_retained_bytes: self.peak_retained_bytes.load(Ordering::Relaxed),
            retained_dependency_pins: retention.dependency_pins,
            peak_retained_dependency_pins: self
                .peak_retained_dependency_pins
                .load(Ordering::Relaxed),
            retained_byte_budget: budgets.retained_bytes,
            dependency_pin_budget: budgets.dependency_pins,
            aggregate_retention_probes: self.aggregate_retention_probes.load(Ordering::Relaxed),
            retained_byte_probe_quantum: retention_probe_quantum(
                budgets.retained_bytes,
                1024 * 1024,
                32 * 1024 * 1024,
            ),
            dependency_pin_probe_quantum: retention_probe_quantum(
                budgets.dependency_pins,
                4096,
                65_536,
            ),
            retained_byte_probe_overshoot_bound: retention.live_families.saturating_mul(
                retention_probe_quantum(budgets.retained_bytes, 1024 * 1024, 32 * 1024 * 1024),
            ),
            dependency_pin_probe_overshoot_bound: retention.live_families.saturating_mul(
                retention_probe_quantum(budgets.dependency_pins, 4096, 65_536),
            ),
            retained_byte_pressure_events: self
                .retained_byte_pressure_events
                .load(Ordering::Relaxed),
            dependency_pin_pressure_events: self
                .dependency_pin_pressure_events
                .load(Ordering::Relaxed),
            retained_byte_overflow_events: self
                .retained_byte_overflow_events
                .load(Ordering::Relaxed),
            dependency_pin_overflow_events: self
                .dependency_pin_overflow_events
                .load(Ordering::Relaxed),
            peak_retained_byte_overage: self.peak_retained_byte_overage.load(Ordering::Relaxed),
            peak_dependency_pin_overage: self.peak_dependency_pin_overage.load(Ordering::Relaxed),
            retained_byte_evictions: self.retained_byte_evictions.load(Ordering::Relaxed),
            dependency_pin_evictions: self.dependency_pin_evictions.load(Ordering::Relaxed),
            retention_growth: self.retention_growth.load(Ordering::Relaxed),
            retention_enforcements: self.retention_enforcements.load(Ordering::Relaxed),
            retention_scan_entries: self.retention_scan_entries.load(Ordering::Relaxed),
            handoff_observations: self.handoff_observations.load(Ordering::Relaxed),
            handoff_observation_visits: self.handoff_observation_visits.load(Ordering::Relaxed),
            peak_active_bodies: self.peak_active_bodies.load(Ordering::Relaxed),
            peak_query_workers: self.peak_query_workers.load(Ordering::Relaxed),
            active_task_leases: self.active_task_leases.load(Ordering::Relaxed),
            peak_task_leases: self.peak_task_leases.load(Ordering::Relaxed),
            active_retained_pins: self.active_retained_pins.load(Ordering::Relaxed),
            peak_retained_pins: self.peak_retained_pins.load(Ordering::Relaxed),
            donated_permits: self.donated_permits.load(Ordering::Relaxed),
            query_worker_active_ns: self.query_worker_active_ns.load(Ordering::Relaxed),
            ready_items: self.ready_items.load(Ordering::Relaxed),
            batch_worker_slots_requested: self.batch_worker_slots_requested.load(Ordering::Relaxed),
            batch_worker_slots_granted: self.batch_worker_slots_granted.load(Ordering::Relaxed),
            batch_worker_lanes_entered: self.batch_worker_lanes_entered.load(Ordering::Relaxed),
            ready_wait_ns: self.ready_wait_ns.load(Ordering::Relaxed),
            max_ready_wait_ns: self.max_ready_wait_ns.load(Ordering::Relaxed),
            longest_query_dependency_chain: self
                .longest_query_dependency_chain
                .load(Ordering::Relaxed),
            retained_revisions: 0,
            revision_limit: REVISION_RETENTION_LIMIT as u64,
        }
    }

    pub(crate) fn body_entered(&self) {
        let active = self.active_bodies.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_active_bodies.fetch_max(active, Ordering::AcqRel);
    }

    pub(crate) fn record_memo_node_identity(&self, bytes: usize) {
        self.memo_node_identity_materializations
            .fetch_add(1, Ordering::Relaxed);
        self.memo_node_identity_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_structured_wait_identity(&self, bytes: usize) {
        self.structured_wait_identity_materializations
            .fetch_add(1, Ordering::Relaxed);
        self.structured_wait_identity_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_abort_fallback_identity(&self, bytes: usize) {
        self.abort_fallback_identity_materializations
            .fetch_add(1, Ordering::Relaxed);
        self.abort_fallback_identity_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn body_left(&self) {
        self.active_bodies.fetch_sub(1, Ordering::AcqRel);
    }

    /// One handoff-observation question and the scope positions it examined.
    /// See the field docs on [`RuntimeMetrics::handoff_observations`]; `visits`
    /// is never zero, so the pair's ratio starts at one and can only be moved
    /// by a scope that accumulates.
    pub(crate) fn handoff_observed(&self, visits: u64) {
        self.handoff_observations.fetch_add(1, Ordering::Relaxed);
        self.handoff_observation_visits
            .fetch_add(visits, Ordering::Relaxed);
    }

    pub(crate) fn permit_acquired(&self) {
        let active = self.active_query_workers.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_query_workers.fetch_max(active, Ordering::AcqRel);
    }

    pub(crate) fn permit_released(&self) {
        self.active_query_workers.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn task_lease_acquired(&self) {
        let active = self.active_task_leases.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_task_leases.fetch_max(active, Ordering::Relaxed);
    }

    pub(crate) fn task_leases_released(&self, count: usize) {
        self.active_task_leases
            .fetch_sub(count as u64, Ordering::Relaxed);
    }

    pub(crate) fn retained_pin_acquired(&self) {
        let active = self.active_retained_pins.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_retained_pins.fetch_max(active, Ordering::Relaxed);
    }

    pub(crate) fn retained_pins_released(&self, count: usize) {
        self.active_retained_pins
            .fetch_sub(count as u64, Ordering::Relaxed);
    }
}
