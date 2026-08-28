//! Deterministic validation-work accounting and proof scopes.

use std::sync::atomic::{AtomicU64, Ordering};

use smallvec::SmallVec;

pub(crate) const VALIDATION_PROOF_REGISTERED: u8 = 0;
pub(crate) const VALIDATION_PROOF_RETRYABLE: u8 = 1;
pub(crate) const VALIDATION_PROOF_UNREGISTERED: u8 = 2;
pub(crate) const VALIDATION_PUBLISH_SWEEP_QUANTUM: usize = 64;

/// Structural retained charge of one node identity (ADR-0074).
///
/// A retained terminal names its own node and every node and input it
/// observed. Since ADR-0074 an identity is a shared family handle plus a
/// 128-bit digest, so each of those names costs the same fixed 16 bytes
/// instead of the byte length of a formatted family/key pair. Charging a
/// constant is what lets the ordinary path stop formatting keys at all.
pub(crate) const IDENTITY_CHARGE_BYTES: u64 = 16;

pub(crate) type ValidationProofStack = SmallVec<[u8; 8]>;

/// Deterministic work performed while validating retained query terminals.
///
/// These counters describe semantic operations rather than elapsed time. The
/// hot path accumulates them on the rooted request's [`Task`]; the runtime
/// merges one aggregate when that task completes. This keeps the counters
/// continuously available without a shared atomic update per validation edge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidationWork {
    /// Retained-terminal validation traversals started.
    pub traversals: u64,
    /// Traversals which proved their retained terminal current.
    pub successful_traversals: u64,
    /// Traversals which proved their retained terminal dirty.
    pub dirty_traversals: u64,
    /// Traversals aborted by cancellation or an engine error.
    pub aborted_traversals: u64,
    /// Direct source/input observations inspected.
    pub input_observations: u64,
    /// Retained dependency observations inspected.
    pub dependency_observations: u64,
    /// Exact node-incarnation resolutions requested by validation.
    pub registry_probes: u64,
    /// Resolutions which had to consult the shared incarnation registry.
    pub registry_index_lookups: u64,
    /// Registry probes which found no live exact incarnation.
    pub registry_misses: u64,
    /// Erased node validation entry-point visits.
    pub node_visits: u64,
    /// Recursive visits pruned because the incarnation was already active.
    pub active_cycle_prunes: u64,
    /// Node visits satisfied by a revision-scoped validation certificate.
    pub memo_hits: u64,
    /// Node visits which had to inspect or re-demand the node.
    pub memo_misses: u64,
    /// Memo misses with no usable exact certificate and live matching terminal.
    pub certificate_misses: u64,
    /// Memo misses caused only by a registered proof scope lacking the exact lease.
    pub proof_reacquisition_misses: u64,
    /// Exact registered-cone endorsement lookups.
    pub endorsement_probes: u64,
    /// Endorsement lookups which actually bypassed recursive or root validation.
    pub endorsement_hits: u64,
    /// Exact query-result terminal lease observations attempted by requests.
    pub terminal_lease_observations: u64,
    /// Query-result terminal observations already leased by the same task.
    pub duplicate_terminal_lease_observations: u64,
    /// Family-owned validation demands issued after a memo miss.
    pub demands: u64,
    /// Validation demands answered by a retained terminal.
    pub demand_reuses: u64,
    /// Validation demands which computed a terminal.
    pub demand_computes: u64,
    /// Validation demands which joined an in-flight terminal.
    pub demand_joins: u64,
    /// Validation demands which aborted without a terminal.
    pub demand_aborts: u64,
    /// Demands whose recorded incarnation had been retired or replaced.
    pub superseded: u64,
    /// New exact node/revision/stamp validation certificates published.
    pub certificates_published: u64,
}

