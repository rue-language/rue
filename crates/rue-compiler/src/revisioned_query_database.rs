//! Phase 1 compatibility layer over the canonical revisioned query runtime.
//!
//! This module deliberately preserves the existing compiler family's typed
//! record shape while moving key identity, execution, immutable attempts,
//! dependency recording, and current/last-good publication into `rue-query`.
//! It is a migration boundary, not a peer database. RUE-1033 / ADR-0063 Phase
//! 12 deletes this selected-state-shaped shim after every family calls the
//! runtime directly.

use std::collections::VecDeque;
use std::sync::Arc;

use rue_query::{
    CancellationToken, InputIdentity, QueryAbort, QueryFamily, QueryKey, QueryOutput,
    QueryRequestAttempt, QueryRuntime, QuerySelection, QueryTerminalKind, RequestExecution,
    Revision,
};

use crate::session::{AttemptId, QueryStructuralWork};
use crate::typed_query_store::{
    AbortedQueryReason, AttemptExecution as CompilerAttemptExecution, AttemptOutcomeKind,
    AttemptView, RuntimeObservation,
};
use crate::typed_query_store::{TerminalKind, TypedQueryFamily};

#[derive(Debug, Clone)]
pub(crate) struct CompatibilityKey<K> {
    key: K,
}

impl<K: PartialEq> PartialEq for CompatibilityKey<K> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<K: Eq> Eq for CompatibilityKey<K> {}

impl<K> QueryKey for CompatibilityKey<K>
where
    K: Clone + Eq + Send + Sync + 'static,
{
    fn stable_identity(&self) -> String {
        // Display only. Exact K equality chooses the memo node and the runtime
        // incarnation makes cycle/wait identity collision-safe.
        "selected-key".to_owned()
    }
}

fn record_equal<F: TypedQueryFamily>(left: &F::Record, right: &F::Record) -> bool {
    F::terminal_kind(left) == F::terminal_kind(right)
        && F::outcome_equal(left, right)
        && F::diagnostics_equal(left, right)
}

pub(crate) struct RevisionedFamily<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    runtime: QueryRuntime,
    family: QueryFamily<CompatibilityKey<F::Key>, F::Record>,
    selection: QuerySelection<CompatibilityKey<F::Key>, F::Record>,
}

impl<F> std::fmt::Debug for RevisionedFamily<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevisionedFamily")
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

