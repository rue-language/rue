//! Immutable diagnostic attempts and their bounded session-owned index.
//!
//! This is deliberately not a query engine. Query orchestration computes one
//! ordered diagnostic batch, freezes it in a [`FrontendDiagnosticSnapshot`],
//! and asks this component only to index or reselect that immutable attempt.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::typed_query_store::AttemptView;
use crate::{
    CompileError, CompileWarning, ImportDiscoveryContext, ImportDiscoveryPlan,
    ImportObservationLedger, ModuleId, ResolvedCodegenRevision, SourceRevision, SourceSnapshot,
};

/// Maximum number of diagnostic attempts owned by a frontend session.
///
/// Eviction is deterministic insertion order, except that the latest attempt,
/// latest successful query, and last successful semantic query are protected.
/// Those three protected entries fit within this limit. Callers can explicitly
/// pin any returned snapshot by retaining its `Arc` after session eviction.
pub const FRONTEND_DIAGNOSTIC_RETENTION_LIMIT: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum FrontendDiagnosticIdentity {
    Syntax,
    Import(ImportDiagnosticInputDescriptor),
    Merge,
    Rir(SourceRevision),
    Semantic(ResolvedCodegenRevision),
}

/// Stable diagnostic phase projection with query identity removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticStage {
    Syntax,
    Import,
    Merge,
    Rir,
    Semantic,
}

impl FrontendDiagnosticIdentity {
    fn phase(&self) -> DiagnosticStage {
        match self {
            Self::Syntax => DiagnosticStage::Syntax,
            Self::Import(_) => DiagnosticStage::Import,
            Self::Merge => DiagnosticStage::Merge,
            Self::Rir(_) => DiagnosticStage::Rir,
            Self::Semantic(_) => DiagnosticStage::Semantic,
        }
    }
}

/// Exact immutable inputs to one canonical import-diagnostic projection.
///
/// Keeping the plan and observation ledger in the descriptor makes reuse an
/// exact attempted-revision property rather than merely a source-text match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ImportDiagnosticInputDescriptor {
    pub(crate) source: SourceRevision,
    pub(crate) context: Option<ImportDiscoveryContext>,
    pub(crate) plan: Option<ImportDiscoveryPlan>,
    pub(crate) ledger: ImportObservationLedger,
    pub(crate) accepted_reads: crate::AcceptedReadManifest,
}

impl ImportDiagnosticInputDescriptor {
    #[cfg(test)]
    pub(crate) fn source_revision(&self) -> &SourceRevision {
        &self.source
    }
    #[cfg(test)]
    pub(crate) fn context(&self) -> Option<&ImportDiscoveryContext> {
        self.context.as_ref()
    }
    #[cfg(test)]
    pub(crate) fn plan(&self) -> Option<&ImportDiscoveryPlan> {
        self.plan.as_ref()
    }
    #[cfg(test)]
    pub(crate) fn ledger(&self) -> &ImportObservationLedger {
        &self.ledger
    }
    #[cfg(test)]
    pub(crate) fn accepted_read_manifest(&self) -> &crate::AcceptedReadManifest {
        &self.accepted_reads
    }
}

/// One immutable, attempt-attached diagnostic and warning batch.
#[derive(Debug, Clone)]
pub struct FrontendDiagnosticSnapshot {
    pub(crate) source: SourceSnapshot,
    pub(crate) stage: FrontendDiagnosticIdentity,
    pub(crate) provenance: DiagnosticAttemptProvenance,
    pub(crate) errors: Arc<[CompileError]>,
    pub(crate) warnings: Arc<[CompileWarning]>,
}

/// Producer inputs that can change presentation without changing the stable
/// source revision or public query stage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum DiagnosticAttemptProvenance {
    Canonical,
    Presentation(crate::shared_segments::SharedList<ModuleId>),
}

impl FrontendDiagnosticSnapshot {
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }
    pub fn source_revision(&self) -> &SourceRevision {
        self.source.source_revision()
    }
    pub fn stage(&self) -> DiagnosticStage {
        self.stage.phase()
    }
    pub(crate) fn identity(&self) -> &FrontendDiagnosticIdentity {
        &self.stage
    }
    pub fn errors(&self) -> &[CompileError] {
        &self.errors
    }
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DiagnosticRetentionMetrics {
    pub entries: usize,
    pub source_attempts: usize,
    pub source_bytes: usize,
}