impl ValidationWork {
    /// Returns the non-negative counter delta from an earlier snapshot.
    #[must_use]
    pub fn saturating_sub(self, earlier: Self) -> Self {
        macro_rules! subtract_fields {
            ($($field:ident),+ $(,)?) => {
                Self {
                    $($field: self.$field.saturating_sub(earlier.$field)),+
                }
            };
        }
        subtract_fields!(
            traversals,
            successful_traversals,
            dirty_traversals,
            aborted_traversals,
            input_observations,
            dependency_observations,
            registry_probes,
            registry_index_lookups,
            registry_misses,
            node_visits,
            active_cycle_prunes,
            memo_hits,
            memo_misses,
            certificate_misses,
            proof_reacquisition_misses,
            endorsement_probes,
            endorsement_hits,
            terminal_lease_observations,
            duplicate_terminal_lease_observations,
            demands,
            demand_reuses,
            demand_computes,
            demand_joins,
            demand_aborts,
            superseded,
            certificates_published,
        )
    }

    /// Adds another request's counters into this aggregate.
    pub fn saturating_add_assign(&mut self, other: Self) {
        macro_rules! add_fields {
            ($($field:ident),+ $(,)?) => {
                $(self.$field = self.$field.saturating_add(other.$field);)+
            };
        }
        add_fields!(
            traversals,
            successful_traversals,
            dirty_traversals,
            aborted_traversals,
            input_observations,
            dependency_observations,
            registry_probes,
            registry_index_lookups,
            registry_misses,
            node_visits,
            active_cycle_prunes,
            memo_hits,
            memo_misses,
            certificate_misses,
            proof_reacquisition_misses,
            endorsement_probes,
            endorsement_hits,
            terminal_lease_observations,
            duplicate_terminal_lease_observations,
            demands,
            demand_reuses,
            demand_computes,
            demand_joins,
            demand_aborts,
            superseded,
            certificates_published,
        );
    }
}

/// Task-local storage for the independent validation outcomes.
///
/// `traversals`, `registry_probes`, `node_visits`, `memo_misses`,
/// `endorsement_probes`, `terminal_lease_observations`, and `demands` are exact
/// sums of fields below, so snapshots derive them instead of paying a second
/// atomic update on the validation hot path.
#[derive(Debug, Default)]
pub(crate) struct AtomicValidationWork {
    pub(crate) successful_traversals: AtomicU64,
    pub(crate) dirty_traversals: AtomicU64,
    pub(crate) aborted_traversals: AtomicU64,
    pub(crate) input_observations: AtomicU64,
    pub(crate) dependency_observations: AtomicU64,
    pub(crate) registry_index_lookups: AtomicU64,
    pub(crate) registry_misses: AtomicU64,
    pub(crate) active_cycle_prunes: AtomicU64,
    pub(crate) memo_hits: AtomicU64,
    pub(crate) certificate_misses: AtomicU64,
    pub(crate) proof_reacquisition_misses: AtomicU64,
    pub(crate) endorsement_misses: AtomicU64,
    pub(crate) endorsement_hits: AtomicU64,
    pub(crate) unique_terminal_lease_observations: AtomicU64,
    pub(crate) duplicate_terminal_lease_observations: AtomicU64,
    demand_without_results: AtomicU64,
    pub(crate) demand_reuses: AtomicU64,
    pub(crate) demand_computes: AtomicU64,
    pub(crate) demand_joins: AtomicU64,
    pub(crate) demand_aborts: AtomicU64,
    pub(crate) superseded: AtomicU64,
    pub(crate) certificates_published: AtomicU64,
}

impl AtomicValidationWork {
    fn derive_totals(mut work: ValidationWork) -> ValidationWork {
        work.traversals = work
            .successful_traversals
            .saturating_add(work.dirty_traversals)
            .saturating_add(work.aborted_traversals);
        work.registry_probes = work
            .dependency_observations
            .saturating_add(work.successful_traversals);
        work.memo_misses = work
            .certificate_misses
            .saturating_add(work.proof_reacquisition_misses);
        work.node_visits = work
            .active_cycle_prunes
            .saturating_add(work.memo_hits)
            .saturating_add(work.memo_misses);
        // `endorsement_probes` carries misses inside the task accumulator.
        // A lookup either bypasses validation or it does not, so the public
        // probe total is the exact sum of those independent outcomes.
        work.endorsement_probes = work
            .endorsement_probes
            .saturating_add(work.endorsement_hits);
        // `terminal_lease_observations` carries unique observations inside the
        // task accumulator. Every attempted lease is either unique or a
        // duplicate, so the public attempt total is their exact sum.
        work.terminal_lease_observations = work
            .terminal_lease_observations
            .saturating_add(work.duplicate_terminal_lease_observations);
        // `demands` carries demands which ended before producing a query
        // result inside the task accumulator. Every other demand has exactly
        // one published execution outcome.
        work.demands = work
            .demands
            .saturating_add(work.demand_reuses)
            .saturating_add(work.demand_computes)
            .saturating_add(work.demand_joins)
            .saturating_add(work.demand_aborts);
        work
    }

