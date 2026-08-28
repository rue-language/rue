//! Compiler-owned import discovery protocol.
//!
//! Candidate policy is pure compiler state. Rooted consumers ask the revisioned
//! database for an import demand frontier, execute that exact ordered batch,
//! and publish typed observations into an immutable successor revision. Hosts
//! never recognize imports or choose resolution precedence. Plans remain a
//! whole-graph compatibility projection for diagnostics and closure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ahash::AHashMap;
use rue_air::normalize_module_path;
use rue_error::{CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_span::FileId;

use crate::{
    ImportDirective, ModuleId, ParsedProgram, SourceMetadata, SourceRevision, SourceSnapshot,
    TRUSTED_STANDARD_LIBRARY_NAMESPACE,
};

/// Version of the target-independent candidate policy represented by plans.
///
/// Version 2 (ADR-0078): one candidate per relative specifier — extensionless
/// paths name the directory facade only, and the importing file's directory is
/// the only search base. `std` anchors to the program: the vendored
/// `{root}/std/_std.rue` precedes the captured toolchain std root.
pub const IMPORT_DISCOVERY_POLICY_VERSION: u32 = 2;

/// Immutable invocation inputs captured once for a discovery epoch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportDiscoveryContext {
    epoch: u64,
    project_root: Arc<str>,
    std_root: Option<Arc<str>>,
    read_policy_revision: Arc<str>,
}

impl ImportDiscoveryContext {
    pub fn new(
        epoch: u64,
        project_root: impl AsRef<str>,
        std_root: Option<&str>,
        read_policy_revision: impl Into<Arc<str>>,
    ) -> CompileResult<Self> {
        let project_root = normalize_absolute(project_root.as_ref())?;
        let std_root = std_root.map(normalize_absolute).transpose()?.map(Arc::from);
        if std_root
            .as_deref()
            .is_some_and(|std_root| Path::new(&project_root).starts_with(std_root))
        {
            return Err(invalid_input(format!(
                "project root {:?} is inside the configured standard-library root",
                project_root
            )));
        }
        Ok(Self {
            epoch,
            project_root: Arc::from(project_root),
            std_root,
            read_policy_revision: read_policy_revision.into(),
        })
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Stable identity of the filesystem *observation regime* (RUE-1137).
    ///
    /// This is the revision compatibility token for import-input publication.
    /// It changes when the rules governing reads change — discovery epoch,
    /// project root, std root, or read policy — and NOT when an individual
    /// file changes. File changes are per-leaf stamp changes, so a retained
    /// terminal stays validatable across an ordinary edit; a regime change
    /// invalidates wholesale because the compiler can no longer relate its
    /// retained observations to the new rules.
    ///
    /// Deriving this from content rather than from a per-request counter is
    /// what allows warm reuse under rooted demand. The counter that used to
    /// occupy this slot minted a fresh epoch per request, so no predecessor
    /// terminal was ever eligible for red/green validation.
    ///
    /// A stable digest is used rather than `DefaultHasher` so the token does
    /// not vary across processes or toolchain versions; the reproducibility
    /// harness compares publication identity across runs.
    pub fn regime_token(&self) -> u64 {
        use sha2::{Digest, Sha256};

        let mut digest = Sha256::new();
        // Length-prefix every field so distinct field boundaries cannot be
        // forged by concatenation (e.g. roots "/a" + "/bc" vs "/ab" + "/c").
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        field(&self.epoch.to_le_bytes());
        field(self.project_root.as_bytes());
        match self.std_root.as_deref() {
            Some(root) => {
                field(b"std");
                field(root.as_bytes());
            }
            None => field(b"no-std"),
        }
        field(self.read_policy_revision.as_bytes());
        let bytes = digest.finalize();
        u64::from_le_bytes(bytes[..8].try_into().expect("sha256 yields 32 bytes"))
    }
    pub fn project_root(&self) -> &str {
        &self.project_root
    }
    pub fn std_root(&self) -> Option<&str> {
        self.std_root.as_deref()
    }
    pub fn read_policy_revision(&self) -> &str {
        &self.read_policy_revision
    }
}

/// Stable key for one parser-owned import occurrence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportOccurrenceKey {
    importer: ModuleId,
    source_offset: u32,
    source_end: u32,
    specifier: Arc<str>,
}

impl ImportOccurrenceKey {
    pub(crate) fn from_directive(site: &ImportDirective) -> Self {
        Self {
            importer: site.importer().clone(),
            source_offset: site.source_offset(),
            source_end: site.source_end(),
            specifier: Arc::from(site.specifier()),
        }
    }
    pub fn importer(&self) -> &ModuleId {
        &self.importer
    }
    pub fn source_offset(&self) -> u32 {
        self.source_offset
    }
    pub fn source_end(&self) -> u32 {
        self.source_end
    }
    pub fn specifier(&self) -> &str {
        &self.specifier
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportCandidateRole {
    ExactFile,
    FileModule,
    DirectoryFacade,
    StandardLibraryFacade,
}

/// One physical operation selected by compiler policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportDiscoveryRequest {
    context: ImportDiscoveryContext,
    occurrence: ImportOccurrenceKey,
    exact_specifier: Arc<str>,
    normalized_specifier: Arc<str>,
    importer_anchor: Arc<str>,
    root_anchor: Arc<str>,
    group: u32,
    position: u32,
    requested_path: Arc<str>,
    role: ImportCandidateRole,
}

impl ImportDiscoveryRequest {
    pub fn context(&self) -> &ImportDiscoveryContext {
        &self.context
    }
    pub fn occurrence(&self) -> &ImportOccurrenceKey {
        &self.occurrence
    }
    pub fn group(&self) -> usize {
        self.group as usize
    }
    pub fn exact_specifier(&self) -> &str {
        &self.exact_specifier
    }
    pub fn normalized_specifier(&self) -> &str {
        &self.normalized_specifier
    }
    pub fn importer_anchor(&self) -> &str {
        &self.importer_anchor
    }
    pub fn root_anchor(&self) -> &str {
        &self.root_anchor
    }
    pub fn position(&self) -> usize {
        self.position as usize
    }
    pub fn requested_path(&self) -> &str {
        &self.requested_path
    }
    pub fn role(&self) -> ImportCandidateRole {
        self.role
    }

    pub(crate) fn runtime_input_key(&self) -> Arc<str> {
        fn field(output: &mut String, value: &str) {
            use std::fmt::Write as _;
            write!(output, "{}:{value}", value.len()).unwrap();
        }
        use std::fmt::Write as _;
        let mut key = String::new();
        write!(
            key,
            "v1:{}:{}:{}:{}:{}:",
            self.context.epoch,
            self.occurrence.source_offset,
            self.occurrence.source_end,
            self.group,
            self.position
        )
        .unwrap();
        field(&mut key, self.context.project_root());
        field(&mut key, self.context.std_root().unwrap_or(""));
        field(&mut key, self.context.read_policy_revision());
        field(&mut key, self.occurrence.importer().as_str());
        field(&mut key, self.exact_specifier());
        field(&mut key, self.normalized_specifier());
        field(&mut key, self.importer_anchor());
        field(&mut key, self.root_anchor());
        field(&mut key, self.requested_path());
        field(
            &mut key,
            match self.role {
                ImportCandidateRole::ExactFile => "exact-file",
                ImportCandidateRole::FileModule => "file-module",
                ImportCandidateRole::DirectoryFacade => "directory-facade",
                ImportCandidateRole::StandardLibraryFacade => "standard-library-facade",
            },
        );
        key.into()
    }
}

/// Result of one host transaction. Denial and absence are intentionally
/// distinct from an accepted source read.
///
/// Hosts construct statuses only for requests emitted by a compiler frontier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImportObservationStatus {
    Absent,
    PresentReadable {
        canonical_path: Arc<str>,
        metadata_identity: PhysicalFileIdentity,
        metadata_fingerprint: FileMetadataFingerprint,
        content_fingerprint: u64,
    },
    PresentUnreadable(Arc<str>),
    DeniedLexical,
    DeniedCanonical {
        canonical_path: Arc<str>,
    },
    InvalidPhysicalType {
        canonical_path: Arc<str>,
    },
    UnstableRead(Arc<str>),
    Cancelled,
}

/// Host-observed identity of one physical file, separate from its mutable metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalFileIdentity {
    volume: u64,
    file: u64,
}

impl PhysicalFileIdentity {
    pub fn new(volume: u64, file: u64) -> Self {
        Self { volume, file }
    }
    pub fn volume(self) -> u64 {
        self.volume
    }
    pub fn file(self) -> u64 {
        self.file
    }
}

/// Metadata fingerprint captured around an accepted stable read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileMetadataFingerprint {
    length: u64,
    modified: u64,
    changed: u64,
}

impl FileMetadataFingerprint {
    pub fn new(length: u64, modified: u64, changed: u64) -> Self {
        Self {
            length,
            modified,
            changed,
        }
    }
    pub fn length(self) -> u64 {
        self.length
    }
    pub fn modified(self) -> u64 {
        self.modified
    }
    pub fn changed(self) -> u64 {
        self.changed
    }
}

impl ImportObservationStatus {
    pub fn is_failure(&self) -> bool {
        !matches!(self, Self::Absent | Self::PresentReadable { .. })
    }

    /// Whether this observation proves that the candidate exists but cannot be
    /// used. Unlike a manifest denial, this must not let a later candidate in
    /// the same precedence chain replace it.
    pub(crate) fn is_conclusive_failure(&self) -> bool {
        matches!(
            self,
            Self::PresentUnreadable(_) | Self::InvalidPhysicalType { .. } | Self::UnstableRead(_)
        )
    }
}

/// Accepted source bytes paired with their transaction observation.
///
/// Source evidence for a compiler-emitted frontier request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AcceptedImportSource {
    requested_path: Arc<str>,
    canonical_path: Arc<str>,
    symlink_route: Arc<[PhysicalFileIdentity]>,
    metadata_identity: PhysicalFileIdentity,
    metadata_fingerprint: FileMetadataFingerprint,
    content_fingerprint: u64,
    source: Arc<String>,
}

impl AcceptedImportSource {
    /// Construct host-observed source evidence for a frontier request.
    pub fn new(
        requested_path: impl Into<Arc<str>>,
        canonical_path: impl Into<Arc<str>>,
        metadata_identity: PhysicalFileIdentity,
        metadata_fingerprint: FileMetadataFingerprint,
        source: Arc<String>,
    ) -> CompileResult<Self> {
        let requested_path = Arc::from(normalize_absolute(&requested_path.into())?);
        let canonical_path = Arc::from(normalize_absolute(&canonical_path.into())?);
        let content_fingerprint = source_fingerprint(&source);
        Ok(Self {
            requested_path,
            canonical_path,
            symlink_route: Arc::from([]),
            metadata_identity,
            metadata_fingerprint,
            content_fingerprint,
            source,
        })
    }
    pub fn requested_path(&self) -> &str {
        &self.requested_path
    }
    pub fn canonical_path(&self) -> &str {
        &self.canonical_path
    }
    /// Record the ordered physical identities of symlink components traversed
    /// below the host's captured logical boundary. This is host evidence, not
    /// part of the physical observation identity: two routes are aliases only
    /// when they resolve through different symlink components.
    pub fn with_symlink_route(mut self, route: Arc<[PhysicalFileIdentity]>) -> Self {
        self.symlink_route = route;
        self
    }
    pub fn symlink_route(&self) -> &[PhysicalFileIdentity] {
        &self.symlink_route
    }
    pub fn content_fingerprint(&self) -> u64 {
        self.content_fingerprint
    }
    pub fn metadata_identity(&self) -> PhysicalFileIdentity {
        self.metadata_identity
    }
    pub fn metadata_fingerprint(&self) -> FileMetadataFingerprint {
        self.metadata_fingerprint
    }
    pub fn source(&self) -> &Arc<String> {
        &self.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportObservation {
    request: ImportDiscoveryRequest,
    status: ImportObservationStatus,
    accepted_source: Option<AcceptedImportSource>,
}

impl ImportObservation {
    /// Construct an absence observation for a frontier request.
    pub fn absent(request: ImportDiscoveryRequest) -> Self {
        Self {
            request,
            status: ImportObservationStatus::Absent,
            accepted_source: None,
        }
    }
    /// Construct an accepted-source observation for a frontier request.
    pub fn accepted(
        request: ImportDiscoveryRequest,
        source: AcceptedImportSource,
    ) -> CompileResult<Self> {
        if request.requested_path() != source.requested_path() {
            return Err(invalid_input(
                "accepted source requested path does not match its request",
            ));
        }
        Ok(Self {
            request,
            status: ImportObservationStatus::PresentReadable {
                canonical_path: source.canonical_path.clone(),
                metadata_identity: source.metadata_identity,
                metadata_fingerprint: source.metadata_fingerprint,
                content_fingerprint: source.content_fingerprint,
            },
            accepted_source: Some(source),
        })
    }
    /// Construct a terminal-failure observation for a frontier request.
    pub fn failure(
        request: ImportDiscoveryRequest,
        status: ImportObservationStatus,
    ) -> CompileResult<Self> {
        if matches!(
            status,
            ImportObservationStatus::Absent | ImportObservationStatus::PresentReadable { .. }
        ) {
            return Err(invalid_input(
                "failure observation must carry a failure status",
            ));
        }
        Ok(Self {
            request,
            status,
            accepted_source: None,
        })
    }
    pub fn request(&self) -> &ImportDiscoveryRequest {
        &self.request
    }
    pub fn status(&self) -> &ImportObservationStatus {
        &self.status
    }
    pub fn accepted_source(&self) -> Option<&AcceptedImportSource> {
        self.accepted_source.as_ref()
    }

    pub(crate) fn fanout_to(&self, request: ImportDiscoveryRequest) -> CompileResult<Self> {
        if request.context() != self.request.context()
            || request.requested_path() != self.request.requested_path()
        {
            return Err(invalid_input(
                "one host import result can fan out only to the same candidate operation",
            ));
        }
        Ok(Self {
            request,
            status: self.status.clone(),
            accepted_source: self.accepted_source.clone(),
        })
    }
}

/// Deterministically ordered observations from one immutable epoch.
///
/// The ledger is a persistent structure (RUE-1112): frozen segments are shared
/// by `Arc` across clones, and entries recorded since the value was cloned live
/// in a private head. Cloning therefore costs O(entries recorded since the
/// parent value was itself cloned) — the delta — never the whole ledger, and
/// the head IS the exact recorded delta of the current step, which the
/// successor publication consumes directly instead of re-deriving additions by
/// diffing complete values. Equality and hashing are over the logical merged
/// sequence, so a flat and a segmented ledger with identical content are
/// indistinguishable to query memoization.
#[derive(Debug, Default)]
pub struct ImportObservationLedger {
    /// Frozen shared segments, oldest first; mutually disjoint by request.
    frozen: Vec<Arc<ImportObservationSegment>>,
    /// Entries recorded since this value was cloned from its parent.
    head: ImportObservationSegment,
}

#[derive(Debug, Default, Clone)]
struct ImportObservationSegment {
    observations: BTreeMap<ImportDiscoveryRequest, ImportObservation>,
    /// Derived exact index used only for demand-scoped validation. Equality,
    /// hashing, and iteration remain defined by `observations`.
    occurrence_counts: BTreeMap<ImportOccurrenceKey, usize>,
}

/// Segment chains longer than this are compacted on clone so lookup depth stays
/// bounded under long frontier-round chains.
const LEDGER_COMPACTION_DEPTH: usize = 16;

impl ImportObservationLedger {
    /// Record one host observation while publishing a compiler-ordered batch.
    pub fn record(&mut self, observation: ImportObservation) -> CompileResult<()> {
        let request = observation.request.clone();
        if let Some(previous) = self.get(&request) {
            if previous == &observation {
                return Ok(());
            }
            return Err(invalid_input(
                "one discovery request received conflicting observations",
            ));
        }
        *self
            .head
            .occurrence_counts
            .entry(request.occurrence().clone())
            .or_default() += 1;
        self.head.observations.insert(request, observation);
        Ok(())
    }
    pub fn get(&self, request: &ImportDiscoveryRequest) -> Option<&ImportObservation> {
        if let Some(observation) = self.head.observations.get(request) {
            return Some(observation);
        }
        self.frozen
            .iter()
            .rev()
            .find_map(|segment| segment.observations.get(request))
    }

    /// Number of observations recorded for one exact parser-owned occurrence.
    ///
    /// The segment chain is bounded, so this is independent of the number of
    /// observations accumulated by the import revision.
    fn observation_count_for_occurrence(&self, occurrence: &ImportOccurrenceKey) -> usize {
        self.frozen
            .iter()
            .map(|segment| {
                segment
                    .occurrence_counts
                    .get(occurrence)
                    .copied()
                    .unwrap_or(0)
            })
            .sum::<usize>()
            + self
                .head
                .occurrence_counts
                .get(occurrence)
                .copied()
                .unwrap_or(0)
    }
    pub fn iter(&self) -> LedgerIter<'_> {
        let mut cursors: Vec<std::collections::btree_map::Iter<'_, _, _>> = self
            .frozen
            .iter()
            .map(|segment| segment.observations.iter())
            .collect();
        cursors.push(self.head.observations.iter());
        LedgerIter {
            remaining: self.len(),
            heads: cursors.iter_mut().map(Iterator::next).collect(),
            cursors,
        }
    }
    pub fn len(&self) -> usize {
        self.frozen
            .iter()
            .map(|segment| segment.observations.len())
            .sum::<usize>()
            + self.head.observations.len()
    }
    pub fn is_empty(&self) -> bool {
        self.head.observations.is_empty()
            && self
                .frozen
                .iter()
                .all(|segment| segment.observations.is_empty())
    }

    /// The exact observations recorded into THIS value since it was cloned from
    /// its parent — the current step's delta, in canonical request order
    /// (RUE-1112). The successor publication consumes this recorded delta
    /// directly rather than re-deriving it by diffing complete ledgers.
    pub(crate) fn recorded_delta(&self) -> LedgerSegmentValues<'_> {
        self.head.observations.values()
    }
}

impl Clone for ImportObservationLedger {
    fn clone(&self) -> Self {
        // Freeze the head into a shared segment so the clone starts with an
        // empty recorded delta; only the head (the parent's own delta) is
        // copied, never the frozen predecessor segments.
        let mut frozen = self.frozen.clone();
        if !self.head.observations.is_empty() {
            frozen.push(Arc::new(self.head.clone()));
        }
        if frozen.len() >= LEDGER_COMPACTION_DEPTH {
            // Compact a long chain into one segment; the logical value and the
            // exact occurrence counts are unchanged.
            let mut merged = ImportObservationSegment::default();
            for segment in &frozen {
                merged.observations.extend(segment.observations.clone());
                for (occurrence, count) in &segment.occurrence_counts {
                    *merged
                        .occurrence_counts
                        .entry(occurrence.clone())
                        .or_default() += count;
                }
            }
            frozen = vec![Arc::new(merged)];
        }
        Self {
            frozen,
            head: ImportObservationSegment::default(),
        }
    }
}

impl PartialEq for ImportObservationLedger {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for ImportObservationLedger {}

impl std::hash::Hash for ImportObservationLedger {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Logical hash over the merged sequence: length then each observation in
        // canonical request order, so representation never affects identity.
        self.len().hash(state);
        for observation in self.iter() {
            observation.hash(state);
        }
    }
}

/// K-way merge over the ledger's disjoint sorted segments, yielding
/// observations in canonical request order.
pub struct LedgerIter<'a> {
    cursors: Vec<std::collections::btree_map::Iter<'a, ImportDiscoveryRequest, ImportObservation>>,
    heads: Vec<Option<(&'a ImportDiscoveryRequest, &'a ImportObservation)>>,
    remaining: usize,
}

type LedgerSegmentValues<'a> =
    std::collections::btree_map::Values<'a, ImportDiscoveryRequest, ImportObservation>;

impl<'a> Iterator for LedgerIter<'a> {
    type Item = &'a ImportObservation;

    fn next(&mut self) -> Option<&'a ImportObservation> {
        let mut best: Option<usize> = None;
        for (index, head) in self.heads.iter().enumerate() {
            if let Some((request, _)) = head {
                match best {
                    None => best = Some(index),
                    Some(current) => {
                        let (best_request, _) = self.heads[current].as_ref().unwrap();
                        if request < best_request {
                            best = Some(index);
                        }
                    }
                }
            }
        }
        let chosen = best?;
        let (_, observation) = self.heads[chosen].take().unwrap();
        self.heads[chosen] = self.cursors[chosen].next();
        self.remaining -= 1;
        Some(observation)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'a> ExactSizeIterator for LedgerIter<'a> {}

/// Authority allowed to expose missing external-input requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImportDemandMode {
    /// Observable rooted work may emit one ordered host batch.
    Rooted,
    /// Speculative work may consume existing leaves but never emit demands.
    Speculative,
}

/// Opaque immutable input revision for one discovery request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportInputRevision {
    pub(crate) revision_id: u64,
    /// Per-request identity. Distinguishes one rooted import-input request
    /// from the next, and is what a trusted-toolchain successor delta must
    /// match. This is deliberately NOT the revision compatibility token
    /// (RUE-1137): keeping the two separate is what lets a successor request
    /// reuse retained terminals while still being recognizable as a distinct
    /// request.
    pub(crate) request_generation: u64,
    /// Observation-regime identity, from [`ImportDiscoveryContext::regime_token`].
    /// This occupies the runtime revision's compatibility slot.
    pub(crate) compatibility_token: u64,
    pub(crate) frontier_round: u64,
}

impl ImportInputRevision {
    /// Runtime revision publication identity.
    pub fn id(self) -> u64 {
        self.revision_id
    }
    /// Number of rooted host batches published since the initial request view.
    pub fn frontier_round(self) -> u64 {
        self.frontier_round
    }
}

/// One compiler-produced, deduplicated external-input frontier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDemandFrontier {
    pub(crate) revision: ImportInputRevision,
    pub(crate) mode: ImportDemandMode,
    pub(crate) requests: Arc<[ImportDiscoveryRequest]>,
    /// Per-operation occurrence requests receiving the representative host
    /// result. Aligned one-to-one with `requests`.
    pub(crate) fanout: Arc<[Arc<[ImportDiscoveryRequest]>]>,
    pub(crate) speculative_blocked: bool,
}

impl ImportDemandFrontier {
    pub fn revision(&self) -> ImportInputRevision {
        self.revision
    }
    pub fn mode(&self) -> ImportDemandMode {
        self.mode
    }
    /// Exact compiler policy order the host must preserve in its result batch.
    pub fn requests(&self) -> &[ImportDiscoveryRequest] {
        &self.requests
    }
    /// Whether speculation encountered absent leaves and parked without a host
    /// request or publishable missing-input failure.
    pub fn speculative_blocked(&self) -> bool {
        self.speculative_blocked
    }

