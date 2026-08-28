//! Retained-terminal budgets, eviction, and pinned references.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use crate::*;

/// Initial runtime-wide soft budget for deterministic retained terminal charge.
///
/// This is an accounting budget rather than an allocator/RSS promise. Protected
/// terminals may exceed it; the runtime records that pressure and reclaims the
/// excess as soon as protection releases.
pub const DEFAULT_RETAINED_BYTE_BUDGET: u64 = 8 * 1024 * 1024 * 1024;

/// Initial runtime-wide soft budget for retained dependency and input
/// observations.
pub const DEFAULT_DEPENDENCY_PIN_BUDGET: u64 = 4_000_000;

/// Runtime-wide soft retention budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionBudgets {
    /// Deterministic terminal/artifact charge in bytes.
    pub retained_bytes: u64,
    /// Retained dependency plus input observation edges.
    pub dependency_pins: u64,
}

impl Default for RetentionBudgets {
    fn default() -> Self {
        Self {
            retained_bytes: DEFAULT_RETAINED_BYTE_BUDGET,
            dependency_pins: DEFAULT_DEPENDENCY_PIN_BUDGET,
        }
    }
}

#[derive(Debug)]
pub(crate) struct RetentionEntry<K, V> {
    pub(crate) node: Weak<Node<K, V>>,
    pub(crate) attempt: u64,
}

pub(crate) struct FamilyRetentionQueue<K, V> {
    entries: VecDeque<RetentionEntry<K, V>>,
    pub(crate) retained_bytes: u64,
    pub(crate) dependency_pins: u64,
    pub(crate) next_byte_probe: u64,
    pub(crate) next_pin_probe: u64,
    pub(crate) byte_probe_quantum: u64,
    pub(crate) pin_probe_quantum: u64,
}

impl<K, V> FamilyRetentionQueue<K, V> {
    pub(crate) fn new(budgets: RetentionBudgets) -> Self {
        let byte_probe_quantum =
            retention_probe_quantum(budgets.retained_bytes, 1024 * 1024, 32 * 1024 * 1024);
        let pin_probe_quantum = retention_probe_quantum(budgets.dependency_pins, 4096, 65_536);
        Self {
            entries: VecDeque::new(),
            retained_bytes: 0,
            dependency_pins: 0,
            next_byte_probe: byte_probe_quantum,
            next_pin_probe: pin_probe_quantum,
            byte_probe_quantum,
            pin_probe_quantum,
        }
    }

    pub(crate) fn publish(
        &mut self,
        entry: RetentionEntry<K, V>,
        retained_bytes: u64,
        dependency_pins: u64,
    ) -> bool {
        self.entries.push_back(entry);
        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        self.dependency_pins = self.dependency_pins.saturating_add(dependency_pins);
        let probe = self.retained_bytes >= self.next_byte_probe
            || self.dependency_pins >= self.next_pin_probe;
        if probe {
            self.next_byte_probe = next_probe(self.retained_bytes, self.byte_probe_quantum);
            self.next_pin_probe = next_probe(self.dependency_pins, self.pin_probe_quantum);
        }
        probe
    }

    pub(crate) fn remove_charge(&mut self, retained_bytes: u64, dependency_pins: u64) {
        self.retained_bytes = self
            .retained_bytes
            .checked_sub(retained_bytes)
            .expect("retained byte charge releases exactly once");
        self.dependency_pins = self
            .dependency_pins
            .checked_sub(dependency_pins)
            .expect("retained dependency-pin charge releases exactly once");
        // A sweep can reclaim most of a family's charge after its publication
        // watermark advanced. Rebase both probes to the new live charge so a
        // subsequent regrowth cannot hide below the stale high watermark.
        self.next_byte_probe = next_probe(self.retained_bytes, self.byte_probe_quantum);
        self.next_pin_probe = next_probe(self.dependency_pins, self.pin_probe_quantum);
    }
}

pub(crate) fn retention_probe_quantum(
    budget: u64,
    normal_minimum: u64,
    normal_maximum: u64,
) -> u64 {
    if budget < normal_minimum {
        // Tiny deterministic policy tests need correspondingly exact probes.
        return (budget / 64).max(1);
    }
    (budget / 128).clamp(normal_minimum, normal_maximum)
}

pub(crate) fn next_probe(current: u64, quantum: u64) -> u64 {
    current
        .checked_div(quantum)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .saturating_mul(quantum)
}

