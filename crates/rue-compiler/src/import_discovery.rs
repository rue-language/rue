//! Compiler-owned import discovery protocol.
//!
//! Candidate policy is pure compiler state. Hosts execute only requests returned
//! by [`ImportDiscoveryPlan::pending_requests`] and report typed observations;
//! they never recognize imports or choose resolution precedence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rue_error::{AmbiguousModuleData, CompileError, CompileErrors, CompileResult, ErrorKind};
use rue_span::FileId;

use crate::{
    ImportDirective, ModuleId, ParsedProgram, SourceMetadata, SourceRevision, SourceSnapshot,
};

/// Version of the target-independent candidate policy represented by plans.
pub const IMPORT_DISCOVERY_POLICY_VERSION: u32 = 1;

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
    fn from_directive(site: &ImportDirective) -> Self {
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
}

/// Result of one host transaction. Denial and absence are intentionally
/// distinct from an accepted source read.
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
    fn is_present(&self) -> bool {
        matches!(self, Self::PresentReadable { .. })
    }
    pub fn is_failure(&self) -> bool {
        !matches!(self, Self::Absent | Self::PresentReadable { .. })
    }
}

/// Accepted source bytes paired with their transaction observation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AcceptedImportSource {
    requested_path: Arc<str>,
    canonical_path: Arc<str>,
    metadata_identity: PhysicalFileIdentity,
    metadata_fingerprint: FileMetadataFingerprint,
    content_fingerprint: u64,
    source: Arc<String>,
}

impl AcceptedImportSource {
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
    pub fn absent(request: ImportDiscoveryRequest) -> Self {
        Self {
            request,
            status: ImportObservationStatus::Absent,
            accepted_source: None,
        }
    }
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
}

/// Deterministically ordered observations from one immutable epoch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ImportObservationLedger(BTreeMap<ImportDiscoveryRequest, ImportObservation>);