impl<F> RevisionedFamily<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    pub(crate) fn new(runtime: &QueryRuntime, name: &'static str) -> Self {
        let family = runtime
            .family_with_equality(name, F::MAX_TERMINALS, record_equal::<F>)
            .expect("compiler query families have unique stable names");
        let selection = family.selection();
        Self {
            runtime: runtime.clone(),
            family,
            selection,
        }
    }

    fn key(&mut self, key: F::Key) -> CompatibilityKey<F::Key> {
        CompatibilityKey { key }
    }

    pub(crate) fn prepare(&mut self, key: F::Key) -> PreparedRevisionedQuery<F> {
        PreparedRevisionedQuery {
            runtime: self.runtime.clone(),
            family: self.family.clone(),
            key: self.key(key),
        }
    }

    pub(crate) fn select(&mut self, attempt: &QueryRequestAttempt<F::Record>) {
        if attempt.execution() == RequestExecution::Aborted {
            self.selection.clear_current();
        }
        if let Some(terminal) = attempt.terminal() {
            self.selection
                .publish(terminal)
                .expect("selected terminal belongs to its compiler family");
        }
    }

    #[cfg(test)]
    pub(crate) fn request(
        &mut self,
        revision: Revision,
        key: F::Key,
        compute: impl FnOnce(&rue_query::QueryContext) -> Result<F::Record, QueryAbort>,
    ) -> Arc<QueryRequestAttempt<F::Record>> {
        let key = self.key(key);
        let attempt = Arc::new(self.runtime.request(
            &self.family,
            revision,
            key,
            CancellationToken::new(),
            |context| {
                let record = compute(context)?;
                assert!(
                    F::record_is_consistent(&record),
                    "typed query record key does not match its terminal artifact revision"
                );
                let kind = match F::terminal_kind(&record) {
                    TerminalKind::Success => QueryTerminalKind::Success,
                    TerminalKind::Failure => QueryTerminalKind::Failure,
                };
                Ok(QueryOutput::success(record).with_terminal_kind(kind))
            },
        ));
        self.select(&attempt);
        attempt
    }

    pub(crate) fn attempt_view(
        &mut self,
        id: AttemptId,
        attempt: Arc<QueryRequestAttempt<F::Record>>,
        work: QueryStructuralWork,
    ) -> Arc<dyn AttemptView> {
        let origin = AttemptId(attempt.origin_request_id());
        let runtime_observations = attempt
            .dependencies()
            .iter()
            .cloned()
            .map(RuntimeObservation::Dependency)
            .chain(
                attempt
                    .inputs()
                    .iter()
                    .cloned()
                    .map(RuntimeObservation::Input),
            )
            .collect::<Vec<_>>()
            .into();
        let runtime_work = attempt.work().to_vec().into();
        Arc::new(RuntimeAttemptView::<F> {
            id,
            origin,
            attempt,
            work,
            runtime_observations,
            runtime_work,
        })
    }

    #[cfg(test)]
    pub(crate) fn current_record(&self) -> Option<&F::Record> {
        let terminal = self.selection.current()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => Some(record),
            rue_query::QueryOutcome::Failure(_) => unreachable!("compiler families retain records"),
        }
    }

    pub(crate) fn last_good_record(&self) -> Option<&F::Record> {
        let terminal = self.selection.last_good()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => Some(record),
            rue_query::QueryOutcome::Failure(_) => unreachable!("compiler families retain records"),
        }
    }

    pub(crate) fn retention(&self) -> rue_query::FamilyRetention {
        self.family.retention()
    }

    pub(crate) fn protected_count(&self) -> usize {
        match (self.selection.current(), self.selection.last_good()) {
            (Some(current), Some(last_good)) if Arc::ptr_eq(current, last_good) => 1,
            (Some(_), Some(_)) => 2,
            (Some(_), None) | (None, Some(_)) => 1,
            (None, None) => 0,
        }
    }

    pub(crate) fn origin_attempt_ids(&self) -> impl Iterator<Item = AttemptId> + '_ {
        let mut origins = self
            .family
            .retained_origin_request_ids()
            .into_iter()
            .map(AttemptId)
            .collect::<std::collections::BTreeSet<_>>();
        origins.extend(
            [self.selection.current(), self.selection.last_good()]
                .into_iter()
                .flatten()
                .map(|terminal| AttemptId(terminal.origin_request_id())),
        );
        origins.into_iter()
    }

    pub(crate) fn retained_aborted_len(&self) -> usize {
        // Runtime aborts are owned by the diagnostic/metrics attempt index;
        // this family retains no separate aborted-attempt history.
        0
    }

    fn any_retained_key(&self, predicate: impl FnMut(&F::Key) -> bool) -> bool {
        let mut predicate = predicate;
        self.family.any_retained_key(|key| predicate(&key.key))
    }
}

pub(crate) struct PreparedRevisionedQuery<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    runtime: QueryRuntime,
    family: QueryFamily<CompatibilityKey<F::Key>, F::Record>,
    key: CompatibilityKey<F::Key>,
}

impl<F> PreparedRevisionedQuery<F>
where
    F: TypedQueryFamily + 'static,
    F::Key: 'static,
    F::Record: 'static,
{
    pub(crate) fn execute(
        self,
        revision: Revision,
        origin: AttemptId,
        compute: impl FnOnce(&rue_query::QueryContext) -> Result<F::Record, QueryAbort>,
    ) -> Arc<QueryRequestAttempt<F::Record>> {
        Arc::new(self.runtime.request_with_origin(
            &self.family,
            revision,
            self.key,
            CancellationToken::new(),
            Some(origin.0),
            |context| {
                let record = compute(context)?;
                assert!(F::record_is_consistent(&record));
                let kind = match F::terminal_kind(&record) {
                    TerminalKind::Success => QueryTerminalKind::Success,
                    TerminalKind::Failure => QueryTerminalKind::Failure,
                };
                Ok(QueryOutput::success(record).with_terminal_kind(kind))
            },
        ))
    }
}

