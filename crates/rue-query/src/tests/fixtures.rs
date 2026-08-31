//! Shared fixtures for the query-runtime test subsystem modules.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Key(pub &'static str);

impl QueryKey for Key {
    fn stable_identity(&self) -> String {
        self.0.to_owned()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
}

pub(super) fn assert_validation_work_consistent(work: ValidationWork) {
    assert_eq!(
        work.traversals,
        work.successful_traversals + work.dirty_traversals + work.aborted_traversals,
        "every validation traversal has exactly one outcome"
    );
    assert_eq!(
        work.node_visits,
        work.active_cycle_prunes + work.memo_hits + work.memo_misses,
        "every erased-node visit has exactly one outcome"
    );
    assert_eq!(
        work.memo_misses,
        work.certificate_misses + work.proof_reacquisition_misses,
        "every memo miss has exactly one cause"
    );
    assert_eq!(
        work.registry_probes,
        work.dependency_observations + work.successful_traversals,
        "validation probes once per dependency and successful root certificate"
    );
    assert!(work.registry_index_lookups <= work.registry_probes);
    assert!(work.registry_misses <= work.registry_index_lookups);
    assert!(work.endorsement_hits <= work.endorsement_probes);
    assert!(work.duplicate_terminal_lease_observations <= work.terminal_lease_observations);
    assert!(
        work.demand_reuses + work.demand_computes + work.demand_joins + work.demand_aborts
            <= work.demands
    );
    assert!(work.certificates_published <= work.successful_traversals);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
// A numeric key for tests that need an unbounded supply of distinct keys
// (e.g. flooding a family past a tiny retention bound).
pub(super) struct Slot(pub u64);

impl QueryKey for Slot {
    fn stable_identity(&self) -> String {
        self.0.to_string()
    }

    fn stable_hash(&self, hasher: &mut StableHasher) {
        self.0.hash(hasher);
    }
}

pub(super) fn revision(id: u64) -> Revision {
    Revision::new(id, id)
}

// Deterministic single-hasher probe used only to demonstrate that two keys
// land in the same bucket. The live memo map uses independently keyed
// AHash; this fixed-seed `DefaultHasher` just makes the collision
// assertion reproducible.
pub(super) fn hash_of<K: std::hash::Hash>(key: &K) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn publish_empty(runtime: &QueryRuntime, revisions: impl IntoIterator<Item = Revision>) {
    for revision in revisions {
        runtime.publish_revision(revision, []).unwrap();
    }
}

#[derive(Debug)]
pub(super) struct CountingHandoff {
    pub(super) commits: Arc<AtomicUsize>,
    pub(super) aborts: Arc<AtomicUsize>,
}

impl QueryAttemptHandoff for CountingHandoff {
    fn commit(&mut self) {
        self.commits.fetch_add(1, Ordering::SeqCst);
    }

    fn abort(&mut self) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }
}