impl ImportObservationLedger {
    pub fn record(&mut self, observation: ImportObservation) -> CompileResult<()> {
        let request = observation.request.clone();
        if let Some(previous) = self.0.get(&request) {
            if previous == &observation {
                return Ok(());
            }
            return Err(invalid_input(
                "one discovery request received conflicting observations",
            ));
        }
        self.0.insert(request, observation);
        Ok(())
    }
    pub fn get(&self, request: &ImportDiscoveryRequest) -> Option<&ImportObservation> {
        self.0.get(request)
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ImportObservation> {
        self.0.values()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportDiscoveryPlan {
    source_revision: SourceRevision,
    context: ImportDiscoveryContext,
    groups: Arc<[Arc<[ImportDiscoveryRequest]>]>,
}

impl ImportDiscoveryPlan {
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
        let root_dir = context.project_root().to_owned();
        let mut groups = Vec::new();
        for site in program.import_directives().iter() {
            let importer_path = requested_path_for_module(&context, site.importer())?;
            let importer_dir = parent_dir(&importer_path);
            let mut bases = vec![importer_dir];
            if !bases.contains(&root_dir) {
                bases.push(root_dir.clone());
            }
            let candidate_groups =
                discovery_candidate_groups(site.specifier(), &bases, context.std_root());
            let occurrence = ImportOccurrenceKey::from_directive(site);
            let normalized_specifier = normalize_path(site.specifier());
            for (group_index, candidates) in candidate_groups.into_iter().enumerate() {
                let group_index = u32::try_from(group_index)
                    .map_err(|_| invalid_input("import candidate group count exceeds u32::MAX"))?;
                let requests = candidates
                    .into_iter()
                    .enumerate()
                    .map(|(position, candidate)| {
                        let position = u32::try_from(position)
                            .expect("candidate group size is bounded by policy");
                        let role = candidate_role(site.specifier(), group_index, position);
                        ImportDiscoveryRequest {
                            context: context.clone(),
                            occurrence: occurrence.clone(),
                            exact_specifier: Arc::from(site.specifier()),
                            normalized_specifier: Arc::from(normalized_specifier.as_str()),
                            importer_anchor: Arc::from(importer_path.as_str()),
                            root_anchor: Arc::from(context.project_root()),
                            group: group_index,
                            position,
                            requested_path: Arc::from(normalize_path(&candidate)),
                            role,
                        }
                    })
                    .collect::<Vec<_>>();
                groups.push(requests.into());
            }
        }
        groups.sort_by(|left: &Arc<[ImportDiscoveryRequest]>, right| left[0].cmp(&right[0]));
        Ok(Self {
            source_revision: program.source_revision().clone(),
            context,
            groups: groups.into(),
        })
    }

    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn context(&self) -> &ImportDiscoveryContext {
        &self.context
    }
    pub fn groups(&self) -> &[Arc<[ImportDiscoveryRequest]>] {
        &self.groups
    }

    pub(crate) fn validate_ledger(&self, ledger: &ImportObservationLedger) -> CompileResult<()> {
        let requests = self
            .groups
            .iter()
            .flat_map(|group| group.iter())
            .collect::<BTreeSet<_>>();
        if ledger.iter().any(|observation| {
            observation.request().context() != &self.context
                || !requests.contains(observation.request())
        }) {
            return Err(invalid_input(
                "observation ledger contains a request outside the current discovery plan and epoch",
            ));
        }
        Ok(())
    }

    pub(crate) fn reduce_graph(
        &self,
        root: ModuleId,
        ledger: &ImportObservationLedger,
        accepted_reads: &[AcceptedReadManifestEntry],
    ) -> CompileResult<crate::CanonicalImportGraph> {
        self.validate_ledger(ledger)?;
        let manifest = accepted_reads
            .iter()
            .map(|entry| (entry.metadata_identity(), entry))
            .collect::<BTreeMap<_, _>>();
        if manifest.len() != accepted_reads.len() {
            return Err(invalid_input(
                "accepted read manifest contains duplicate physical identities",
            ));
        }
        let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
            BTreeMap::new();
        for group in self.groups.iter() {
            by_site.entry(&group[0].occurrence).or_default().push(group);
        }
        let mut records = Vec::with_capacity(by_site.len());
        for (site, groups) in by_site {
            let winning = groups.iter().find_map(|group| {
                let present = group
                    .iter()
                    .filter_map(|request| ledger.get(request))
                    .filter_map(ImportObservation::accepted_source)
                    .collect::<Vec<_>>();
                (!present.is_empty()).then_some(present)
            });
            let module_for = |source: &AcceptedImportSource| -> CompileResult<ModuleId> {
                let entry = manifest.get(&source.metadata_identity()).ok_or_else(|| {
                    invalid_input(format!(
                        "accepted observation for physical identity {:?} is absent from the accepted read manifest",
                        source.metadata_identity()
                    ))
                })?;
                if entry.metadata_identity() != source.metadata_identity()
                    || entry.metadata_fingerprint() != source.metadata_fingerprint()
                    || entry.content_fingerprint() != source.content_fingerprint()
                {
                    return Err(invalid_input(
                        "accepted observation does not match its accepted read provenance",
                    ));
                }
                Ok(entry.module().clone())
            };
            let resolution = match winning.as_deref() {
                Some([source]) => crate::CanonicalImportResolution::Resolved(module_for(source)?),
                Some([file, directory, ..]) => crate::CanonicalImportResolution::Ambiguous {
                    file_module: module_for(file)?,
                    directory_module: module_for(directory)?,
                },
                Some([]) => unreachable!(),
                None => crate::CanonicalImportResolution::Missing,
            };
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

    /// Return exactly the next operations allowed by candidate precedence.
    pub fn pending_requests(
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
                let observations = group
                    .iter()
                    .map(|request| ledger.get(request))
                    .collect::<Vec<_>>();
                if observations
                    .iter()
                    .any(|observation| observation.is_some_and(|o| o.status.is_failure()))
                {
                    break;
                }
                let missing = group
                    .iter()
                    .zip(&observations)
                    .filter_map(|(request, observation)| {
                        observation.is_none().then_some(request.clone())
                    })
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    pending.extend(missing);
                    break;
                }
                if observations
                    .iter()
                    .any(|observation| observation.is_some_and(|o| o.status.is_present()))
                {
                    break;
                }
            }
        }
        pending
    }

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
        let mut by_site: BTreeMap<&ImportOccurrenceKey, Vec<&Arc<[ImportDiscoveryRequest]>>> =
            BTreeMap::new();
        for group in self.groups.iter() {
            by_site.entry(&group[0].occurrence).or_default().push(group);
        }
        let mut accepted = Vec::new();
        for groups in by_site.values() {
            for group in groups {
                if group
                    .iter()
                    .any(|request| ledger.get(request).is_some_and(|o| o.status.is_failure()))
                {
                    break;
                }
                let present = group
                    .iter()
                    .filter_map(|request| ledger.get(request))
                    .filter_map(ImportObservation::accepted_source)
                    .collect::<Vec<_>>();
                if !present.is_empty() {
                    accepted.extend(present);
                    break;
                }
            }
        }
        accepted.sort_by(|left, right| {
            left.requested_path
                .cmp(&right.requested_path)
                .then(left.canonical_path.cmp(&right.canonical_path))
        });
        accepted.dedup_by(|left, right| {
            left.canonical_path == right.canonical_path
                && left.content_fingerprint == right.content_fingerprint
        });
        accepted
    }

