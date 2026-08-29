use super::*;
/// Two-phase rendezvous for deterministic compiler schedule tests. The first
/// wait tells the test that the owner entered the canonical evaluator; the
/// second releases that owner after the runtime has observed the intended
/// join/cancellation lifecycle.
#[cfg(test)]
pub(crate) struct TestCodegenEvaluatorGate {
    rendezvous: std::sync::Barrier,
}

#[cfg(test)]
impl TestCodegenEvaluatorGate {
    fn new() -> Self {
        Self {
            rendezvous: std::sync::Barrier::new(2),
        }
    }

    pub(super) fn evaluator_wait(&self) {
        self.rendezvous.wait();
        self.rendezvous.wait();
    }

    pub(crate) fn wait_until_entered(&self) {
        self.rendezvous.wait();
    }

    pub(crate) fn release(&self) {
        self.rendezvous.wait();
    }
}

#[cfg(test)]
pub(super) fn new_test_codegen_evaluator_gate() -> TestCodegenEvaluatorGate {
    TestCodegenEvaluatorGate::new()
}

#[cfg(test)]
pub(crate) struct TestBackendBatchEvaluatorGate {
    remaining: std::sync::atomic::AtomicUsize,
    entered: std::sync::atomic::AtomicUsize,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    rendezvous: Option<TestBackendBatchRendezvous>,
}

#[cfg(test)]
struct TestBackendBatchRendezvous {
    expected: usize,
    released: Mutex<bool>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
impl TestBackendBatchEvaluatorGate {
    fn new(gated_children: usize, rendezvous: bool) -> Self {
        assert!(gated_children > 0);
        Self {
            remaining: std::sync::atomic::AtomicUsize::new(gated_children),
            entered: std::sync::atomic::AtomicUsize::new(0),
            active: std::sync::atomic::AtomicUsize::new(0),
            peak: std::sync::atomic::AtomicUsize::new(0),
            rendezvous: rendezvous.then(|| TestBackendBatchRendezvous {
                expected: gated_children,
                released: Mutex::new(false),
                changed: std::sync::Condvar::new(),
            }),
        }
    }