    /// Every import occurrence whose projection emitted a request into this
    /// frontier — the whole fanout, not just each host operation's deduplicated
    /// representative.
    ///
    /// Within one request generation these are exactly the occurrences that can
    /// still demand host input after this batch is answered. An occurrence that
    /// emitted nothing is conclusive: its candidate observations are all
    /// recorded, the ledger only grows and rejects conflicting re-observation,
    /// and a projection over a fully-observed occurrence emits no request. So a
    /// round that roots in the plan's new occurrences plus these reproduces the
    /// whole-plan frontier's request set exactly.
    pub fn demanded_occurrences(&self) -> impl Iterator<Item = &ImportOccurrenceKey> {
        self.fanout
            .iter()
            .flat_map(|requests| requests.iter())
            .map(ImportDiscoveryRequest::occurrence)
    }
}

/// One physical host operation: every candidate request naming the same
/// operation under the same captured context receives one host answer, which
/// then fans out to each requesting occurrence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ImportHostOperationKey {
    context: ImportDiscoveryContext,
    requested_path: Arc<str>,
}

impl ImportHostOperationKey {
    pub(crate) fn new(request: &ImportDiscoveryRequest) -> Self {
        Self {
            context: request.context().clone(),
            requested_path: Arc::from(request.requested_path()),
        }
    }
}

/// Exact parser-owned import occurrences demanded by canonical query
/// consumers. This type performs no reachability inference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportDemandRoots {
    occurrences: Arc<[ImportOccurrenceKey]>,
}

impl ImportDemandRoots {
    pub fn new(occurrences: impl IntoIterator<Item = ImportOccurrenceKey>) -> Self {
        let mut occurrences = occurrences.into_iter().collect::<Vec<_>>();
        occurrences.sort();
        occurrences.dedup();
        Self {
            occurrences: occurrences.into(),
        }
    }

    pub fn occurrences(&self) -> &[ImportOccurrenceKey] {
        &self.occurrences
    }

    pub(crate) fn whole_plan(plan: &ImportDiscoveryPlan) -> Self {
        Self::new(
            plan.groups
                .iter()
                .map(|group| group[0].occurrence().clone()),
        )
    }
}

/// Canonical order for request groups: by the first request of each group.
fn group_order(
    a: &Arc<[ImportDiscoveryRequest]>,
    b: &Arc<[ImportDiscoveryRequest]>,
) -> std::cmp::Ordering {
    a[0].cmp(&b[0])
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportDiscoveryPlan {
    source_revision: SourceRevision,
    context: ImportDiscoveryContext,
    groups: crate::shared_segments::SharedSegments<Arc<[ImportDiscoveryRequest]>>,
}

impl ImportDiscoveryPlan {
    /// Exact parser-owned occurrences in this immutable plan. Rooted consumers
    /// pass this value explicitly to the query frontier rather than relying on
    /// a selected-plan compatibility adapter inside the session.
    pub fn demand_roots(&self) -> ImportDemandRoots {
        ImportDemandRoots::whole_plan(self)
    }

    pub(crate) fn shape_diagnostics(program: &ParsedProgram) -> CompileErrors {
        let mut errors = CompileErrors::new();
        for invalid in program.invalid_imports() {
            let kind = match invalid.shape() {
                crate::InvalidImportShape::WrongArity { actual } => {
                    ErrorKind::IntrinsicWrongArgCount {
                        name: "import".into(),
                        expected: 1,
                        found: *actual as usize,
                    }
                }
                crate::InvalidImportShape::NonStringArgument => {
                    ErrorKind::ImportRequiresStringLiteral
                }
            };
            errors.push(CompileError::new(kind, invalid.span()));
        }
        errors
    }

    pub(crate) fn new(
        program: &ParsedProgram,
        context: ImportDiscoveryContext,
    ) -> CompileResult<Self> {
        let mut groups = Vec::new();
        for site in program.import_directives().iter() {
            let occurrence = ImportOccurrenceKey::from_directive(site);
            let importer_path = requested_path_for_module(&context, site.importer())?;
            groups.extend(discovery_groups_for_occurrence(
                &context,
                &occurrence,
                &importer_path,
            )?);
        }
        groups.sort_by(group_order);
        Ok(Self {
            source_revision: program.source_revision().clone(),
            context,
            groups: crate::shared_segments::SharedSegments::flat(groups.into(), group_order),
        })
    }

    /// Build a strictly-additive successor plan by carrying a predecessor plan's
    /// request groups verbatim and constructing groups ONLY for the newly
    /// appended modules' import occurrences (RUE-1112). The predecessor modules'
    /// sources are byte-identical in the successor, so their groups are reused
    /// rather than reconstructed; `discovery_groups_for_occurrence` runs only for
    /// occurrences owned by `new_modules`. Returns the successor plan and the
    /// number of groups actually constructed (the new ones), so the caller can
    /// prove plan construction is O(new leaves), independent of the predecessor
    /// import topology.
    pub(crate) fn extend_successor(
        predecessor: &ImportDiscoveryPlan,
        program: &ParsedProgram,
        context: ImportDiscoveryContext,
        new_modules: &[ModuleId],
    ) -> CompileResult<(Self, u64)> {
        // Construct groups ONLY for the newly appended modules' occurrences; the
        // predecessor plan's groups are carried by reference through
        // `SharedSegments::extend`; untouched tiers remain shared and tail-tier
        // compaction preserves canonical order.
        let mut delta = Vec::new();
        let mut constructed = 0u64;
        for module_id in new_modules {
            let Some(module) = program.module(module_id) else {
                continue;
            };
            let Some(first_site) = module.imports().first() else {
                continue;
            };
            // Every import site in a parsed module has the same importer. Build
            // its normalized anchor once instead of repeating the path
            // normalization for every site in the module.
            let importer_path = requested_path_for_module(&context, first_site.importer())?;
            for site in module.imports() {
                let occurrence = ImportOccurrenceKey::from_directive(site);
                let built = discovery_groups_for_occurrence(&context, &occurrence, &importer_path)?;
                constructed += built.len() as u64;
                delta.extend(built);
            }
        }
        Ok((
            Self {
                source_revision: program.source_revision().clone(),
                context,
                groups: crate::shared_segments::SharedSegments::extend(&predecessor.groups, delta),
            },
            constructed,
        ))
    }

    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn context(&self) -> &ImportDiscoveryContext {
        &self.context
    }
    /// The successor delta groups: request groups owned only by modules added
    /// since the predecessor close (empty for a full plan). Iterating these
    /// avoids materializing or filtering the full merged plan on the acquisition
    /// path (RUE-1112).
    pub(crate) fn delta_groups(&self) -> &[Arc<[ImportDiscoveryRequest]>] {
        self.groups.delta_segment()
    }

    /// The shared request-group representation (RUE-1112 successor sharing).
    pub(crate) fn group_segments(
        &self,
    ) -> &crate::shared_segments::SharedSegments<Arc<[ImportDiscoveryRequest]>> {
        &self.groups
    }

    /// The demand roots for the successor delta occurrences alone — the frontier
    /// and close-time projection for a trusted-toolchain successor root only in
    /// these, never in the predecessor topology (RUE-1112). Derived directly from
    /// the delta segment, never by filtering the merged plan.
    pub(crate) fn delta_roots(&self) -> ImportDemandRoots {
        ImportDemandRoots::new(
            self.delta_groups()
                .iter()
                .map(|group| group[0].occurrence().clone()),
        )
    }

    /// The demand roots for one ordinary discovery round that continues
    /// `previous`: the occurrences this round's stage ADDED to the plan, plus
    /// the occurrences `previous` actually demanded host answers for.
    ///
    /// Every other occurrence in the plan was already conclusive when `previous`
    /// was built and stays conclusive — the ledger only grows within a request
    /// generation — so rooting in this union emits exactly the request set a
    /// whole-plan rooting would, in the same canonical order. Both halves are
    /// compiler-derived: the plan's own delta segment and the frontier's own
    /// fanout. The host chooses neither.
    pub(crate) fn round_roots(&self, previous: &ImportDemandFrontier) -> ImportDemandRoots {
        ImportDemandRoots::new(
            self.delta_groups()
                .iter()
                .map(|group| group[0].occurrence().clone())
                .chain(previous.demanded_occurrences().cloned()),
        )
    }

    /// Ordered candidate groups retained by the compiler-owned plan.
    pub fn groups(&self) -> &[Arc<[ImportDiscoveryRequest]>] {
        self.groups.as_slice()
    }

    /// Return exactly the next operations allowed by candidate precedence.
    #[cfg(test)]
    pub(crate) fn pending_requests(
        &self,
        ledger: &ImportObservationLedger,
    ) -> Vec<ImportDiscoveryRequest> {
        let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
            BTreeMap::new();
        for group in self.groups.iter() {
            by_site.entry(&group[0].occurrence).or_default().push(group);
        }
        let mut pending = Vec::new();
        for groups in by_site.values() {
            for group in groups {
                match exact_import_group_state(group, ledger) {
                    ExactImportGroupState::ConclusiveFailure(_)
                    | ExactImportGroupState::Cancelled(_)
                    | ExactImportGroupState::Resolved(_) => break,
                    ExactImportGroupState::Pending => {
                        pending.extend(
                            group
                                .iter()
                                .filter(|request| ledger.get(request).is_none())
                                .cloned(),
                        );
                        break;
                    }
                    ExactImportGroupState::Skippable => {}
                }
            }
        }
        pending
    }

    #[cfg(test)]
    pub fn failures<'a>(
        &'a self,
        ledger: &'a ImportObservationLedger,
    ) -> impl Iterator<Item = &'a ImportObservation> {
        self.groups
            .iter()
            .flat_map(move |group| group.iter().filter_map(|request| ledger.get(request)))
            .filter(|observation| observation.status.is_failure())
    }

    /// Accepted reads in winning groups only. Later candidate groups are never
    /// requested, so policy cannot be reconstructed from this value by a host.
    pub fn accepted_sources<'a>(
        &'a self,
        ledger: &'a ImportObservationLedger,
    ) -> Vec<&'a AcceptedImportSource> {
        accepted_sources_for(self.groups.iter(), ledger)
    }

    /// The accepted sources of a trusted-toolchain successor plan's delta groups
    /// alone — those owned by modules added since the predecessor close (RUE-1112).
    /// The predecessor leaves are already assembled, so a successor add only needs
    /// these; the merged plan is never scanned.
    pub(crate) fn delta_accepted_sources<'a>(
        &'a self,
        ledger: &'a ImportObservationLedger,
    ) -> Vec<&'a AcceptedImportSource> {
        accepted_sources_for(self.groups.delta_segment().iter(), ledger)
    }

    /// The candidate groups of exactly `occurrences`, in candidate-precedence
    /// order.
    ///
    /// A discovery wave continues the precedence walk of the occurrences its
    /// starting frontier demanded answers for, so it needs those occurrences'
    /// groups without materializing the merged plan. Each occurrence's groups
    /// are found by binary search: every request in a plan carries the plan's
    /// own context, so the canonical group order sorts by context and then by
    /// occurrence, and one occurrence's groups are a contiguous range of every
    /// stored segment.
    pub(crate) fn groups_at(
        &self,
        occurrences: &ImportDemandRoots,
    ) -> Vec<(ImportOccurrenceKey, Vec<Arc<[ImportDiscoveryRequest]>>)> {
        let mut found = Vec::with_capacity(occurrences.occurrences().len());
        for occurrence in occurrences.occurrences() {
            let mut candidates: Vec<Arc<[ImportDiscoveryRequest]>> = Vec::new();
            self.groups.for_each_matching(
                |group| (&group[0].context, &group[0].occurrence).cmp(&(&self.context, occurrence)),
                |group| candidates.push(group.clone()),
            );
            candidates.sort_by(group_order);
            found.push((occurrence.clone(), candidates));
        }
        found
    }
}

/// One discovery wave: the transitive import closure reachable from a round's
/// starting frontier, resolved hop by hop and published as ONE input revision
/// (ADR-0075).
///
/// A hop-granular round can only resolve the imports of sources the current
/// revision already carries, so a depth-n chain costs n revision mints, ledger
/// appends, batch publications, and validation sweeps. A wave keeps the hop
/// structure — each hop emits exactly the operations, in exactly the order, the
/// corresponding round's frontier would have emitted — but resolves the next hop
/// against its own running ledger instead of a published successor. The reads it
/// performs, and the order the ledger records them in, are therefore identical
/// to the sequence of rounds it replaces; only the number of publications
/// changes.
///
/// Candidate policy stays compiler-owned and singular: a hop's next operations
/// come from [`discovery_groups_for_occurrence`] and
/// [`exact_import_pending_requests`], the same functions the `ResolveImport`
/// query evaluates. The wave runs them eagerly because the memoized query cannot
/// serve sources no revision carries yet.
#[derive(Debug)]
pub struct ImportDiscoveryWave {
    revision: ImportInputRevision,
    context: ImportDiscoveryContext,
    /// The wave's running ledger: the revision's carried observations plus every
    /// answer the wave has recorded, fanned out exactly as publication will.
    ledger: ImportObservationLedger,
    /// Occurrences whose candidate-precedence walk is still open, with the
    /// canonical groups each owns. A concluded occurrence is dropped, so a hop
    /// costs O(open occurrences), never O(plan).
    open: BTreeMap<ImportOccurrenceKey, Vec<Arc<[ImportDiscoveryRequest]>>>,
    /// Modules whose occurrences are already accounted for — the revision's own
    /// modules plus everything the wave has read.
    known_modules: BTreeSet<ModuleId>,
    /// Physical representatives already assembled before this wave. This
    /// prevents a hard-link import of the root (or another carried source)
    /// from manufacturing a second parsed module that the assembler will not
    /// publish.
    known_physical: BTreeMap<PhysicalFileIdentity, ModuleId>,
    /// This hop's deduplicated host operations and the occurrence requests each
    /// one answers.
    requests: Vec<ImportDiscoveryRequest>,
    fanout: Vec<Arc<[ImportDiscoveryRequest]>>,
    /// Every hop's operations and answers in hop order — the single batch this
    /// wave publishes.
    batch_requests: Vec<ImportDiscoveryRequest>,
    batch_fanout: Vec<Arc<[ImportDiscoveryRequest]>>,
    batch_observations: Vec<ImportObservation>,
    /// Accepted sources reduced hop by hop, in the canonical order each hop
    /// offers them to the assembler.
    accepted: Vec<AcceptedImportSource>,
    /// Each accepted source's eager parse, staged for the published revision's
    /// canonical parse query so the same bytes are not lexed and parsed twice.
    staged_parses: Vec<crate::parsed_modules::StagedModuleParse>,
    /// A source that did not parse ends the wave at that hop's boundary, so the
    /// published revision holds exactly what the equivalent round would have and
    /// the canonical staging parse reports the diagnostics.
    sealed: bool,
}

impl ImportDiscoveryWave {
    /// Begin a wave from a round's starting frontier. `ledger` is the revision's
    /// carried ledger; `frontier` is the batch the host is about to answer.
    pub(crate) fn begin_with_accepted_reads(
        plan: &ImportDiscoveryPlan,
        frontier: &ImportDemandFrontier,
        ledger: ImportObservationLedger,
        accepted_reads: Option<&AcceptedReadManifest>,
    ) -> CompileResult<Self> {
        if frontier.mode != ImportDemandMode::Rooted {
            return Err(invalid_input(
                "speculative import work cannot open a discovery wave",
            ));
        }
        let demanded = ImportDemandRoots::new(frontier.demanded_occurrences().cloned());
        let open = plan.groups_at(&demanded).into_iter().collect();
        let known_modules = plan
            .source_revision()
            .modules()
            .iter()
            .map(|revision| revision.module.clone())
            .collect();
        let known_physical = accepted_reads
            .into_iter()
            .flat_map(AcceptedReadManifest::iter)
            .map(|entry| (entry.metadata_identity(), entry.module().clone()))
            .collect();
        Ok(Self {
            revision: frontier.revision,
            context: plan.context().clone(),
            ledger,
            open,
            known_modules,
            known_physical,
            requests: frontier.requests.to_vec(),
            fanout: frontier.fanout.to_vec(),
            batch_requests: Vec::new(),
            batch_fanout: Vec::new(),
            batch_observations: Vec::new(),
            accepted: Vec::new(),
            staged_parses: Vec::new(),
            sealed: false,
        })
    }

    /// The exact ordered operations the host must answer for this hop.
    pub fn requests(&self) -> &[ImportDiscoveryRequest] {
        &self.requests
    }

    /// Whether the wave has reached its fixed point — the frontier of the
    /// transitive closure raised no further demand.
    pub fn is_complete(&self) -> bool {
        self.requests.is_empty()
    }

    /// The accepted sources this wave read, in the canonical order they were
    /// reduced. The host re-verifies their stamps before publication so a
    /// mid-wave rewrite can never enter a mixed-stamp revision.
    pub fn accepted_reads(&self) -> &[AcceptedImportSource] {
        &self.accepted
    }

    /// Record one hop's answers and derive the next hop.
    ///
    /// The answers are checked against this hop's operations exactly as batch
    /// publication checks them, then fanned out into the running ledger. Sources
    /// the hop accepted are read for their own `@import` occurrences, whose
    /// candidate groups join the open set; occurrences whose precedence walk
    /// concluded leave it.
    pub(crate) fn extend(&mut self, observations: Vec<ImportObservation>) -> CompileResult<()> {
        if observations.len() != self.requests.len()
            || observations
                .iter()
                .zip(self.requests.iter())
                .any(|(observation, request)| observation.request() != request)
        {
            return Err(invalid_input(
                "host import results must exactly preserve the compiler-produced batch order",
            ));
        }
        let hop_requests = std::mem::take(&mut self.requests);
        let hop_fanout = std::mem::take(&mut self.fanout);
        let mut answered: BTreeSet<&ImportOccurrenceKey> = BTreeSet::new();
        for (observation, fanout) in observations.iter().zip(hop_fanout.iter()) {
            for request in fanout.iter() {
                answered.insert(request.occurrence());
                self.ledger
                    .record(observation.fanout_to(request.clone())?)?;
            }
        }
        // Reduce exactly the occurrences whose ledger answers this hop changed —
        // the same reduction the round it replaces performs, over the same
        // occurrences, in the same canonical order.
        let reduced = {
            let groups = answered
                .iter()
                .filter_map(|occurrence| self.open.get(*occurrence))
                .flat_map(|groups| groups.iter());
            accepted_sources_for(groups, &self.ledger)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        for source in &reduced {
            let module = classify_module(
                &self.context,
                source.requested_path(),
                source.canonical_path(),
            )?;
            if self
                .known_physical
                .contains_key(&source.metadata_identity())
            {
                // The physical representative is already known. Its import
                // occurrence remains in the ledger for close-time diagnostics,
                // but parsing it as a second logical module would create a
                // successor source the assembler intentionally does not carry.
                continue;
            }
            self.known_physical
                .insert(source.metadata_identity(), module.clone());
            if !self.known_modules.insert(module.clone()) {
                continue;
            }
            let Some((sites, staged)) = crate::parsed_modules::parse_unpublished_module(
                &module,
                source.canonical_path(),
                source.source(),
            ) else {
                self.sealed = true;
                continue;
            };
            self.staged_parses.push(staged);
            let importer_path = requested_path_for_module(&self.context, &module)?;
            for site in &sites {
                let occurrence = ImportOccurrenceKey::from_directive(site);
                let groups =
                    discovery_groups_for_occurrence(&self.context, &occurrence, &importer_path)?;
                self.open.insert(occurrence, groups);
            }
        }
        self.accepted.extend(reduced);
        self.batch_requests.extend(hop_requests);
        self.batch_fanout.extend(hop_fanout);
        self.batch_observations.extend(observations);
        self.advance_frontier();
        Ok(())
    }

    /// Derive the next hop's operations from the open set: each open occurrence's
    /// pending candidate requests, in occurrence order, deduplicated to one host
    /// operation with a fanout — the frontier construction, over the wave's own
    /// running ledger rather than a published revision.
    fn advance_frontier(&mut self) {
        let mut requests = Vec::new();
        let mut fanout = Vec::<Vec<ImportDiscoveryRequest>>::new();
        let mut operations = BTreeMap::<ImportHostOperationKey, usize>::new();
        let mut concluded = Vec::new();
        for (occurrence, groups) in &self.open {
            let pending = exact_import_pending_requests(groups, &self.ledger);
            if pending.is_empty() {
                // The precedence walk is over: the ledger only grows, and no
                // growth can reopen an occurrence whose candidates are all
                // decided.
                concluded.push(occurrence.clone());
                continue;
            }
            for request in pending {
                let operation = ImportHostOperationKey::new(&request);
                match operations.get(&operation).copied() {
                    Some(index) => fanout[index].push(request),
                    None => {
                        operations.insert(operation, requests.len());
                        requests.push(request.clone());
                        fanout.push(vec![request]);
                    }
                }
            }
        }
        for occurrence in concluded {
            self.open.remove(&occurrence);
        }
        if self.sealed {
            // A hop that read an unparsable source publishes what it has; the
            // still-open occurrences are re-rooted by the next ordinary round.
            return;
        }
        self.requests = requests;
        self.fanout = fanout
            .into_iter()
            .map(Arc::<[ImportDiscoveryRequest]>::from)
            .collect();
    }

    /// Take the wave's staged eager parses for the parse-query stage. Called by
    /// publication so a staged tree becomes consumable exactly when the revision
    /// carrying its source does.
    pub(crate) fn take_staged_parses(&mut self) -> Vec<crate::parsed_modules::StagedModuleParse> {
        std::mem::take(&mut self.staged_parses)
    }

    /// The single batch this wave publishes: every hop's operations, fanout, and
    /// answers, concatenated in hop order.
    pub(crate) fn into_batch(self) -> (ImportDemandFrontier, Vec<ImportObservation>) {
        (
            ImportDemandFrontier {
                revision: self.revision,
                mode: ImportDemandMode::Rooted,
                requests: self.batch_requests.into(),
                fanout: self.batch_fanout.into_iter().collect::<Vec<_>>().into(),
                speculative_blocked: false,
            },
            self.batch_observations,
        )
    }
}

/// The winning accepted sources across a set of candidate request groups.
fn accepted_sources_for<'a>(
    groups: impl Iterator<Item = &'a Arc<[ImportDiscoveryRequest]>>,
    ledger: &'a ImportObservationLedger,
) -> Vec<&'a AcceptedImportSource> {
    let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
        BTreeMap::new();
    for group in groups {
        by_site.entry(&group[0].occurrence).or_default().push(group);
    }
    let mut accepted = Vec::new();
    for groups in by_site.values() {
        extend_accepted_from_site(&mut accepted, groups.iter().copied(), ledger);
    }
    canonicalize_accepted(accepted)
}