    /// Project this plan's parser-owned occurrences and physical observations
    /// into the sole import diagnostic batch for a closed attempt.
    ///
    /// Shape errors precede discovery failures, which precede graph outcomes.
    /// The ordered occurrence key keeps repeated source sites distinct even
    /// though the durable graph deliberately collapses equivalent records.
    pub(crate) fn diagnostics(
        &self,
        program: &ParsedProgram,
        ledger: &ImportObservationLedger,
    ) -> CompileErrors {
        let mut errors = Self::shape_diagnostics(program);

        let mut groups_by_site: BTreeMap<
            &ImportOccurrenceKey,
            Vec<&Arc<[ImportDiscoveryRequest]>>,
        > = BTreeMap::new();
        for group in self.groups.iter() {
            groups_by_site
                .entry(group[0].occurrence())
                .or_default()
                .push(group);
        }
        let mut failed_sites = BTreeSet::new();
        for (site, groups) in &groups_by_site {
            let file_id = program
                .module(site.importer())
                .expect("plan importer belongs to parsed program")
                .file_id();
            let span = rue_span::Span::with_file(file_id, site.source_offset(), site.source_end());
            if let Some(failure) = groups
                .iter()
                .flat_map(|group| group.iter())
                .filter_map(|request| ledger.get(request))
                .find(|observation| observation.status().is_failure())
            {
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
                    | ImportObservationStatus::PresentReadable { .. } => {
                        unreachable!("only failure observations reach diagnostic projection")
                    }
                };
                errors.push(CompileError::new(
                    ErrorKind::InvalidCompilerInput(message),
                    span,
                ));
                failed_sites.insert((*site).clone());
            }
        }

        for (site, groups) in groups_by_site {
            if failed_sites.contains(site) {
                continue;
            }
            let file_id = program
                .module(site.importer())
                .expect("plan importer belongs to parsed program")
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
                Some([file, directory, ..]) => errors.push(CompileError::new(
                    ErrorKind::AmbiguousModule(Box::new(AmbiguousModuleData {
                        path: site.specifier().into(),
                        file_module: file.requested_path().into(),
                        dir_module: directory.requested_path().into(),
                    })),
                    span,
                )),
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
        }
        errors
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AcceptedReadManifestEntry {
    module: ModuleId,
    requested_path: Arc<str>,
    canonical_path: Arc<str>,
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
        self.add_source(&AcceptedImportSource::new(
            Arc::from(normalize_absolute(requested_path)?),
            Arc::from(canonical_path),
            metadata_identity,
            metadata_fingerprint,
            source,
        )?)
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
        let mut added = 0;
        for source in plan.accepted_sources(ledger) {
            added += usize::from(self.add_source(source)?);
        }
        Ok(added)
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
                self.entries.get_mut(&owner.module).unwrap().canonical_path =
                    source.canonical_path.clone();
                self.accepted_reads
                    .get_mut(&owner.module)
                    .unwrap()
                    .canonical_path = source.canonical_path.clone();
                self.physical
                    .get_mut(&source.metadata_identity)
                    .unwrap()
                    .canonical_path = source.canonical_path.clone();
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
        self.accepted_reads.insert(
            module.clone(),
            AcceptedReadManifestEntry {
                module,
                requested_path: source.requested_path.clone(),
                canonical_path: source.canonical_path.clone(),
                metadata_identity: source.metadata_identity,
                metadata_fingerprint: source.metadata_fingerprint,
                content_fingerprint: source.content_fingerprint,
            },
        );
        Ok(true)
    }

