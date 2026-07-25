//! Accounting for the request-scoped immutable declaration base (RUE-1135).
//!
//! The base changes what the ordinary per-body declaration-context counters
//! describe: prepare, project, install, and endpoints now run once per request
//! instead of once per body, so those counters fall by construction. This meter
//! is the other half of that statement — it records how many bases were built,
//! how many bodies were served from one, and what each derivation shared versus
//! copied, so the drop is accounted for rather than merely observed.
//!
//! Every count is charged at the operation it measures: a base build, a body
//! derivation, or a unit read out of a derived container. None is computed from
//! an input slice length, so a change that shares less (or copies more) moves
//! these numbers instead of hiding inside a stage that no longer runs.

use std::sync::atomic::{AtomicU64, Ordering};

/// An owned snapshot of the declaration-base meter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclarationBaseMetrics {
    /// Declaration bases built from scratch. One per rooted attempt is the
    /// intended result.
    pub bases_built: u64,
    /// Bases rebuilt because a body's epoch inputs — the materialized anonymous
    /// nominals and the well-known `Option` registry, both installed *into* the
    /// epoch — differed from the retained base's. A non-zero value bounds how
    /// much of a program one per-request base actually serves.
    pub base_input_misses: u64,
    /// Body-local epochs derived from a base.
    pub epochs_derived: u64,
    /// Units a derivation read from the base without copying them.
    pub units_shared: u64,
    /// Units a derivation genuinely copied: the per-body owned state.
    pub units_copied: u64,
    /// Bodies whose two arms were compared under the differential mode.
    pub parity_comparisons: u64,
    /// Bodies whose independently built epoch and base-derived epoch published
    /// different transactions. Must be zero: a non-zero value means sharing the
    /// base is not equivalent to building one per body.
    pub parity_mismatches: u64,
}

/// The session-held declaration-base meter. Lives behind an `Arc` like the
/// recipe-cache, overlay, and clone-probe meters so a request-scoped base can
/// charge it without borrowing the session.
#[derive(Debug, Default)]
pub(crate) struct DeclarationBaseMeter {
    bases_built: AtomicU64,
    base_input_misses: AtomicU64,
    epochs_derived: AtomicU64,
    units_shared: AtomicU64,
    units_copied: AtomicU64,
    parity_comparisons: AtomicU64,
    parity_mismatches: AtomicU64,
}

impl DeclarationBaseMeter {
    pub(crate) fn snapshot(&self) -> DeclarationBaseMetrics {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        DeclarationBaseMetrics {
            bases_built: load(&self.bases_built),
            base_input_misses: load(&self.base_input_misses),
            epochs_derived: load(&self.epochs_derived),
            units_shared: load(&self.units_shared),
            units_copied: load(&self.units_copied),
            parity_comparisons: load(&self.parity_comparisons),
            parity_mismatches: load(&self.parity_mismatches),
        }
    }

    /// A base was derived from scratch. `replaced` distinguishes the first build
    /// of a request from a rebuild forced by differing per-body epoch inputs.
    pub(crate) fn record_base_built(&self, replaced: bool) {
        self.bases_built.fetch_add(1, Ordering::Relaxed);
        if replaced {
            self.base_input_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One body-local epoch was derived from a base.
    pub(crate) fn record_epoch_derived(&self, units: &rue_air::EpochDerivationUnits) {
        self.epochs_derived.fetch_add(1, Ordering::Relaxed);
        self.units_shared
            .fetch_add(units.total_units_shared(), Ordering::Relaxed);
        self.units_copied
            .fetch_add(units.body_local_entries_copied, Ordering::Relaxed);
    }

    /// One differential comparison completed.
    pub(crate) fn record_parity(&self, equal: bool) {
        self.parity_comparisons.fetch_add(1, Ordering::Relaxed);
        if !equal {
            self.parity_mismatches.fetch_add(1, Ordering::Relaxed);
        }
    }
}