    pub(crate) fn snapshot(&self) -> ValidationWork {
        Self::derive_totals(ValidationWork {
            successful_traversals: self.successful_traversals.load(Ordering::Relaxed),
            dirty_traversals: self.dirty_traversals.load(Ordering::Relaxed),
            aborted_traversals: self.aborted_traversals.load(Ordering::Relaxed),
            input_observations: self.input_observations.load(Ordering::Relaxed),
            dependency_observations: self.dependency_observations.load(Ordering::Relaxed),
            registry_index_lookups: self.registry_index_lookups.load(Ordering::Relaxed),
            registry_misses: self.registry_misses.load(Ordering::Relaxed),
            active_cycle_prunes: self.active_cycle_prunes.load(Ordering::Relaxed),
            memo_hits: self.memo_hits.load(Ordering::Relaxed),
            certificate_misses: self.certificate_misses.load(Ordering::Relaxed),
            proof_reacquisition_misses: self.proof_reacquisition_misses.load(Ordering::Relaxed),
            endorsement_probes: self.endorsement_misses.load(Ordering::Relaxed),
            endorsement_hits: self.endorsement_hits.load(Ordering::Relaxed),
            terminal_lease_observations: self
                .unique_terminal_lease_observations
                .load(Ordering::Relaxed),
            duplicate_terminal_lease_observations: self
                .duplicate_terminal_lease_observations
                .load(Ordering::Relaxed),
            demands: self.demand_without_results.load(Ordering::Relaxed),
            demand_reuses: self.demand_reuses.load(Ordering::Relaxed),
            demand_computes: self.demand_computes.load(Ordering::Relaxed),
            demand_joins: self.demand_joins.load(Ordering::Relaxed),
            demand_aborts: self.demand_aborts.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
            certificates_published: self.certificates_published.load(Ordering::Relaxed),
            ..ValidationWork::default()
        })
    }

    pub(crate) fn take(&self) -> ValidationWork {
        Self::derive_totals(ValidationWork {
            successful_traversals: self.successful_traversals.swap(0, Ordering::Relaxed),
            dirty_traversals: self.dirty_traversals.swap(0, Ordering::Relaxed),
            aborted_traversals: self.aborted_traversals.swap(0, Ordering::Relaxed),
            input_observations: self.input_observations.swap(0, Ordering::Relaxed),
            dependency_observations: self.dependency_observations.swap(0, Ordering::Relaxed),
            registry_index_lookups: self.registry_index_lookups.swap(0, Ordering::Relaxed),
            registry_misses: self.registry_misses.swap(0, Ordering::Relaxed),
            active_cycle_prunes: self.active_cycle_prunes.swap(0, Ordering::Relaxed),
            memo_hits: self.memo_hits.swap(0, Ordering::Relaxed),
            certificate_misses: self.certificate_misses.swap(0, Ordering::Relaxed),
            proof_reacquisition_misses: self.proof_reacquisition_misses.swap(0, Ordering::Relaxed),
            endorsement_probes: self.endorsement_misses.swap(0, Ordering::Relaxed),
            endorsement_hits: self.endorsement_hits.swap(0, Ordering::Relaxed),
            terminal_lease_observations: self
                .unique_terminal_lease_observations
                .swap(0, Ordering::Relaxed),
            duplicate_terminal_lease_observations: self
                .duplicate_terminal_lease_observations
                .swap(0, Ordering::Relaxed),
            demands: self.demand_without_results.swap(0, Ordering::Relaxed),
            demand_reuses: self.demand_reuses.swap(0, Ordering::Relaxed),
            demand_computes: self.demand_computes.swap(0, Ordering::Relaxed),
            demand_joins: self.demand_joins.swap(0, Ordering::Relaxed),
            demand_aborts: self.demand_aborts.swap(0, Ordering::Relaxed),
            superseded: self.superseded.swap(0, Ordering::Relaxed),
            certificates_published: self.certificates_published.swap(0, Ordering::Relaxed),
            ..ValidationWork::default()
        })
    }