#[derive(Debug, Clone)]
struct IndexedDiagnosticAttempt {
    id: u64,
    attempt: Arc<dyn AttemptView>,
}

impl IndexedDiagnosticAttempt {
    fn snapshot(&self) -> &Arc<FrontendDiagnosticSnapshot> {
        self.attempt
            .diagnostics()
            .expect("only diagnostic-bearing attempts are indexed")
    }
}

/// Bounded index and explicit selectors for immutable diagnostic attempts.
///
/// Selector IDs intentionally do not add extra strong owners. Each indexed
/// batch has exactly one store-owned history entry; producer caches can retain
/// the same origin `Arc` so reuse can restore it after index eviction, and
/// caller-owned `Arc`s remain outside the store's retention policy.
#[derive(Debug, Default, Clone)]
pub(crate) struct DiagnosticAttemptStore {
    entries: VecDeque<IndexedDiagnosticAttempt>,
    latest: Option<u64>,
    latest_successful: Option<u64>,
    last_good_semantic: Option<u64>,
}

impl DiagnosticAttemptStore {
    #[cfg(test)]
    pub(crate) fn find(
        &self,
        source: &SourceSnapshot,
        stage: &FrontendDiagnosticIdentity,
    ) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.latest
            .and_then(|latest| self.entries.iter().find(|entry| entry.id == latest))
            .filter(|entry| {
                snapshot_matches_source_attempt(entry.snapshot(), source)
                    && entry.snapshot().identity() == stage
            })
            .or_else(|| {
                self.entries.iter().rev().find(|entry| {
                    snapshot_matches_source_attempt(entry.snapshot(), source)
                        && entry.snapshot().identity() == stage
                })
            })
            .map(IndexedDiagnosticAttempt::snapshot)
    }

    pub fn find_exact(
        &self,
        source: &SourceSnapshot,
        stage: &FrontendDiagnosticIdentity,
        provenance: &DiagnosticAttemptProvenance,
    ) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.entries
            .iter()
            .rev()
            .find(|entry| {
                snapshot_matches_source_attempt(entry.snapshot(), source)
                    && entry.snapshot().identity() == stage
                    && &entry.snapshot().provenance == provenance
            })
            .map(IndexedDiagnosticAttempt::snapshot)
    }

    pub fn latest(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.selected(self.latest)
    }

    /// Test observer for the latest-successful selector. The selector itself
    /// stays production state: it protects its entry from eviction.
    #[cfg(test)]
    pub(crate) fn latest_successful(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.selected(self.latest_successful)
    }

    /// Test observer for the last-good-semantic selector. Syntax or semantic
    /// failures never replace this baseline, and the selector protects its
    /// entry from eviction.
    #[cfg(test)]
    pub(crate) fn last_good_semantic(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        self.selected(self.last_good_semantic)
    }

    /// Select a reused origin batch, restoring its bounded index entry if a
    /// caller or another attempt kept it alive beyond earlier index eviction.
    pub(crate) fn select(&mut self, attempt: Arc<dyn AttemptView>) {
        let snapshot = attempt
            .diagnostics()
            .expect("selected diagnostic attempt owns a batch");
        let id = self
            .entries
            .iter()
            .find(|entry| Arc::ptr_eq(entry.snapshot(), snapshot))
            .map(|entry| entry.id)
            .unwrap_or_else(|| self.push(attempt));
        self.select_id(id);
        self.evict();
    }

    pub(crate) fn select_snapshot(&mut self, snapshot: &Arc<FrontendDiagnosticSnapshot>) -> bool {
        let id = self
            .entries
            .iter()
            .find(|entry| Arc::ptr_eq(entry.snapshot(), snapshot))
            .map(|entry| entry.id)
            .unwrap_or_else(|| {
                use std::sync::atomic::{AtomicU64, Ordering};

                static NEXT_ID: AtomicU64 = AtomicU64::new(1 << 63);
                self.push(Arc::new(SnapshotDiagnosticAttempt {
                    id: crate::session::AttemptId(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
                    snapshot: snapshot.clone(),
                }))
            });
        self.select_id(id);
        self.evict();
        true
    }

    /// Index a newly frozen producer batch. The caller must have checked that
    /// its exact source-attempt/stage key is not already present.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, attempt: Arc<dyn AttemptView>) {
        let snapshot = attempt
            .diagnostics()
            .expect("indexed diagnostic attempt owns a batch");
        debug_assert!(
            self.find_exact(snapshot.source(), snapshot.identity(), &snapshot.provenance)
                .is_none(),
            "diagnostic attempt keys are published once"
        );
        let id = self.push(attempt);
        self.select_id(id);
        self.evict();
    }

    pub fn retention_metrics(&self) -> DiagnosticRetentionMetrics {
        let mut attempts: Vec<&SourceSnapshot> = Vec::new();
        let mut source_bytes = 0;
        for entry in &self.entries {
            let source = entry.snapshot().source();
            if attempts
                .iter()
                .any(|attempt| same_source_attempt(attempt, source))
            {
                continue;
            }
            source_bytes += source.total_source_bytes();
            attempts.push(source);
        }
        DiagnosticRetentionMetrics {
            entries: self.entries.len(),
            source_attempts: attempts.len(),
            source_bytes,
        }
    }

    fn push(&mut self, attempt: Arc<dyn AttemptView>) -> u64 {
        let id = attempt.id().0;
        self.entries
            .push_back(IndexedDiagnosticAttempt { id, attempt });
        id
    }

    fn select_id(&mut self, id: u64) {
        let snapshot = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .expect("selected diagnostic attempt is indexed")
            .snapshot();
        self.latest = Some(id);
        if snapshot.is_success() {
            self.latest_successful = Some(id);
            if matches!(snapshot.identity(), FrontendDiagnosticIdentity::Semantic(_)) {
                self.last_good_semantic = Some(id);
            }
        }
    }

    fn selected(&self, id: Option<u64>) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        let id = id?;
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(IndexedDiagnosticAttempt::snapshot)
    }

    fn evict(&mut self) {
        while self.entries.len() > FRONTEND_DIAGNOSTIC_RETENTION_LIMIT {
            let evict = self.entries.iter().position(|entry| {
                Some(entry.id) != self.latest
                    && Some(entry.id) != self.latest_successful
                    && Some(entry.id) != self.last_good_semantic
            });
            let Some(evict) = evict else {
                break;
            };
            self.entries.remove(evict);
        }
        debug_assert!(self.entries.len() <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
    }

    #[cfg(test)]
    pub(crate) fn select_test_snapshot(&mut self, snapshot: Arc<FrontendDiagnosticSnapshot>) {
        self.select_snapshot(&snapshot);
    }
}