/// Whether a strict family pass is already known to have no retention work.
///
/// Keep this non-generic check out of line: every query family monomorphizes
/// its enforcement path, while the convergence predicate depends only on the
/// shared numeric state. A publisher which races either load still owns the
/// strict watermark transition and therefore schedules the required pass.
#[inline(never)]
pub(crate) fn retention_already_converged(
    retained_count: &AtomicUsize,
    next_publish_sweep: &AtomicUsize,
    retention_limit: usize,
) -> bool {
    retained_count.load(Ordering::Acquire) <= retention_limit
        && next_publish_sweep.load(Ordering::Acquire) <= retention_limit.saturating_add(1)
}

pub(crate) fn evict_one_from_family<K, V>(
    core: &Arc<RuntimeCore>,
    family: &Arc<FamilyInner<K, V>>,
) -> bool
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    let mut retention = lock(&family.retention);
    let mut remaining = retention.entries.len();
    while remaining > 0 {
        remaining -= 1;
        let entry = retention
            .entries
            .pop_front()
            .expect("retention scan is nonempty");
        core.metrics
            .retention_scan_entries
            .fetch_add(1, Ordering::Relaxed);
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
            AttemptState::Terminal {
                terminal, waiters, ..
            } => {
                *waiters > 0
                    || terminal.pins.load(Ordering::Acquire) > 0
                    || lock(&family.retained_revisions).contains_key(&terminal.revision)
            }
        };
        if protected {
            drop(state);
            retention.entries.push_back(entry);
            continue;
        }
        let removed = state
            .remove_attempt(index)
            .expect("retention selected an existing attempt");
        let (terminal, handoffs) = match removed.state {
            AttemptState::Terminal {
                terminal, handoffs, ..
            } => (terminal, handoffs),
            AttemptState::Computing { .. } => unreachable!(),
        };
        let empty = state.attempts.is_empty();
        drop(state);
        core.metrics.evictions.fetch_add(1, Ordering::Relaxed);
        core.metrics
            .retained_terminals
            .fetch_sub(1, Ordering::Relaxed);
        family.retained_count.fetch_sub(1, Ordering::Relaxed);
        retention.remove_charge(terminal.retained_charge, terminal.dependency_pin_charge);
        if empty && node.users.load(Ordering::Acquire) == 0 {
            let mut nodes = family.nodes.shard(node.key());
            if node.users.load(Ordering::Acquire) == 0
                && lock(&node.state).attempts.is_empty()
                && nodes
                    .get(node.key())
                    .is_some_and(|candidate| Arc::ptr_eq(candidate, &node))
            {
                nodes.remove(node.key());
                family.retained_nodes.fetch_sub(1, Ordering::Relaxed);
            }
        }
        drop(retention);
        handoffs.abort();
        return true;
    }
    false
}

pub(crate) fn family_charge_snapshot<K, V>(family: &FamilyInner<K, V>) -> FamilyChargeSnapshot
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    let retention = lock(&family.retention);
    FamilyChargeSnapshot {
        retained_bytes: retention.retained_bytes,
        dependency_pins: retention.dependency_pins,
    }
}

/// An explicit retained terminal root.
pub struct TerminalPin<K: QueryKey, V: Clone + Send + Sync + 'static> {
    pub(crate) family: QueryFamily<K, V>,
    pub(crate) terminal: Arc<QueryTerminal<V>>,
    /// When set, `Drop` performs neither the pin decrement nor the per-pin
    /// `enforce_retention`: the batched teardown path (`release_deferred`) has
    /// already decremented this pin and folded the owning family's single
    /// enforcement pass into a deduplicated [`FamilyEnforcer`]. False for every
    /// ordinary per-pin user (session pins, attempt/result leases, test pins),
    /// whose `Drop` semantics are unchanged.
    pub(crate) deferred: AtomicBool,
}

impl<K, V> TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// The immutable terminal protected by this root.
    pub fn terminal(&self) -> &Arc<QueryTerminal<V>> {
        &self.terminal
    }
}

/// A terminal cannot be pinned by a different family or runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinError {
    /// The terminal's unforgeable family token does not match.
    ForeignFamily,
}

/// A final-terminal cone could not be proven complete from the current task's
/// live registered-query observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainTerminalConeError {
    /// The caller did not establish a lexical registered-validation authority.
    NoRegisteredValidationScope,
    /// The proposed root was not observed by this task.
    RootNotObserved,
    /// A fallback lease universe belongs to another query runtime.
    ForeignRuntime,
    /// One immutable edge in the proposed root's transitive cone had no
    /// matching live task lease.
    DependencyNotObserved(Observation),
}

