//! Shared typed storage for immutable query terminal attempts.
//!
//! The store owns lookup, insertion validation, and deterministic retention.
//! `CompilerSession` remains responsible for deciding when and how a query is
//! computed; this component never calls compiler phases.

use std::collections::VecDeque;
use std::marker::PhantomData;

/// Deterministic FIFO retention for the first migrated query families.
///
/// These records are revision-local and are also cleared when the current
/// source publication changes. The bound prevents option variants and failed
/// attempts from creating an unbounded session-owned history within a revision.
pub(crate) const QUERY_TERMINAL_RETENTION_LIMIT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    Success,
    Failure,
}

/// Session-local publication identity for either kind of terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalStamp {
    Success(u64),
    Failure(u64),
}

pub(crate) trait TypedQueryFamily {
    type Key: Eq;
    type Record: std::fmt::Debug;
    const MAX_TERMINALS: usize;

    fn key(record: &Self::Record) -> &Self::Key;
    fn terminal_kind(record: &Self::Record) -> TerminalKind;
    fn record_is_consistent(record: &Self::Record) -> bool;
}

/// A family-owned secondary index predicate with a statically complete key.
///
/// This supports deliberate cross-variant reuse without exposing records or an
/// arbitrary predicate to `CompilerSession`.
pub(crate) trait TypedSecondaryLookupFamily: TypedQueryFamily {
    type SecondaryKey: Eq;

    fn matches_secondary(record: &Self::Record, key: &Self::SecondaryKey) -> bool;
}

#[derive(Debug)]
struct TerminalAttempt<F: TypedQueryFamily> {
    stamp: TerminalStamp,
    record: F::Record,
}

impl<F: TypedQueryFamily> TerminalAttempt<F> {
    fn record(&self) -> &F::Record {
        debug_assert_eq!(
            matches!(self.stamp, TerminalStamp::Success(_)),
            matches!(F::terminal_kind(&self.record), TerminalKind::Success),
            "terminal stamp kind must match its immutable record"
        );
        &self.record
    }
}

/// Bounded memo table for one statically typed query family.
#[derive(Debug)]
pub(crate) struct TypedQueryStore<F: TypedQueryFamily> {
    attempts: VecDeque<TerminalAttempt<F>>,
    next_publication: u64,
    evictions: usize,
    family: PhantomData<fn() -> F>,
}

impl<F: TypedQueryFamily> Default for TypedQueryStore<F> {
    fn default() -> Self {
        Self {
            attempts: VecDeque::new(),
            next_publication: 0,
            evictions: 0,
            family: PhantomData,
        }
    }
}

impl<F: TypedQueryFamily> TypedQueryStore<F> {
    pub(crate) fn get(&self, key: &F::Key) -> Option<&F::Record> {
        self.attempts
            .iter()
            .find(|attempt| F::key(attempt.record()) == key)
            .map(TerminalAttempt::record)
    }

    pub(crate) fn insert(&mut self, record: F::Record) -> TerminalStamp {
        assert!(
            F::record_is_consistent(&record),
            "typed query record key does not match its terminal artifact revision"
        );
        assert!(
            self.get(F::key(&record)).is_none(),
            "retained typed query keys are unique"
        );
        let publication = self.next_publication;
        self.next_publication = self.next_publication.wrapping_add(1);
        let stamp = match F::terminal_kind(&record) {
            TerminalKind::Success => TerminalStamp::Success(publication),
            TerminalKind::Failure => TerminalStamp::Failure(publication),
        };
        self.attempts.push_back(TerminalAttempt { stamp, record });
        while self.attempts.len() > F::MAX_TERMINALS {
            self.attempts.pop_front();
            self.evictions += 1;
        }
        stamp
    }

    pub(crate) fn len(&self) -> usize {
        self.attempts.len()
    }

    pub(crate) fn clear(&mut self) {
        self.attempts.clear();
    }

    #[cfg(test)]
    pub(crate) fn stamp(&self, key: &F::Key) -> Option<TerminalStamp> {
        self.attempts
            .iter()
            .find(|attempt| F::key(&attempt.record) == key)
            .map(|attempt| attempt.stamp)
    }

    pub(crate) fn evictions(&self) -> usize {
        self.evictions
    }
}

impl<F: TypedSecondaryLookupFamily> TypedQueryStore<F> {
    pub(crate) fn get_secondary(&self, key: &F::SecondaryKey) -> Option<&F::Record> {
        self.attempts
            .iter()
            .map(TerminalAttempt::record)
            .find(|record| F::matches_secondary(record, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestRecord {
        key: u32,
        artifact_revision: u32,
        terminal: TerminalKind,
    }

    #[derive(Debug)]
    struct TestFamily;

    impl TypedQueryFamily for TestFamily {
        type Key = u32;
        type Record = TestRecord;
        const MAX_TERMINALS: usize = QUERY_TERMINAL_RETENTION_LIMIT;

        fn key(record: &Self::Record) -> &Self::Key {
            &record.key
        }

        fn terminal_kind(record: &Self::Record) -> TerminalKind {
            record.terminal
        }

        fn record_is_consistent(record: &Self::Record) -> bool {
            record.key == record.artifact_revision
        }
    }

    fn record(key: u32, terminal: TerminalKind) -> TestRecord {
        TestRecord {
            key,
            artifact_revision: key,
            terminal,
        }
    }

    #[test]
    fn success_and_failure_receive_typed_terminal_stamps() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        assert_eq!(
            store.insert(record(1, TerminalKind::Success)),
            TerminalStamp::Success(0)
        );
        assert_eq!(
            store.insert(record(2, TerminalKind::Failure)),
            TerminalStamp::Failure(1)
        );
        assert_eq!(store.stamp(&1), Some(TerminalStamp::Success(0)));
        assert_eq!(store.stamp(&2), Some(TerminalStamp::Failure(1)));
    }

    #[test]
    fn retention_evicts_oldest_terminal_attempt() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        for key in 0..=QUERY_TERMINAL_RETENTION_LIMIT as u32 {
            store.insert(record(key, TerminalKind::Success));
        }
        assert!(store.get(&0).is_none());
        assert!(store.get(&1).is_some());
        assert_eq!(store.len(), QUERY_TERMINAL_RETENTION_LIMIT);
        assert_eq!(store.evictions(), 1);
    }

    #[test]
    #[should_panic(expected = "typed query record key does not match")]
    fn insertion_rejects_artifact_from_another_revision() {
        let mut store = TypedQueryStore::<TestFamily>::default();
        store.insert(TestRecord {
            key: 1,
            artifact_revision: 2,
            terminal: TerminalKind::Success,
        });
    }
}