/// The precedence state of one candidate group.
pub(crate) enum ExactImportGroupState<'ledger> {
    Pending,
    Skippable,
    ConclusiveFailure(&'ledger ImportObservation),
    Cancelled(&'ledger ImportObservation),
    Resolved(&'ledger AcceptedImportSource),
}

/// Derive one candidate group's state from the ledger. The same state machine
/// drives the wave, revisioned queries, accepted-source reduction, winner
/// selection, and diagnostics so a conclusive vendored failure cannot be
/// bypassed by a later toolchain observation.
pub(crate) fn exact_import_group_state<'ledger>(
    group: &Arc<[ImportDiscoveryRequest]>,
    ledger: &'ledger ImportObservationLedger,
) -> ExactImportGroupState<'ledger> {
    if let Some(failure) = group
        .iter()
        .filter_map(|request| ledger.get(request))
        .find(|observation| observation.status().is_conclusive_failure())
    {
        return ExactImportGroupState::ConclusiveFailure(failure);
    }
    if let Some(cancelled) = group
        .iter()
        .filter_map(|request| ledger.get(request))
        .find(|observation| matches!(observation.status(), ImportObservationStatus::Cancelled))
    {
        return ExactImportGroupState::Cancelled(cancelled);
    }
    if let Some(source) = group
        .iter()
        .filter_map(|request| ledger.get(request))
        .filter_map(ImportObservation::accepted_source)
        .next()
    {
        return ExactImportGroupState::Resolved(source);
    }
    if group.iter().any(|request| ledger.get(request).is_none()) {
        ExactImportGroupState::Pending
    } else {
        // Absent, DeniedLexical, and DeniedCanonical advance the precedence
        // walk. Denials are intentionally no-probe outcomes. Cancellation is
        // handled above as a terminal non-closing state.
        ExactImportGroupState::Skippable
    }
}

/// Select the failure diagnostic for one occurrence while respecting the
/// precedence walk. A skipped failure is retained only if the walk eventually
/// has no resolution; a conclusive failure or cancellation is reported
/// immediately and cannot be hidden by a later accepted observation.
fn exact_import_failure_for_site<'groups, 'ledger>(
    groups: impl IntoIterator<Item = &'groups Arc<[ImportDiscoveryRequest]>>,
    ledger: &'ledger ImportObservationLedger,
) -> Option<&'ledger ImportObservation> {
    let mut skipped_failure = None;
    for group in groups {
        match exact_import_group_state(group, ledger) {
            ExactImportGroupState::ConclusiveFailure(failure) => return Some(failure),
            ExactImportGroupState::Cancelled(failure) => return Some(failure),
            ExactImportGroupState::Resolved(_) => return None,
            ExactImportGroupState::Pending => return skipped_failure,
            ExactImportGroupState::Skippable => {
                if skipped_failure.is_none() {
                    skipped_failure = group
                        .iter()
                        .filter_map(|request| ledger.get(request))
                        .find(|observation| observation.status().is_failure());
                }
            }
        }
    }
    skipped_failure
}

/// One occurrence's winner: the first candidate group, in precedence order,
/// that resolved. A conclusive failure or cancellation ends the walk before a
/// later candidate can win; skippable denials and absence continue it.
fn extend_accepted_from_site<'a>(
    accepted: &mut Vec<&'a AcceptedImportSource>,
    groups: impl Iterator<Item = &'a Arc<[ImportDiscoveryRequest]>>,
    ledger: &'a ImportObservationLedger,
) {
    for group in groups {
        match exact_import_group_state(group, ledger) {
            ExactImportGroupState::ConclusiveFailure(_)
            | ExactImportGroupState::Cancelled(_)
            | ExactImportGroupState::Pending => break,
            ExactImportGroupState::Skippable => {}
            ExactImportGroupState::Resolved(source) => {
                accepted.push(source);
                break;
            }
        }
    }
}

/// The canonical order and deduplication every accepted-source set is published
/// in, whichever occurrences it was reduced from.
fn canonicalize_accepted(mut accepted: Vec<&AcceptedImportSource>) -> Vec<&AcceptedImportSource> {
    accepted.sort_by(|left, right| {
        left.requested_path
            .cmp(&right.requested_path)
            .then(left.canonical_path.cmp(&right.canonical_path))
    });
    accepted.dedup_by(|left, right| {
        // A hard link has a distinct canonical path but is still one physical
        // source. Keep the first source in canonical request order as the
        // representative; the complete observation ledger remains available
        // to the close-time diagnostic projection, which can report the
        // incompatible logical identities at their import sites. Do not
        // collapse unlike observations: a changed stamp must still reach the
        // assembler's fail-closed guard.
        left.metadata_identity == right.metadata_identity
            && left.metadata_fingerprint == right.metadata_fingerprint
            && left.content_fingerprint == right.content_fingerprint
    });
    accepted
}

/// Compute candidate precedence for one exact indexed occurrence from captured
/// invocation context and the importer's accepted-read provenance.
/// The normalized candidate a relative occurrence would probe, when that
/// candidate lies outside its lexical boundary (ADR-0078: project-relative or
/// trusted-standard-library identity is total, so an escaping import is
/// rejected before any filesystem request is derived). `std` is a reserved
/// program-anchored specifier and is not subject to the check.
pub(crate) fn escaping_import_candidate(
    context: &ImportDiscoveryContext,
    specifier: &str,
    importer: &ModuleId,
    importer_requested_path: &str,
) -> CompileResult<Option<String>> {
    if specifier == "std" {
        return Ok(None);
    }
    // A specifier that is not a relative path derives no candidate at all, so
    // there is nothing to place inside or outside the boundary. Reporting it
    // here as well would give one site two diagnostics for one mistake.
    if non_relative_specifier(specifier).is_some() {
        return Ok(None);
    }
    let importer_path = normalize_absolute(importer_requested_path)?;
    // The trusted-standard-library boundary belongs to the module's accepted
    // provenance, not to whatever filesystem spelling happened to accompany
    // the read. A project source may live beneath a misconfigured std root;
    // treating that lexical fact as authority would grant it std-only
    // semantics and let it mint trusted identities.
    let boundary_root = if importer.is_trusted_standard_library() {
        context.std_root().ok_or_else(|| {
            invalid_input("trusted standard-library module has no captured std root")
        })?
    } else {
        context.project_root()
    };
    let importer_dir = parent_dir(&importer_path);
    let groups = discovery_candidate_groups(
        specifier,
        &importer_dir,
        context.project_root(),
        context.std_root(),
    );
    let candidate = normalize_path(&groups[0][0]);
    if Path::new(&candidate).starts_with(boundary_root) {
        Ok(None)
    } else {
        Ok(Some(candidate))
    }
}

/// Why a specifier is not a relative path, and so names no module. Both shapes
/// are decided from the specifier text alone, before any candidate is derived
/// (RUE-1706).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NonRelativeSpecifier {
    /// `@import("")`. It names nothing; `discovery_candidate_groups` would
    /// spell it as the root-anchored `/_.rue`, an artifact of joining an empty
    /// basename onto an empty specifier rather than a path any project holds.
    Empty,
    /// `@import("/etc/hostname.rue")`, or any other rooted path. `Path::join`
    /// REPLACES its base when the right-hand side is absolute, so such a
    /// specifier would become its own candidate, unanchored from the importing
    /// file — and one that happened to land inside the project root resolved,
    /// making the program depend on where the tree sits on this machine.
    Absolute,
}

/// Classify a specifier that resolution cannot accept. `std` is a reserved
/// program-anchored specifier (spec 10.2:6), not a path, and is exempt.
pub(crate) fn non_relative_specifier(specifier: &str) -> Option<NonRelativeSpecifier> {
    if specifier == "std" {
        return None;
    }
    if specifier.is_empty() {
        return Some(NonRelativeSpecifier::Empty);
    }
    // Deliberately `Path::is_absolute`, the same predicate `Path::join` uses to
    // decide whether to discard its base: this rejects exactly the specifiers
    // that would otherwise escape the importer anchor.
    if Path::new(specifier).is_absolute() {
        return Some(NonRelativeSpecifier::Absolute);
    }
    None
}

/// E0714 diagnostics for occurrences whose specifier is not a relative path.
/// Recomputed from the parsed program like `escape_diagnostics`, and for the
/// same reason: such an occurrence owns no discovery requests, so it appears in
/// neither the request groups nor the observation ledger.
pub(crate) fn specifier_diagnostics(program: &ParsedProgram) -> CompileErrors {
    let mut errors = CompileErrors::new();
    for site in program.import_directives().iter() {
        let Some(shape) = non_relative_specifier(site.specifier()) else {
            continue;
        };
        let kind = match shape {
            NonRelativeSpecifier::Empty => ErrorKind::ImportSpecifierEmpty,
            NonRelativeSpecifier::Absolute => ErrorKind::ImportSpecifierAbsolute {
                path: site.specifier().to_owned(),
            },
        };
        let file_id = program
            .module(site.importer())
            .expect("import directive belongs to parsed program")
            .file_id();
        let span = rue_span::Span::with_file(file_id, site.source_offset(), site.source_end());
        errors.push(CompileError::new(kind, span));
    }
    errors
}

pub(crate) fn discovery_groups_for_occurrence(
    context: &ImportDiscoveryContext,
    occurrence: &ImportOccurrenceKey,
    importer_requested_path: &str,
) -> CompileResult<Vec<Arc<[ImportDiscoveryRequest]>>> {
    // A specifier that is not a relative path derives no filesystem requests:
    // like an escaping one, its rejection is decided lexically, and the
    // diagnostic projection recomputes the same check (`specifier_diagnostics`).
    if non_relative_specifier(occurrence.specifier()).is_some() {
        return Ok(Vec::new());
    }
    // An escaping occurrence derives no filesystem requests: its rejection is
    // decided lexically from the importer anchor and its project or captured
    // standard-library root, and the diagnostic projection recomputes the same
    // check (`escape_diagnostics`).
    if escaping_import_candidate(
        context,
        occurrence.specifier(),
        occurrence.importer(),
        importer_requested_path,
    )?
    .is_some()
    {
        return Ok(Vec::new());
    }
    let importer_path = normalize_absolute(importer_requested_path)?;
    let importer_dir = parent_dir(&importer_path);
    let candidate_groups = discovery_candidate_groups(
        occurrence.specifier(),
        &importer_dir,
        context.project_root(),
        context.std_root(),
    );
    let normalized_specifier = normalize_path(occurrence.specifier());
    candidate_groups
        .into_iter()
        .enumerate()
        .map(|(group_index, candidates)| {
            let group_index = u32::try_from(group_index)
                .map_err(|_| invalid_input("import candidate group count exceeds u32::MAX"))?;
            Ok(candidates
                .into_iter()
                .enumerate()
                .map(|(position, candidate)| {
                    let position =
                        u32::try_from(position).expect("candidate group size is bounded by policy");
                    ImportDiscoveryRequest {
                        context: context.clone(),
                        occurrence: occurrence.clone(),
                        exact_specifier: Arc::from(occurrence.specifier()),
                        normalized_specifier: Arc::from(normalized_specifier.as_str()),
                        importer_anchor: Arc::from(importer_path.as_str()),
                        root_anchor: Arc::from(context.project_root()),
                        group: group_index,
                        position,
                        requested_path: Arc::from(normalize_path(&candidate)),
                        role: candidate_role(occurrence.specifier()),
                    }
                })
                .collect::<Vec<_>>()
                .into())
        })
        .collect()
}