    pub fn snapshot(&self) -> CompileResult<SourceSnapshot> {
        let root_module = &self.root_module;
        let ordered = std::iter::once((root_module, self.entries.get(root_module).unwrap())).chain(
            self.entries
                .iter()
                .filter(|(module, _)| *module != root_module),
        );
        let ordered = ordered.collect::<Vec<_>>();
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
        SourceSnapshot::new(metadata, contents)
    }

    pub fn accepted_read_manifest(&self) -> Arc<[AcceptedReadManifestEntry]> {
        self.accepted_reads.values().cloned().collect()
    }
}

fn candidate_role(specifier: &str, group: u32, position: u32) -> ImportCandidateRole {
    if specifier == "std" && group == 0 {
        ImportCandidateRole::StandardLibraryFacade
    } else if specifier.ends_with(".rue") {
        ImportCandidateRole::ExactFile
    } else if position == 0 {
        ImportCandidateRole::FileModule
    } else {
        ImportCandidateRole::DirectoryFacade
    }
}

fn discovery_candidate_groups(
    specifier: &str,
    base_dirs: &[String],
    std_root: Option<&str>,
) -> Vec<Vec<String>> {
    let join = |base: &str, relative: &str| {
        Path::new(base)
            .join(relative)
            .to_string_lossy()
            .into_owned()
    };
    if specifier == "std" {
        return std_root
            .into_iter()
            .map(|root| vec![join(root, "_std.rue")])
            .chain(
                base_dirs
                    .iter()
                    .map(|base| vec![join(base, "std/_std.rue")]),
            )
            .collect();
    }
    if specifier.ends_with(".rue") {
        return base_dirs
            .iter()
            .map(|base| vec![join(base, specifier)])
            .collect();
    }
    let basename = Path::new(specifier)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(specifier);
    base_dirs
        .iter()
        .map(|base| {
            vec![
                join(base, &format!("{specifier}.rue")),
                join(base, &format!("{specifier}/_{basename}.rue")),
            ]
        })
        .collect()
}