#[derive(Debug)]
struct SnapshotDiagnosticAttempt {
    id: crate::session::AttemptId,
    snapshot: Arc<FrontendDiagnosticSnapshot>,
}

impl AttemptView for SnapshotDiagnosticAttempt {
    fn id(&self) -> crate::session::AttemptId {
        self.id
    }
    fn execution(&self) -> crate::typed_query_store::AttemptExecution {
        crate::typed_query_store::AttemptExecution::Computed
    }
    fn outcome(&self) -> crate::typed_query_store::AttemptOutcomeKind {
        if self.snapshot.is_success() {
            crate::typed_query_store::AttemptOutcomeKind::Success
        } else {
            crate::typed_query_store::AttemptOutcomeKind::Failure
        }
    }
    fn origin_id(&self) -> crate::session::AttemptId {
        self.id
    }
    fn work(&self) -> &crate::session::QueryStructuralWork {
        &crate::session::QueryStructuralWork::None
    }
    fn diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
        Some(&self.snapshot)
    }
}

fn same_source_attempt(left: &SourceSnapshot, right: &SourceSnapshot) -> bool {
    left.source_revision() == right.source_revision()
        && left.metadata() == right.metadata()
        && left
            .files()
            .map(|file| file.file_id)
            .eq(right.files().map(|file| file.file_id))
}