pub(crate) fn validate_exact_import_ledger(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> CompileResult<()> {
    let requests = groups
        .iter()
        .flat_map(|group| group.iter())
        .collect::<BTreeSet<_>>();
    if ledger
        .iter()
        .any(|observation| !requests.contains(observation.request()))
    {
        return Err(invalid_input(
            "observation ledger contains a request outside canonical exact-occurrence results",
        ));
    }
    Ok(())
}

pub(crate) fn exact_import_pending_requests(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> Vec<ImportDiscoveryRequest> {
    let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
        BTreeMap::new();
    for group in groups {
        by_site
            .entry(group[0].occurrence())
            .or_default()
            .push(group);
    }
    let mut pending = Vec::new();
    for groups in by_site.values() {
        for group in groups {
            match exact_import_group_state(group, ledger) {
                ExactImportGroupState::ConclusiveFailure(_)
                | ExactImportGroupState::Cancelled(_)
                | ExactImportGroupState::Resolved(_) => break,
                ExactImportGroupState::Pending => {
                    pending.extend(
                        group
                            .iter()
                            .filter(|request| ledger.get(request).is_none())
                            .cloned(),
                    );
                    break;
                }
                ExactImportGroupState::Skippable => {}
            }
        }
    }
    pending
}

/// Whether any occurrence has a failure after its precedence walk terminates
/// without a resolution. Conclusive failures and cancellation stop the walk
/// immediately; skippable failures are reported only when no later candidate
/// resolves.
pub(crate) fn exact_import_has_failures(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> bool {
    let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
        BTreeMap::new();
    for group in groups {
        by_site
            .entry(group[0].occurrence())
            .or_default()
            .push(group);
    }
    by_site
        .values()
        .any(|groups| exact_import_failure_for_site(groups.iter().copied(), ledger).is_some())
}

/// E0713 diagnostics for occurrences whose single candidate escapes its
/// project or captured standard-library root. Recomputed from the parsed
/// program and captured context — escaping occurrences own no discovery
/// requests, so they never appear in request groups or the observation ledger.
pub(crate) fn escape_diagnostics(
    program: &ParsedProgram,
    context: &ImportDiscoveryContext,
) -> CompileErrors {
    let mut errors = CompileErrors::new();
    for site in program.import_directives().iter() {
        let importer_path = match requested_path_for_module(context, site.importer()) {
            Ok(path) => path,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        match escaping_import_candidate(context, site.specifier(), site.importer(), &importer_path)
        {
            Ok(Some(candidate)) => {
                let file_id = program
                    .module(site.importer())
                    .expect("import directive belongs to parsed program")
                    .file_id();
                let span =
                    rue_span::Span::with_file(file_id, site.source_offset(), site.source_end());
                errors.push(CompileError::new(
                    ErrorKind::ImportEscapesRoot {
                        path: site.specifier().to_owned(),
                        candidate,
                    },
                    span,
                ));
            }
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    errors
}

pub(crate) fn exact_import_diagnostics(
    program: &ParsedProgram,
    context: &ImportDiscoveryContext,
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
    accepted_reads: &AcceptedReadManifest,
) -> CompileErrors {
    let mut errors = ImportDiscoveryPlan::shape_diagnostics(program);
    errors.extend(specifier_diagnostics(program));
    errors.extend(escape_diagnostics(program, context));
    let mut groups_by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
        BTreeMap::new();
    for group in groups {
        groups_by_site
            .entry(group[0].occurrence())
            .or_default()
            .push(group);
    }
    let mut failed_sites = BTreeSet::new();
    for (site, groups) in &groups_by_site {
        let file_id = program
            .module(site.importer())
            .expect("indexed importer belongs to parsed program")
            .file_id();
        let span = rue_span::Span::with_file(file_id, site.source_offset(), site.source_end());
        if let Some(failure) = exact_import_failure_for_site(groups.iter().copied(), ledger) {
            let candidate = failure.request().requested_path();
            let message = match failure.status() {
                ImportObservationStatus::PresentUnreadable(reason) => {
                    format!("could not read import candidate '{candidate}': {reason}")
                }
                ImportObservationStatus::DeniedLexical => format!(
                    "import candidate '{candidate}' is not listed in the source manifest read policy"
                ),
                ImportObservationStatus::DeniedCanonical { canonical_path } => format!(
                    "import candidate '{candidate}' resolves to '{canonical_path}', which is not listed in the source manifest read policy"
                ),
                ImportObservationStatus::InvalidPhysicalType { canonical_path } => format!(
                    "import candidate '{candidate}' resolves to '{canonical_path}', which is not a regular source file"
                ),
                ImportObservationStatus::UnstableRead(reason) => format!(
                    "import candidate '{candidate}' changed during its stable read transaction: {reason}"
                ),
                ImportObservationStatus::Cancelled => {
                    format!("import discovery was cancelled while reading '{candidate}'")
                }
                ImportObservationStatus::Absent
                | ImportObservationStatus::PresentReadable { .. } => unreachable!(),
            };
            errors.push(CompileError::new(
                ErrorKind::InvalidCompilerInput(message),
                span,
            ));
            failed_sites.insert((*site).clone());
        }
    }
    // The assembler receives one physical representative so that an alias
    // conflict can reach this canonical close-time projection with the exact
    // import occurrence spans still available in the ledger. Seed the identity
    // map with the assembled manifest (including the root), then compare every
    // winning import under that identity in deterministic occurrence order.
    let mut physical_modules = BTreeMap::<
        PhysicalFileIdentity,
        (ModuleId, Arc<str>, Arc<str>, Arc<[PhysicalFileIdentity]>),
    >::new();
    for entry in accepted_reads.iter() {
        physical_modules
            .entry(entry.metadata_identity())
            .or_insert_with(|| {
                (
                    entry.module().clone(),
                    Arc::from(entry.canonical_path()),
                    safe_import_display(context, entry.requested_path()),
                    Arc::from(entry.symlink_route().to_vec()),
                )
            });
    }
    for (site, groups) in groups_by_site {
        if failed_sites.contains(site) {
            continue;
        }
        let file_id = program
            .module(site.importer())
            .expect("indexed importer belongs to parsed program")
            .file_id();
        let span = rue_span::Span::with_file(file_id, site.source_offset(), site.source_end());
        let winning = groups.iter().find_map(|group| {
            let present = group
                .iter()
                .filter_map(|request| ledger.get(request))
                .filter_map(ImportObservation::accepted_source)
                .collect::<Vec<_>>();
            (!present.is_empty()).then_some(present)
        });
        match winning.as_deref() {
            Some([_]) => {}
            // Policy v2 derives exactly one candidate per group.
            Some([_, _, ..]) => unreachable!("candidate groups hold one candidate"),
            Some([]) => unreachable!(),
            None if site.specifier() == "std" => {
                errors.push(CompileError::new(ErrorKind::StdLibNotFound, span));
            }
            None => errors.push(CompileError::new(
                ErrorKind::ModuleNotFound {
                    path: site.specifier().into(),
                    candidates: groups
                        .iter()
                        .flat_map(|group| group.iter())
                        .map(|request| request.requested_path().into())
                        .collect(),
                },
                span,
            )),
        }
        if let Some(source) = winning.as_ref().and_then(|sources| sources.first()) {
            let module =
                match classify_module(context, source.requested_path(), source.canonical_path()) {
                    Ok(module) => module,
                    Err(error) => {
                        errors.push(error);
                        continue;
                    }
                };
            if let Some((previous, previous_canonical, previous_spelling, previous_route)) =
                physical_modules.get(&source.metadata_identity())
            {
                let different_symlink_routes = previous_canonical.as_ref()
                    == source.canonical_path()
                    && previous_route.as_ref() != source.symlink_route();
                if previous != &module && !different_symlink_routes {
                    errors.push(CompileError::new(
                        ErrorKind::ImportSpellingsSameFile {
                            first: previous_spelling.to_string(),
                            second: site.specifier().to_owned(),
                        },
                        span,
                    ));
                }
            } else {
                physical_modules.insert(
                    source.metadata_identity(),
                    (
                        module,
                        Arc::from(source.canonical_path()),
                        Arc::from(site.specifier()),
                        Arc::from(source.symlink_route().to_vec()),
                    ),
                );
            }
        }
    }
    errors
}

/// Render an accepted manifest path without exposing compiler-only logical
/// module namespaces (notably the NUL-prefixed trusted standard-library IDs).
fn safe_import_display(context: &ImportDiscoveryContext, path: &str) -> Arc<str> {
    let path = Path::new(path);
    if let Some(std_root) = context.std_root()
        && let Ok(relative) = path.strip_prefix(std_root)
    {
        return Arc::from(format!("std/{}", relative.to_string_lossy()));
    }
    if let Ok(relative) = path.strip_prefix(context.project_root()) {
        return Arc::from(relative.to_string_lossy().into_owned());
    }
    Arc::from(path.to_string_lossy().into_owned())
}

pub(crate) fn reduce_exact_import_graph(
    root: ModuleId,
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
    accepted_reads: &AcceptedReadManifest,
) -> CompileResult<crate::CanonicalImportGraph> {
    validate_exact_import_ledger(groups, ledger)?;
    let manifest = accepted_import_manifest(accepted_reads)?;
    let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
        BTreeMap::new();
    for group in groups {
        by_site
            .entry(group[0].occurrence())
            .or_default()
            .push(group);
    }
    let mut records = Vec::with_capacity(by_site.len());
    for (site, groups) in by_site {
        let winner = exact_import_winner(groups.iter().copied(), ledger);
        let resolution = resolve_exact_import_winner(winner, |source| {
            module_for_accepted_source(source, &manifest)
        })?;
        records.push(crate::CanonicalImportRecord::new(
            site.importer().clone(),
            site.specifier(),
            resolution,
        ));
    }
    Ok(crate::CanonicalImportGraph::from_discovery_records(
        root, records,
    ))
}

/// Validate one demand-scoped exact occurrence before applying the canonical
/// winning-group policy. Observations for other occurrences may coexist in the
/// ledger because declaration queries intentionally share an import revision.
pub(crate) fn validate_exact_import_occurrence(
    groups: &[Arc<[ImportDiscoveryRequest]>],
    ledger: &ImportObservationLedger,
) -> CompileResult<()> {
    let first = groups
        .first()
        .and_then(|group| group.first())
        .ok_or_else(|| invalid_input("exact import occurrence has no candidate groups"))?;
    if groups.iter().any(|group| {
        group.is_empty()
            || group
                .iter()
                .any(|request| request.occurrence() != first.occurrence())
    }) {
        return Err(invalid_input(
            "exact import occurrence groups contain mixed or empty sites",
        ));
    }
    let requests = groups
        .iter()
        .flat_map(|group| group.iter())
        .collect::<BTreeSet<_>>();
    let canonical_present = requests
        .iter()
        .filter(|request| ledger.get(request).is_some())
        .count();
    if ledger.observation_count_for_occurrence(first.occurrence()) != canonical_present {
        return Err(invalid_input(
            "observation ledger contains a non-canonical request for the exact occurrence",
        ));
    }
    Ok(())
}

pub(crate) fn validate_accepted_import_manifest(
    accepted_reads: &AcceptedReadManifest,
) -> CompileResult<()> {
    accepted_import_manifest(accepted_reads).map(drop)
}

fn accepted_import_manifest(
    accepted_reads: &AcceptedReadManifest,
) -> CompileResult<BTreeMap<PhysicalFileIdentity, &AcceptedReadManifestEntry>> {
    let manifest = accepted_reads
        .iter()
        .map(|entry| (entry.metadata_identity(), entry))
        .collect::<BTreeMap<_, _>>();
    if manifest.len() != accepted_reads.len() {
        return Err(invalid_input(
            "accepted read manifest contains duplicate physical identities",
        ));
    }
    Ok(manifest)
}

/// The one canonical precedence decision for an exact occurrence. The winner
/// borrows accepted-source evidence from the immutable observation ledger;
/// consumers decide how exact provenance maps those identities to modules.
pub(crate) enum ExactImportWinner<'a> {
    Missing,
    Resolved(&'a AcceptedImportSource),
}

pub(crate) fn exact_import_winner<'ledger, 'groups>(
    groups: impl IntoIterator<Item = &'groups Arc<[ImportDiscoveryRequest]>>,
    ledger: &'ledger ImportObservationLedger,
) -> ExactImportWinner<'ledger> {
    for group in groups {
        match exact_import_group_state(group, ledger) {
            ExactImportGroupState::Resolved(source) => return ExactImportWinner::Resolved(source),
            ExactImportGroupState::ConclusiveFailure(_)
            | ExactImportGroupState::Cancelled(_)
            | ExactImportGroupState::Pending => {
                return ExactImportWinner::Missing;
            }
            ExactImportGroupState::Skippable => {}
        }
    }
    ExactImportWinner::Missing
}

/// Obtain the canonical resolution by visiting exactly the accepted sources
/// selected by precedence. Query consumers inject an exact provenance lookup;
/// whole-graph closure injects its already-validated manifest map.
pub(crate) fn resolve_exact_import_winner<E>(
    winner: ExactImportWinner<'_>,
    mut module_for: impl FnMut(&AcceptedImportSource) -> Result<ModuleId, E>,
) -> Result<crate::CanonicalImportResolution, E> {
    Ok(match winner {
        ExactImportWinner::Missing => crate::CanonicalImportResolution::Missing,
        ExactImportWinner::Resolved(source) => {
            crate::CanonicalImportResolution::Resolved(module_for(source)?)
        }
    })
}

fn module_for_accepted_source(
    source: &AcceptedImportSource,
    manifest: &BTreeMap<PhysicalFileIdentity, &AcceptedReadManifestEntry>,
) -> CompileResult<ModuleId> {
    let entry = manifest.get(&source.metadata_identity()).ok_or_else(|| {
        invalid_input("accepted observation is absent from accepted read provenance")
    })?;
    validate_accepted_import_source(source, entry)
}

pub(crate) fn accepted_import_module(
    source: &AcceptedImportSource,
    accepted_reads: &AcceptedReadManifest,
    meter: &crate::source_snapshot::IdentityResolutionMeter,
) -> CompileResult<ModuleId> {
    let entry = match accepted_reads.lookup_physical_identity(source.metadata_identity(), meter) {
        ManifestIdentityLookup::Unique(entry) => entry,
        ManifestIdentityLookup::Absent => {
            return Err(invalid_input(
                "accepted observation is absent from accepted read provenance",
            ));
        }
        ManifestIdentityLookup::Duplicate => {
            return Err(invalid_input(
                "accepted read manifest contains duplicate physical identities",
            ));
        }
    };
    validate_accepted_import_source(source, entry)
}

fn validate_accepted_import_source(
    source: &AcceptedImportSource,
    entry: &AcceptedReadManifestEntry,
) -> CompileResult<ModuleId> {
    if entry.metadata_fingerprint() != source.metadata_fingerprint()
        || entry.content_fingerprint() != source.content_fingerprint()
    {
        return Err(invalid_input(
            "accepted observation does not match accepted read provenance",
        ));
    }
    Ok(entry.module().clone())
}

/// Compiler-owned durable identity assembly for a discovery epoch.
#[derive(Debug, Clone)]
pub struct DiscoverySourceAssembler {
    context: ImportDiscoveryContext,
    root_module: ModuleId,
    entries: BTreeMap<ModuleId, AssembledSource>,
    physical: BTreeMap<PhysicalFileIdentity, PhysicalSourceOwner>,
    canonical_identities: BTreeMap<Arc<str>, PhysicalFileIdentity>,
    accepted_reads: BTreeMap<ModuleId, AcceptedReadManifestEntry>,
    /// The last produced snapshot; the next [`Self::snapshot`] extends it with
    /// only the modules added since, using bounded shared size tiers (RUE-1112).
    /// Predecessor `FileId`s stay stable across extensions.
    cached_snapshot: Option<SourceSnapshot>,
    /// Modules added since `cached_snapshot` was produced, in insertion order.
    appended_since_snapshot: Vec<ModuleId>,
    /// Cumulative sources materialized by full snapshot builds (the first
    /// snapshot of a lineage); never incremented on the extension path.
    snapshot_sources_rebuilt: u64,
    /// Cumulative sources appended by extension builds — the only per-round
    /// snapshot work once a lineage's first snapshot exists.
    snapshot_sources_appended: u64,
    /// The last produced provenance manifest; the next
    /// [`Self::accepted_read_manifest`] extends it with only the entries added
    /// since, using bounded shared size tiers (RUE-1112). Invalidated when a
    /// retained entry is rewritten (a canonical-path improvement), which falls
    /// back to a counted full rebuild.
    cached_manifest: Option<AcceptedReadManifest>,
    /// Modules whose provenance entries were added since `cached_manifest` was
    /// produced, in insertion order.
    appended_since_manifest: Vec<ModuleId>,
    /// Cumulative provenance entries materialized by full manifest builds;
    /// never incremented on the extension path.
    manifest_entries_rebuilt: u64,
    /// Cumulative provenance entries appended by extension manifest builds —
    /// the only per-round manifest work once a lineage's first manifest exists.
    manifest_entries_appended: u64,
    /// Cumulative accepted sources reduced out of a plan and offered to this
    /// assembler — the exact per-round read volume. A round that reduces the
    /// whole plan offers every already assembled leaf again, so this grows as
    /// rounds times plan; a round that reduces only the occurrences it changed
    /// keeps it linear in the discovered module count.
    plan_reads_reduced: u64,
}

#[derive(Debug, Clone)]
struct PhysicalSourceOwner {
    module: ModuleId,
    canonical_path: Arc<str>,
}

#[derive(Debug, Clone)]
struct AssembledSource {
    requested_path: Arc<str>,
    canonical_path: Arc<str>,
    source: Arc<String>,
}

/// The accepted-read provenance manifest as a persistent segmented sequence
/// (RUE-1112): bounded size-tiered segments plus an exact appended delta, sorted
/// by module identity. Untouched tiers and the lazily materialized contiguous
/// slice are shared by clones. Equality and hashing are logical over the merged
/// sequence.
#[derive(Clone)]
pub struct AcceptedReadManifest {
    entries: crate::shared_segments::SharedSegments<AcceptedReadManifestEntry>,
    /// Lazily materialized merged slice for consumers that need `Arc<[T]>`
    /// (the compiler-owned staging boundary and retained artifacts).
    merged: Arc<std::sync::OnceLock<Arc<[AcceptedReadManifestEntry]>>>,
    /// Lazily materialized inverse of the entry sequence: host-observed
    /// physical identity to its entry's segment position. The sequence is
    /// sorted by module identity, so a physical identity has no binary search;
    /// authorizing a discovery batch asks this question once per accepted
    /// observation, which is a scan of every entry per observation without an
    /// index. Derived state: it never participates in equality, hashing, or
    /// rendering, and clones share it because they are the same value.
    by_physical_identity: Arc<std::sync::OnceLock<AHashMap<PhysicalFileIdentity, IdentitySlot>>>,
}

/// Where one physical identity lives in a manifest's segmented entry sequence,
/// or that the manifest names it more than once.
#[derive(Debug, Clone, Copy)]
enum IdentitySlot {
    Unique { segment: u32, position: u32 },
    Duplicate,
}

/// The outcome of resolving one host-observed physical identity against a
/// manifest.
pub(crate) enum ManifestIdentityLookup<'a> {
    Absent,
    Unique(&'a AcceptedReadManifestEntry),
    Duplicate,
}

fn accepted_read_order(
    a: &AcceptedReadManifestEntry,
    b: &AcceptedReadManifestEntry,
) -> std::cmp::Ordering {
    a.module.cmp(&b.module)
}

/// Logical rendering over the merged entry sequence: the lazily materialized
/// merged cache is derived state and never participates.
impl std::fmt::Debug for AcceptedReadManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Logical equality over the merged entry sequence: the lazily materialized
/// merged cache is derived state and never participates.
impl PartialEq for AcceptedReadManifest {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for AcceptedReadManifest {}

impl std::hash::Hash for AcceptedReadManifest {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.entries.hash(state);
    }
}

impl AcceptedReadManifest {
    /// A complete manifest built in one piece (fresh discovery). Entries are
    /// canonicalized to module order here; duplicate modules are preserved for
    /// the publication validation to reject.
    pub(crate) fn from_entries(mut entries: Vec<AcceptedReadManifestEntry>) -> Self {
        entries.sort_by(accepted_read_order);
        Self {
            entries: crate::shared_segments::SharedSegments::flat(
                entries.into(),
                accepted_read_order,
            ),
            merged: Arc::new(std::sync::OnceLock::new()),
            by_physical_identity: Arc::new(std::sync::OnceLock::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_shared(entries: Arc<[AcceptedReadManifestEntry]>) -> Self {
        if !entries.is_sorted_by(|a, b| accepted_read_order(a, b) != std::cmp::Ordering::Greater) {
            return Self::from_entries(entries.to_vec());
        }
        let merged = std::sync::OnceLock::new();
        let _ = merged.set(entries.clone());
        Self {
            entries: crate::shared_segments::SharedSegments::flat(entries, accepted_read_order),
            merged: Arc::new(merged),
            by_physical_identity: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// A strictly-additive successor sharing every segment of `base` by
    /// reference and appending only `appended` (sorted here; must be new
    /// modules).
    pub(crate) fn extend_with_appended(
        base: &AcceptedReadManifest,
        appended: Vec<AcceptedReadManifestEntry>,
    ) -> Self {
        Self {
            entries: crate::shared_segments::SharedSegments::extend(&base.entries, appended),
            merged: Arc::new(std::sync::OnceLock::new()),
            by_physical_identity: Arc::new(std::sync::OnceLock::new()),
        }
    }

    pub(crate) fn segments(
        &self,
    ) -> &crate::shared_segments::SharedSegments<AcceptedReadManifestEntry> {
        &self.entries
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &AcceptedReadManifestEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.len() == 0
    }

    /// The provenance entry for `module`, by per-segment binary search.
    pub(crate) fn find_module(&self, module: &ModuleId) -> Option<&AcceptedReadManifestEntry> {
        self.entries.find_by(|entry| entry.module.cmp(module))
    }

    /// The entry a host-observed physical identity names, by one hash lookup
    /// into the lazily materialized inverse index.
    ///
    /// A manifest that names the same physical file twice reports
    /// [`ManifestIdentityLookup::Duplicate`] rather than picking a winner, so
    /// the ambiguity still reaches the caller that must reject it.
    ///
    /// `meter` records the lookup and every manifest entry examined to answer
    /// it, including the entries walked once to materialize the index. Charging
    /// the materialization to its triggering lookup keeps the counter an honest
    /// total: a manifest that is rebuilt and re-indexed once per module would
    /// show the same quadratic entry count the scan showed.
    pub(crate) fn lookup_physical_identity(
        &self,
        identity: PhysicalFileIdentity,
        meter: &crate::source_snapshot::IdentityResolutionMeter,
    ) -> ManifestIdentityLookup<'_> {
        let mut visited = 0;
        let index = self.by_physical_identity.get_or_init(|| {
            let mut index: AHashMap<PhysicalFileIdentity, IdentitySlot> =
                AHashMap::with_capacity(self.entries.len());
            for (segment, entries) in self.entries.segments().iter().enumerate() {
                for (position, entry) in entries.iter().enumerate() {
                    visited += 1;
                    index
                        .entry(entry.metadata_identity)
                        .and_modify(|slot| *slot = IdentitySlot::Duplicate)
                        .or_insert(IdentitySlot::Unique {
                            segment: segment as u32,
                            position: position as u32,
                        });
                }
            }
            index
        });
        // The hash probe itself reads at most the one entry it names.
        meter.record_physical_identity_lookup(visited + 1);
        match index.get(&identity) {
            None => ManifestIdentityLookup::Absent,
            Some(IdentitySlot::Duplicate) => ManifestIdentityLookup::Duplicate,
            Some(IdentitySlot::Unique { segment, position }) => ManifestIdentityLookup::Unique(
                &self.entries.segments()[*segment as usize][*position as usize],
            ),
        }
    }

    /// Whether `entry` appears byte-identical in this manifest.
    pub(crate) fn contains_entry(&self, entry: &AcceptedReadManifestEntry) -> bool {
        self.find_module(&entry.module) == Some(entry)
    }

    /// The merged contiguous manifest, materialized lazily at most once per
    /// value; a flat manifest shares its single segment without copying. The
    /// acquisition path's publication never calls this.
    pub fn shared_slice(&self) -> Arc<[AcceptedReadManifestEntry]> {
        self.merged
            .get_or_init(|| {
                let segments = self.entries.segments();
                if segments.len() == 1 {
                    segments[0].clone()
                } else {
                    self.entries.iter().cloned().collect::<Vec<_>>().into()
                }
            })
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AcceptedReadManifestEntry {
    module: ModuleId,
    requested_path: Arc<str>,
    canonical_path: Arc<str>,
    symlink_route: Arc<[PhysicalFileIdentity]>,
    metadata_identity: PhysicalFileIdentity,
    metadata_fingerprint: FileMetadataFingerprint,
    content_fingerprint: u64,
}

impl AcceptedReadManifestEntry {
    pub fn module(&self) -> &ModuleId {
        &self.module
    }
    pub fn requested_path(&self) -> &str {
        &self.requested_path
    }
    pub fn canonical_path(&self) -> &str {
        &self.canonical_path
    }
    pub fn symlink_route(&self) -> &[PhysicalFileIdentity] {
        &self.symlink_route
    }
    pub fn content_fingerprint(&self) -> u64 {
        self.content_fingerprint
    }
    pub fn metadata_identity(&self) -> PhysicalFileIdentity {
        self.metadata_identity
    }
    pub fn metadata_fingerprint(&self) -> FileMetadataFingerprint {
        self.metadata_fingerprint
    }
}

impl DiscoverySourceAssembler {
    pub fn new(
        context: ImportDiscoveryContext,
        root_requested_path: impl Into<Arc<str>>,
        root_canonical_path: impl Into<Arc<str>>,
        metadata_identity: PhysicalFileIdentity,
        metadata_fingerprint: FileMetadataFingerprint,
        root_source: Arc<String>,
    ) -> CompileResult<Self> {
        Self::new_with_symlink_route(
            context,
            root_requested_path,
            root_canonical_path,
            metadata_identity,
            metadata_fingerprint,
            root_source,
            Arc::from([]),
        )
    }

    pub fn new_with_symlink_route(
        context: ImportDiscoveryContext,
        root_requested_path: impl Into<Arc<str>>,
        root_canonical_path: impl Into<Arc<str>>,
        metadata_identity: PhysicalFileIdentity,
        metadata_fingerprint: FileMetadataFingerprint,
        root_source: Arc<String>,
        symlink_route: Arc<[PhysicalFileIdentity]>,
    ) -> CompileResult<Self> {
        let requested_path = Arc::from(normalize_absolute(&root_requested_path.into())?);
        let canonical_path = Arc::from(normalize_absolute(&root_canonical_path.into())?);
        let module = classify_module(&context, &requested_path, &canonical_path)?;
        let assembled = AssembledSource {
            requested_path: requested_path.clone(),
            canonical_path: canonical_path.clone(),
            source: root_source.clone(),
        };
        let mut entries = BTreeMap::new();
        entries.insert(module.clone(), assembled);
        let mut physical = BTreeMap::new();
        physical.insert(
            metadata_identity,
            PhysicalSourceOwner {
                module: module.clone(),
                canonical_path: canonical_path.clone(),
            },
        );
        let canonical_identities = BTreeMap::from([(canonical_path.clone(), metadata_identity)]);
        let mut accepted_reads = BTreeMap::new();
        accepted_reads.insert(
            module.clone(),
            AcceptedReadManifestEntry {
                module: module.clone(),
                requested_path,
                canonical_path: canonical_path.clone(),
                symlink_route,
                metadata_identity,
                metadata_fingerprint,
                content_fingerprint: source_fingerprint(&root_source),
            },
        );
        Ok(Self {
            context,
            root_module: module,
            entries,
            physical,
            canonical_identities,
            accepted_reads,
            cached_snapshot: None,
            appended_since_snapshot: Vec::new(),
            snapshot_sources_rebuilt: 0,
            snapshot_sources_appended: 0,
            cached_manifest: None,
            appended_since_manifest: Vec::new(),
            manifest_entries_rebuilt: 0,
            manifest_entries_appended: 0,
            plan_reads_reduced: 0,
        })
    }

    pub fn add_explicit(
        &mut self,
        requested_path: &str,
        canonical_path: &str,
        metadata_identity: PhysicalFileIdentity,
        metadata_fingerprint: FileMetadataFingerprint,
        source: Arc<String>,
    ) -> CompileResult<bool> {
        self.add_explicit_with_symlink_route(
            requested_path,
            canonical_path,
            metadata_identity,
            metadata_fingerprint,
            source,
            Arc::from([]),
        )
    }

    pub fn add_explicit_with_symlink_route(
        &mut self,
        requested_path: &str,
        canonical_path: &str,
        metadata_identity: PhysicalFileIdentity,
        metadata_fingerprint: FileMetadataFingerprint,
        source: Arc<String>,
        symlink_route: Arc<[PhysicalFileIdentity]>,
    ) -> CompileResult<bool> {
        self.add_source(
            &AcceptedImportSource::new(
                Arc::from(normalize_absolute(requested_path)?),
                Arc::from(canonical_path),
                metadata_identity,
                metadata_fingerprint,
                source,
            )?
            .with_symlink_route(symlink_route),
        )
    }

    pub fn add_plan_reads(
        &mut self,
        plan: &ImportDiscoveryPlan,
        ledger: &ImportObservationLedger,
    ) -> CompileResult<usize> {
        if plan.context != self.context {
            return Err(invalid_input(
                "discovery plan belongs to a different epoch or captured context",
            ));
        }
        let reduced = plan.accepted_sources(ledger);
        self.plan_reads_reduced = self.plan_reads_reduced.saturating_add(reduced.len() as u64);
        let mut added = 0;
        for source in reduced {
            added += usize::from(self.add_reduced_source(source)?);
        }
        Ok(added)
    }

    /// Add the accepted reads one discovery wave reduced (ADR-0075).
    ///
    /// The wave reduced them hop by hop from its own running ledger, in the same
    /// canonical order and by the same candidate precedence the rounds it
    /// replaces would have used, so this adds exactly what those rounds' plan
    /// reductions would have added.
    pub fn add_wave_reads(&mut self, wave: &ImportDiscoveryWave) -> CompileResult<usize> {
        if wave.context != self.context {
            return Err(invalid_input(
                "discovery wave belongs to a different epoch or captured context",
            ));
        }
        let reduced = wave.accepted_reads();
        self.plan_reads_reduced = self.plan_reads_reduced.saturating_add(reduced.len() as u64);
        let mut added = 0;
        for source in reduced {
            added += usize::from(self.add_reduced_source(source)?);
        }
        Ok(added)
    }

    /// Add only the accepted reads of a trusted-toolchain successor plan's delta
    /// groups (RUE-1112). The predecessor leaves are already assembled, so the
    /// merged plan and its winner map are never rebuilt.
    pub fn add_successor_plan_reads(
        &mut self,
        plan: &ImportDiscoveryPlan,
        ledger: &ImportObservationLedger,
    ) -> CompileResult<usize> {
        if plan.context != self.context {
            return Err(invalid_input(
                "discovery plan belongs to a different epoch or captured context",
            ));
        }
        let reduced = plan.delta_accepted_sources(ledger);
        self.plan_reads_reduced = self.plan_reads_reduced.saturating_add(reduced.len() as u64);
        let mut added = 0;
        for source in reduced {
            added += usize::from(self.add_reduced_source(source)?);
        }
        Ok(added)
    }

    /// Reduce a canonical accepted-source projection against sources already
    /// assembled in this lineage. A root/import alias can share a physical
    /// identity before the wave's own projection sees both entries; retain the
    /// existing representative so the assembler backstop remains reserved for
    /// malformed direct input, while close-time diagnostics reject the alias.
    fn add_reduced_source(&mut self, source: &AcceptedImportSource) -> CompileResult<bool> {
        let module = classify_module(
            &self.context,
            source.requested_path(),
            source.canonical_path(),
        )?;
        if let Some(previous_identity) = self.canonical_identities.get(source.canonical_path())
            && previous_identity != &source.metadata_identity
        {
            return Err(invalid_input(format!(
                "canonical source {:?} changed physical identity during discovery epoch",
                source.canonical_path()
            )));
        }
        if let Some(owner) = self.physical.get(&source.metadata_identity()) {
            let existing = self
                .entries
                .get(&owner.module)
                .expect("physical map references entry");
            if source_fingerprint(&existing.source) != source.content_fingerprint {
                return Err(invalid_input(format!(
                    "physical source {:?} changed during discovery epoch",
                    source.canonical_path()
                )));
            }
            let provenance = self
                .accepted_reads
                .get(&owner.module)
                .expect("accepted source retains provenance");
            if provenance.metadata_identity != source.metadata_identity
                || provenance.metadata_fingerprint != source.metadata_fingerprint
            {
                return Err(invalid_input(format!(
                    "physical source {:?} metadata changed during discovery epoch",
                    source.canonical_path()
                )));
            }
            if owner.module != module {
                // The close-time diagnostic projection owns the user-facing
                // conflict. All source and observation invariants were checked
                // above before retaining the existing representative.
                return Ok(false);
            }
        }
        self.add_source(source)
    }

    fn add_source(&mut self, source: &AcceptedImportSource) -> CompileResult<bool> {
        let module = classify_module(
            &self.context,
            source.requested_path(),
            source.canonical_path(),
        )?;
        if let Some(previous_identity) = self.canonical_identities.get(source.canonical_path())
            && previous_identity != &source.metadata_identity
        {
            return Err(invalid_input(format!(
                "canonical source {:?} changed physical identity during discovery epoch",
                source.canonical_path()
            )));
        }
        if let Some(owner) = self.physical.get(&source.metadata_identity).cloned() {
            if owner.module != module {
                return Err(invalid_input(format!(
                    "physical source identity for {:?} is claimed by incompatible logical IDs {} and {}",
                    source.canonical_path(),
                    owner.module,
                    module
                )));
            }
            let existing = self
                .entries
                .get(&owner.module)
                .expect("physical map references entry");
            if source_fingerprint(&existing.source) != source.content_fingerprint {
                return Err(invalid_input(format!(
                    "physical source {:?} changed during discovery epoch",
                    source.canonical_path()
                )));
            }
            let provenance = self
                .accepted_reads
                .get(&owner.module)
                .expect("accepted source retains provenance");
            if provenance.metadata_identity != source.metadata_identity
                || provenance.metadata_fingerprint != source.metadata_fingerprint
            {
                return Err(invalid_input(format!(
                    "physical source {:?} metadata changed during discovery epoch",
                    source.canonical_path()
                )));
            }
            if source.canonical_path() < owner.canonical_path.as_ref() {
                // The canonical representative may improve only while this
                // entry is PENDING — not yet carried by any produced snapshot
                // or manifest. Once produced, the representative is frozen:
                // rewriting it would diverge the cached shared snapshot from
                // the provenance manifest and mutate published predecessor
                // state, which the strictly-additive publication rejects. A
                // later alias still registers below and joins by physical
                // identity; only the retained display/provenance path keeps
                // its published spelling.
                let snapshot_pending = self.cached_snapshot.is_none()
                    || self.appended_since_snapshot.contains(&owner.module);
                let manifest_pending = self.cached_manifest.is_none()
                    || self.appended_since_manifest.contains(&owner.module);
                if snapshot_pending && manifest_pending {
                    self.entries.get_mut(&owner.module).unwrap().canonical_path =
                        source.canonical_path.clone();
                    self.accepted_reads
                        .get_mut(&owner.module)
                        .unwrap()
                        .canonical_path = source.canonical_path.clone();
                    self.accepted_reads
                        .get_mut(&owner.module)
                        .unwrap()
                        .symlink_route = source.symlink_route.clone();
                    self.physical
                        .get_mut(&source.metadata_identity)
                        .unwrap()
                        .canonical_path = source.canonical_path.clone();
                }
            }
            self.canonical_identities
                .insert(source.canonical_path.clone(), source.metadata_identity);
            return Ok(false);
        }
        if let Some(existing) = self.entries.get(&module) {
            return Err(invalid_input(format!(
                "logical module {} names distinct physical sources {:?} and {:?}",
                module,
                existing.canonical_path,
                source.canonical_path()
            )));
        }
        self.physical.insert(
            source.metadata_identity,
            PhysicalSourceOwner {
                module: module.clone(),
                canonical_path: source.canonical_path.clone(),
            },
        );
        self.canonical_identities
            .insert(source.canonical_path.clone(), source.metadata_identity);
        self.entries.insert(
            module.clone(),
            AssembledSource {
                requested_path: source.requested_path.clone(),
                canonical_path: source.canonical_path.clone(),
                source: source.source.clone(),
            },
        );
        self.appended_since_snapshot.push(module.clone());
        self.appended_since_manifest.push(module.clone());
        self.accepted_reads.insert(
            module.clone(),
            AcceptedReadManifestEntry {
                module,
                requested_path: source.requested_path.clone(),
                canonical_path: source.canonical_path.clone(),
                symlink_route: source.symlink_route.clone(),
                metadata_identity: source.metadata_identity,
                metadata_fingerprint: source.metadata_fingerprint,
                content_fingerprint: source.content_fingerprint,
            },
        );
        Ok(true)
    }

    pub fn snapshot(&mut self) -> CompileResult<SourceSnapshot> {
        // Extension route: once a lineage's first snapshot exists, each
        // successor snapshot appends only the modules added since. Untouched
        // tiers remain shared; compaction copies metadata but never re-hashes or
        // copies source bytes, and predecessor FileIds stay stable.
        if let Some(cached) = self.cached_snapshot.clone() {
            if self.appended_since_snapshot.is_empty() {
                return Ok(cached);
            }
            // Reject a file count the u32 identifier cannot distinguish before
            // the `usize -> u32` narrowing below wraps two sources onto one
            // FileId (spec C.3:2, C.1:2).
            crate::source_snapshot::validate_source_file_count(
                cached.len() + self.appended_since_snapshot.len(),
            )?;
            let next_index = cached.len() as u32;
            let appended: Vec<crate::source_snapshot::AppendedSource> = self
                .appended_since_snapshot
                .iter()
                .enumerate()
                .map(|(offset, module)| {
                    let entry = self
                        .entries
                        .get(module)
                        .expect("appended modules retain their assembled entries");
                    crate::source_snapshot::AppendedSource {
                        file_id: FileId::new(next_index + offset as u32 + 1),
                        physical_path: display_path_for_module(module, entry),
                        logical_path: module.as_str().to_owned(),
                        trusted_standard_library: module.is_trusted_standard_library(),
                        text: entry.source.clone(),
                    }
                })
                .collect();
            self.snapshot_sources_appended = self
                .snapshot_sources_appended
                .saturating_add(appended.len() as u64);
            let snapshot = SourceSnapshot::extend_with_appended(&cached, appended)?;
            self.cached_snapshot = Some(snapshot.clone());
            self.appended_since_snapshot.clear();
            return Ok(snapshot);
        }
        let root_module = &self.root_module;
        let ordered = std::iter::once((root_module, self.entries.get(root_module).unwrap())).chain(
            self.entries
                .iter()
                .filter(|(module, _)| *module != root_module),
        );
        let ordered = ordered.collect::<Vec<_>>();
        crate::source_snapshot::validate_source_file_count(ordered.len())?;
        let physical_paths = ordered
            .iter()
            .enumerate()
            .map(|(index, (module, entry))| {
                (
                    FileId::new((index + 1) as u32),
                    display_path_for_module(module, entry),
                )
            })
            .collect();
        let logical_paths = ordered
            .iter()
            .enumerate()
            .map(|(index, (module, _))| {
                (FileId::new((index + 1) as u32), module.as_str().to_owned())
            })
            .collect();
        let trusted_standard_library_files = ordered
            .iter()
            .enumerate()
            .filter_map(|(index, (module, _))| {
                module
                    .is_trusted_standard_library()
                    .then_some(FileId::new((index + 1) as u32))
            })
            .collect();
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            FileId::new(1),
            physical_paths,
            logical_paths,
            trusted_standard_library_files,
        )?;
        let contents = ordered
            .into_iter()
            .enumerate()
            .map(|(index, (_, entry))| (FileId::new((index + 1) as u32), entry.source.clone()))
            .collect();
        self.snapshot_sources_rebuilt = self
            .snapshot_sources_rebuilt
            .saturating_add(self.entries.len() as u64);
        let snapshot = SourceSnapshot::new(metadata, contents)?;
        self.cached_snapshot = Some(snapshot.clone());
        self.appended_since_snapshot.clear();
        Ok(snapshot)
    }

    /// Cumulative sources materialized by full snapshot builds; the extension
    /// path never increments this.
    pub fn snapshot_sources_rebuilt(&self) -> u64 {
        self.snapshot_sources_rebuilt
    }

    /// Cumulative sources appended by extension snapshot builds.
    pub fn snapshot_sources_appended(&self) -> u64 {
        self.snapshot_sources_appended
    }

    /// The accepted-read provenance manifest. Once a lineage's first manifest
    /// exists, each successor manifest appends only the entries added since.
    /// Untouched tiers remain shared and equal-magnitude tails compact without
    /// re-validating entries (RUE-1112).
    pub fn accepted_read_manifest(&mut self) -> AcceptedReadManifest {
        if let Some(cached) = self.cached_manifest.clone() {
            if self.appended_since_manifest.is_empty() {
                return cached;
            }
            let appended: Vec<AcceptedReadManifestEntry> = self
                .appended_since_manifest
                .iter()
                .map(|module| {
                    self.accepted_reads
                        .get(module)
                        .expect("appended modules retain their provenance entries")
                        .clone()
                })
                .collect();
            self.manifest_entries_appended = self
                .manifest_entries_appended
                .saturating_add(appended.len() as u64);
            let manifest = AcceptedReadManifest::extend_with_appended(&cached, appended);
            self.cached_manifest = Some(manifest.clone());
            self.appended_since_manifest.clear();
            return manifest;
        }
        self.manifest_entries_rebuilt = self
            .manifest_entries_rebuilt
            .saturating_add(self.accepted_reads.len() as u64);
        let manifest =
            AcceptedReadManifest::from_entries(self.accepted_reads.values().cloned().collect());
        self.cached_manifest = Some(manifest.clone());
        self.appended_since_manifest.clear();
        manifest
    }

    /// Cumulative provenance entries materialized by full manifest builds; the
    /// extension path never increments this.
    pub fn manifest_entries_rebuilt(&self) -> u64 {
        self.manifest_entries_rebuilt
    }

    /// Cumulative provenance entries appended by extension manifest builds.
    pub fn manifest_entries_appended(&self) -> u64 {
        self.manifest_entries_appended
    }

    /// Cumulative accepted sources reduced out of a plan and offered to this
    /// assembler; see the field docs on `plan_reads_reduced`. This is the exact
    /// per-round read volume, so a host can prove its rounds stay linear in the
    /// discovered module count rather than rounds times plan.
    pub fn plan_reads_reduced(&self) -> u64 {
        self.plan_reads_reduced
    }
}

fn candidate_role(specifier: &str) -> ImportCandidateRole {
    if specifier == "std" {
        ImportCandidateRole::StandardLibraryFacade
    } else if specifier.ends_with(".rue") {
        ImportCandidateRole::ExactFile
    } else {
        ImportCandidateRole::DirectoryFacade
    }
}

/// Derive the ordered candidate groups for one occurrence under policy
/// version 2 (ADR-0078). Every group holds exactly one candidate:
///
/// - `std` probes the program's vendored `{root}/std/_std.rue` first, then
///   the captured toolchain root's facade. Ambient environment never replaces
///   a standard library the program ships.
/// - A `.rue`-suffixed specifier names exactly that path relative to the
///   importing file's directory.
/// - An extensionless specifier names the directory facade
///   `{P}/_{basename}.rue`, importer-relative. File modules are spelled with
///   their extension; there is no file/facade ambiguity and no root-relative
///   fallback.
fn discovery_candidate_groups(
    specifier: &str,
    importer_dir: &str,
    project_root: &str,
    std_root: Option<&str>,
) -> Vec<Vec<String>> {
    let join = |base: &str, relative: &str| {
        Path::new(base)
            .join(relative)
            .to_string_lossy()
            .into_owned()
    };
    if specifier == "std" {
        return std::iter::once(vec![join(project_root, "std/_std.rue")])
            .chain(
                std_root
                    .into_iter()
                    .map(|root| vec![join(root, "_std.rue")]),
            )
            .collect();
    }
    if specifier.ends_with(".rue") {
        return vec![vec![join(importer_dir, specifier)]];
    }
    let basename = Path::new(specifier)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(specifier);
    vec![vec![join(
        importer_dir,
        &format!("{specifier}/_{basename}.rue"),
    )]]
}

fn requested_path_for_module(
    context: &ImportDiscoveryContext,
    module: &ModuleId,
) -> CompileResult<String> {
    if let Some(relative) = module
        .as_str()
        .strip_prefix(TRUSTED_STANDARD_LIBRARY_NAMESPACE)
    {
        let std_root = context
            .std_root()
            .ok_or_else(|| invalid_input("standard-library module has no captured std root"))?;
        return Ok(normalize_path(
            &Path::new(std_root).join(relative).to_string_lossy(),
        ));
    }
    Ok(normalize_path(
        &Path::new(context.project_root())
            .join(module.as_str())
            .to_string_lossy(),
    ))
}

/// Derive the trusted logical path (`\0rue-std/<relative>`) the compiler
/// assigns to a standard-library source requested at `requested` — the same
/// lexical classification `classify_module` applies against the context's
/// captured std root, in the same normalized spelling `ModuleId` publishes.
/// Returns `None` when the context has no std root or the requested path is
/// outside it, so callers attribute such paths as themselves rather than
/// inventing a trusted spelling.
pub fn trusted_logical_path_for_requested(
    context: &ImportDiscoveryContext,
    requested: &str,
) -> Option<String> {
    let std_root = context.std_root()?;
    let relative = Path::new(requested).strip_prefix(std_root).ok()?;
    Some(normalize_module_path(&format!(
        "{TRUSTED_STANDARD_LIBRARY_NAMESPACE}{}",
        relative.to_string_lossy()
    )))
}

fn display_path_for_module(module: &ModuleId, entry: &AssembledSource) -> String {
    if module
        .as_str()
        .starts_with(TRUSTED_STANDARD_LIBRARY_NAMESPACE)
    {
        entry.requested_path.to_string()
    } else {
        module.as_str().to_owned()
    }
}

fn classify_module(
    context: &ImportDiscoveryContext,
    requested: &str,
    canonical: &str,
) -> CompileResult<ModuleId> {
    let requested = Path::new(requested);
    let canonical = Path::new(canonical);
    if let Some(std_root) = context.std_root() {
        let std_root = Path::new(std_root);
        if let Ok(relative) = requested.strip_prefix(std_root) {
            if !canonical.starts_with(std_root) {
                return Err(invalid_input(format!(
                    "standard-library candidate {:?} escapes captured std root {:?}",
                    requested, std_root
                )));
            }
            // Escape checks run on the unnormalized relative path: the shared
            // derivation below collapses `..` lexically, so a traversal must be
            // refused before it can normalize into an in-namespace identity.
            if relative
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(invalid_input(format!(
                    "trusted standard-library source {:?} would receive an escaping identity",
                    requested
                )));
            }
            let logical = trusted_logical_path_for_requested(context, &requested.to_string_lossy())
                .expect("the requested path was just stripped against this context's std root");
            return ModuleId::from_trusted_standard_library_path(logical);
        }
    }
    let project_root = Path::new(context.project_root());
    let logical = lexical_relative_path(project_root, requested).ok_or_else(|| {
        invalid_input(format!(
            "source {:?} cannot receive a project-relative durable identity",
            requested
        ))
    })?;
    if logical
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_input(format!(
            "project source {:?} would receive an escaping identity",
            requested
        )));
    }
    ModuleId::from_logical_path(logical.to_string_lossy())
}

fn parent_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn normalize_path(path: &str) -> String {
    let mut result = PathBuf::new();
    let absolute = Path::new(path).is_absolute();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = result
                    .components()
                    .next_back()
                    .is_some_and(|component| matches!(component, Component::Normal(_)));
                if can_pop {
                    result.pop();
                } else if !absolute {
                    result.push("..");
                }
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result.to_string_lossy().into_owned()
}
fn normalize_absolute(path: &str) -> CompileResult<String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(invalid_input(format!(
            "discovery identity path {path:?} is not absolute"
        )));
    }
    Ok(normalize_path(path.to_string_lossy().as_ref()))
}
fn lexical_relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    if base.first() != target.first() {
        return None;
    }
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut result = PathBuf::new();
    for component in &base[common..] {
        if matches!(component, Component::Normal(_)) {
            result.push("..");
        }
    }
    for component in &target[common..] {
        if let Component::Normal(part) = component {
            result.push(part);
        }
    }
    if result
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(result)
}
pub(crate) fn source_fingerprint(source: &str) -> u64 {
    source.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
fn invalid_input(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(epoch: u64) -> ImportDiscoveryContext {
        ImportDiscoveryContext::new(epoch, "/project", Some("/sdk"), "all").unwrap()
    }

    #[test]
    fn project_root_inside_or_equal_to_std_root_is_rejected() {
        for (project, std_root) in [("/sdk/project", "/sdk"), ("/sdk", "/sdk")] {
            let error = ImportDiscoveryContext::new(1, project, Some(std_root), "all")
                .expect_err("a project rooted inside std must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("inside the configured standard-library root")
            );
        }
        // The supported layout remains valid: the captured std tree is a
        // child of the project, so project-relative and trusted identities have
        // disjoint roots.
        ImportDiscoveryContext::new(1, "/project", Some("/project/std"), "all")
            .expect("std nested inside the project is valid");
    }

    #[test]
    fn project_provenance_does_not_inherit_std_boundary_from_its_path() {
        let context =
            ImportDiscoveryContext::new(1, "/project", Some("/project/std"), "all").unwrap();
        let importer = ModuleId::from_logical_path("std/main.rue").unwrap();
        // The spelling is beneath std, but the accepted module provenance is a
        // caller module. Its escape is measured against the project root.
        assert_eq!(
            escaping_import_candidate(
                &context,
                "../outside.rue",
                &importer,
                "/project/std/main.rue",
            )
            .unwrap(),
            None
        );
    }

    fn occurrence(importer: &str, specifier: &str) -> ImportOccurrenceKey {
        let relative = importer
            .strip_prefix("/sdk/")
            .expect("test trusted std importer is under the SDK root");
        ImportOccurrenceKey {
            importer: ModuleId::from_trusted_standard_library_path(format!("\0rue-std/{relative}"))
                .unwrap(),
            source_offset: 0,
            source_end: specifier.len() as u32,
            specifier: Arc::from(specifier),
        }
    }

    #[test]
    fn trusted_std_escapes_are_rejected_before_filesystem_discovery() {
        let context = context(1);
        let direct = occurrence("/sdk/_std.rue", "../outside.rue");
        let nested = occurrence("/sdk/nested/mod.rue", "../../outside.rue");
        let facade = occurrence("/sdk/nested/mod.rue", "../../outside");

        for (occurrence, importer, expected) in [
            (&direct, "/sdk/_std.rue", "/outside.rue"),
            (&nested, "/sdk/nested/mod.rue", "/outside.rue"),
            (&facade, "/sdk/nested/mod.rue", "/outside/_outside.rue"),
        ] {
            assert_eq!(
                escaping_import_candidate(
                    &context,
                    occurrence.specifier(),
                    occurrence.importer(),
                    importer
                )
                .unwrap()
                .as_deref(),
                Some(expected)
            );
            assert!(
                discovery_groups_for_occurrence(&context, occurrence, importer)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    fn project_occurrence(importer: &str, specifier: &str) -> ImportOccurrenceKey {
        ImportOccurrenceKey {
            importer: ModuleId::from_logical_path(importer).unwrap(),
            source_offset: 0,
            source_end: specifier.len() as u32,
            specifier: Arc::from(specifier),
        }
    }

    #[test]
    fn non_relative_specifiers_are_classified_from_their_text() {
        assert_eq!(
            non_relative_specifier(""),
            Some(NonRelativeSpecifier::Empty)
        );
        assert_eq!(
            non_relative_specifier("/etc/hostname.rue"),
            Some(NonRelativeSpecifier::Absolute)
        );
        assert_eq!(
            non_relative_specifier("/project/helper.rue"),
            Some(NonRelativeSpecifier::Absolute)
        );
        // Relative spellings, including the ones that look unusual, are not
        // this rule's business: an escaping `..` chain is E0713's.
        for relative in ["helper.rue", "./helper.rue", "../outside.rue", "nested/mod"] {
            assert_eq!(non_relative_specifier(relative), None, "{relative}");
        }
        // `std` is a reserved program-anchored specifier, not a path.
        assert_eq!(non_relative_specifier("std"), None);
    }

    #[test]
    fn non_relative_specifiers_derive_no_discovery_requests() {
        let context = context(1);
        // The absolute specifier here names a file that really is inside the
        // project root. Before RUE-1706 `Path::join` discarded the importer
        // anchor, this candidate stayed within the boundary, and the import
        // RESOLVED — a program that compiled only while its tree sat at this
        // path on this machine.
        for specifier in ["", "/project/helper.rue", "/etc/hostname.rue"] {
            let occurrence = project_occurrence("main.rue", specifier);
            assert!(
                discovery_groups_for_occurrence(&context, &occurrence, "/project/main.rue")
                    .unwrap()
                    .is_empty(),
                "{specifier} derived requests"
            );
            // One mistake, one diagnostic: the escape check declines these so
            // they are not also reported as leaving the project root.
            assert_eq!(
                escaping_import_candidate(
                    &context,
                    specifier,
                    &ModuleId::from_logical_path("main.rue").unwrap(),
                    "/project/main.rue",
                )
                .unwrap(),
                None,
                "{specifier} also reported as escaping"
            );
        }
    }

    #[test]
    fn relative_specifiers_still_derive_discovery_requests() {
        let context = context(1);
        for specifier in ["helper.rue", "./helper.rue", "nested/mod"] {
            let occurrence = project_occurrence("main.rue", specifier);
            assert!(
                !discovery_groups_for_occurrence(&context, &occurrence, "/project/main.rue")
                    .unwrap()
                    .is_empty(),
                "{specifier} derived no requests"
            );
        }
    }

    #[test]
    fn trusted_std_in_root_and_program_anchored_imports_remain_discoverable() {
        let context = context(1);
        let in_root = occurrence("/sdk/nested/mod.rue", "../nested/helper.rue");
        assert_eq!(
            escaping_import_candidate(
                &context,
                in_root.specifier(),
                in_root.importer(),
                "/sdk/nested/mod.rue",
            )
            .unwrap(),
            None
        );
        assert!(
            !discovery_groups_for_occurrence(&context, &in_root, "/sdk/nested/mod.rue")
                .unwrap()
                .is_empty()
        );

        let std = occurrence("/sdk/nested/mod.rue", "std");
        assert_eq!(
            escaping_import_candidate(
                &context,
                std.specifier(),
                std.importer(),
                "/sdk/nested/mod.rue",
            )
            .unwrap(),
            None
        );
        assert!(
            !discovery_groups_for_occurrence(&context, &std, "/sdk/nested/mod.rue")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn trusted_std_escape_projects_e0713_from_the_same_candidate_helper() {
        let root = FileId::new(1);
        let trusted = FileId::new(2);
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root,
            AHashMap::from([
                (root, "/project/main.rue".to_owned()),
                (trusted, "/sdk/nested/mod.rue".to_owned()),
            ]),
            AHashMap::from([
                (root, "main.rue".to_owned()),
                (trusted, "\0rue-std/nested/mod.rue".to_owned()),
            ]),
            ahash::AHashSet::from([trusted]),
        )
        .unwrap();
        let source = SourceSnapshot::new(
            metadata,
            vec![
                (root, Arc::new("fn main() {}".to_owned())),
                (
                    trusted,
                    Arc::new("const outside = @import(\"../../outside.rue\");".to_owned()),
                ),
            ],
        )
        .unwrap();
        let mut session = crate::CompilerSession::new();
        let program = session.update(&source).into_owner_result().unwrap();
        let errors = escape_diagnostics(&program, &context(1));

        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors.first().expect("trusted std escape has a diagnostic").kind,
            ErrorKind::ImportEscapesRoot { candidate, .. } if candidate == "/outside.rue"
        ));
    }

    #[test]
    fn classify_module_keeps_project_identities_stable_across_relocation() {
        fn classify(context: &ImportDiscoveryContext, path: &Path) -> ModuleId {
            let path = normalize_path(&path.to_string_lossy());
            classify_module(context, &path, &path).unwrap()
        }

        fn identities(root: &Path) -> Vec<ModuleId> {
            let project = root.join("project");
            let context =
                ImportDiscoveryContext::new(1, project.to_string_lossy(), None, "all").unwrap();
            [
                project.join("main.rue"),
                // The production path normalizer resolves nested and parent-relative
                // spellings before classify_module assigns the durable identity.
                project.join("./left/nested/../entry.rue"),
                project.join("right/shared.rue"),
            ]
            .iter()
            .map(|path| classify(&context, path))
            .collect()
        }

        fn rejects_external_source(root: &Path) {
            let project = root.join("project");
            let context =
                ImportDiscoveryContext::new(1, project.to_string_lossy(), None, "all").unwrap();
            let path = normalize_path(&root.join("dep.rue").to_string_lossy());
            assert!(classify_module(&context, &path, &path).is_err());
        }

        let base = if cfg!(windows) {
            PathBuf::from(r"C:\rue-import-discovery-relocation")
        } else {
            PathBuf::from("/tmp/rue-import-discovery-relocation")
        };
        let expected = ["main.rue", "left/entry.rue", "right/shared.rue"]
            .into_iter()
            .map(ModuleId::from_logical_path)
            .map(Result::unwrap)
            .collect::<Vec<_>>();

        assert_eq!(
            identities(&base.join("a")),
            expected,
            "project-relative identities include nested and parent-relative sources"
        );
        assert_eq!(
            identities(&base.join("a-deliberately-much-longer-relocated-source-root")),
            expected,
            "moving the physical project root does not change module identities"
        );
        rejects_external_source(&base.join("a"));
    }

    #[test]
    fn classify_module_keeps_external_std_namespace_stable() {
        fn classify(context: &ImportDiscoveryContext, path: &Path) -> ModuleId {
            let path = normalize_path(&path.to_string_lossy());
            classify_module(context, &path, &path).unwrap()
        }

        fn identities(project: &Path, std_root: &Path) -> Vec<ModuleId> {
            let context = ImportDiscoveryContext::new(
                1,
                project.to_string_lossy(),
                Some(std_root.to_string_lossy().as_ref()),
                "all",
            )
            .unwrap();
            [
                project.join("main.rue"),
                std_root.join("_std.rue"),
                std_root.join("math/float.rue"),
                project.join("@rue-std/math/float.rue"),
            ]
            .iter()
            .map(|path| classify(&context, path))
            .collect()
        }

        let base = if cfg!(windows) {
            PathBuf::from(r"C:\rue-import-discovery-std")
        } else {
            PathBuf::from("/tmp/rue-import-discovery-std")
        };
        let first = identities(&base.join("build-a/project"), &base.join("toolchain-a/std"));
        let second = identities(
            &base.join("different-depth/build-b/project"),
            &base.join("a-different-toolchain-location/std"),
        );
        let expected = vec![
            ModuleId::from_logical_path("main.rue").unwrap(),
            ModuleId::from_trusted_standard_library_path("\0rue-std/_std.rue").unwrap(),
            ModuleId::from_trusted_standard_library_path("\0rue-std/math/float.rue").unwrap(),
            ModuleId::from_logical_path("@rue-std/math/float.rue").unwrap(),
        ];

        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert!(first[1].is_trusted_standard_library());
        assert!(!first[3].is_trusted_standard_library());
    }

    #[test]
    fn trusted_logical_path_matches_classified_module_identity() {
        let (project, std_root) = if cfg!(windows) {
            (r"C:\rue-parity\project", r"C:\rue-parity\toolchain\std")
        } else {
            ("/rue-parity/project", "/rue-parity/toolchain/std")
        };
        let context = ImportDiscoveryContext::new(1, project, Some(std_root), "all").unwrap();

        // The attribution helper must publish exactly the identity spelling
        // classification mints for the same requested std path, including the
        // normalized form of a non-canonical spelling.
        for relative in ["_std.rue", "math/float.rue", "math/./float.rue"] {
            let requested = Path::new(std_root).join(relative);
            let requested = requested.to_string_lossy();
            let module = classify_module(&context, &requested, &requested).unwrap();
            assert!(module.is_trusted_standard_library());
            assert_eq!(
                trusted_logical_path_for_requested(&context, &requested).as_deref(),
                Some(module.as_str())
            );
        }

        // A path outside the std root has no trusted spelling, and neither does
        // any path when the context captured no std root.
        let outside = Path::new(project).join("main.rue");
        assert_eq!(
            trusted_logical_path_for_requested(&context, &outside.to_string_lossy()),
            None
        );
        let rootless = ImportDiscoveryContext::new(1, project, None, "all").unwrap();
        let inside = Path::new(std_root).join("_std.rue");
        assert_eq!(
            trusted_logical_path_for_requested(&rootless, &inside.to_string_lossy()),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn classify_module_rejects_unnamed_cross_volume_source() {
        let context = ImportDiscoveryContext::new(1, r"C:\project", None, "all").unwrap();
        let error = classify_module(
            &context,
            r"D:\dependency\helper.rue",
            r"D:\dependency\helper.rue",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("project-relative durable identity")
        );
    }

    fn identity() -> PhysicalFileIdentity {
        PhysicalFileIdentity::new(1, 2)
    }

    fn metadata_fingerprint() -> FileMetadataFingerprint {
        FileMetadataFingerprint::new(3, 4, 5)
    }

    #[test]
    fn absolute_normalization_never_pops_above_root() {
        assert_eq!(normalize_path("/project/../../../x"), "/x");
        assert_eq!(normalize_path("/../../x"), "/x");
        assert_eq!(normalize_absolute("/../../../x").unwrap(), "/x");
        assert!(
            AcceptedImportSource::new(
                "relative.rue",
                "/physical/relative.rue",
                identity(),
                metadata_fingerprint(),
                Arc::new(String::new()),
            )
            .is_err()
        );
        assert!(
            DiscoverySourceAssembler::new(
                context(1),
                "/project/main.rue",
                "relative-physical-main.rue",
                identity(),
                metadata_fingerprint(),
                Arc::new(String::new()),
            )
            .is_err()
        );
    }

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        SourceSnapshot::new(
            SourceMetadata::new(
                FileId::new(root),
                entries
                    .iter()
                    .map(|(id, path, _, _)| (FileId::new(*id), (*path).into()))
                    .collect::<AHashMap<_, _>>(),
                entries
                    .iter()
                    .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).into()))
                    .collect::<AHashMap<_, _>>(),
            )
            .unwrap(),
            entries
                .iter()
                .map(|(id, _, _, source)| (FileId::new(*id), Arc::new((*source).into())))
                .collect(),
        )
        .unwrap()
    }

    fn accepted_reads(snapshot: &SourceSnapshot) -> Arc<[AcceptedReadManifestEntry]> {
        snapshot
            .files()
            .map(|source| AcceptedReadManifestEntry {
                module: snapshot.module_id(source.file_id).unwrap().clone(),
                requested_path: Arc::from(source.path),
                canonical_path: Arc::from(source.path),
                symlink_route: Arc::from([]),
                metadata_identity: PhysicalFileIdentity::new(1, source.file_id.index() as u64),
                metadata_fingerprint: metadata_fingerprint(),
                content_fingerprint: source_fingerprint(source.source),
            })
            .collect()
    }

    #[test]
    fn physical_identity_lookup_matches_a_scan_across_compacted_segments() {
        let entry = |index: u64, identity: u64| AcceptedReadManifestEntry {
            module: ModuleId::from_logical_path(format!("module_{index:04}.rue")).unwrap(),
            requested_path: Arc::from(format!("/project/module_{index:04}.rue").as_str()),
            canonical_path: Arc::from(format!("/project/module_{index:04}.rue").as_str()),
            symlink_route: Arc::from([]),
            metadata_identity: PhysicalFileIdentity::new(1, identity),
            metadata_fingerprint: metadata_fingerprint(),
            content_fingerprint: index,
        };

        // One-entry additive rounds. The entry count is deliberately not a
        // power of two, so size-tiered compaction leaves the sequence spread
        // over several segments rather than one flat tier.
        const ENTRIES: u64 = 100;
        let mut manifest = AcceptedReadManifest::from_entries(vec![entry(0, 0)]);
        for index in 1..ENTRIES {
            manifest =
                AcceptedReadManifest::extend_with_appended(&manifest, vec![entry(index, index)]);
        }
        assert!(manifest.segments().segments().len() > 1);

        let meter = crate::source_snapshot::IdentityResolutionMeter::default();
        for index in 0..ENTRIES {
            let identity = PhysicalFileIdentity::new(1, index);
            let scanned = manifest
                .iter()
                .filter(|candidate| candidate.metadata_identity() == identity)
                .collect::<Vec<_>>();
            assert_eq!(scanned.len(), 1);
            match manifest.lookup_physical_identity(identity, &meter) {
                ManifestIdentityLookup::Unique(found) => assert_eq!(found, scanned[0]),
                _ => panic!("module {index} resolves to exactly one entry"),
            }
        }
        assert!(matches!(
            manifest.lookup_physical_identity(PhysicalFileIdentity::new(9, 9), &meter),
            ManifestIdentityLookup::Absent
        ));
        // The index is materialized once for this manifest value and shared by
        // every later lookup, so the whole run costs one pass over the entries
        // plus one entry per lookup — not one pass per lookup, which is what
        // the scan cost and what `visits` would show if the index were rebuilt.
        assert_eq!(meter.physical_identity_lookups(), ENTRIES + 1);
        assert_eq!(
            meter.physical_identity_visits(),
            ENTRIES + meter.physical_identity_lookups()
        );

        // Two modules naming one physical file stay an ambiguity the caller
        // rejects rather than a silently chosen winner.
        let ambiguous = AcceptedReadManifest::from_entries(vec![entry(0, 7), entry(1, 7)]);
        assert!(matches!(
            ambiguous.lookup_physical_identity(PhysicalFileIdentity::new(1, 7), &meter),
            ManifestIdentityLookup::Duplicate
        ));
    }

    fn indexed_test_request(occurrence_index: u32, position: u32) -> ImportDiscoveryRequest {
        let importer =
            ModuleId::from_logical_path(format!("module-{occurrence_index}.rue")).unwrap();
        let occurrence = ImportOccurrenceKey {
            importer,
            source_offset: occurrence_index.saturating_mul(10),
            source_end: occurrence_index.saturating_mul(10).saturating_add(5),
            specifier: format!("dep-{occurrence_index}").into(),
        };
        ImportDiscoveryRequest {
            context: context(900),
            occurrence,
            exact_specifier: format!("dep-{occurrence_index}").into(),
            normalized_specifier: format!("dep-{occurrence_index}").into(),
            importer_anchor: format!("/project/module-{occurrence_index}.rue").into(),
            root_anchor: "/project".into(),
            group: 0,
            position,
            requested_path: format!("/project/dep-{occurrence_index}-{position}.rue").into(),
            role: ImportCandidateRole::ExactFile,
        }
    }

    #[test]
    fn occurrence_index_preserves_flat_and_segmented_ledger_identity() {
        use std::hash::{Hash, Hasher};

        let observations = (0..64)
            .flat_map(|occurrence| {
                (0..4).map(move |position| {
                    ImportObservation::absent(indexed_test_request(occurrence, position))
                })
            })
            .collect::<Vec<_>>();
        let mut flat = ImportObservationLedger::default();
        let mut segmented = ImportObservationLedger::default();
        for (index, observation) in observations.iter().cloned().enumerate() {
            flat.record(observation.clone()).unwrap();
            segmented.record(observation).unwrap();
            if index % 11 == 10 {
                segmented = segmented.clone();
            }
        }

        assert_eq!(flat, segmented);
        let mut canonical = observations;
        canonical.sort_by(|left, right| left.request().cmp(right.request()));
        assert_eq!(
            flat.iter().cloned().collect::<Vec<_>>(),
            canonical,
            "the occurrence index preserves canonical request order"
        );
        let hash = |ledger: &ImportObservationLedger| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            ledger.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&flat), hash(&segmented));
    }

    #[test]
    fn exact_occurrence_validation_uses_only_its_indexed_bucket() {
        let target = indexed_test_request(7, 0);
        let groups = vec![Arc::<[ImportDiscoveryRequest]>::from([target.clone()])];
        let mut ledger = ImportObservationLedger::default();
        for occurrence in 0..512 {
            for position in 0..4 {
                ledger
                    .record(ImportObservation::absent(indexed_test_request(
                        occurrence, position,
                    )))
                    .unwrap();
            }
            if occurrence % 31 == 30 {
                ledger = ledger.clone();
            }
        }

        assert_eq!(ledger.len(), 2048);
        assert_eq!(
            ledger.observation_count_for_occurrence(target.occurrence()),
            4,
            "exact validation sees one occurrence bucket, not the whole ledger"
        );
        assert!(
            validate_exact_import_occurrence(&groups, &ledger).is_err(),
            "the three non-canonical requests in the exact bucket still invalidate it"
        );

        let isolated = indexed_test_request(999, 0);
        let isolated_groups = vec![Arc::<[ImportDiscoveryRequest]>::from([isolated.clone()])];
        assert!(
            validate_exact_import_occurrence(&isolated_groups, &ledger).is_ok(),
            "a missing canonical observation is not itself a malformed ledger"
        );
        ledger
            .record(ImportObservation::absent(isolated.clone()))
            .unwrap();
        assert!(
            validate_exact_import_occurrence(&isolated_groups, &ledger).is_ok(),
            "unrelated occurrence buckets do not affect exact validation"
        );

        let mut foreign = isolated.clone();
        foreign.requested_path = "/project/non-canonical.rue".into();
        ledger.record(ImportObservation::absent(foreign)).unwrap();
        assert!(
            validate_exact_import_occurrence(&isolated_groups, &ledger).is_err(),
            "a non-canonical request for the exact occurrence still invalidates it"
        );
    }

    #[test]
    fn occurrence_count_validation_matches_the_full_scan_oracle() {
        let canonical = (0..3)
            .map(|position| indexed_test_request(17, position))
            .collect::<Vec<_>>();
        let groups = vec![
            Arc::<[ImportDiscoveryRequest]>::from([canonical[0].clone(), canonical[1].clone()]),
            Arc::<[ImportDiscoveryRequest]>::from([canonical[1].clone(), canonical[2].clone()]),
        ];
        let old_oracle = |ledger: &ImportObservationLedger| {
            let requests = groups
                .iter()
                .flat_map(|group| group.iter())
                .collect::<BTreeSet<_>>();
            !ledger.iter().any(|observation| {
                observation.request().occurrence() == canonical[0].occurrence()
                    && !requests.contains(observation.request())
            })
        };

        for case in 0u32..128 {
            let mut ledger = ImportObservationLedger::default();
            for (index, request) in canonical.iter().enumerate() {
                if case & (1 << index) != 0 {
                    ledger
                        .record(ImportObservation::absent(request.clone()))
                        .unwrap();
                }
            }
            for unrelated in 0..24 {
                if case.rotate_left(unrelated) & 1 != 0 {
                    ledger
                        .record(ImportObservation::absent(indexed_test_request(
                            100 + unrelated,
                            case % 3,
                        )))
                        .unwrap();
                }
                if unrelated % 5 == 4 {
                    ledger = ledger.clone();
                }
            }
            if case & (1 << 6) != 0 {
                let mut extra = canonical[0].clone();
                extra.requested_path = format!("/project/extra-{case}.rue").into();
                ledger.record(ImportObservation::absent(extra)).unwrap();
            }

            assert_eq!(
                validate_exact_import_occurrence(&groups, &ledger).is_ok(),
                old_oracle(&ledger),
                "indexed validation diverged from the full-scan oracle in case {case}"
            );
        }
    }

    #[test]
    fn assembler_is_order_independent_and_rejects_alias_conflicts() {
        let mut a = DiscoverySourceAssembler::new(
            context(1),
            "/project/main.rue",
            "/real/main",
            PhysicalFileIdentity::new(1, 1),
            metadata_fingerprint(),
            Arc::new(String::new()),
        )
        .unwrap();
        a.add_explicit(
            "/project/b.rue",
            "/real/b",
            PhysicalFileIdentity::new(2, 2),
            metadata_fingerprint(),
            Arc::new("b".into()),
        )
        .unwrap();
        a.add_explicit(
            "/project/a.rue",
            "/real/a",
            PhysicalFileIdentity::new(3, 3),
            metadata_fingerprint(),
            Arc::new("a".into()),
        )
        .unwrap();
        let snapshot = a.snapshot().unwrap();
        assert_eq!(
            snapshot
                .metadata()
                .logical_paths()
                .map(|(_, p)| p)
                .collect::<Vec<_>>(),
            vec!["main.rue", "a.rue", "b.rue"]
        );
        let error = a
            .add_explicit(
                "/project/alias.rue",
                "/real/a",
                PhysicalFileIdentity::new(3, 3),
                metadata_fingerprint(),
                Arc::new("a".into()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("incompatible logical IDs"));
    }

    fn assembler_for_physical_identity_test() -> DiscoverySourceAssembler {
        DiscoverySourceAssembler::new(
            context(1),
            "/project/main.rue",
            "/real/main",
            PhysicalFileIdentity::new(1, 1),
            metadata_fingerprint(),
            Arc::new(String::new()),
        )
        .unwrap()
    }

    #[test]
    fn hard_link_identity_rejects_incompatible_modules_in_either_order() {
        for reverse in [false, true] {
            let mut assembler = assembler_for_physical_identity_test();
            let (first, second) = if reverse {
                (
                    ("/project/b.rue", "/real/hard-b"),
                    ("/project/a.rue", "/real/hard-a"),
                )
            } else {
                (
                    ("/project/a.rue", "/real/hard-a"),
                    ("/project/b.rue", "/real/hard-b"),
                )
            };
            assembler
                .add_explicit(
                    first.0,
                    first.1,
                    PhysicalFileIdentity::new(9, 9),
                    metadata_fingerprint(),
                    Arc::new("same inode".into()),
                )
                .unwrap();
            let error = assembler
                .add_explicit(
                    second.0,
                    second.1,
                    PhysicalFileIdentity::new(9, 9),
                    metadata_fingerprint(),
                    Arc::new("same inode".into()),
                )
                .unwrap_err();
            assert!(error.to_string().contains("incompatible logical IDs"));
        }
    }

    #[test]
    fn reduced_alias_conflicts_remain_fail_closed_before_suppression() {
        let mut assembler = assembler_for_physical_identity_test();
        assembler
            .add_explicit(
                "/project/a.rue",
                "/real/a.rue",
                PhysicalFileIdentity::new(9, 9),
                metadata_fingerprint(),
                Arc::new("same inode".into()),
            )
            .unwrap();

        let mismatched_content = AcceptedImportSource::new(
            "/project/b.rue",
            "/real/b.rue",
            PhysicalFileIdentity::new(9, 9),
            metadata_fingerprint(),
            Arc::new("changed content".into()),
        )
        .unwrap();
        assert!(
            assembler
                .add_reduced_source(&mismatched_content)
                .unwrap_err()
                .to_string()
                .contains("changed during discovery epoch")
        );

        let mismatched_metadata = AcceptedImportSource::new(
            "/project/b.rue",
            "/real/b.rue",
            PhysicalFileIdentity::new(9, 9),
            FileMetadataFingerprint::new(2, 3, 4),
            Arc::new("same inode".into()),
        )
        .unwrap();
        assert!(
            assembler
                .add_reduced_source(&mismatched_metadata)
                .unwrap_err()
                .to_string()
                .contains("metadata changed during discovery epoch")
        );

        let mismatched_canonical_identity = AcceptedImportSource::new(
            "/project/a.rue",
            "/real/a.rue",
            PhysicalFileIdentity::new(9, 10),
            metadata_fingerprint(),
            Arc::new("same inode".into()),
        )
        .unwrap();
        assert!(
            assembler
                .add_reduced_source(&mismatched_canonical_identity)
                .unwrap_err()
                .to_string()
                .contains("changed physical identity during discovery epoch")
        );
    }

    #[test]
    fn accepted_projection_deduplicates_hard_links_but_keeps_distinct_logicals_for_diagnostics() {
        let first = AcceptedImportSource::new(
            "/project/a.rue",
            "/real/a.rue",
            PhysicalFileIdentity::new(9, 9),
            metadata_fingerprint(),
            Arc::new("same inode".into()),
        )
        .unwrap();
        let second = AcceptedImportSource::new(
            "/project/b.rue",
            "/real/b.rue",
            PhysicalFileIdentity::new(9, 9),
            metadata_fingerprint(),
            Arc::new("same inode".into()),
        )
        .unwrap();
        let reduced = canonicalize_accepted(vec![&second, &first]);
        assert_eq!(reduced.len(), 1);
        assert_eq!(reduced[0].requested_path(), "/project/a.rue");
    }

    #[test]
    fn hard_link_logical_conflict_is_reported_at_the_later_import_site() {
        let source = snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const a = @import(\"a.rue\");\nconst b = @import(\"b.rue\");\nfn main() -> i32 { 0 }",
                ),
                (2, "/project/a.rue", "a.rue", "pub fn value() -> i32 { 1 }"),
            ],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(51),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        for request in plan.pending_requests(&ledger) {
            let (canonical, source_text) = if request.requested_path().ends_with("/a.rue") {
                ("/real/a.rue", "pub fn value() -> i32 { 1 }")
            } else if request.requested_path().ends_with("/b.rue") {
                ("/real/b.rue", "pub fn value() -> i32 { 1 }")
            } else {
                continue;
            };
            ledger
                .record(
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            request.requested_path(),
                            canonical,
                            PhysicalFileIdentity::new(1, 2),
                            metadata_fingerprint(),
                            Arc::new(source_text.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert!(matches!(
            &errors.as_slice()[0].kind,
            ErrorKind::ImportSpellingsSameFile { first, second }
                if first == "a.rue" && second == "b.rue"
        ));
        assert_eq!(errors.as_slice()[0].span().unwrap().start, 38);
    }

    #[test]
    fn case_alias_conflict_is_reported_but_symlink_alias_is_deduplicated() {
        let source = snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const lower = @import(\"real.rue\");\nconst upper = @import(\"REAL.rue\");\nfn main() -> i32 { 0 }",
                ),
                (
                    2,
                    "/project/real.rue",
                    "real.rue",
                    "pub fn value() -> i32 { 1 }",
                ),
            ],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(52),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        for request in plan.pending_requests(&ledger) {
            ledger
                .record(
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            request.requested_path(),
                            "/real/shared.rue",
                            PhysicalFileIdentity::new(1, 2),
                            metadata_fingerprint(),
                            Arc::new("pub fn value() -> i32 { 1 }".into()),
                        )
                        .unwrap()
                        .with_symlink_route(Arc::from([])),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert!(
            errors.iter().any(|error| {
                matches!(
                    &error.kind,
                    ErrorKind::ImportSpellingsSameFile { first, second }
                        if first == "real.rue" && second == "REAL.rue"
                )
            }),
            "{errors:?}"
        );

        let mut symlink_ledger = ImportObservationLedger::default();
        for request in plan.pending_requests(&ImportObservationLedger::default()) {
            let route = request
                .requested_path()
                .ends_with("REAL.rue")
                .then(|| PhysicalFileIdentity::new(9, 99))
                .map_or_else(|| Arc::from([]), |identity| Arc::from([identity]));
            symlink_ledger
                .record(
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            request.requested_path(),
                            "/real/shared.rue",
                            PhysicalFileIdentity::new(1, 2),
                            metadata_fingerprint(),
                            Arc::new("pub fn value() -> i32 { 1 }".into()),
                        )
                        .unwrap()
                        .with_symlink_route(route),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let mut symlink_session = crate::CompilerSession::new();
        let mut symlink_manifest = accepted_reads(&source).to_vec();
        symlink_manifest[1].canonical_path = Arc::from("/real/shared.rue");
        symlink_manifest[1].symlink_route = Arc::from([]);
        let symlink_plan = symlink_session
            .stage_import_discovery(
                &source,
                context(52),
                Arc::from(symlink_manifest),
                ImportObservationLedger::default(),
            )
            .unwrap();
        assert_eq!(symlink_plan.groups(), plan.groups());
        if let Err(symlink_errors) = symlink_session.close_import_discovery(symlink_ledger) {
            assert!(
                !symlink_errors.iter().any(|error| {
                    matches!(error.kind, ErrorKind::ImportSpellingsSameFile { .. })
                })
            );
        }
    }

    #[test]
    fn trusted_manifest_conflict_uses_safe_spellings_at_the_import_site() {
        let root = FileId::new(1);
        let trusted_std = FileId::new(2);
        let trusted_option = FileId::new(3);
        let metadata = SourceMetadata::new_with_trusted_standard_library(
            root,
            AHashMap::from([
                (root, "/project/main.rue".to_owned()),
                (trusted_std, "/sdk/_std.rue".to_owned()),
                (trusted_option, "/sdk/option.rue".to_owned()),
            ]),
            AHashMap::from([
                (root, "main.rue".to_owned()),
                (trusted_std, "\0rue-std/_std.rue".to_owned()),
                (trusted_option, "\0rue-std/option.rue".to_owned()),
            ]),
            ahash::AHashSet::from([trusted_std, trusted_option]),
        )
        .unwrap();
        let main_text = "const std = @import(\"std\"); fn main() -> i32 { 0 }";
        let std_text = "pub const lower = @import(\"option.rue\");\npub const upper = @import(\"OPTION.rue\");";
        let option_text = "pub fn value() -> i32 { 1 }";
        let source = SourceSnapshot::new(
            metadata,
            vec![
                (root, Arc::new(main_text.into())),
                (trusted_std, Arc::new(std_text.into())),
                (trusted_option, Arc::new(option_text.into())),
            ],
        )
        .unwrap();
        let manifest = AcceptedReadManifest::from_entries(vec![
            AcceptedReadManifestEntry {
                module: ModuleId::from_logical_path("main.rue").unwrap(),
                requested_path: Arc::from("/project/main.rue"),
                canonical_path: Arc::from("/project/main.rue"),
                symlink_route: Arc::from([]),
                metadata_identity: PhysicalFileIdentity::new(1, 1),
                metadata_fingerprint: metadata_fingerprint(),
                content_fingerprint: source_fingerprint(main_text),
            },
            AcceptedReadManifestEntry {
                module: ModuleId::from_trusted_standard_library_path("\0rue-std/_std.rue").unwrap(),
                requested_path: Arc::from("/sdk/_std.rue"),
                canonical_path: Arc::from("/sdk/_std.rue"),
                symlink_route: Arc::from([]),
                metadata_identity: PhysicalFileIdentity::new(1, 2),
                metadata_fingerprint: metadata_fingerprint(),
                content_fingerprint: source_fingerprint(std_text),
            },
            AcceptedReadManifestEntry {
                module: ModuleId::from_trusted_standard_library_path("\0rue-std/option.rue")
                    .unwrap(),
                requested_path: Arc::from("/sdk/option.rue"),
                canonical_path: Arc::from("/sdk/option.rue"),
                symlink_route: Arc::from([]),
                metadata_identity: PhysicalFileIdentity::new(1, 3),
                metadata_fingerprint: metadata_fingerprint(),
                content_fingerprint: source_fingerprint(option_text),
            },
        ]);
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(54),
                manifest.shared_slice(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                let requested = request.requested_path();
                let observation = if requested.starts_with("/project/") {
                    ImportObservation::absent(request)
                } else if requested.ends_with("/option.rue") || requested.ends_with("/OPTION.rue") {
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            requested,
                            "/sdk/option.rue",
                            PhysicalFileIdentity::new(1, 3),
                            metadata_fingerprint(),
                            Arc::new(option_text.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                } else {
                    ImportObservation::accepted(
                        request.clone(),
                        AcceptedImportSource::new(
                            requested,
                            "/sdk/_std.rue",
                            PhysicalFileIdentity::new(1, 2),
                            metadata_fingerprint(),
                            Arc::new(std_text.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                };
                ledger.record(observation).unwrap();
            }
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        let diagnostic = errors
            .iter()
            .find(|error| matches!(error.kind, ErrorKind::ImportSpellingsSameFile { .. }))
            .expect("trusted standard-library alias has E0715");
        match &diagnostic.kind {
            ErrorKind::ImportSpellingsSameFile { first, second } => {
                assert_eq!(first, "std/option.rue");
                assert_eq!(second, "OPTION.rue");
                assert!(!first.contains('\0'));
                assert!(!second.contains('\0'));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn hard_link_alias_reuse_is_insertion_order_independent() {
        let build = |paths: [&str; 2]| {
            let mut assembler = assembler_for_physical_identity_test();
            for path in paths {
                assembler
                    .add_explicit(
                        "/project/a.rue",
                        path,
                        PhysicalFileIdentity::new(9, 9),
                        metadata_fingerprint(),
                        Arc::new("same inode".into()),
                    )
                    .unwrap();
            }
            (
                assembler.snapshot().unwrap(),
                assembler.accepted_read_manifest(),
            )
        };
        let forward = build(["/real/z-hard-a", "/real/a-hard-a"]);
        let reverse = build(["/real/a-hard-a", "/real/z-hard-a"]);
        assert_eq!(forward.0.metadata(), reverse.0.metadata());
        assert_eq!(forward.0.source_revision(), reverse.0.source_revision());
        assert_eq!(forward.1, reverse.1);
        assert!(
            forward
                .1
                .iter()
                .any(|entry| entry.canonical_path() == "/real/a-hard-a")
        );
    }

    #[test]
    fn close_joins_hard_link_alias_observation_by_physical_identity() {
        let source = snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const a = @import(\"a.rue\"); fn main() -> i32 { a.value() }",
                ),
                (2, "/project/a.rue", "a.rue", "pub fn value() -> i32 { 1 }"),
            ],
            1,
        );
        let mut manifest = accepted_reads(&source).to_vec();
        manifest
            .iter_mut()
            .find(|entry| entry.module().as_str() == "a.rue")
            .unwrap()
            .canonical_path = Arc::from("/real/a-hard-link");

        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(50),
                manifest.into(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        for request in plan.pending_requests(&ledger) {
            let observation = if request.requested_path().ends_with("/a.rue") {
                ImportObservation::accepted(
                    request.clone(),
                    AcceptedImportSource::new(
                        request.requested_path(),
                        "/real/z-hard-link",
                        PhysicalFileIdentity::new(1, 2),
                        metadata_fingerprint(),
                        Arc::new("pub fn value() -> i32 { 1 }".into()),
                    )
                    .unwrap(),
                )
                .unwrap()
            } else {
                ImportObservation::absent(request.clone())
            };
            ledger.record(observation).unwrap();
        }
        let closed = session.close_import_discovery(ledger).unwrap();
        assert_eq!(
            closed.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedValid
        );
        assert!(matches!(
            closed.graph().unwrap().graph().records()[0].resolution(),
            crate::CanonicalImportResolution::Resolved(module) if module.as_str() == "a.rue"
        ));
    }

    #[test]
    fn epoch_is_part_of_plan_identity() {
        let snapshot =
            SourceSnapshot::single("/project/main.rue", "fn main() -> i32 { 0 }").unwrap();
        let mut session = crate::CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let one = session.import_discovery_plan(context(1)).unwrap();
        let two = session.import_discovery_plan(context(2)).unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn malformed_import_is_retained_but_never_requests_discovery() {
        let snapshot =
            SourceSnapshot::single("/project/main.rue", "fn main() { @import(1); }").unwrap();
        let mut session = crate::CompilerSession::new();
        let parsed = session.update(&snapshot).into_owner_result().unwrap();
        assert_eq!(parsed.invalid_imports().len(), 1);
        assert!(matches!(
            parsed.invalid_imports()[0].shape(),
            crate::InvalidImportShape::NonStringArgument
        ));
        let plan = session.import_discovery_plan(context(1)).unwrap();
        assert!(
            plan.pending_requests(&ImportObservationLedger::default())
                .is_empty()
        );
    }

    #[test]
    fn cancellation_is_non_closure_and_denial_is_failure() {
        let snapshot = SourceSnapshot::single(
            "/project/main.rue",
            "const m = @import(\"thing\"); fn main() -> i32 { 0 }",
        )
        .unwrap();
        let mut session = crate::CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let plan = session.import_discovery_plan(context(7)).unwrap();
        // Policy v2: an extensionless specifier owes exactly one candidate,
        // the directory facade.
        let pending = plan.pending_requests(&ImportObservationLedger::default());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].requested_path(), "/project/thing/_thing.rue");
        assert_eq!(pending[0].role(), ImportCandidateRole::DirectoryFacade);

        let mut cancelled = ImportObservationLedger::default();
        cancelled
            .record(
                ImportObservation::failure(pending[0].clone(), ImportObservationStatus::Cancelled)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(plan.failures(&cancelled).count(), 1);
        assert!(plan.accepted_sources(&cancelled).is_empty());

        let mut complete = ImportObservationLedger::default();
        let source = AcceptedImportSource::new(
            Arc::from(pending[0].requested_path()),
            Arc::from("/real/facade"),
            identity(),
            metadata_fingerprint(),
            Arc::new("pub fn answer() -> i32 { 42 }".into()),
        )
        .unwrap();
        complete
            .record(ImportObservation::accepted(pending[0].clone(), source).unwrap())
            .unwrap();
        assert_eq!(plan.accepted_sources(&complete).len(), 1);
        assert!(plan.pending_requests(&complete).is_empty());

        let mut denied_arm = ImportObservationLedger::default();
        denied_arm
            .record(
                ImportObservation::failure(
                    pending[0].clone(),
                    ImportObservationStatus::DeniedLexical,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(plan.failures(&denied_arm).count(), 1);
        assert!(matches!(
            plan.failures(&denied_arm).next().unwrap().status(),
            ImportObservationStatus::DeniedLexical
        ));
        assert!(plan.accepted_sources(&denied_arm).is_empty());
    }

    #[test]
    fn conclusive_vendored_std_failures_block_a_later_toolchain_source() {
        let context = context(8);
        let occurrence = project_occurrence("main.rue", "std");
        let groups =
            discovery_groups_for_occurrence(&context, &occurrence, "/project/main.rue").unwrap();
        assert_eq!(groups.len(), 2, "vendored std precedes toolchain std");

        for (index, status) in [
            ImportObservationStatus::PresentUnreadable(Arc::from("permission denied")),
            ImportObservationStatus::InvalidPhysicalType {
                canonical_path: Arc::from("/project/std/_std.rue"),
            },
            ImportObservationStatus::UnstableRead(Arc::from("metadata changed")),
        ]
        .into_iter()
        .enumerate()
        {
            let mut ledger = ImportObservationLedger::default();
            ledger
                .record(ImportObservation::failure(groups[0][0].clone(), status).unwrap())
                .unwrap();
            let toolchain = AcceptedImportSource::new(
                groups[1][0].requested_path(),
                groups[1][0].requested_path(),
                PhysicalFileIdentity::new(2, index as u64 + 1),
                metadata_fingerprint(),
                Arc::new("pub fn answer() -> i32 { 42 }".into()),
            )
            .unwrap();
            ledger
                .record(ImportObservation::accepted(groups[1][0].clone(), toolchain).unwrap())
                .unwrap();

            assert!(exact_import_pending_requests(&groups, &ledger).is_empty());
            assert!(exact_import_has_failures(&groups, &ledger));
            assert!(exact_import_failure_for_site(groups.iter(), &ledger).is_some());
            assert!(matches!(
                exact_import_winner(groups.iter(), &ledger),
                ExactImportWinner::Missing
            ));
            assert!(accepted_sources_for(groups.iter(), &ledger).is_empty());
        }
    }

    #[test]
    fn cancelled_vendored_std_read_blocks_a_stale_toolchain_source() {
        let context = context(10);
        let occurrence = project_occurrence("main.rue", "std");
        let groups =
            discovery_groups_for_occurrence(&context, &occurrence, "/project/main.rue").unwrap();
        assert_eq!(groups.len(), 2, "vendored std precedes toolchain std");

        let mut ledger = ImportObservationLedger::default();
        ledger
            .record(
                ImportObservation::failure(
                    groups[0][0].clone(),
                    ImportObservationStatus::Cancelled,
                )
                .unwrap(),
            )
            .unwrap();
        let stale_toolchain = AcceptedImportSource::new(
            groups[1][0].requested_path(),
            groups[1][0].requested_path(),
            PhysicalFileIdentity::new(5, 1),
            metadata_fingerprint(),
            Arc::new("pub fn answer() -> i32 { 7 }".into()),
        )
        .unwrap();
        ledger
            .record(ImportObservation::accepted(groups[1][0].clone(), stale_toolchain).unwrap())
            .unwrap();

        assert!(exact_import_pending_requests(&groups, &ledger).is_empty());
        assert!(exact_import_has_failures(&groups, &ledger));
        assert!(matches!(
            exact_import_failure_for_site(groups.iter(), &ledger)
                .expect("cancellation must remain diagnostic")
                .status(),
            ImportObservationStatus::Cancelled
        ));
        assert!(matches!(
            exact_import_winner(groups.iter(), &ledger),
            ExactImportWinner::Missing
        ));
        assert!(accepted_sources_for(groups.iter(), &ledger).is_empty());
    }

    #[test]
    fn std_manifest_denial_and_absence_advance_to_toolchain_std() {
        let context = context(9);
        let occurrence = project_occurrence("main.rue", "std");
        let groups =
            discovery_groups_for_occurrence(&context, &occurrence, "/project/main.rue").unwrap();
        let mut ledger = ImportObservationLedger::default();
        ledger
            .record(
                ImportObservation::failure(
                    groups[0][0].clone(),
                    ImportObservationStatus::DeniedLexical,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            exact_import_pending_requests(&groups, &ledger),
            vec![groups[1][0].clone()]
        );

        let mut absent = ledger.clone();
        absent
            .record(ImportObservation::absent(groups[1][0].clone()))
            .unwrap();
        assert!(exact_import_pending_requests(&groups, &absent).is_empty());
        assert!(exact_import_has_failures(&groups, &absent));

        let mut fallback = ImportObservationLedger::default();
        fallback
            .record(ImportObservation::absent(groups[0][0].clone()))
            .unwrap();
        assert_eq!(
            exact_import_pending_requests(&groups, &fallback),
            vec![groups[1][0].clone()]
        );
        let source = AcceptedImportSource::new(
            groups[1][0].requested_path(),
            groups[1][0].requested_path(),
            PhysicalFileIdentity::new(3, 1),
            metadata_fingerprint(),
            Arc::new("pub fn answer() -> i32 { 7 }".into()),
        )
        .unwrap();
        fallback
            .record(ImportObservation::accepted(groups[1][0].clone(), source).unwrap())
            .unwrap();
        assert!(exact_import_failure_for_site(groups.iter(), &fallback).is_none());
        assert!(matches!(
            exact_import_winner(groups.iter(), &fallback),
            ExactImportWinner::Resolved(_)
        ));
    }

    #[test]
    fn discovery_state_preserves_last_good_and_distinguishes_attempted_from_nonclosure() {
        let valid = snapshot(
            &[(1, "/project/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = crate::CompilerSession::new();
        session
            .stage_import_discovery(
                &valid,
                context(1),
                accepted_reads(&valid),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let committed = session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap();
        assert_eq!(
            committed.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedValid
        );
        assert!(Arc::ptr_eq(
            committed
                .graph()
                .expect("closed-valid revision retains graph"),
            &session.committed_import_graph().unwrap()
        ));
        let last_good = session
            .last_good_discovery()
            .unwrap()
            .source_revision()
            .clone();

        let importing = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "const h = @import(\"helper\"); fn main() -> i32 { 0 }",
            )],
            1,
        );
        let plan = session
            .stage_import_discovery(
                &importing,
                context(2),
                accepted_reads(&importing),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let request = plan.pending_requests(&ImportObservationLedger::default())[0].clone();
        let mut carried = ImportObservationLedger::default();
        carried
            .record(
                ImportObservation::accepted(
                    request.clone(),
                    AcceptedImportSource::new(
                        request.requested_path(),
                        request.requested_path(),
                        PhysicalFileIdentity::new(9, 9),
                        FileMetadataFingerprint::new(9, 9, 9),
                        Arc::new("fn broken( {".into()),
                    )
                    .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let parse_failed = snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const h = @import(\"helper\"); fn main() -> i32 { 0 }",
                ),
                (2, "/project/helper.rue", "helper.rue", "fn broken( {"),
            ],
            1,
        );
        assert!(
            session
                .stage_import_discovery(
                    &parse_failed,
                    context(2),
                    accepted_reads(&parse_failed),
                    carried.clone(),
                )
                .is_err()
        );
        let attempted = session.discovery_attempt().unwrap();
        assert_eq!(
            attempted.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert_eq!(attempted.source_revision(), parse_failed.source_revision());
        assert_eq!(attempted.ledger(), &carried);
        assert!(attempted.plan().is_none());
        assert!(attempted.graph().is_none());
        assert!(!attempted.diagnostics().is_empty());
        let parse_diagnostics = session.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(
            &parse_diagnostics,
            &session.import_diagnostics().unwrap()
        ));
        assert_eq!(
            session.last_good_discovery().unwrap().source_revision(),
            &last_good
        );
        assert!(session.merge().is_err());

        let missing = snapshot(
            &[(
                (1),
                "/project/main.rue",
                "main.rue",
                "const x = @import(\"missing\"); fn main() -> i32 { 0 }",
            )],
            1,
        );
        let plan = session
            .stage_import_discovery(
                &missing,
                context(3),
                accepted_reads(&missing),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                ledger.record(ImportObservation::absent(request)).unwrap();
            }
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert!(matches!(
            &errors.first().unwrap().kind,
            ErrorKind::ModuleNotFound { .. }
        ));
        assert_eq!(
            session.discovery_attempt().unwrap().status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert_eq!(
            session.last_good_discovery().unwrap().source_revision(),
            &last_good
        );
        assert!(session.merge().is_err());

        let plan = session
            .stage_import_discovery(
                &missing,
                context(4),
                accepted_reads(&missing),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut cancelled = ImportObservationLedger::default();
        cancelled
            .record(
                ImportObservation::failure(
                    plan.pending_requests(&cancelled)[0].clone(),
                    ImportObservationStatus::Cancelled,
                )
                .unwrap(),
            )
            .unwrap();
        session.close_import_discovery(cancelled).unwrap_err();
        let attempt = session.discovery_attempt().unwrap();
        assert_eq!(
            attempt.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert!(attempt.graph().is_none());
        let diagnostics = session.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(
            &diagnostics,
            &session.import_diagnostics().unwrap()
        ));
        assert_eq!(
            session.last_good_discovery().unwrap().source_revision(),
            &last_good
        );
    }

    #[test]
    fn closed_import_free_discovery_publishes_the_exact_parse_without_reparsing() {
        let source = snapshot(
            &[(1, "/project/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = crate::CompilerSession::new();
        session
            .stage_import_discovery(
                &source,
                context(10),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let staged = session
            .discovery_attempt()
            .unwrap()
            .program()
            .unwrap()
            .clone();
        let closed = session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap();

        assert!(Arc::ptr_eq(closed.program().unwrap(), &staged));
        assert!(Arc::ptr_eq(session.published_owner().unwrap(), &staged));
        assert_eq!(closed.parse_work().syntax.lexer_invocations, 1);
        assert_eq!(closed.parse_work().syntax.parser_invocations, 1);
        assert_eq!(
            session.work().last_parse,
            crate::ParsedModulesWork::default()
        );

        let exact = session.update_for_presentation(&source);
        assert_eq!(exact.work().syntax.lexer_invocations, 0);
        assert_eq!(exact.work().syntax.parser_invocations, 0);
        assert!(Arc::ptr_eq(exact.result_owner().unwrap(), &staged));
    }

    #[test]
    fn open_and_failed_discovery_attempts_never_seed_the_published_parse() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "fn main() { @import(\"missing\"); }",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        session
            .stage_import_discovery(
                &source,
                context(12),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        assert!(session.published_owner().is_none());
        session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap_err();
        assert!(session.published_owner().is_none());

        let direct = session.update_for_presentation(&source);
        assert_eq!(direct.work(), crate::ParsedModulesWork::default());
        direct.into_result().unwrap();
    }

    #[test]
    fn discovery_work_resets_for_context_changes_and_superseding_edits() {
        let original = snapshot(
            &[(1, "/project/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let edited = snapshot(
            &[(1, "/project/main.rue", "main.rue", "fn main() -> i32 { 1 }")],
            1,
        );
        let mut session = crate::CompilerSession::new();
        session
            .stage_import_discovery(
                &original,
                context(20),
                accepted_reads(&original),
                ImportObservationLedger::default(),
            )
            .unwrap();
        assert_eq!(
            session
                .discovery_attempt()
                .unwrap()
                .parse_work()
                .syntax
                .parser_invocations,
            1
        );

        session
            .stage_import_discovery(
                &original,
                context(21),
                accepted_reads(&original),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let changed_context = session.discovery_attempt().unwrap().parse_work();
        assert_eq!(changed_context, crate::ParsedModulesWork::default());

        session
            .stage_import_discovery(
                &edited,
                context(21),
                accepted_reads(&edited),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let superseded = session.discovery_attempt().unwrap().parse_work();
        assert_eq!(superseded.syntax.parser_invocations, 1);
        assert_eq!(superseded.modules_considered, 1);
        assert_eq!(superseded.modules_reparsed, 1);
    }

    #[test]
    fn fixed_point_discovery_accumulates_exact_work_and_seeds_incremental_parse() {
        let main_text = "const h = @import(\"helper.rue\"); fn main() -> i32 { h.answer() }";
        let helper_text = "pub fn answer() -> i32 { 42 }";
        let root_only = snapshot(&[(1, "/project/main.rue", "main.rue", main_text)], 1);
        let complete = snapshot(
            &[
                (1, "/project/main.rue", "main.rue", main_text),
                (2, "/project/helper.rue", "helper.rue", helper_text),
            ],
            1,
        );
        let discovery_context = context(11);
        let mut session = crate::CompilerSession::new();
        let first = session
            .stage_import_discovery(
                &root_only,
                discovery_context.clone(),
                accepted_reads(&root_only),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        for request in first.pending_requests(&ledger) {
            let observation = if request.requested_path() == "/project/helper.rue" {
                ImportObservation::accepted(
                    request,
                    AcceptedImportSource::new(
                        "/project/helper.rue",
                        "/project/helper.rue",
                        PhysicalFileIdentity::new(1, 2),
                        metadata_fingerprint(),
                        Arc::new(helper_text.into()),
                    )
                    .unwrap(),
                )
                .unwrap()
            } else {
                ImportObservation::absent(request)
            };
            ledger.record(observation).unwrap();
        }
        let final_plan = session
            .stage_import_discovery(
                &complete,
                discovery_context,
                accepted_reads(&complete),
                ledger.clone(),
            )
            .unwrap();
        assert!(final_plan.pending_requests(&ledger).is_empty());
        let staged = session
            .discovery_attempt()
            .unwrap()
            .program()
            .unwrap()
            .clone();
        let closed = session.close_import_discovery(ledger).unwrap();
        let work = closed.parse_work();

        assert!(Arc::ptr_eq(closed.program().unwrap(), &staged));
        assert!(Arc::ptr_eq(session.published_owner().unwrap(), &staged));
        assert_eq!(work.syntax.lexer_invocations, 2);
        assert_eq!(work.syntax.parser_invocations, 2);
        assert_eq!(work.syntax.tokens, staged.token_count());
        assert_eq!(work.modules_considered, 3);
        assert_eq!(work.modules_reparsed, 2);
        assert_eq!(work.modules_reused, 1);

        let exact = session.update_for_presentation(&complete);
        assert_eq!(exact.work(), crate::ParsedModulesWork::default());
        assert!(Arc::ptr_eq(exact.result_owner().unwrap(), &staged));

        let broken_in_presentation_order = snapshot(
            &[
                (2, "/project/helper.rue", "helper.rue", "fn helper( {"),
                (1, "/project/main.rue", "main.rue", "fn main( {"),
            ],
            1,
        );
        let errors = session
            .update_for_presentation(&broken_in_presentation_order)
            .into_result()
            .unwrap_err();
        assert_eq!(
            errors
                .iter()
                .map(|error| error.span().unwrap().file_id)
                .collect::<Vec<_>>(),
            [FileId::new(2), FileId::new(1)]
        );

        let edited = snapshot(
            &[
                (1, "/project/main.rue", "main.rue", main_text),
                (
                    2,
                    "/project/helper.rue",
                    "helper.rue",
                    "pub fn answer() -> i32 { 43 }",
                ),
            ],
            1,
        );
        let edited_update = session.update_for_presentation(&edited);
        assert_eq!(edited_update.work().syntax.parser_invocations, 1);
        assert_eq!(edited_update.work().modules_reparsed, 1);
        assert_eq!(edited_update.work().modules_reused, 1);
        edited_update.into_result().unwrap();
    }

    #[test]
    fn incomplete_plain_ledger_closes_attempt_without_resolution_diagnostics() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "const x = @import(\"missing\"); fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        session
            .stage_import_discovery(
                &source,
                context(40),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let errors = session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap_err();
        assert!(matches!(
            &errors.first().unwrap().kind,
            ErrorKind::InvalidCompilerInput(_)
        ));
        let attempt = session.discovery_attempt().unwrap();
        assert_eq!(
            attempt.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert!(attempt.graph().is_none());
        assert!(!attempt.diagnostics().iter().any(|error| matches!(
            error.kind,
            ErrorKind::ModuleNotFound { .. } | ErrorKind::StdLibNotFound
        )));
        let diagnostics = session.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(
            &diagnostics,
            &session.import_diagnostics().unwrap()
        ));
    }

    #[test]
    fn minimum_diagnostics_put_malformed_shape_before_graph_outcomes() {
        let source = snapshot(
            &[(
                (1),
                "/project/main.rue",
                "main.rue",
                "fn main() { @import(1); @import(\"missing\"); }",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(9),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                ledger.record(ImportObservation::absent(request)).unwrap();
            }
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        let errors = errors.iter().collect::<Vec<_>>();
        assert!(matches!(
            &errors[0].kind,
            ErrorKind::ImportRequiresStringLiteral
        ));
        assert!(matches!(&errors[1].kind, ErrorKind::ModuleNotFound { .. }));
        assert_eq!(
            session.discovery_attempt().unwrap().status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
    }

    #[test]
    fn discovery_failures_globally_precede_outcomes_and_suppress_missing() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "fn main() { @import(\"missing\"); @import(\"denied\"); }",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(90),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                let observation = if request.exact_specifier() == "denied" {
                    ImportObservation::failure(request, ImportObservationStatus::DeniedLexical)
                        .unwrap()
                } else {
                    ImportObservation::absent(request)
                };
                ledger.record(observation).unwrap();
            }
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        let kinds = errors.iter().map(|error| &error.kind).collect::<Vec<_>>();
        assert_eq!(kinds.len(), 2, "the denied site must not also be Missing");
        assert!(matches!(kinds[0], ErrorKind::InvalidCompilerInput(_)));
        assert!(matches!(kinds[1], ErrorKind::ModuleNotFound { path, .. } if path == "missing"));
    }

    #[test]
    fn completed_io_and_policy_failures_close_with_one_memoized_batch() {
        let failures = [
            ImportObservationStatus::PresentUnreadable(Arc::from("permission denied")),
            ImportObservationStatus::DeniedLexical,
            ImportObservationStatus::DeniedCanonical {
                canonical_path: Arc::from("/outside/project/helper.rue"),
            },
            ImportObservationStatus::InvalidPhysicalType {
                canonical_path: Arc::from("/project/helper.rue"),
            },
            ImportObservationStatus::UnstableRead(Arc::from("metadata changed")),
        ];
        for (index, failure) in failures.into_iter().enumerate() {
            let source = snapshot(
                &[(
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "fn main() { @import(\"helper\"); }",
                )],
                1,
            );
            let mut session = crate::CompilerSession::new();
            let plan = session
                .stage_import_discovery(
                    &source,
                    context(120 + index as u64),
                    accepted_reads(&source),
                    ImportObservationLedger::default(),
                )
                .unwrap();
            let mut ledger = ImportObservationLedger::default();
            let pending = plan.pending_requests(&ledger);
            for (request_index, request) in pending.into_iter().enumerate() {
                let observation = if request_index == 0 {
                    ImportObservation::failure(request, failure.clone()).unwrap()
                } else {
                    ImportObservation::absent(request)
                };
                ledger.record(observation).unwrap();
            }
            assert!(plan.pending_requests(&ledger).is_empty());
            assert!(exact_import_has_failures(plan.groups(), &ledger));
            session.close_import_discovery(ledger).unwrap_err();
            let attempt = session.discovery_attempt().unwrap();
            assert_eq!(
                attempt.status(),
                crate::ImportDiscoveryRevisionStatus::ClosedAttempted
            );
            assert!(attempt.graph().is_none());
            let diagnostics = session.import_diagnostics().unwrap();
            assert_eq!(diagnostics.errors().len(), 1);
            assert!(!diagnostics.errors()[0].to_string().contains("not found"));
            assert!(Arc::ptr_eq(
                &diagnostics,
                &session.import_diagnostics().unwrap()
            ));
        }
    }

    #[test]
    fn legal_import_cycle_closes_valid_without_diagnostics() {
        let source = snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const a = @import(\"a.rue\"); fn main() -> i32 { 0 }",
                ),
                (
                    2,
                    "/project/a.rue",
                    "a.rue",
                    "const root = @import(\"main.rue\"); pub fn answer() -> i32 { 42 }",
                ),
            ],
            1,
        );
        let manifest = accepted_reads(&source);
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(130),
                manifest.clone(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                let observation = if let Some(entry) = manifest
                    .iter()
                    .find(|entry| entry.requested_path() == request.requested_path())
                {
                    let text = if entry.module().as_str() == "a.rue" {
                        "const root = @import(\"main.rue\"); pub fn answer() -> i32 { 42 }"
                    } else {
                        "const a = @import(\"a.rue\"); fn main() -> i32 { 0 }"
                    };
                    ImportObservation::accepted(
                        request,
                        AcceptedImportSource::new(
                            entry.requested_path(),
                            entry.canonical_path(),
                            entry.metadata_identity(),
                            entry.metadata_fingerprint(),
                            Arc::new(text.into()),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                } else {
                    ImportObservation::absent(request)
                };
                ledger.record(observation).unwrap();
            }
        }
        let closed = session.close_import_discovery(ledger).unwrap();
        assert_eq!(
            closed.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedValid
        );
        assert!(closed.diagnostics().is_empty());
        assert!(session.import_diagnostics().unwrap().errors().is_empty());
    }

    #[test]
    fn repeated_missing_occurrences_each_receive_one_ordered_diagnostic() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "fn main() { @import(\"same\"); @import(\"same\"); }",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(91),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        let first_offset = plan.groups()[0][0].occurrence().source_offset();
        let expected_candidates = plan
            .groups()
            .iter()
            .filter(|group| group[0].occurrence().source_offset() == first_offset)
            .flat_map(|group| group.iter())
            .map(|request| request.requested_path().to_owned())
            .collect::<Vec<_>>();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                ledger.record(ImportObservation::absent(request)).unwrap();
            }
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(|error| matches!(
            &error.kind,
            ErrorKind::ModuleNotFound { path, candidates }
                if path == "same" && candidates == &expected_candidates
        )));
        assert!(
            errors.as_slice()[0].span().unwrap().start < errors.as_slice()[1].span().unwrap().start
        );
    }

    #[test]
    fn canonical_import_batch_is_reused_and_manifest_provenance_prevents_stale_reuse() {
        let source = snapshot(
            &[(1, "/project/main.rue", "main.rue", "fn main() -> i32 { 0 }")],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let mut first_manifest = accepted_reads(&source).to_vec();
        first_manifest[0].content_fingerprint ^= 1;
        session
            .stage_import_discovery(
                &source,
                context(92),
                first_manifest.into(),
                ImportObservationLedger::default(),
            )
            .unwrap_err();
        let first = session.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(&first, &session.import_diagnostics().unwrap()));

        let mut second_manifest = accepted_reads(&source).to_vec();
        second_manifest[0].content_fingerprint ^= 2;
        session
            .stage_import_discovery(
                &source,
                context(92),
                second_manifest.into(),
                ImportObservationLedger::default(),
            )
            .unwrap_err();
        let second = session.import_diagnostics().unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        let crate::FrontendDiagnosticIdentity::Import(input) = second.identity() else {
            panic!("failed discovery must publish the import diagnostic stage")
        };
        assert_eq!(input.source_revision(), source.source_revision());
    }

    #[test]
    fn context_plan_and_ledger_are_independent_diagnostic_cache_provenance() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "fn main() { @import(\"missing\"); }",
            )],
            1,
        );
        let manifest = accepted_reads(&source);
        let mut session = crate::CompilerSession::new();

        session
            .stage_import_discovery(
                &source,
                context(140),
                manifest.clone(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap_err();
        let first = session.import_diagnostics().unwrap();

        let second_plan = session
            .stage_import_discovery(
                &source,
                context(141),
                manifest.clone(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap_err();
        let second = session.import_diagnostics().unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        let crate::FrontendDiagnosticIdentity::Import(first_input) = first.identity() else {
            unreachable!()
        };
        let crate::FrontendDiagnosticIdentity::Import(second_input) = second.identity() else {
            unreachable!()
        };
        assert_ne!(first_input.context(), second_input.context());
        assert_ne!(first_input.plan(), second_input.plan());
        assert_eq!(first_input.ledger(), second_input.ledger());
        assert_eq!(
            first_input.accepted_read_manifest(),
            second_input.accepted_read_manifest()
        );

        let mut partial = ImportObservationLedger::default();
        partial
            .record(ImportObservation::absent(
                second_plan.pending_requests(&partial)[0].clone(),
            ))
            .unwrap();
        session
            .stage_import_discovery(
                &source,
                context(141),
                manifest.clone(),
                ImportObservationLedger::default(),
            )
            .unwrap();
        session.close_import_discovery(partial.clone()).unwrap_err();
        let third = session.import_diagnostics().unwrap();
        assert!(!Arc::ptr_eq(&second, &third));
        let crate::FrontendDiagnosticIdentity::Import(third_input) = third.identity() else {
            unreachable!()
        };
        assert_eq!(second_input.context(), third_input.context());
        assert_eq!(second_input.plan(), third_input.plan());
        assert_ne!(second_input.ledger(), third_input.ledger());
        assert_eq!(
            second_input.accepted_read_manifest(),
            third_input.accepted_read_manifest()
        );

        session
            .stage_import_discovery(
                &source,
                context(141),
                manifest,
                ImportObservationLedger::default(),
            )
            .unwrap();
        session.close_import_discovery(partial).unwrap_err();
        assert!(Arc::ptr_eq(&third, &session.import_diagnostics().unwrap()));
    }

    #[test]
    fn accepted_observation_without_manifest_provenance_closes_with_memoized_diagnostics() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "const helper = @import(\"helper\"); fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(94),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let request = plan.pending_requests(&ImportObservationLedger::default())[0].clone();
        let accepted = AcceptedImportSource::new(
            request.requested_path(),
            request.requested_path(),
            PhysicalFileIdentity::new(9, 9),
            metadata_fingerprint(),
            Arc::new("pub fn answer() -> i32 { 42 }".into()),
        )
        .unwrap();
        let mut ledger = ImportObservationLedger::default();
        for pending in plan.pending_requests(&ledger) {
            let observation = if pending == request {
                ImportObservation::accepted(pending, accepted.clone()).unwrap()
            } else {
                ImportObservation::absent(pending)
            };
            ledger.record(observation).unwrap();
        }
        assert!(plan.pending_requests(&ledger).is_empty());

        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert!(matches!(
            errors.first().unwrap().kind,
            ErrorKind::InvalidCompilerInput(_)
        ));
        let attempt = session.discovery_attempt().unwrap();
        assert_eq!(
            attempt.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert!(attempt.graph().is_none());
        let diagnostics = session.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(
            &diagnostics,
            &session.import_diagnostics().unwrap()
        ));
    }

    #[test]
    fn failed_std_import_blocks_air_before_secondary_unknown_type_errors() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "const std = @import(\"std\"); struct Holder { value: std.Missing } fn main() {}",
            )],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                context(93),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                ledger.record(ImportObservation::absent(request)).unwrap();
            }
        }
        session.close_import_discovery(ledger).unwrap_err();
        let errors = session
            .rooted_cfg(&crate::CompileOptions::default())
            .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.first().unwrap().kind,
            ErrorKind::StdLibNotFound
        ));
    }

    #[test]
    fn direct_batch_session_uses_compiler_shape_preflight_before_air() {
        let source = SourceSnapshot::single(
            "/project/main.rue",
            "fn main() -> i32 { let bad = @import(1); 0 }",
        )
        .unwrap();
        let mut session = crate::CompilerSession::new();
        session.update(&source).into_result().unwrap();
        let errors = session.canonical_rir().unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.first().unwrap().kind,
            ErrorKind::ImportRequiresStringLiteral
        ));
        let first = session.import_diagnostics().unwrap();
        let second = session.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.work().import_diagnostics.executions, 1);
        assert_eq!(session.work().import_diagnostics.reuses, 2);
        let crate::FrontendDiagnosticIdentity::Import(input) = first.identity() else {
            panic!("direct batch import preflight must use the import diagnostic stage")
        };
        assert!(input.context().is_none());
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().last_rir, crate::CanonicalRirWork::default());
        assert!(
            session
                .rooted_cfg(&crate::CompileOptions::default())
                .is_err()
        );
        let one_shot =
            crate::compile_snapshot(&source, &crate::CompileOptions::default()).unwrap_err();
        assert!(matches!(
            one_shot.first().unwrap().kind,
            ErrorKind::ImportRequiresStringLiteral
        ));
    }

    #[test]
    fn malformed_import_shape_diagnostics_preserve_preflight_spans() {
        for (call, expected) in [
            ("@import()", "@import()"),
            ("@import(1)", "1"),
            ("@import(\"a\", \"b\")", "@import(\"a\", \"b\")"),
        ] {
            let source_text = format!("fn main() {{ let bad = {call}; }}");
            let source = SourceSnapshot::single("/project/main.rue", &source_text).unwrap();
            let mut session = crate::CompilerSession::new();
            session.update(&source).into_result().unwrap();
            let diagnostics = session.import_diagnostics().unwrap();
            let span = diagnostics.errors()[0].span().unwrap();
            let expected_start = source_text.find(expected).unwrap() as u32;
            assert_eq!(span.start, expected_start, "wrong start for {call}");
            assert_eq!(
                span.end,
                expected_start + expected.len() as u32,
                "wrong end for {call}"
            );
        }
    }

    #[test]
    fn staged_malformed_import_retains_its_failed_plan_diagnostics() {
        let source =
            SourceSnapshot::single("/project/main.rue", "fn main() { let bad = @import(1); }")
                .unwrap();
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &source,
                crate::ImportDiscoveryContext::new(1, "/project", Some("/project/std"), "all")
                    .unwrap(),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        assert!(
            plan.pending_requests(&ImportObservationLedger::default())
                .is_empty()
        );
        session
            .close_import_discovery(ImportObservationLedger::default())
            .unwrap_err();
        let diagnostics = session.import_diagnostics().unwrap();
        assert!(
            matches!(
                diagnostics.errors()[0].kind,
                ErrorKind::ImportRequiresStringLiteral
            ),
            "unexpected staged diagnostics: {:?}",
            diagnostics.errors()
        );
    }

    #[test]
    fn both_forms_resolve_to_facade_and_std_missing_closes_attempted() {
        // Policy v2: a tree containing BOTH thing.rue and thing/_thing.rue is
        // not ambiguous — the extensionless specifier names the facade alone,
        // and the file spelling requires its extension. The former E0708
        // surface is deleted, not diagnosed.
        let both_forms = snapshot(
            &[
                (
                    1,
                    "/project/main.rue",
                    "main.rue",
                    "const x = @import(\"thing\"); fn main() -> i32 { 0 }",
                ),
                (2, "/project/thing.rue", "thing.rue", "const x = 1;"),
                (
                    3,
                    "/project/thing/_thing.rue",
                    "thing/_thing.rue",
                    "const x = 2;",
                ),
            ],
            1,
        );
        let mut session = crate::CompilerSession::new();
        let plan = session
            .stage_import_discovery(
                &both_forms,
                context(10),
                accepted_reads(&both_forms),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        let pending = plan.pending_requests(&ledger);
        assert_eq!(pending.len(), 1, "one facade candidate, never the file");
        for request in pending {
            assert!(request.requested_path().ends_with("/_thing.rue"));
            let source = AcceptedImportSource::new(
                Arc::from(request.requested_path()),
                Arc::from(request.requested_path()),
                PhysicalFileIdentity::new(1, 3),
                metadata_fingerprint(),
                Arc::new("const x = 2;".into()),
            )
            .unwrap();
            ledger
                .record(ImportObservation::accepted(request, source).unwrap())
                .unwrap();
        }
        let closed = session.close_import_discovery(ledger).unwrap();
        assert_eq!(
            closed.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedValid
        );
        let graph = closed.graph().expect("valid close retains graph");
        let record = graph
            .graph()
            .records()
            .iter()
            .find(|record| record.normalized_specifier() == "thing")
            .expect("occurrence resolves");
        assert!(matches!(
            record.resolution(),
            crate::CanonicalImportResolution::Resolved(module)
                if module.as_str() == "thing/_thing.rue"
        ));

        let std_missing = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "const x = @import(\"std\"); fn main() -> i32 { 0 }",
            )],
            1,
        );
        let plan = session
            .stage_import_discovery(
                &std_missing,
                context(11),
                accepted_reads(&std_missing),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                ledger.record(ImportObservation::absent(request)).unwrap();
            }
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert!(matches!(
            &errors.first().unwrap().kind,
            ErrorKind::StdLibNotFound
        ));
        assert_eq!(
            session.discovery_attempt().unwrap().status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
    }

    #[test]
    fn foreign_epoch_observations_cannot_close_a_staging_revision() {
        let source = snapshot(
            &[(
                1,
                "/project/main.rue",
                "main.rue",
                "const x = @import(\"missing\"); fn main() -> i32 { 0 }",
            )],
            1,
        );
        let mut current = crate::CompilerSession::new();
        current
            .stage_import_discovery(
                &source,
                context(30),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut foreign = crate::CompilerSession::new();
        let foreign_plan = foreign
            .stage_import_discovery(
                &source,
                context(31),
                accepted_reads(&source),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        ledger
            .record(ImportObservation::absent(
                foreign_plan.pending_requests(&ledger)[0].clone(),
            ))
            .unwrap();
        let errors = current.close_import_discovery(ledger).unwrap_err();
        assert!(matches!(
            &errors.first().unwrap().kind,
            ErrorKind::InvalidCompilerInput(_)
        ));
        assert_eq!(
            current.discovery_attempt().unwrap().status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert!(current.discovery_attempt().unwrap().graph().is_none());
        let diagnostics = current.import_diagnostics().unwrap();
        assert!(Arc::ptr_eq(
            &diagnostics,
            &current.import_diagnostics().unwrap()
        ));
        assert!(current.merge().is_err());
    }
}