fn requested_path_for_module(
    context: &ImportDiscoveryContext,
    module: &ModuleId,
) -> CompileResult<String> {
    const STD_PREFIX: &str = "\0rue-std/";
    if let Some(relative) = module.as_str().strip_prefix(STD_PREFIX) {
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

fn display_path_for_module(module: &ModuleId, entry: &AssembledSource) -> String {
    if module.as_str().starts_with("\0rue-std/") {
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
            return ModuleId::from_trusted_standard_library_path(
                Path::new("\0rue-std").join(relative).to_string_lossy(),
            );
        }
    }
    let project_root = Path::new(context.project_root());
    let logical = lexical_relative_path(project_root, requested).ok_or_else(|| {
        invalid_input(format!(
            "source {:?} cannot receive a project-relative durable identity",
            requested
        ))
    })?;
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
    use std::collections::HashMap;

    fn context(epoch: u64) -> ImportDiscoveryContext {
        ImportDiscoveryContext::new(epoch, "/project", Some("/sdk"), "all").unwrap()
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
                    .collect::<HashMap<_, _>>(),
                entries
                    .iter()
                    .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).into()))
                    .collect::<HashMap<_, _>>(),
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
                metadata_identity: PhysicalFileIdentity::new(1, source.file_id.index() as u64),
                metadata_fingerprint: metadata_fingerprint(),
                content_fingerprint: source_fingerprint(source.source),
            })
            .collect()
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
                    "const a = @import(\"a\"); fn main() -> i32 { a.value() }",
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
        let semantic = session.semantic(&crate::CompileOptions::default()).unwrap();
        assert_eq!(semantic.functions().len(), 2);
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
        let parsed = session.update(&snapshot).into_result().unwrap();
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
    fn cancellation_is_non_closure_and_accepted_ambiguity_reads_both_candidates() {
        let snapshot = SourceSnapshot::single(
            "/project/main.rue",
            "const m = @import(\"thing\"); fn main() -> i32 { 0 }",
        )
        .unwrap();
        let mut session = crate::CompilerSession::new();
        session.update(&snapshot).into_result().unwrap();
        let plan = session.import_discovery_plan(context(7)).unwrap();
        let pending = plan.pending_requests(&ImportObservationLedger::default());
        assert_eq!(pending.len(), 2);

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
        for (request, canonical) in pending.into_iter().zip(["/real/file", "/real/facade"]) {
            let source = AcceptedImportSource::new(
                Arc::from(request.requested_path()),
                Arc::from(canonical),
                identity(),
                metadata_fingerprint(),
                Arc::new("pub fn answer() -> i32 { 42 }".into()),
            )
            .unwrap();
            complete
                .record(ImportObservation::accepted(request, source).unwrap())
                .unwrap();
        }
        assert_eq!(plan.accepted_sources(&complete).len(), 2);
        assert!(plan.pending_requests(&complete).is_empty());

        let pending = plan.pending_requests(&ImportObservationLedger::default());
        let mut denied_arm = ImportObservationLedger::default();
        let accepted = AcceptedImportSource::new(
            Arc::from(pending[0].requested_path()),
            Arc::from("/real/file"),
            identity(),
            metadata_fingerprint(),
            Arc::new("pub fn answer() -> i32 { 42 }".into()),
        )
        .unwrap();
        denied_arm
            .record(ImportObservation::accepted(pending[0].clone(), accepted).unwrap())
            .unwrap();
        denied_arm
            .record(
                ImportObservation::failure(
                    pending[1].clone(),
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
        assert!(Arc::ptr_eq(
            committed
                .graph()
                .expect("closed-valid revision retains graph"),
            &session.import_graph(Some("/sdk")).unwrap()
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
                    "const a = @import(\"a\"); fn main() -> i32 { 0 }",
                ),
                (
                    2,
                    "/project/a.rue",
                    "a.rue",
                    "const root = @import(\"main\"); pub fn answer() -> i32 { 42 }",
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
                        "const root = @import(\"main\"); pub fn answer() -> i32 { 42 }"
                    } else {
                        "const a = @import(\"a\"); fn main() -> i32 { 0 }"
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
        let crate::FrontendDiagnosticStage::Import(input) = second.stage() else {
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
        let crate::FrontendDiagnosticStage::Import(first_input) = first.stage() else {
            unreachable!()
        };
        let crate::FrontendDiagnosticStage::Import(second_input) = second.stage() else {
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
        let crate::FrontendDiagnosticStage::Import(third_input) = third.stage() else {
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
            .semantic(&crate::CompileOptions::default())
            .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.first().unwrap().kind,
            ErrorKind::StdLibNotFound
        ));
        assert_eq!(session.work().semantic.executions, 0);
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
        let errors = session.rir().unwrap_err();
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
        let crate::FrontendDiagnosticStage::Import(input) = first.stage() else {
            panic!("direct batch import preflight must use the import diagnostic stage")
        };
        assert!(input.context().is_none());
        assert_eq!(session.work().rir.executions, 0);
        assert_eq!(session.work().last_rir, crate::CanonicalRirWork::default());
        assert!(session.semantic(&crate::CompileOptions::default()).is_err());
        assert_eq!(session.work().semantic.executions, 0);
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
    fn ambiguous_and_std_missing_close_as_attempted_with_typed_diagnostics() {
        let ambiguous = snapshot(
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
                &ambiguous,
                context(10),
                accepted_reads(&ambiguous),
                ImportObservationLedger::default(),
            )
            .unwrap();
        let mut ledger = ImportObservationLedger::default();
        for request in plan.pending_requests(&ledger) {
            let (contents, identity) = if request.requested_path().ends_with("/_thing.rue") {
                ("const x = 2;", PhysicalFileIdentity::new(1, 3))
            } else {
                ("const x = 1;", PhysicalFileIdentity::new(1, 2))
            };
            let source = AcceptedImportSource::new(
                Arc::from(request.requested_path()),
                Arc::from(request.requested_path()),
                identity,
                metadata_fingerprint(),
                Arc::new(contents.into()),
            )
            .unwrap();
            ledger
                .record(ImportObservation::accepted(request, source).unwrap())
                .unwrap();
        }
        let errors = session.close_import_discovery(ledger).unwrap_err();
        assert!(matches!(
            &errors.first().unwrap().kind,
            ErrorKind::AmbiguousModule(_)
        ));
        let attempted = session.discovery_attempt().unwrap();
        assert_eq!(
            attempted.status(),
            crate::ImportDiscoveryRevisionStatus::ClosedAttempted
        );
        assert!(attempted.graph().is_some());

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