fn snapshot_matches_source_attempt(
    snapshot: &FrontendDiagnosticSnapshot,
    source: &SourceSnapshot,
) -> bool {
    if snapshot.source_revision() != source.source_revision()
        || snapshot.source().metadata() != source.metadata()
    {
        return false;
    }
    match &snapshot.provenance {
        DiagnosticAttemptProvenance::Canonical => true,
        DiagnosticAttemptProvenance::Presentation(order) => order.iter().eq(source
            .files()
            .map(|file| source.module_id(file.file_id).unwrap())),
    }
}

#[cfg(test)]
mod tests {
    use ahash::AHashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rue_span::FileId;

    use super::*;
    use crate::{SourceMetadata, StableOptLevel};

    #[derive(Debug)]
    struct FakeAttempt {
        id: crate::session::AttemptId,
        diagnostics: Arc<FrontendDiagnosticSnapshot>,
    }

    impl AttemptView for FakeAttempt {
        fn id(&self) -> crate::session::AttemptId {
            self.id
        }
        fn execution(&self) -> crate::typed_query_store::AttemptExecution {
            crate::typed_query_store::AttemptExecution::Computed
        }
        fn outcome(&self) -> crate::typed_query_store::AttemptOutcomeKind {
            if self.diagnostics.is_success() {
                crate::typed_query_store::AttemptOutcomeKind::Success
            } else {
                crate::typed_query_store::AttemptOutcomeKind::Failure
            }
        }
        fn origin_id(&self) -> crate::session::AttemptId {
            self.id
        }
        fn work(&self) -> &crate::session::QueryStructuralWork {
            &crate::session::QueryStructuralWork::None
        }
        fn diagnostics(&self) -> Option<&Arc<FrontendDiagnosticSnapshot>> {
            Some(&self.diagnostics)
        }
    }

