//! Compatibility traits for revisioned query-family records and lifecycle views.
//!
//! Storage and dependency authority live in `RevisionedQueryDatabase`. This
//! module deliberately contains no selected-state cache or peer graph.

use std::sync::Arc;

use crate::session::{AttemptId, QueryStructuralWork};

/// Bounded revisioned-family retention used by the canonical runtime.
pub(crate) const QUERY_TERMINAL_RETENTION_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    Success,
    Failure,
}

pub(crate) trait TypedQueryFamily: std::fmt::Debug + Send + Sync {
    type Key: Eq + std::hash::Hash + Clone + std::fmt::Debug + Send + Sync;
    type Record: std::fmt::Debug + Clone + Send + Sync;
    const MAX_TERMINALS: usize;

    #[allow(dead_code)]
    fn key(record: &Self::Record) -> &Self::Key;
    fn terminal_kind(record: &Self::Record) -> TerminalKind;
    fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool;
    fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool;
    fn diagnostics(record: &Self::Record) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        let _ = record;
        None
    }
    fn record_is_consistent(record: &Self::Record) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptExecution {
    Computed,
    Reused,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbortedQueryReason {
    Canceled,
    DependencyCycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptOutcomeKind {
    Success,
    Failure,
    Aborted(AbortedQueryReason),
}

/// Runtime-native observations are retained without allocating compatibility
/// graph nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeObservation {
    Dependency(rue_query::Observation),
    Input(rue_query::InputObservation),
}

/// Immutable lifecycle view used by metrics and the diagnostic attempt index.
pub(crate) trait AttemptView: std::fmt::Debug + Send + Sync {
    fn id(&self) -> AttemptId;
    fn execution(&self) -> AttemptExecution;
    fn outcome(&self) -> AttemptOutcomeKind;
    fn origin_id(&self) -> AttemptId;
    fn runtime_observations(&self) -> &[RuntimeObservation] {
        &[]
    }
    fn runtime_work(&self) -> &[(Arc<str>, u64)] {
        &[]
    }
    fn work(&self) -> &QueryStructuralWork;
    fn diagnostics(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>>;
}