    pub(crate) fn add(&self, work: ValidationWork) {
        macro_rules! add_fields {
            ($($field:ident),+ $(,)?) => {
                $(if work.$field != 0 {
                    self.$field.fetch_add(work.$field, Ordering::Relaxed);
                })+
            };
        }
        add_fields!(
            successful_traversals,
            dirty_traversals,
            aborted_traversals,
            input_observations,
            dependency_observations,
            registry_index_lookups,
            registry_misses,
            active_cycle_prunes,
            memo_hits,
            certificate_misses,
            proof_reacquisition_misses,
            endorsement_hits,
            duplicate_terminal_lease_observations,
            demand_reuses,
            demand_computes,
            demand_joins,
            demand_aborts,
            superseded,
            certificates_published,
        );
        let endorsement_misses = work
            .endorsement_probes
            .saturating_sub(work.endorsement_hits);
        if endorsement_misses != 0 {
            self.endorsement_misses
                .fetch_add(endorsement_misses, Ordering::Relaxed);
        }
        let unique_terminal_lease_observations = work
            .terminal_lease_observations
            .saturating_sub(work.duplicate_terminal_lease_observations);
        if unique_terminal_lease_observations != 0 {
            self.unique_terminal_lease_observations
                .fetch_add(unique_terminal_lease_observations, Ordering::Relaxed);
        }
        let demand_results = work
            .demand_reuses
            .saturating_add(work.demand_computes)
            .saturating_add(work.demand_joins)
            .saturating_add(work.demand_aborts);
        let demand_without_results = work.demands.saturating_sub(demand_results);
        if demand_without_results != 0 {
            self.demand_without_results
                .fetch_add(demand_without_results, Ordering::Relaxed);
        }
    }
}

/// Publishes exactly one completed validation-traversal outcome. Keeping the
/// outcome in the guard makes an evaluator unwind count as an aborted
/// traversal, so the aggregate total can be derived without a separate atomic.
pub(crate) struct ValidationTraversalWork<'a> {
    pub(crate) work: &'a AtomicValidationWork,
    pub(crate) outcome: Option<bool>,
}

impl ValidationTraversalWork<'_> {
    pub(crate) fn finish(mut self, valid: bool) {
        self.outcome = Some(valid);
    }
}

impl Drop for ValidationTraversalWork<'_> {
    fn drop(&mut self) {
        let counter = match self.outcome {
            Some(true) => &self.work.successful_traversals,
            Some(false) => &self.work.dirty_traversals,
            None => &self.work.aborted_traversals,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Records a demand which unwinds or retires before yielding a query result.
/// Completed demands publish their execution outcome directly, allowing the
/// public total to be derived without a separate atomic update.
pub(crate) struct ValidationDemandWork<'a> {
    work: &'a AtomicValidationWork,
    produced_result: bool,
}

impl<'a> ValidationDemandWork<'a> {
    pub(crate) fn new(work: &'a AtomicValidationWork) -> Self {
        Self {
            work,
            produced_result: false,
        }
    }

    pub(crate) fn finish(mut self) {
        self.produced_result = true;
    }
}

impl Drop for ValidationDemandWork<'_> {
    fn drop(&mut self) {
        if !self.produced_result {
            self.work
                .demand_without_results
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Publishes one validation traversal's dependency-edge work in a bounded
/// batch, including when a registered evaluator unwinds through validation.
pub(crate) struct DependencyValidationWork<'a> {
    pub(crate) work: &'a AtomicValidationWork,
    pub(crate) observations: u64,
}

impl DependencyValidationWork<'_> {
    pub(crate) fn observe(&mut self) {
        self.observations += 1;
    }
}

impl Drop for DependencyValidationWork<'_> {
    fn drop(&mut self) {
        if self.observations == 0 {
            return;
        }
        self.work
            .dependency_observations
            .fetch_add(self.observations, Ordering::Relaxed);
    }
}