    fn indexed(snapshot: Arc<FrontendDiagnosticSnapshot>) -> Arc<dyn AttemptView> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(FakeAttempt {
            id: crate::session::AttemptId(NEXT.fetch_add(1, Ordering::Relaxed)),
            diagnostics: snapshot,
        })
    }

    fn source(text: &str) -> SourceSnapshot {
        let root = FileId::new(1);
        SourceSnapshot::new(
            SourceMetadata::new(
                root,
                AHashMap::from([(root, "/p/main.rue".to_owned())]),
                AHashMap::from([(root, "main.rue".to_owned())]),
            )
            .unwrap(),
            vec![(root, Arc::new(text.to_owned()))],
        )
        .unwrap()
    }

    fn snapshot(
        source: &SourceSnapshot,
        stage: FrontendDiagnosticIdentity,
        success: bool,
    ) -> Arc<FrontendDiagnosticSnapshot> {
        Arc::new(FrontendDiagnosticSnapshot {
            source: source.clone(),
            stage,
            provenance: DiagnosticAttemptProvenance::Canonical,
            errors: if success {
                Arc::from([])
            } else {
                Arc::from([CompileError::without_span(
                    crate::ErrorKind::InvalidCompilerInput("test failure".into()),
                )])
            },
            warnings: Arc::from([]),
        })
    }

    #[test]
    fn exact_attempt_keys_and_selectors_are_independent() {
        let valid_source = source("fn main() {}");
        let syntax = snapshot(&valid_source, FrontendDiagnosticIdentity::Syntax, true);
        let semantic = snapshot(
            &valid_source,
            FrontendDiagnosticIdentity::Semantic(crate::ResolvedCodegenRevision::new(
                crate::ResolvedProgramRevision::new(
                    crate::SemanticInputDescriptor::new(
                        &valid_source,
                        crate::Target::default(),
                        &crate::PreviewFeatures::default(),
                    ),
                    crate::test_support::test_import_graph(&valid_source).unwrap(),
                ),
                StableOptLevel::O0,
            )),
            true,
        );
        let failure_source = source("fn main( {");
        let failure = snapshot(&failure_source, FrontendDiagnosticIdentity::Syntax, false);
        let mut store = DiagnosticAttemptStore::default();
        store.insert(indexed(syntax.clone()));
        store.insert(indexed(semantic.clone()));
        store.insert(indexed(failure.clone()));

        assert!(Arc::ptr_eq(store.latest().unwrap(), &failure));
        assert!(Arc::ptr_eq(store.latest_successful().unwrap(), &semantic));
        assert!(Arc::ptr_eq(store.last_good_semantic().unwrap(), &semantic));
        assert!(Arc::ptr_eq(
            store
                .find(&valid_source, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &syntax
        ));
    }

    #[test]
    fn eviction_is_bounded_and_reselection_reindexes_without_copying_batch() {
        let mut store = DiagnosticAttemptStore::default();
        let pinned_source = source("fn main() -> i32 { 0 }");
        let pinned = snapshot(&pinned_source, FrontendDiagnosticIdentity::Syntax, true);
        store.insert(indexed(pinned.clone()));
        for revision in 0..=FRONTEND_DIAGNOSTIC_RETENTION_LIMIT {
            let current = source(&format!("fn main() -> i32 {{ {revision} }}"));
            store.insert(indexed(snapshot(
                &current,
                FrontendDiagnosticIdentity::Merge,
                true,
            )));
        }
        assert!(store.retention_metrics().entries <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
        assert!(
            store
                .find(&pinned_source, &FrontendDiagnosticIdentity::Syntax)
                .is_none()
        );

        store.select(indexed(pinned.clone()));
        assert!(Arc::ptr_eq(store.latest().unwrap(), &pinned));
        assert!(Arc::ptr_eq(
            store
                .find(&pinned_source, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &pinned
        ));
        assert!(store.retention_metrics().entries <= FRONTEND_DIAGNOSTIC_RETENTION_LIMIT);
    }

    #[test]
    fn public_lookup_prefers_selected_provenance_for_one_public_stage() {
        let root = FileId::new(1);
        let other = FileId::new(2);
        let metadata = SourceMetadata::new(
            root,
            AHashMap::from([
                (root, "/p/main.rue".to_owned()),
                (other, "/p/other.rue".to_owned()),
            ]),
            AHashMap::from([
                (root, "main.rue".to_owned()),
                (other, "other.rue".to_owned()),
            ]),
        )
        .unwrap();
        let root_text = Arc::new("fn main() {}".to_owned());
        let other_text = Arc::new("fn other() {}".to_owned());
        let forward = SourceSnapshot::new(
            metadata.clone(),
            vec![(root, root_text.clone()), (other, other_text.clone())],
        )
        .unwrap();
        let reverse =
            SourceSnapshot::new(metadata, vec![(other, other_text), (root, root_text)]).unwrap();
        let forward_order = crate::shared_segments::SharedList::flat(
            forward
                .files()
                .map(|file| forward.module_id(file.file_id).unwrap().clone())
                .collect(),
        );
        let reverse_order = crate::shared_segments::SharedList::flat(
            reverse
                .files()
                .map(|file| reverse.module_id(file.file_id).unwrap().clone())
                .collect(),
        );
        let attempt = |source: &SourceSnapshot, provenance| {
            Arc::new(FrontendDiagnosticSnapshot {
                source: source.clone(),
                stage: FrontendDiagnosticIdentity::Syntax,
                provenance,
                errors: Arc::from([]),
                warnings: Arc::from([]),
            })
        };
        let forward_attempt = attempt(
            &forward,
            DiagnosticAttemptProvenance::Presentation(forward_order),
        );
        let reverse_attempt = attempt(
            &reverse,
            DiagnosticAttemptProvenance::Presentation(reverse_order),
        );
        let canonical_attempt = attempt(&reverse, DiagnosticAttemptProvenance::Canonical);
        let mut store = DiagnosticAttemptStore::default();
        store.insert(indexed(forward_attempt.clone()));
        store.insert(indexed(reverse_attempt.clone()));
        store.insert(indexed(canonical_attempt));

        store.select(indexed(reverse_attempt.clone()));

        assert!(Arc::ptr_eq(
            store
                .find(&reverse, &FrontendDiagnosticIdentity::Syntax)
                .unwrap(),
            &reverse_attempt
        ));
    }
}