    pub(super) fn evaluator_wait(&self) {
        use std::sync::atomic::Ordering;
        if self
            .remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_err()
        {
            return;
        }
        self.entered.fetch_add(1, Ordering::AcqRel);
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::AcqRel);
        if let Some(rendezvous) = &self.rendezvous {
            rendezvous.changed.notify_all();
            let mut released = rendezvous
                .released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released {
                released = rendezvous
                    .changed
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn wait_until_all_entered_and_release(&self) -> bool {
        let rendezvous = self
            .rendezvous
            .as_ref()
            .expect("only a rendezvous gate can wait for concurrent entry");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut released = rendezvous
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while self.entered() < rendezvous.expected {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            let (next, timeout) = rendezvous
                .changed
                .wait_timeout(released, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            released = next;
            if timeout.timed_out() {
                break;
            }
        }
        let all_entered = self.entered() == rendezvous.expected;
        *released = true;
        rendezvous.changed.notify_all();
        all_entered
    }

    pub(crate) fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn entered(&self) -> usize {
        self.entered.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
pub(super) fn new_test_backend_batch_evaluator_gate(
    gated_children: usize,
    rendezvous: bool,
) -> TestBackendBatchEvaluatorGate {
    TestBackendBatchEvaluatorGate::new(gated_children, rendezvous)
}

impl std::fmt::Debug for RevisionedQueryDatabase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevisionedQueryDatabase")
            .field("next_revision", &self.next_revision)
            .field("next_source_stamp", &self.next_source_stamp)
            .field("parse_retention", &self.parse.retention())
            .finish_non_exhaustive()
    }
}

/// Cumulative provider-op observation counters, one per §4 fact family. The
/// exact provider increments these as it observes each backing terminal in the
/// registered production body evaluator. The declaration aggregate is derived
/// from its four disjoint fact-family outcomes. Atomic because body queries may
/// run in parallel.
#[derive(Debug, Default)]
pub(crate) struct ProviderObservationCounters {
    pub(super) name_lookups: std::sync::atomic::AtomicU64,
    pub(super) import_lookups: std::sync::atomic::AtomicU64,
    pub(super) method_candidates: std::sync::atomic::AtomicU64,
    pub(super) operator_candidates: std::sync::atomic::AtomicU64,
    pub(super) identity_facts: std::sync::atomic::AtomicU64,
    pub(super) signature_facts: std::sync::atomic::AtomicU64,
    pub(super) type_facts: std::sync::atomic::AtomicU64,
    pub(super) const_facts: std::sync::atomic::AtomicU64,
    pub(super) const_materializations: std::sync::atomic::AtomicU64,
    pub(super) nominal_materializations: std::sync::atomic::AtomicU64,
    pub(super) function_materializations: std::sync::atomic::AtomicU64,
    pub(super) method_materializations: std::sync::atomic::AtomicU64,
    pub(super) nominal_materialization_reuses: std::sync::atomic::AtomicU64,
    pub(super) function_materialization_reuses: std::sync::atomic::AtomicU64,
    pub(super) anonymous_facts: std::sync::atomic::AtomicU64,
    pub(super) producer_facts: std::sync::atomic::AtomicU64,
    pub(super) toolchain_facts: std::sync::atomic::AtomicU64,
    pub(super) import_nominal_registration_requests: std::sync::atomic::AtomicU64,
    pub(super) import_nominal_type_visits: std::sync::atomic::AtomicU64,
    pub(super) import_named_nominal_probes: std::sync::atomic::AtomicU64,
    pub(super) import_named_nominal_complete_hits: std::sync::atomic::AtomicU64,
    pub(super) import_named_nominal_cycle_hits: std::sync::atomic::AtomicU64,
    pub(super) import_named_nominals_registered: std::sync::atomic::AtomicU64,
    pub(super) import_nominal_type_edges_traversed: std::sync::atomic::AtomicU64,
    pub(super) import_anonymous_nominals_registered: std::sync::atomic::AtomicU64,
    pub(super) staged_probe_nodes: std::sync::atomic::AtomicU64,
    pub(super) staged_frontier_bodies: std::sync::atomic::AtomicU64,
    pub(super) staged_resolved_instructions: std::sync::atomic::AtomicU64,
    pub(super) staged_fact_nodes: std::sync::atomic::AtomicU64,
    pub(super) staged_canonical_evaluations: std::sync::atomic::AtomicU64,
    pub(super) staged_constraints_generated: std::sync::atomic::AtomicU64,
    pub(super) staged_binding_scope_nodes: std::sync::atomic::AtomicU64,
    pub(super) staged_binding_materializations: std::sync::atomic::AtomicU64,
    pub(super) staged_binding_trie_updates: std::sync::atomic::AtomicU64,
    pub(super) staged_binding_trie_lookups: std::sync::atomic::AtomicU64,
    pub(super) staged_precompute_nodes: std::sync::atomic::AtomicU64,
}

impl ProviderObservationCounters {
    pub(super) fn accrue_provider_body_work(&self, work: rue_air::ProviderBodyWork) {
        use std::sync::atomic::Ordering::Relaxed;
        self.import_nominal_registration_requests
            .fetch_add(work.import_nominal_registration_requests as u64, Relaxed);
        self.import_nominal_type_visits
            .fetch_add(work.import_nominal_type_visits as u64, Relaxed);
        self.import_named_nominal_probes
            .fetch_add(work.import_named_nominal_probes as u64, Relaxed);
        self.import_named_nominal_complete_hits
            .fetch_add(work.import_named_nominal_complete_hits as u64, Relaxed);
        self.import_named_nominal_cycle_hits
            .fetch_add(work.import_named_nominal_cycle_hits as u64, Relaxed);
        self.import_named_nominals_registered
            .fetch_add(work.import_named_nominals_registered as u64, Relaxed);
        self.import_nominal_type_edges_traversed
            .fetch_add(work.import_nominal_type_edges_traversed as u64, Relaxed);
        self.import_anonymous_nominals_registered
            .fetch_add(work.import_anonymous_nominals_registered as u64, Relaxed);
        self.staged_probe_nodes
            .fetch_add(work.staged_probe_nodes, Relaxed);
        self.staged_frontier_bodies
            .fetch_add(work.staged_frontier_bodies, Relaxed);
        self.staged_resolved_instructions
            .fetch_add(work.staged_resolved_instructions, Relaxed);
        self.staged_fact_nodes
            .fetch_add(work.staged_fact_nodes, Relaxed);
        self.staged_canonical_evaluations
            .fetch_add(work.staged_canonical_evaluations, Relaxed);
        self.staged_constraints_generated
            .fetch_add(work.staged_constraints_generated, Relaxed);
        self.staged_binding_scope_nodes
            .fetch_add(work.staged_binding_scope_nodes, Relaxed);
        self.staged_binding_materializations
            .fetch_add(work.staged_binding_materializations, Relaxed);
        self.staged_binding_trie_updates
            .fetch_add(work.staged_binding_trie_updates, Relaxed);
        self.staged_binding_trie_lookups
            .fetch_add(work.staged_binding_trie_lookups, Relaxed);
        self.staged_precompute_nodes
            .fetch_add(work.staged_precompute_nodes, Relaxed);
    }

    pub(super) fn snapshot(&self) -> crate::unstable::ProviderObservationMetrics {
        use std::sync::atomic::Ordering::Relaxed;
        let identity_facts = self.identity_facts.load(Relaxed);
        let signature_facts = self.signature_facts.load(Relaxed);
        let type_facts = self.type_facts.load(Relaxed);
        let const_facts = self.const_facts.load(Relaxed);
        let const_materializations = self.const_materializations.load(Relaxed);
        let nominal_materializations = self.nominal_materializations.load(Relaxed);
        let function_materializations = self.function_materializations.load(Relaxed);
        let method_materializations = self.method_materializations.load(Relaxed);
        crate::unstable::ProviderObservationMetrics {
            name_lookups: self.name_lookups.load(Relaxed),
            import_lookups: self.import_lookups.load(Relaxed),
            method_candidates: self.method_candidates.load(Relaxed),
            operator_candidates: self.operator_candidates.load(Relaxed),
            declaration_facts: identity_facts
                .saturating_add(signature_facts)
                .saturating_add(type_facts)
                .saturating_add(const_facts),
            identity_facts,
            signature_facts,
            type_facts,
            const_facts,
            materializations: const_materializations
                .saturating_add(nominal_materializations)
                .saturating_add(function_materializations)
                .saturating_add(method_materializations),
            shared_payload_materializations: nominal_materializations
                .saturating_add(function_materializations)
                .saturating_add(method_materializations),
            owned_payload_materializations: const_materializations,
            const_materializations,
            nominal_materializations,
            function_materializations,
            method_materializations,
            nominal_materialization_reuses: self.nominal_materialization_reuses.load(Relaxed),
            function_materialization_reuses: self.function_materialization_reuses.load(Relaxed),
            anonymous_facts: self.anonymous_facts.load(Relaxed),
            producer_facts: self.producer_facts.load(Relaxed),
            toolchain_facts: self.toolchain_facts.load(Relaxed),
            import_nominal_registration_requests: self
                .import_nominal_registration_requests
                .load(Relaxed),
            import_nominal_type_visits: self.import_nominal_type_visits.load(Relaxed),
            import_named_nominal_probes: self.import_named_nominal_probes.load(Relaxed),
            import_named_nominal_complete_hits: self
                .import_named_nominal_complete_hits
                .load(Relaxed),
            import_named_nominal_cycle_hits: self.import_named_nominal_cycle_hits.load(Relaxed),
            import_named_nominals_registered: self.import_named_nominals_registered.load(Relaxed),
            import_nominal_type_edges_traversed: self
                .import_nominal_type_edges_traversed
                .load(Relaxed),
            import_anonymous_nominals_registered: self
                .import_anonymous_nominals_registered
                .load(Relaxed),
            staged_probe_nodes: self.staged_probe_nodes.load(Relaxed),
            staged_frontier_bodies: self.staged_frontier_bodies.load(Relaxed),
            staged_resolved_instructions: self.staged_resolved_instructions.load(Relaxed),
            staged_fact_nodes: self.staged_fact_nodes.load(Relaxed),
            staged_canonical_evaluations: self.staged_canonical_evaluations.load(Relaxed),
            staged_constraints_generated: self.staged_constraints_generated.load(Relaxed),
            staged_binding_scope_nodes: self.staged_binding_scope_nodes.load(Relaxed),
            staged_binding_materializations: self.staged_binding_materializations.load(Relaxed),
            staged_binding_trie_updates: self.staged_binding_trie_updates.load(Relaxed),
            staged_binding_trie_lookups: self.staged_binding_trie_lookups.load(Relaxed),
            staged_precompute_nodes: self.staged_precompute_nodes.load(Relaxed),
        }
    }
}

/// Upper bound on the per-key node-incarnation history the published-root lookup
/// lease keeps for rederivation-after-eviction detection (RUE-1091, ADR-0066 §4).
/// The history is bounded FIFO: it exists only to notice that a re-observed key's
/// terminal was evicted and rebuilt (a fresh incarnation), never to retain a
/// terminal.
pub(super) const LOOKUP_INCARNATION_HISTORY_BOUND: usize = 4096;

/// One rooted request's exact observed lookup-terminal pin set, collected while
/// the request lease is still live so every terminal is continuously protected
/// (the pin-under-lock discipline: no birth-eviction window). Promotion transfers
/// this into the session-held [`PublishedRootLookupLease`].
///
/// Production body analysis records the candidate-set keys consulted by the
/// epoch analyzer, then resolves and pins their exact query terminals before
/// publishing the body transaction.
#[derive(Debug)]
pub(crate) struct ObservedLookupRoot {
    /// The exact pins, retained past the request through the batched-release set.
    pub(super) pins: rue_query::RetainedPinSet,
    /// `(typed logical key, node incarnation)` per distinct observed terminal,
    /// so promotion can detect a rederived-after-eviction key by its fresh
    /// incarnation. Parallel to the pins: only terminals newly leased (not
    /// deduplicated re-observations) are recorded.
    pub(super) observed_keys: Vec<(LookupObservationKey, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LookupObservationKey {
    Name(LookupNameKey),
    Import(LookupImportKey),
}

impl RetainedCharge for LookupObservationKey {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Name(key) => key
                .module
                .retained_charge()
                .saturating_add(key.name.retained_charge()),
            Self::Import(key) => key
                .module
                .retained_charge()
                .saturating_add(key.specifier.retained_charge()),
        }
    }
}

impl ObservedLookupRoot {
    pub(super) fn new() -> Self {
        Self {
            pins: rue_query::RetainedPinSet::new(),
            observed_keys: Vec::new(),
        }
    }

    /// Pin one observed lookup terminal into the set and record its logical
    /// identity. Acquiring the pin while the observing request task still holds
    /// its request-scoped lease keeps the terminal protected across the transfer.
    /// A terminal already present (same incarnation/stamp/revision) deduplicates:
    /// the redundant pin is dropped and no duplicate key is recorded.
    ///
    pub(super) fn record<K, V>(
        &mut self,
        family: &QueryFamily<K, V>,
        terminal: &Arc<rue_query::QueryTerminal<V>>,
        key: LookupObservationKey,
    ) where
        K: QueryKey,
        V: Clone + Send + Sync + 'static,
    {
        let Ok(pin) = family.pin_terminal(terminal) else {
            return;
        };
        let incarnation = terminal.node_incarnation();
        if self.pins.lease(pin) {
            self.observed_keys.push((key, incarnation));
        }
    }

    pub(super) fn descriptors(&self) -> crate::body_query::BodyLookupObservations {
        crate::body_query::BodyLookupObservations {
            terminals: self.observed_keys.to_vec().into(),
        }
    }
}

/// Session-held retention lease over the lookup families (`compiler.lookup-name`
/// and `compiler.lookup-import`) — the `PublishedRootLookupLease` of ADR-0066 §4.
///
/// On semantic-root publication, success or deterministic failure, a rooted
/// request's exact observed lookup-terminal pin set is promoted here, keyed by
/// the root's stable identity, atomically replacing that root's prior published
/// set; the superseded set is then batch-released. An attempt that aborts or
/// cancels before publishing a root is never promoted, and a merely speculative
/// lookup no published root observed is never pinned into a set, so it stays
/// evictable. The current root's set may grow a family beyond its historical
/// floor under pressure (grow-with-pressure-and-meter) but cannot be evicted
/// because a large program consults more names than the floor.
///
/// One published root's retained lookup pins plus the distinct logical keys they
/// cover, so the lease can report its currently retained logical working set.
#[derive(Debug)]
pub(super) struct RootLeaseEntry {
    pub(super) observations: ObservedLookupRoot,
    pub(super) publication: u64,
}

#[derive(Debug, Default)]
pub(super) struct PublishedRootLookupLease {
    /// One retained pin set per published root, keyed by the root's stable
    /// identity. A promotion installs the successor set here before releasing the
    /// predecessor, so the shared terminals of an edit/error/fix loop are never
    /// left unprotected across the swap.
    pub(super) roots: BTreeMap<String, RootLeaseEntry>,
    /// Monotonic identity for conditionally rolling back a publication without
    /// overwriting a newer concurrent successor.
    pub(super) next_root_publication: u64,
    /// Last observed node incarnation per distinct logical lookup key, bounded
    /// FIFO, for rederivation-after-eviction detection. The typed key shares its
    /// immutable strings with the observation; bookkeeping never materializes a
    /// presentation identity.
    /// Probed and updated by key only; `incarnation_order` owns the recency
    /// order, so nothing iterates this and a `BTreeMap` here only bought a
    /// module-path and name string comparison at every level of every probe.
    pub(super) incarnations: ahash::AHashMap<LookupObservationKey, (u64, u64)>,
    /// Recency order of `incarnations`, keyed by a monotonic observation
    /// generation. Refreshing a hot key removes its one old order entry in
    /// logarithmic time rather than scanning the full 4096-key history.
    pub(super) incarnation_order: BTreeSet<(u64, LookupObservationKey)>,
    pub(super) next_incarnation_generation: u64,
    /// Cumulative lookup keys re-observed with a changed node incarnation — a
    /// previously seen key whose retained terminal is gone (evicted, or otherwise
    /// a fresh node), so the re-observation sees a new incarnation. Under
    /// retention pressure this is eviction-forced rederivation (the acceptance
    /// falsifier, invisible to correctness); a legitimate source-driven recompute
    /// that changes the incarnation counts here too.
    pub(super) rederivations_after_eviction: u64,
    /// Lookup-family terminal evictions attributed to lease supersession — the
    /// runtime-metric delta captured while a superseded root's pins batch-release.
    pub(super) supersession_evictions: u64,
}

impl PublishedRootLookupLease {
    pub(super) fn seen_incarnation(&self, key: &LookupObservationKey) -> Option<u64> {
        self.incarnations
            .get(key)
            .map(|(incarnation, _)| *incarnation)
    }

    /// Refresh one observation and return the exact inverse operation.
    ///
    /// Publication commits are common while aborting a committed handoff is
    /// exceptional. A per-observation inverse keeps the success path
    /// proportional to the new root instead of snapshotting both bounded
    /// history trees before every commit.
    pub(super) fn record_incarnation(
        &mut self,
        key: LookupObservationKey,
        incarnation: u64,
    ) -> LookupIncarnationMutation {
        let generation = self.next_incarnation_generation;
        self.next_incarnation_generation = self
            .next_incarnation_generation
            .checked_add(1)
            .expect("lookup-incarnation observation generation overflow");
        let previous = self
            .incarnations
            .insert(key.clone(), (incarnation, generation));
        if let Some((_, previous_generation)) = previous {
            self.incarnation_order
                .remove(&(previous_generation, key.clone()));
        }
        self.incarnation_order.insert((generation, key.clone()));
        let mut evicted = None;
        while self.incarnations.len() > LOOKUP_INCARNATION_HISTORY_BOUND {
            let Some((_, oldest)) = self.incarnation_order.pop_first() else {
                break;
            };
            let previous = self
                .incarnations
                .remove(&oldest)
                .expect("lookup incarnation order names a retained entry");
            assert!(
                evicted.is_none(),
                "one observation evicts at most one entry"
            );
            evicted = Some((oldest, previous));
        }
        LookupIncarnationMutation {
            key,
            installed: (incarnation, generation),
            previous,
            evicted,
        }
    }

    pub(super) fn rollback_incarnation_mutations(
        &mut self,
        mutations: Vec<LookupIncarnationMutation>,
    ) {
        // A key may be refreshed more than once by a body-closure publication,
        // so restore the journal in strict reverse order.
        for mutation in mutations.into_iter().rev() {
            let installed = self
                .incarnations
                .remove(&mutation.key)
                .expect("rollback removes the installed lookup incarnation");
            assert_eq!(installed, mutation.installed);
            assert!(
                self.incarnation_order
                    .remove(&(mutation.installed.1, mutation.key.clone())),
                "rollback removes the installed lookup recency entry"
            );
            if let Some((key, previous)) = mutation.evicted {
                assert!(self.incarnations.insert(key.clone(), previous).is_none());
                assert!(self.incarnation_order.insert((previous.1, key)));
            }
            if let Some(previous) = mutation.previous {
                assert!(
                    self.incarnations
                        .insert(mutation.key.clone(), previous)
                        .is_none()
                );
                assert!(self.incarnation_order.insert((previous.1, mutation.key)));
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct LookupIncarnationMutation {
    pub(super) key: LookupObservationKey,
    pub(super) installed: (u64, u64),
    pub(super) previous: Option<(u64, u64)>,
    pub(super) evicted: Option<(LookupObservationKey, (u64, u64))>,
}

pub(crate) fn body_lookup_root_identity(key: &crate::body_query::BodyQueryKey) -> String {
    format!("body:{}", key.stable_identity())
}

pub(super) fn replace_published_lookup_root(
    lease: &Arc<Mutex<PublishedRootLookupLease>>,
    runtime: &QueryRuntime,
    root: String,
    observed: ObservedLookupRoot,
) {
    let mut lease = lease
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (key, incarnation) in &observed.observed_keys {
        if let Some(previous) = lease.seen_incarnation(key)
            && previous != *incarnation
        {
            lease.rederivations_after_eviction += 1;
        }
        lease.record_incarnation(key.clone(), *incarnation);
    }
    let publication = lease.next_root_publication;
    lease.next_root_publication = lease
        .next_root_publication
        .checked_add(1)
        .expect("lookup-root publication generation overflow");
    let prior = lease.roots.insert(
        root,
        RootLeaseEntry {
            observations: observed,
            publication,
        },
    );
    let evictions_before = runtime.metrics().evictions;
    drop(prior);
    lease.supersession_evictions += runtime.metrics().evictions - evictions_before;
}

#[derive(Debug)]
pub(super) struct PublishedLookupRootHandoff {
    pub(super) lease: Arc<Mutex<PublishedRootLookupLease>>,
    pub(super) runtime: QueryRuntime,
    pub(super) root: String,
    pub(super) observed: Option<ObservedLookupRoot>,
    pub(super) rollback: Option<PublishedLookupRootRollback>,
}

#[derive(Debug)]
pub(super) struct PublishedLookupRootRollback {
    pub(super) previous: Option<RootLeaseEntry>,
    publication: u64,
    pub(super) incarnation_mutations: Vec<LookupIncarnationMutation>,
    pub(super) previous_next_incarnation_generation: u64,
    pub(super) previous_rederivations_after_eviction: u64,
    pub(super) previous_supersession_evictions: u64,
    pub(super) previous_next_root_publication: u64,
    pub(super) expected_next_root_publication: u64,
}

impl rue_query::QueryAttemptHandoff for PublishedLookupRootHandoff {
    fn commit(&mut self) {
        assert!(
            self.rollback.is_none(),
            "lookup-root handoff commits from pending"
        );
        let observed = self
            .observed
            .take()
            .expect("lookup-root handoff commits at most once");
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_next_incarnation_generation = lease.next_incarnation_generation;
        let previous_rederivations_after_eviction = lease.rederivations_after_eviction;
        let previous_supersession_evictions = lease.supersession_evictions;
        let previous_next_root_publication = lease.next_root_publication;
        let mut incarnation_mutations = Vec::with_capacity(observed.observed_keys.len());
        for (key, incarnation) in &observed.observed_keys {
            if let Some(previous) = lease.seen_incarnation(key)
                && previous != *incarnation
            {
                lease.rederivations_after_eviction += 1;
            }
            incarnation_mutations.push(lease.record_incarnation(key.clone(), *incarnation));
        }
        let publication = lease.next_root_publication;
        lease.next_root_publication = lease
            .next_root_publication
            .checked_add(1)
            .expect("lookup-root publication generation overflow");
        let previous = lease.roots.insert(
            self.root.clone(),
            RootLeaseEntry {
                observations: observed,
                publication,
            },
        );
        self.rollback = Some(PublishedLookupRootRollback {
            previous,
            publication,
            incarnation_mutations,
            previous_next_incarnation_generation,
            previous_rederivations_after_eviction,
            previous_supersession_evictions,
            previous_next_root_publication,
            expected_next_root_publication: lease.next_root_publication,
        });
    }

    fn abort(&mut self) {
        let Some(rollback) = self.rollback.take() else {
            drop(self.observed.take());
            return;
        };
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            lease.next_root_publication, rollback.expected_next_root_publication,
            "a concurrently superseded lookup publication cannot be retried"
        );
        let current = lease
            .roots
            .get(&self.root)
            .expect("an installed lookup root remains present until rollback");
        assert_eq!(
            current.publication, rollback.publication,
            "lookup rollback cannot overwrite a newer root publication"
        );
        let installed = lease
            .roots
            .remove(&self.root)
            .expect("the checked lookup publication remains installed");
        if let Some(previous) = rollback.previous {
            lease.roots.insert(self.root.clone(), previous);
        }
        lease.rollback_incarnation_mutations(rollback.incarnation_mutations);
        lease.next_incarnation_generation = rollback.previous_next_incarnation_generation;
        lease.rederivations_after_eviction = rollback.previous_rederivations_after_eviction;
        lease.supersession_evictions = rollback.previous_supersession_evictions;
        lease.next_root_publication = rollback.previous_next_root_publication;
        self.observed = Some(installed.observations);
    }
}

impl Drop for PublishedLookupRootHandoff {
    fn drop(&mut self) {
        let Some(rollback) = self.rollback.take() else {
            return;
        };
        let evictions_before = self.runtime.metrics().evictions;
        drop(rollback.previous);
        let evictions = self.runtime.metrics().evictions - evictions_before;
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lease.supersession_evictions = lease.supersession_evictions.saturating_add(evictions);
    }
}

#[derive(Debug)]
pub(super) struct PublishedBodyClosureLookupHandoff {
    pub(super) lease: Arc<Mutex<PublishedRootLookupLease>>,
    pub(super) runtime: QueryRuntime,
    pub(super) observed: Option<BTreeMap<String, ObservedLookupRoot>>,
    pub(super) retire_absent: bool,
    pub(super) rollback: Option<PublishedBodyClosureLookupRollback>,
}

#[derive(Debug)]
pub(super) struct PublishedBodyClosureLookupRollback {
    pub(super) previous_roots: BTreeMap<String, RootLeaseEntry>,
    pub(super) installed: BTreeMap<String, u64>,
    pub(super) incarnation_mutations: Vec<LookupIncarnationMutation>,
    pub(super) previous_next_incarnation_generation: u64,
    pub(super) previous_rederivations_after_eviction: u64,
    pub(super) previous_supersession_evictions: u64,
    pub(super) previous_next_root_publication: u64,
    pub(super) expected_next_root_publication: u64,
}

#[derive(Debug, Default)]
pub(super) struct PublishedBodyClosureRoot {
    pub(super) lease: Arc<rue_query::RetainedPinSet>,
    pub(super) reached: BTreeSet<crate::FunctionInstanceKey>,
    pub(super) additions: u64,
    pub(super) deletions: u64,
}

#[derive(Debug, Default)]
pub(super) struct PublishedBodyReachabilityRoot {
    pub(super) lease: Arc<rue_query::RetainedPinSet>,
}

/// Exact candidate-artifact terminals bridging the session's declaration-root
/// discovery request to the subsequent body-closure request.
///
/// The projection is intentionally requested before the closure because it
/// determines `main` and the exported roots. Its candidate artifacts use a
/// small family history, so dropping the first rooted request before starting
/// the closure would make validation evict and rederive the same const/comptime
/// artifacts. Pinning only that bounded family avoids cloning the much larger
/// declaration projection cone. This lease owns only the inter-request gap;
/// the final body closure cone supersedes it atomically.
#[derive(Debug, Default)]
pub(super) struct PublishedDeclarationSemanticsRoot {
    pub(super) lease: Arc<rue_query::RetainedPinSet>,
}

/// RUE-1576: one backend collection's retained child cones, borrowed as
/// fallback authority by the scopes that run after it in the same rooted
/// compile so they do not re-lease a cone the predecessor just certified.
#[derive(Default)]
pub(super) struct PublishedCollectionRoot {
    pub(super) lease: Arc<rue_query::RetainedPinSet>,
}

pub(super) fn backend_retention_fallbacks(
    backend: &Arc<Mutex<PublishedBackendRoot>>,
    body_closure: &Arc<Mutex<PublishedBodyClosureRoot>>,
    body_reachability: &Arc<Mutex<PublishedBodyReachabilityRoot>>,
    cfg_collection: &Arc<Mutex<PublishedCollectionRoot>>,
    codegen_collection: &Arc<Mutex<PublishedCollectionRoot>>,
) -> [Arc<rue_query::RetainedPinSet>; 5] {
    [
        backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .clone(),
        body_closure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .clone(),
        body_reachability
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .clone(),
        cfg_collection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .clone(),
        codegen_collection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lease
            .clone(),
    ]
}

/// One in-flight rooted backend collection. Merely collecting or pinning a
/// terminal cannot alter the session's published root: only
/// [`RevisionedQueryDatabase::publish_backend_root`] installs this candidate.
/// Dropping a failed, canceled, or speculative collection releases only its
/// candidate pins and leaves the last successful root untouched.
#[derive(Debug, Default)]
pub(crate) struct BackendRootCandidate {
    pub(super) lease: rue_query::RetainedPinSet,
    pub(super) functions: BTreeSet<crate::FunctionInstanceKey>,
    pub(super) cfg_keys: AHashSet<crate::cfg_query::CfgQueryKey>,
    pub(super) optimized_cfg_terminals: usize,
    pub(super) codegen_unit_terminals: usize,
    pub(super) object_projection_terminals: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct OptimizedCfgBatchKey {
    pub(crate) keys: Arc<[crate::cfg_query::OptimizedCfgQueryKey]>,
    /// Roots supplied by the canonical body closure.  Whole-program CFG
    /// reachability must use the same roots as semantic body discovery,
    /// including C-ABI exports, rather than inferring ownership from a CFG
    /// body's presentation fields.
    pub(crate) roots: Arc<[crate::FunctionInstanceKey]>,
    /// O2/O3 batches are request-local because their values may contain
    /// rewritten callers. Child CFG terminals remain reusable; this token
    /// prevents a rewritten whole-program result crossing request boundaries.
    pub(crate) generation: u64,
    /// Digest and memo bucket derived once at construction.
    ///
    /// This key names every optimized-CFG unit in the batch, so it is
    /// whole-program sized: on Lattice it absorbs up to 30,744 bytes. Every
    /// `CodegenUnitQueryKey` carries a shared handle to one of these, and a
    /// derived `Hash` made each of the 1,280 codegen keys re-walk the entire
    /// list — 11.7 MB of hashing per fresh build, 98.4% of everything the
    /// codegen key absorbed. Deriving both values once here makes hashing the
    /// batch constant-time for every holder.
    pub(super) digest: rue_query::StableKeyHash,
    pub(super) memo_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RawCfgBatchKey {
    pub(crate) keys: Arc<[crate::cfg_query::CfgQueryKey]>,
}

impl QueryKey for RawCfgBatchKey {
    fn stable_identity(&self) -> String {
        let mut identity = format!("raw-cfg-batch;units={}", self.keys.len());
        for key in self.keys.iter() {
            identity.push('\u{1e}');
            identity.push_str(&key.shared_stable_identity());
        }
        identity
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        hasher.write_usize(self.keys.len());
        for key in self.keys.iter() {
            key.stable_hash(hasher);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RawCfgBatchOutput {
    pub(crate) values: Arc<[crate::cfg_query::CfgValue]>,
    /// Exact raw-CFG child cones captured while their request leases are live.
    pub(super) _retained_children: Arc<rue_query::RetainedPinSet>,
}

impl RetainedCharge for RawCfgBatchOutput {
    fn retained_charge(&self) -> u64 {
        self.values.retained_charge()
    }
}

impl OptimizedCfgBatchKey {
    pub(crate) fn new(
        keys: Arc<[crate::cfg_query::OptimizedCfgQueryKey]>,
        generation: u64,
        roots: Arc<[crate::FunctionInstanceKey]>,
    ) -> Self {
        let mut hasher = rue_query::StableHasher::new();
        hasher.write_usize(keys.len());
        for key in keys.iter() {
            key.stable_hash(&mut hasher);
        }
        roots.hash(&mut hasher);
        generation.hash(&mut hasher);
        let digest = hasher.finish128();
        let memo_hash = hasher.finish();
        Self {
            keys,
            roots,
            generation,
            digest,
            memo_hash,
        }
    }
}

impl PartialEq for OptimizedCfgBatchKey {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && (Arc::ptr_eq(&self.keys, &other.keys) || self.keys == other.keys)
            && (Arc::ptr_eq(&self.roots, &other.roots) || self.roots == other.roots)
    }
}

impl Eq for OptimizedCfgBatchKey {}

impl Hash for OptimizedCfgBatchKey {
    /// A memo bucket selector; `PartialEq` above stays authoritative.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.memo_hash.hash(state);
    }
}

impl QueryKey for OptimizedCfgBatchKey {
    fn stable_identity(&self) -> String {
        let mut identity = format!("optimized-cfg-batch;units={}", self.keys.len());
        for key in self.keys.iter() {
            identity.push('\u{1e}');
            identity.push_str(&key.shared_stable_identity());
        }
        identity.push_str(&format!(";roots={:?}", self.roots));
        identity.push_str(&format!(";generation={}", self.generation));
        identity
    }

    /// The digest derived once in [`Self::new`], over the same field set this
    /// used to absorb per call.
    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        let digest = self.digest.to_u128();
        hasher.write_u64(digest as u64);
        hasher.write_u64((digest >> 64) as u64);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OptimizedCfgBatchOutput {
    pub(crate) values: Arc<[crate::cfg_query::CfgValue]>,
    /// Functions whose batch result was changed by general inlining. Their
    /// function-local CFG children are intentionally omitted from durable
    /// backend retention for Phase 2. The active request may still transport
    /// and pin these records for codegen; the request-local batch generation
    /// ensures no successor can reuse that whole-program result.
    pub(crate) non_reusable_functions: Arc<[crate::FunctionInstanceKey]>,
    /// Functions absent from the post-inline whole-program reachability
    /// closure.  The batch retains value/key alignment; rooted backend
    /// consumers filter these identities before making codegen requests.
    pub(crate) unreachable_functions: Arc<[crate::FunctionInstanceKey]>,
    /// Exact child leases acquired while the rooted batch evaluator still owns
    /// every request lease. The memoized root encapsulates these pins so
    /// retaining it also retains the scheduled children through publication.
    pub(super) _retained_children: Arc<rue_query::RetainedPinSet>,
}

impl RetainedCharge for OptimizedCfgBatchOutput {
    fn retained_charge(&self) -> u64 {
        self.values.retained_charge()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CodegenUnitBatchKey {
    pub(crate) keys: Arc<[crate::codegen_query::CodegenUnitQueryKey]>,
}

impl QueryKey for CodegenUnitBatchKey {
    fn stable_identity(&self) -> String {
        let mut identity = format!("codegen-unit-batch;units={}", self.keys.len());
        for key in self.keys.iter() {
            identity.push('\u{1e}');
            identity.push_str(&key.shared_stable_identity());
        }
        identity
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        hasher.write_usize(self.keys.len());
        for key in self.keys.iter() {
            key.stable_hash(hasher);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CodegenUnitBatchOutput {
    pub(crate) values: Arc<[crate::codegen_query::CodegenUnitValue]>,
    /// See `OptimizedCfgBatchOutput::_retained_children`.
    pub(super) _retained_children: Arc<rue_query::RetainedPinSet>,
}

impl RetainedCharge for CodegenUnitBatchOutput {
    fn retained_charge(&self) -> u64 {
        self.values.retained_charge()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObjectProjectionBatchKey {
    pub(crate) keys: Arc<[crate::object_query::ObjectProjectionQueryKey]>,
}

impl QueryKey for ObjectProjectionBatchKey {
    fn stable_identity(&self) -> String {
        let mut identity = format!("object-projection-batch;units={}", self.keys.len());
        for key in self.keys.iter() {
            identity.push('\u{1e}');
            identity.push_str(&key.shared_stable_identity());
        }
        identity
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        hasher.write_usize(self.keys.len());
        for key in self.keys.iter() {
            key.stable_hash(hasher);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectProjectionBatchOutput {
    pub(crate) values: Arc<[crate::object_query::ObjectProjectionValue]>,
    /// Exact object children and their CodegenUnit dependency cones are pinned
    /// from evaluation through backend-root publication.
    pub(super) _retained_children: Arc<rue_query::RetainedPinSet>,
}

impl RetainedCharge for ObjectProjectionBatchOutput {
    fn retained_charge(&self) -> u64 {
        self.values.retained_charge()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct BackendRootPublicationKey {
    pub(super) epoch: u64,
    pub(super) input: BackendRootPublicationInput,
    pub(super) functions: Arc<[crate::FunctionInstanceKey]>,
    pub(super) cfg_terminals: usize,
    pub(super) optimized_cfg_terminals: usize,
    pub(super) codegen_unit_terminals: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum BackendRootPublicationInput {
    Codegen(CodegenUnitBatchKey),
    Objects(ObjectProjectionBatchKey),
}

#[derive(Debug, Default)]
pub(super) struct BackendRootPublicationGate(pub(super) Mutex<()>);

impl BackendRootPublicationGate {
    pub(super) fn enter(&self) -> std::sync::MutexGuard<'_, ()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl QueryKey for BackendRootPublicationKey {
    fn stable_identity(&self) -> String {
        format!(
            "backend-root;epoch={};units={}",
            self.epoch,
            match &self.input {
                BackendRootPublicationInput::Codegen(key) => key.keys.len(),
                BackendRootPublicationInput::Objects(key) => key.keys.len(),
            }
        )
    }

    fn stable_hash(&self, hasher: &mut rue_query::StableHasher) {
        self.epoch.hash(hasher);
        match &self.input {
            BackendRootPublicationInput::Codegen(key) => {
                0_u8.hash(hasher);
                key.stable_hash(hasher);
            }
            BackendRootPublicationInput::Objects(key) => {
                1_u8.hash(hasher);
                key.stable_hash(hasher);
            }
        }
        self.functions.hash(hasher);
        self.cfg_terminals.hash(hasher);
        self.optimized_cfg_terminals.hash(hasher);
        self.codegen_unit_terminals.hash(hasher);
    }
}

#[derive(Debug, Default)]
pub(super) struct PublishedBackendRoot {
    #[allow(dead_code)] // Owning the set is the retention root.
    pub(super) lease: Arc<rue_query::RetainedPinSet>,
    pub(super) functions: BTreeSet<crate::FunctionInstanceKey>,
    #[allow(dead_code)]
    pub(super) cfg_terminals: usize,
    #[allow(dead_code)]
    pub(super) optimized_cfg_terminals: usize,
    #[allow(dead_code)]
    pub(super) codegen_unit_terminals: usize,
    #[allow(dead_code)]
    pub(super) object_projection_terminals: usize,
    pub(super) publications: u64,
    pub(super) additions: u64,
    pub(super) deletions: u64,
}

#[derive(Debug)]
pub(super) struct PublishedBackendRootHandoff {
    pub(super) root: Arc<Mutex<PublishedBackendRoot>>,
    pub(super) pending: Option<Arc<rue_query::RetainedPinSet>>,
    pub(super) functions: Option<BTreeSet<crate::FunctionInstanceKey>>,
    pub(super) cfg_terminals: usize,
    pub(super) optimized_cfg_terminals: usize,
    pub(super) codegen_unit_terminals: usize,
    pub(super) object_projection_terminals: usize,
    pub(super) previous: Option<PublishedBackendRoot>,
    pub(super) installed: bool,
}

impl rue_query::QueryAttemptHandoff for PublishedBackendRootHandoff {
    fn commit(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("backend-root handoff commits at most once");
        let functions = self
            .functions
            .take()
            .expect("backend-root handoff retains exact membership");
        let mut root = self
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let additions = functions.difference(&root.functions).count() as u64;
        let deletions = root.functions.difference(&functions).count() as u64;
        let next = PublishedBackendRoot {
            lease: pending,
            functions,
            cfg_terminals: self.cfg_terminals,
            optimized_cfg_terminals: self.optimized_cfg_terminals,
            codegen_unit_terminals: self.codegen_unit_terminals,
            object_projection_terminals: self.object_projection_terminals,
            publications: root.publications.saturating_add(1),
            additions: root.additions.saturating_add(additions),
            deletions: root.deletions.saturating_add(deletions),
        };
        self.previous = Some(std::mem::replace(&mut *root, next));
        self.installed = true;
    }

    fn abort(&mut self) {
        if self.installed {
            let mut root = self
                .root
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = self
                .previous
                .take()
                .expect("an installed backend root retains its predecessor");
            let installed = std::mem::replace(&mut *root, previous);
            self.pending = Some(installed.lease);
            self.functions = Some(installed.functions);
            self.installed = false;
        } else {
            drop(self.pending.take());
            drop(self.functions.take());
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublishedBackendRootMetrics {
    pub(crate) functions: usize,
    pub(crate) cfg_terminals: usize,
    pub(crate) optimized_cfg_terminals: usize,
    pub(crate) codegen_unit_terminals: usize,
    pub(crate) object_projection_terminals: usize,
    pub(crate) publications: u64,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InputStampRetentionMetrics {
    pub module_views: usize,
    pub module_source_stamps: usize,
    pub import_views: usize,
    pub import_context_stamps: usize,
    pub accepted_topology_stamps: usize,
    pub accepted_read_provenance_stamps: usize,
    pub import_observation_stamps: usize,
}

#[derive(Debug)]
pub(super) struct PublishedBodyReachabilityTerminalHandoff {
    pub(super) root: Arc<Mutex<PublishedBodyReachabilityRoot>>,
    pub(super) pending: Option<Arc<rue_query::RetainedPinSet>>,
    pub(super) previous: Option<PublishedBodyReachabilityRoot>,
    pub(super) installed: bool,
}

impl rue_query::QueryAttemptHandoff for PublishedBodyReachabilityTerminalHandoff {
    fn commit(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("body-reachability terminal handoff commits at most once");
        let mut root = self
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.previous = Some(std::mem::replace(
            &mut *root,
            PublishedBodyReachabilityRoot { lease: pending },
        ));
        self.installed = true;
    }

    fn abort(&mut self) {
        if self.installed {
            let previous = self
                .previous
                .take()
                .expect("an installed reachability lease retains its predecessor");
            let mut root = self
                .root
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let installed = std::mem::replace(&mut *root, previous);
            self.pending = Some(installed.lease);
            self.installed = false;
        } else {
            drop(self.pending.take());
        }
    }
}

#[derive(Debug)]
pub(super) struct PublishedDeclarationSemanticsTerminalHandoff {
    pub(super) root: Arc<Mutex<PublishedDeclarationSemanticsRoot>>,
    pub(super) pending: Option<Arc<rue_query::RetainedPinSet>>,
    pub(super) previous: Option<PublishedDeclarationSemanticsRoot>,
    pub(super) installed: bool,
}

impl rue_query::QueryAttemptHandoff for PublishedDeclarationSemanticsTerminalHandoff {
    fn commit(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("declaration-semantics terminal handoff commits at most once");
        let mut root = self
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.previous = Some(std::mem::replace(
            &mut *root,
            PublishedDeclarationSemanticsRoot { lease: pending },
        ));
        self.installed = true;
    }

    fn abort(&mut self) {
        if self.installed {
            let previous = self
                .previous
                .take()
                .expect("an installed declaration-semantics lease retains its predecessor");
            let mut root = self
                .root
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let installed = std::mem::replace(&mut *root, previous);
            self.pending = Some(installed.lease);
            self.installed = false;
        } else {
            drop(self.pending.take());
        }
    }
}

#[derive(Debug)]
pub(super) struct PublishedBodyClosureTerminalHandoff {
    pub(super) root: Arc<Mutex<PublishedBodyClosureRoot>>,
    pub(super) pending: Option<Arc<rue_query::RetainedPinSet>>,
    pub(super) pending_reached: Option<BTreeSet<crate::FunctionInstanceKey>>,
    pub(super) previous: Option<PublishedBodyClosureRoot>,
    pub(super) installed: bool,
}

impl rue_query::QueryAttemptHandoff for PublishedBodyClosureTerminalHandoff {
    fn commit(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("body-closure terminal handoff commits at most once");
        let pending_reached = self
            .pending_reached
            .take()
            .expect("body-closure terminal handoff retains exact membership");
        let mut root = self
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let additions = pending_reached.difference(&root.reached).count() as u64;
        let deletions = root.reached.difference(&pending_reached).count() as u64;
        let next = PublishedBodyClosureRoot {
            lease: pending,
            reached: pending_reached,
            additions: root.additions.saturating_add(additions),
            deletions: root.deletions.saturating_add(deletions),
        };
        let previous = std::mem::replace(&mut *root, next);
        self.previous = Some(previous);
        self.installed = true;
    }

    fn abort(&mut self) {
        if self.installed {
            let previous = self
                .previous
                .take()
                .expect("an installed closure lease retains its predecessor");
            let mut root = self
                .root
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let installed = std::mem::replace(&mut *root, previous);
            self.pending = Some(installed.lease);
            self.pending_reached = Some(installed.reached);
            self.installed = false;
        } else {
            drop(self.pending.take());
            drop(self.pending_reached.take());
        }
    }
}

impl rue_query::QueryAttemptHandoff for PublishedBodyClosureLookupHandoff {
    fn commit(&mut self) {
        assert!(
            self.rollback.is_none(),
            "body-closure lookup handoff commits from pending"
        );
        let observed = self
            .observed
            .take()
            .expect("body-closure lookup handoff commits at most once");
        let reached = observed.keys().cloned().collect::<BTreeSet<_>>();
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut rollback = PublishedBodyClosureLookupRollback {
            previous_roots: BTreeMap::new(),
            installed: BTreeMap::new(),
            incarnation_mutations: Vec::new(),
            previous_next_incarnation_generation: lease.next_incarnation_generation,
            previous_rederivations_after_eviction: lease.rederivations_after_eviction,
            previous_supersession_evictions: lease.supersession_evictions,
            previous_next_root_publication: lease.next_root_publication,
            expected_next_root_publication: 0,
        };
        for (root, observed) in observed {
            for (key, incarnation) in &observed.observed_keys {
                if let Some(previous) = lease.seen_incarnation(key)
                    && previous != *incarnation
                {
                    lease.rederivations_after_eviction += 1;
                }
                rollback
                    .incarnation_mutations
                    .push(lease.record_incarnation(key.clone(), *incarnation));
            }
            let publication = lease.next_root_publication;
            lease.next_root_publication = lease
                .next_root_publication
                .checked_add(1)
                .expect("lookup-root publication generation overflow");
            let previous = lease.roots.insert(
                root.clone(),
                RootLeaseEntry {
                    observations: observed,
                    publication,
                },
            );
            if let Some(previous) = previous {
                rollback.previous_roots.insert(root.clone(), previous);
            }
            rollback.installed.insert(root, publication);
        }
        if self.retire_absent {
            let retired = lease
                .roots
                .keys()
                .filter(|root| root.starts_with("body:") && !reached.contains(*root))
                .cloned()
                .collect::<Vec<_>>();
            for root in retired {
                let previous = lease
                    .roots
                    .remove(&root)
                    .expect("a selected retired lookup root remains present");
                rollback.previous_roots.insert(root, previous);
            }
        }
        rollback.expected_next_root_publication = lease.next_root_publication;
        self.rollback = Some(rollback);
    }

    fn abort(&mut self) {
        let Some(rollback) = self.rollback.take() else {
            drop(self.observed.take());
            return;
        };
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            lease.next_root_publication, rollback.expected_next_root_publication,
            "a concurrently superseded lookup publication cannot be retried"
        );
        let mut observed = BTreeMap::new();
        for (root, publication) in &rollback.installed {
            let current = lease
                .roots
                .get(root)
                .expect("an installed lookup root remains present until rollback");
            assert_eq!(
                current.publication, *publication,
                "lookup rollback cannot overwrite a newer root publication"
            );
        }
        for root in rollback.installed.keys() {
            let installed = lease
                .roots
                .remove(root)
                .expect("the checked lookup publication remains installed");
            observed.insert(root.clone(), installed.observations);
        }
        for (root, previous) in rollback.previous_roots {
            lease.roots.insert(root, previous);
        }
        lease.rollback_incarnation_mutations(rollback.incarnation_mutations);
        lease.next_incarnation_generation = rollback.previous_next_incarnation_generation;
        lease.rederivations_after_eviction = rollback.previous_rederivations_after_eviction;
        lease.supersession_evictions = rollback.previous_supersession_evictions;
        lease.next_root_publication = rollback.previous_next_root_publication;
        self.observed = Some(observed);
    }
}

impl Drop for PublishedBodyClosureLookupHandoff {
    fn drop(&mut self) {
        let Some(rollback) = self.rollback.take() else {
            return;
        };
        let evictions_before = self.runtime.metrics().evictions;
        drop(rollback.previous_roots);
        let evictions = self.runtime.metrics().evictions - evictions_before;
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lease.supersession_evictions = lease.supersession_evictions.saturating_add(evictions);
    }
}