#[derive(Debug)]
struct RuntimeAttemptView<F: TypedQueryFamily> {
    id: AttemptId,
    origin: AttemptId,
    attempt: Arc<QueryRequestAttempt<F::Record>>,
    work: QueryStructuralWork,
    runtime_observations: Arc<[RuntimeObservation]>,
    runtime_work: Arc<[(Arc<str>, u64)]>,
}

impl<F> AttemptView for RuntimeAttemptView<F>
where
    F: TypedQueryFamily + 'static,
    F::Record: 'static,
{
    fn id(&self) -> AttemptId {
        self.id
    }

    fn execution(&self) -> CompilerAttemptExecution {
        match self.attempt.execution() {
            RequestExecution::Computed => CompilerAttemptExecution::Computed,
            RequestExecution::Reused | RequestExecution::Joined => CompilerAttemptExecution::Reused,
            RequestExecution::Aborted => CompilerAttemptExecution::Rejected,
        }
    }

    fn outcome(&self) -> AttemptOutcomeKind {
        if let Some(terminal) = self.attempt.terminal() {
            return match terminal.kind() {
                QueryTerminalKind::Success => AttemptOutcomeKind::Success,
                QueryTerminalKind::Failure => AttemptOutcomeKind::Failure,
            };
        }
        let reason = match self.attempt.abort() {
            Some(QueryAbort::Cycle(_)) => AbortedQueryReason::DependencyCycle,
            Some(QueryAbort::Canceled) => AbortedQueryReason::Canceled,
            Some(
                QueryAbort::ForeignRuntime
                | QueryAbort::MissingInput(_)
                | QueryAbort::UnpublishedRevision(_),
            )
            | None => AbortedQueryReason::Canceled,
        };
        AttemptOutcomeKind::Aborted(reason)
    }

    fn origin_id(&self) -> AttemptId {
        self.origin
    }

    fn dependencies(&self) -> &[crate::query_graph::ObservedDependency] {
        &[]
    }

    fn runtime_observations(&self) -> &[RuntimeObservation] {
        &self.runtime_observations
    }

    fn runtime_work(&self) -> &[(Arc<str>, u64)] {
        &self.runtime_work
    }

    fn work(&self) -> &QueryStructuralWork {
        if matches!(self.attempt.execution(), RequestExecution::Computed) {
            &self.work
        } else {
            static NONE: QueryStructuralWork = QueryStructuralWork::None;
            &NONE
        }
    }

    fn diagnostics(&self) -> Option<&Arc<crate::FrontendDiagnosticSnapshot>> {
        let terminal = self.attempt.terminal()?;
        match terminal.outcome() {
            rue_query::QueryOutcome::Success(record) => F::diagnostics(record),
            rue_query::QueryOutcome::Failure(_) => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct RevisionedQueryDatabase {
    runtime: QueryRuntime,
    next_revision: u64,
    next_source_stamp: u64,
    source_stamps: VecDeque<(super::session::ExactSourceInput, u64)>,
    pub(crate) parse: RevisionedFamily<super::session::ParseQuery>,
}

impl Default for RevisionedQueryDatabase {
    fn default() -> Self {
        let runtime = QueryRuntime::new(1);
        Self {
            parse: RevisionedFamily::new(&runtime, "compiler.parse"),
            runtime,
            next_revision: 1,
            next_source_stamp: 1,
            source_stamps: VecDeque::new(),
        }
    }
}

impl RevisionedQueryDatabase {
    pub(crate) const SOURCE_INPUT: &'static str = "selected-source";

    pub(crate) fn source_revision(
        &mut self,
        source: &super::session::ExactSourceInput,
    ) -> Revision {
        // The parse family is allocated with the shared runtime now so callers
        // can migrate without creating a peer executor.
        let _parse_migration_family = &self.parse;
        let stamp = self
            .source_stamps
            .iter()
            .find_map(|(candidate, stamp)| (candidate == source).then_some(*stamp))
            .unwrap_or_else(|| {
                let stamp = self.next_source_stamp;
                self.next_source_stamp += 1;
                self.source_stamps.push_back((source.clone(), stamp));
                stamp
            });
        let revision = Revision::new(self.next_revision, 1);
        self.next_revision += 1;
        self.runtime
            .publish_revision(
                revision,
                [(InputIdentity::new(Self::SOURCE_INPUT, "current"), stamp)],
            )
            .expect("compiler input revisions are immutable and uniquely numbered");
        revision
    }

    pub(crate) fn select_parse(
        &mut self,
        attempt: &QueryRequestAttempt<super::session::ParseQueryRecord>,
    ) {
        self.parse.select(attempt);
        // Exact source stamps live exactly as long as a parse memo key (or the
        // current request before selection). They are never independently FIFO
        // evicted while a terminal can still observe the stamp.
        self.source_stamps
            .retain(|(source, _)| self.parse.any_retained_key(|key| key.source() == source));
        debug_assert!(self.source_stamps.len() <= self.parse.retention().memo_nodes);
    }

    pub(crate) fn parse_retention(&self) -> crate::typed_query_store::QueryStoreRetention {
        let retention = self.parse.retention();
        crate::typed_query_store::QueryStoreRetention {
            retained: retention.terminals,
            protected: self.parse.protected_count(),
            pinned: 0,
            tombstones: 0,
            evictions: self.runtime.metrics().evictions as usize,
        }
    }
}

#[cfg(test)]
pub(crate) fn execution(attempt: &QueryRequestAttempt<impl Sized>) -> RequestExecution {
    attempt.execution()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Key(&'static str);

    #[derive(Debug, Clone)]
    struct Record {
        key: Key,
        value: u64,
        diagnostic_payload: u64,
        failed: bool,
    }

    #[derive(Debug)]
    struct Family;

    impl TypedQueryFamily for Family {
        type Key = Key;
        type Record = Record;
        const MAX_TERMINALS: usize = 4;

        fn key(record: &Self::Record) -> &Self::Key {
            &record.key
        }

        fn terminal_kind(record: &Self::Record) -> TerminalKind {
            if record.failed {
                TerminalKind::Failure
            } else {
                TerminalKind::Success
            }
        }

        fn outcome_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.value == right.value
        }

        fn diagnostics_equal(left: &Self::Record, right: &Self::Record) -> bool {
            left.diagnostic_payload == right.diagnostic_payload
        }

        fn record_is_consistent(record: &Self::Record) -> bool {
            !record.key.0.is_empty()
        }
    }

    #[test]
    fn selected_state_shim_uses_runtime_attempts_and_last_good_publication() {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("test", "leaf");
        let first_revision = Revision::new(1, 1);
        let second_revision = Revision::new(2, 2);
        runtime
            .publish_revision(first_revision, [(input.clone(), 1)])
            .unwrap();
        runtime
            .publish_revision(second_revision, [(input.clone(), 2)])
            .unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.test-family");

        let computed = family.request(first_revision, Key("key"), |context| {
            context.input(input.clone())?;
            Ok(Record {
                key: Key("key"),
                value: 7,
                diagnostic_payload: 11,
                failed: false,
            })
        });
        assert_eq!(execution(&computed), RequestExecution::Computed);
        assert_eq!(computed.inputs().len(), 1);

        let reused = family.request(first_revision, Key("key"), |_| {
            panic!("the exact keyed terminal must be runtime-reused")
        });
        assert_eq!(execution(&reused), RequestExecution::Reused);
        assert!(reused.work().is_empty());

        let failed = family.request(second_revision, Key("key"), |context| {
            context.input(input)?;
            Ok(Record {
                key: Key("key"),
                value: 9,
                diagnostic_payload: 12,
                failed: true,
            })
        });
        assert_eq!(
            failed.terminal().unwrap().kind(),
            QueryTerminalKind::Failure
        );
        assert!(family.current_record().unwrap().failed);
        assert_eq!(family.last_good_record().unwrap().value, 7);

        let aborted = family.request(second_revision, Key("abort"), |_| Err(QueryAbort::Canceled));
        assert_eq!(execution(&aborted), RequestExecution::Aborted);
        assert!(family.current_record().is_none());
        assert_eq!(family.last_good_record().unwrap().value, 7);

        let recovered = family.request(second_revision, Key("recovered"), |context| {
            context.input(InputIdentity::new("test", "leaf"))?;
            Ok(Record {
                key: Key("recovered"),
                value: 10,
                diagnostic_payload: 13,
                failed: false,
            })
        });
        assert_eq!(execution(&recovered), RequestExecution::Computed);
        assert_eq!(family.current_record().unwrap().value, 10);
        assert_eq!(family.last_good_record().unwrap().value, 10);
        assert_eq!(family.retention().memo_nodes, 2);
    }

    #[test]
    fn aborted_attempt_view_projects_runtime_work_without_forging_typed_work() {
        let runtime = QueryRuntime::new(1);
        let input = InputIdentity::new("test", "prefix");
        let revision = Revision::new(10, 1);
        runtime
            .publish_revision(revision, [(input.clone(), 3)])
            .unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.abort-prefix");
        let prepared = family.prepare(Key("prefix"));
        let attempt = prepared.execute(revision, AttemptId(77), |context| {
            context.input(input)?;
            context.record_work(rue_query::WorkItem::new("runtime-prefix", 2));
            Err(QueryAbort::Canceled)
        });
        family.select(&attempt);
        let structural = QueryStructuralWork::Parse(crate::ParsedModulesWork {
            modules_considered: 1,
            ..crate::ParsedModulesWork::default()
        });
        let view = family.attempt_view(AttemptId(77), attempt.clone(), structural.clone());
        assert_eq!(view.origin_id(), AttemptId(77));
        assert_eq!(
            view.outcome(),
            AttemptOutcomeKind::Aborted(AbortedQueryReason::Canceled)
        );
        assert!(view.dependencies().is_empty());
        assert_eq!(view.runtime_observations().len(), 1);
        assert!(matches!(
            &view.runtime_observations()[0],
            RuntimeObservation::Input(input) if input.stamp == 3
        ));
        assert_eq!(view.work(), &QueryStructuralWork::None);
        assert_eq!(
            view.runtime_work(),
            &[(Arc::<str>::from("runtime-prefix"), 2)]
        );
        assert_eq!(attempt.work(), &[(Arc::<str>::from("runtime-prefix"), 2)]);
        assert_eq!(family.retained_aborted_len(), 0);
    }

    #[test]
    fn runtime_frozen_origin_survives_reuse_without_a_peer_registry() {
        let runtime = QueryRuntime::new(1);
        let revision = Revision::new(11, 1);
        runtime.publish_revision(revision, []).unwrap();
        let mut family = RevisionedFamily::<Family>::new(&runtime, "compiler.origin");
        let computed = family
            .prepare(Key("origin"))
            .execute(revision, AttemptId(41), |_| {
                Ok(Record {
                    key: Key("origin"),
                    value: 1,
                    diagnostic_payload: 1,
                    failed: false,
                })
            });
        family.select(&computed);
        assert_eq!(computed.origin_request_id(), 41);
        let reused = family
            .prepare(Key("origin"))
            .execute(revision, AttemptId(42), |_| {
                panic!("retained terminal must be reused")
            });
        assert_eq!(reused.execution(), RequestExecution::Reused);
        assert_eq!(reused.origin_request_id(), 41);
        let view = family.attempt_view(AttemptId(42), reused, QueryStructuralWork::None);
        assert_eq!(view.origin_id(), AttemptId(41));
        assert_eq!(
            family.origin_attempt_ids().collect::<Vec<_>>(),
            vec![AttemptId(41)]
        );
    }
}