/// Errors from minting or recording an exact-terminal adoption capability
/// ([`QueryFamily::adoptable_terminal`] /
/// [`QueryFamily::observe_adopted_terminal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptTerminalError {
    /// The terminal's unforgeable family token does not match this family.
    ForeignFamily,
    /// The computing task belongs to a different runtime than this family.
    ForeignRuntime,
    /// The family is not registered content-addressed, so it has no authority
    /// to mint adoption capabilities: an input-dependent family endorsing a
    /// held value input-free at another revision could validate a stale
    /// result green.
    NotContentAddressed,
    /// The terminal (or its node) is no longer retained: a stale or evicted
    /// terminal is rejected, never silently re-derived.
    Evicted,
}

/// The exact-terminal adoption capability: a terminal of a family whose
/// CONTENT-ADDRESSED registration is the sole minting authority
/// ([`QueryFamily::adoptable_terminal`]). Holding one proves the family
/// asserted its key alone pins the terminal's value, which is what makes an
/// input-free endorsement at another revision sound.
#[derive(Debug, Clone)]
pub struct AdoptableTerminal<V> {
    pub(crate) terminal: Arc<QueryTerminal<V>>,
}

impl<V> AdoptableTerminal<V> {
    /// The held terminal.
    pub fn terminal(&self) -> &Arc<QueryTerminal<V>> {
        &self.terminal
    }
}

impl<K, V> Drop for TerminalPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        // Batched teardown (`release_deferred`) already decremented this pin and
        // deferred the family's single enforcement pass; do nothing further here,
        // else the pin would be double-decremented and the linearity lost.
        if self.deferred.load(Ordering::Relaxed) {
            return;
        }
        let previous = self.terminal.pins.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "a terminal pin releases exactly once");
        if previous == 1 {
            // Only the last pin can make this terminal newly evictable. A
            // duplicate/root-overlap release cannot enable retention progress,
            // so scanning the full family here would be pure quadratic work.
            self.family.enforce_retention();
            self.family.core.enforce_runtime_retention();
        }
    }
}

/// An explicit current/last-good revision root.
pub struct RevisionPin<K: QueryKey, V: Clone + Send + Sync + 'static> {
    pub(crate) family: QueryFamily<K, V>,
    pub(crate) revision: Revision,
    pub(crate) view: Option<RevisionLease>,
}

impl<K, V> Drop for RevisionPin<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let mut revisions = lock(&self.family.inner.retained_revisions);
        let count = revisions
            .get_mut(&self.revision)
            .expect("revision pin owns a retained root");
        *count -= 1;
        let released = *count == 0;
        if released {
            revisions.remove(&self.revision);
        }
        drop(revisions);
        if released {
            self.family.enforce_retention();
            self.family.core.enforce_runtime_retention();
        }
        // The revision-view lease drops after terminal retention bookkeeping.
        let _ = &self.view;
    }
}

/// Request/session publication over immutable terminal attempts.
///
/// This deliberately lives above memo nodes. Selecting a failed current
/// attempt preserves the preceding successful terminal as last-good.
pub struct QuerySelection<K: QueryKey, V: Clone + Send + Sync + 'static> {
    pub(crate) family: QueryFamily<K, V>,
    pub(crate) current: Option<TerminalPin<K, V>>,
    pub(crate) last_good: Option<TerminalPin<K, V>>,
}

impl<K, V> QuerySelection<K, V>
where
    K: QueryKey,
    V: Clone + Send + Sync + 'static,
{
    /// Publishes one immutable attempt as the request's current result.
    pub fn publish(&mut self, terminal: &Arc<QueryTerminal<V>>) -> Result<(), PinError> {
        let current = self.family.pin_terminal(terminal)?;
        if terminal.kind() == QueryTerminalKind::Success {
            self.last_good = Some(self.family.pin_terminal(terminal)?);
        }
        self.current = Some(current);
        Ok(())
    }

    /// Current selected attempt, including a deterministic failure.
    pub fn current(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.current.as_ref().map(TerminalPin::terminal)
    }

    /// Most recently selected successful attempt.
    pub fn last_good(&self) -> Option<&Arc<QueryTerminal<V>>> {
        self.last_good.as_ref().map(TerminalPin::terminal)
    }

    /// Clears request-current publication after a non-terminal abort while
    /// preserving the independently pinned last-good success.
    pub fn clear_current(&mut self) {
        self.current = None;
    }
}
