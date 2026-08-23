use std::env;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ahash::{AHashMap, AHashSet};
use rue_compiler::unstable::{
    AcceptedImportSource, DiscoverySourceAssembler, ImportDemandFrontier, ImportDemandMode,
    ImportDiscoveryPlan, ImportDiscoveryRequest, ImportDiscoveryWave, ImportInputRevision,
    ImportObservation, ImportObservationStatus, RootedParkOutcome, TrustedSuccessorDelta,
    begin_import_input_request, begin_import_wave, close_import_discovery_successor,
    close_import_input_request, closed_discovery_continuation, discovery_attempt,
    extend_import_wave, import_demand_frontier_for_roots, import_observation_ledger,
    plan_delta_roots, plan_round_roots, publish_import_observation_batch, publish_import_wave,
    publish_trusted_toolchain_successor, rooted_or_toolchain_park,
    stage_import_discovery_successor, stage_import_input_request,
};
#[cfg(test)]
use rue_compiler::unstable::{
    accepted_read_identity_lookups, accepted_read_identity_visits, committed_successor_sharing,
    exact_import_groups_dispatched, handoff_observation_visits, handoff_observations,
    import_close_records_reduced, import_frontier_roots_requested, import_plan_groups_constructed,
    import_view_full_leaves_published, import_view_ledger_entries_cloned,
    import_view_overlay_leaves_published, import_view_read_entries_compared,
    import_view_source_entries_compared, parse_invalidation_entries_compared,
    parse_key_entries_compared, parse_modules_dispatched, parse_sources_materialized,
    snapshot_module_resolution_visits, snapshot_module_resolutions,
};
#[cfg(test)]
use rue_compiler::unstable::{frontend_query_invalidations, rooted_cfg};
use rue_compiler::{
    AcceptedReadManifest, CompileErrors, CompileOptions, CompilerSession, DependencyEnvelope,
    FileId, FileMetadataFingerprint, ImportDiscoveryContext, ImportDiscoveryStatus,
    ImportDiscoveryView, PhysicalFileIdentity, SourceMetadata, SourceSnapshot,
    TrustedToolchainModuleDemand,
};

/// The content fingerprint used by the long-lived filesystem observer.
///
/// Keeping the hash policy with the filesystem host means the producer's
/// accepted-read snapshot and the CLI watcher cannot silently diverge.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WatchFingerprint(u64);

impl WatchFingerprint {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hash = 0xcbf29ce484222325_u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self(hash)
    }

    pub fn read(path: &Path) -> Option<Self> {
        fs::read(path).ok().map(|bytes| Self::from_bytes(&bytes))
    }
}

/// One accepted filesystem path and its content fingerprint.
///
/// `requested_path` is retained separately from `canonical_path`: the watcher
/// must notice an import alias being deleted or retargeted even though the
/// canonical file may still exist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchInput {
    requested_path: PathBuf,
    canonical_path: PathBuf,
    fingerprint: WatchFingerprint,
}

impl WatchInput {
    pub fn new(
        requested_path: PathBuf,
        canonical_path: PathBuf,
        fingerprint: WatchFingerprint,
    ) -> Self {
        Self {
            requested_path,
            canonical_path,
            fingerprint,
        }
    }

    pub fn requested_path(&self) -> &Path {
        &self.requested_path
    }

    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn fingerprint(&self) -> WatchFingerprint {
        self.fingerprint
    }
}

#[derive(Debug)]
pub(crate) struct SourceManifest {
    path: PathBuf,
    content_hash: WatchFingerprint,
    allowed: AHashSet<PathBuf>,
    declared_paths: AHashSet<PathBuf>,
}

impl SourceManifest {
    pub(crate) fn load(path: &str) -> Result<Self, String> {
        let manifest_path = Path::new(path);
        let content = fs::read_to_string(manifest_path)
            .map_err(|e| format!("Error reading source manifest '{}': {}", path, e))?;
        let base_dir = manifest_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let base_dir = if base_dir.is_absolute() {
            normalize_lexical_path(base_dir)
        } else {
            normalize_lexical_path(
                &env::current_dir().map_err(|error| {
                    format!(
                        "Error reading source manifest '{}': cannot resolve current directory: {}",
                        path, error
                    )
                })?
                .join(base_dir),
            )
        };

        let mut allowed = AHashSet::new();
        let mut declared_paths = AHashSet::new();
        for (line_index, raw_line) in content.lines().enumerate() {
            let line_number = line_index + 1;
            let entry = parse_source_manifest_entry(raw_line);
            if entry.is_empty() {
                continue;
            }

            let entry_path = Path::new(&entry);
            let resolved = if entry_path.is_absolute() {
                entry_path.to_path_buf()
            } else {
                base_dir.join(entry_path)
            };
            declared_paths.insert(normalize_lexical_path(&resolved));
            match fs::canonicalize(&resolved) {
                Ok(canonical) if canonical.is_file() => {
                    allowed.insert(canonical);
                }
                Ok(_) => {
                    return Err(format!(
                        "Error reading source manifest '{}': line {} entry '{}' is not a file",
                        path, line_number, entry
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // A manifest grants an operation, not a claim that the
                    // candidate exists. Declared absent ambiguity arms must be
                    // observable without probing paths the policy did not name.
                }
                Err(error) => {
                    return Err(format!(
                        "Error reading source manifest '{}': line {} entry '{}' cannot be resolved: {}",
                        path, line_number, entry, error
                    ));
                }
            }
        }

        Ok(Self {
            path: manifest_path.to_path_buf(),
            content_hash: WatchFingerprint::from_bytes(content.as_bytes()),
            allowed,
            declared_paths,
        })
    }

    pub(crate) fn allows_canonical(&self, canonical: &Path) -> bool {
        self.allowed.contains(canonical)
    }

    fn declares_path_without_probe(&self, path: &Path) -> bool {
        self.declared_paths.contains(&normalize_lexical_path(path))
    }

    fn display_path(&self) -> String {
        self.path.display().to_string()
    }

    fn policy_revision(&self) -> String {
        let mut declared = self
            .declared_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        declared.sort();
        let mut allowed = self
            .allowed
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        allowed.sort();
        format!(
            "manifest-v1\ndeclared={}\nallowed={}",
            declared.join("\0"),
            allowed.join("\0")
        )
    }
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Capture the standard-library root in the same physical spelling every
/// candidate read canonicalizes to.
///
/// Discovery classifies a candidate as standard-library by comparing its
/// *canonical* path against this captured root, so the two must agree on how
/// the filesystem spells the location. A configured root frequently reaches the
/// compiler through a symlinked prefix: on macOS a `mktemp -d` root is spelled
/// `/var/folders/...` while canonicalization resolves it to
/// `/private/var/folders/...`. Normalizing that root only lexically rejected
/// every module in the bundle as escaping its own root (RUE-991).
///
/// A root that cannot be canonicalized keeps its lexical spelling. That case is
/// a missing or unreadable toolchain, which the trusted-acquisition
/// diagnostics report against the path the user configured.
fn capture_std_root(std_root: &Path) -> PathBuf {
    fs::canonicalize(std_root).unwrap_or_else(|_| normalize_lexical_path(std_root))
}

#[derive(Debug)]
enum StableReadError {
    Io(std::io::Error),
    Changed,
}

struct StableRead {
    source: String,
    identity: PhysicalFileIdentity,
    fingerprint: FileMetadataFingerprint,
}

fn stable_read_to_string(path: &Path) -> Result<StableRead, StableReadError> {
    let before = fs::metadata(path).map_err(StableReadError::Io)?;
    let mut file = fs::File::open(path).map_err(StableReadError::Io)?;
    let opened = file.metadata().map_err(StableReadError::Io)?;
    if !same_file_observation(&before, &opened) {
        return Err(StableReadError::Changed);
    }
    let mut source = String::new();
    file.read_to_string(&mut source)
        .map_err(StableReadError::Io)?;
    let after = file.metadata().map_err(StableReadError::Io)?;
    if !same_file_observation(&opened, &after) {
        return Err(StableReadError::Changed);
    }
    Ok(StableRead {
        source,
        identity: physical_file_identity(&opened),
        fingerprint: file_metadata_fingerprint(&opened),
    })
}

#[cfg(unix)]
fn physical_file_identity(metadata: &fs::Metadata) -> PhysicalFileIdentity {
    use std::os::unix::fs::MetadataExt;
    PhysicalFileIdentity::new(metadata.dev(), metadata.ino())
}

#[cfg(unix)]
fn file_metadata_fingerprint(metadata: &fs::Metadata) -> FileMetadataFingerprint {
    use std::os::unix::fs::MetadataExt;
    let modified = (metadata.mtime() as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(metadata.mtime_nsec() as u64);
    let changed = (metadata.ctime() as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(metadata.ctime_nsec() as u64);
    FileMetadataFingerprint::new(metadata.len(), modified, changed)
}

#[cfg(not(unix))]
fn physical_file_identity(_metadata: &fs::Metadata) -> PhysicalFileIdentity {
    PhysicalFileIdentity::new(0, 0)
}

#[cfg(not(unix))]
fn file_metadata_fingerprint(metadata: &fs::Metadata) -> FileMetadataFingerprint {
    use std::time::UNIX_EPOCH;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    FileMetadataFingerprint::new(metadata.len(), modified, 0)
}

#[cfg(unix)]
fn same_file_observation(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file_observation(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && left.is_file() == right.is_file()
}

// Cover the coarsest timestamp granularity still common on supported hosts
// (notably FAT's two-second modification times). Hashing is conservative; a
// shorter window can make two same-instant rewrites indistinguishable.
const FILESYSTEM_TIMESTAMP_WINDOW: Duration = Duration::from_secs(2);

fn metadata_requires_content_hash(
    previous_identity: PhysicalFileIdentity,
    previous_fingerprint: FileMetadataFingerprint,
    current: &fs::Metadata,
    now: SystemTime,
) -> bool {
    if physical_file_identity(current) != previous_identity
        || file_metadata_fingerprint(current) != previous_fingerprint
    {
        return true;
    }

    let Some(safe_before) = now.checked_sub(FILESYSTEM_TIMESTAMP_WINDOW) else {
        return true;
    };
    match current.modified() {
        Ok(modified) => modified >= safe_before,
        Err(_) => true,
    }
}

fn cached_source_for_module(
    snapshot: &SourceSnapshot,
    module: &rue_compiler::ModuleId,
) -> Option<Arc<String>> {
    snapshot.files().find_map(|source| {
        (snapshot.module_id(source.file_id) == Some(module))
            .then(|| snapshot.shared_source_text(source.file_id))
            .flatten()
    })
}

fn accepted_source_from_read(
    entry: &rue_compiler::AcceptedReadManifestEntry,
    read: StableRead,
    cached_source: Arc<String>,
) -> Result<AcceptedImportSource, SourceLoadError> {
    let observed = AcceptedImportSource::new(
        Arc::from(entry.requested_path()),
        Arc::from(entry.canonical_path()),
        read.identity,
        read.fingerprint,
        Arc::new(read.source),
    )
    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    let source = if observed.content_fingerprint() == entry.content_fingerprint() {
        cached_source
    } else {
        observed.source().clone()
    };
    AcceptedImportSource::new(
        Arc::from(entry.requested_path()),
        Arc::from(entry.canonical_path()),
        read.identity,
        read.fingerprint,
        source,
    )
    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))
}

fn reobserve_accepted_reads(
    snapshot: &SourceSnapshot,
    manifest: &AcceptedReadManifest,
    source_manifest: Option<&SourceManifest>,
) -> Result<AHashMap<String, AcceptedImportSource>, SourceLoadError> {
    let now = SystemTime::now();
    let mut observed = AHashMap::with_capacity(manifest.len());
    for entry in manifest.iter() {
        let cached_source =
            cached_source_for_module(snapshot, entry.module()).ok_or_else(|| {
                SourceLoadError::Message(format!(
                    "Error: accepted read for {} has no cached source",
                    entry.module()
                ))
            })?;
        let path = Path::new(entry.canonical_path());
        if source_manifest.is_some_and(|policy| {
            !policy.declares_path_without_probe(Path::new(entry.requested_path()))
                || !policy.allows_canonical(path)
        }) {
            continue;
        }
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let accepted = if metadata_requires_content_hash(
            entry.metadata_identity(),
            entry.metadata_fingerprint(),
            &metadata,
            now,
        ) {
            let read = match stable_read_to_string(path) {
                Ok(read) => read,
                Err(_) => continue,
            };
            accepted_source_from_read(entry, read, cached_source)?
        } else {
            AcceptedImportSource::new(
                Arc::from(entry.requested_path()),
                Arc::from(entry.canonical_path()),
                entry.metadata_identity(),
                entry.metadata_fingerprint(),
                cached_source,
            )
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?
        };
        observed.insert(entry.requested_path().to_owned(), accepted);
    }
    Ok(observed)
}

fn execute_import_request(
    request: ImportDiscoveryRequest,
    source_manifest: Option<&SourceManifest>,
    reobserved_reads: Option<&AHashMap<String, AcceptedImportSource>>,
) -> ImportObservation {
    if let Some(source) = reobserved_reads
        .and_then(|reads| reads.get(request.requested_path()))
        .cloned()
    {
        return ImportObservation::accepted(request, source)
            .expect("re-observed source matches its compiler request");
    }
    let candidate = Path::new(request.requested_path());
    if source_manifest.is_some_and(|manifest| !manifest.declares_path_without_probe(candidate)) {
        return ImportObservation::failure(request, ImportObservationStatus::DeniedLexical)
            .expect("lexical denial is a valid terminal observation");
    }

    match fs::canonicalize(candidate) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ImportObservation::absent(request)
        }
        Err(error) => ImportObservation::failure(
            request,
            ImportObservationStatus::PresentUnreadable(Arc::from(error.to_string())),
        )
        .expect("unreadable candidates are valid terminal observations"),
        Ok(canonical)
            if source_manifest.is_some_and(|manifest| !manifest.allows_canonical(&canonical)) =>
        {
            ImportObservation::failure(
                request,
                ImportObservationStatus::DeniedCanonical {
                    canonical_path: Arc::from(canonical.to_string_lossy().into_owned()),
                },
            )
            .expect("canonical denial is a valid terminal observation")
        }
        Ok(canonical) if !canonical.is_file() => ImportObservation::failure(
            request,
            ImportObservationStatus::InvalidPhysicalType {
                canonical_path: Arc::from(canonical.to_string_lossy().into_owned()),
            },
        )
        .expect("non-file candidates are valid terminal observations"),
        Ok(canonical) => match stable_read_to_string(&canonical) {
            Ok(read) => {
                let accepted = AcceptedImportSource::new(
                    Arc::from(request.requested_path()),
                    Arc::from(canonical.to_string_lossy().into_owned()),
                    read.identity,
                    read.fingerprint,
                    Arc::new(read.source),
                )
                .expect("stable file reads satisfy accepted import invariants");
                ImportObservation::accepted(request, accepted)
                    .expect("accepted source matches its compiler request")
            }
            Err(StableReadError::Io(error)) => ImportObservation::failure(
                request,
                ImportObservationStatus::PresentUnreadable(Arc::from(error.to_string())),
            )
            .expect("read failures are valid terminal observations"),
            Err(StableReadError::Changed) => ImportObservation::failure(
                request,
                ImportObservationStatus::UnstableRead(Arc::from(
                    "candidate metadata changed during read",
                )),
            )
            .expect("unstable reads are valid terminal observations"),
        },
    }
}

/// Compute `target` relative to `base` without consulting the filesystem.
///
/// Both inputs are expected to be absolute and lexically normalized. Keeping
/// this lexical is important: following symlinks would turn source identity
/// back into a property of the machine's physical directory layout.
#[cfg(test)]
fn lexical_relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    let base_anchor: Vec<_> = base_components
        .iter()
        .take_while(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
        .collect();
    let target_anchor: Vec<_> = target_components
        .iter()
        .take_while(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
        .collect();
    if base_anchor != target_anchor {
        return None;
    }
    let common = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();

    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        match component {
            Component::Normal(_) => relative.push(".."),
            Component::CurDir | Component::ParentDir => unreachable!("paths are normalized"),
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    for component in &target_components[common..] {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir | Component::ParentDir => unreachable!("paths are normalized"),
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some(relative)
}

#[cfg(test)]
const STD_SYMBOL_NAMESPACE: &str = "\0rue-std";

/// Derive relocation-stable source identities for generated symbol names.
///
/// Project files are named relative to the semantic root module's directory,
/// so relative and absolute command-line spellings agree and `../` imports
/// remain stable when the whole source layout moves. The standard library has
/// its own namespace because `$RUE_STD_PATH` may live outside (and at a
/// different depth from) the relocated project root.
#[cfg(test)]
pub(crate) fn derive_symbol_paths_with_std_root(
    sources: &[(String, String)],
    std_root: Option<&Path>,
) -> Result<Vec<String>, String> {
    let absolute_paths: Vec<_> = sources
        .iter()
        .map(|(path, _)| normalize_lexical_path(Path::new(path)))
        .collect();
    let root_dir = absolute_paths
        .first()
        .and_then(|path| path.parent())
        .unwrap_or_else(|| Path::new("/"));
    let std_root = std_root.map(normalize_lexical_path);

    let symbol_paths: Vec<String> = absolute_paths
        .iter()
        .map(|path| {
            let logical = std_root
                .as_deref()
                .and_then(|std_root| path.strip_prefix(std_root).ok())
                // A NUL cannot occur in a filesystem path, so this namespace
                // is provably disjoint from every project-relative identity.
                .map(|relative| Path::new(STD_SYMBOL_NAMESPACE).join(relative))
                .or_else(|| lexical_relative_path(root_dir, path))
                .ok_or_else(|| {
                    format!(
                        "source '{}' cannot be assigned a stable identity relative to root '{}'; sources on another filesystem volume require a named dependency root",
                        path.display(),
                        root_dir.display()
                    )
                })?;
            Ok(logical.to_string_lossy().into_owned())
        })
        .collect::<Result<_, String>>()?;

    Ok(symbol_paths)
}

pub(crate) fn parse_source_manifest_entry(raw_line: &str) -> String {
    let mut entry = String::new();
    let mut escaped = false;

    for ch in raw_line.chars() {
        if escaped {
            if ch == '#' {
                entry.push('#');
            } else {
                entry.push('\\');
                entry.push(ch);
            }
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '#' => break,
            _ => entry.push(ch),
        }
    }

    if escaped {
        entry.push('\\');
    }

    entry.trim().to_string()
}

pub(crate) fn validate_manifest_allows_source(
    manifest: Option<&SourceManifest>,
    source_path: &str,
    role: &str,
) -> Result<(), String> {
    let Some(manifest) = manifest else {
        return Ok(());
    };

    let Ok(canonical) = fs::canonicalize(source_path) else {
        // The normal source read path will produce the precise filesystem error.
        return Ok(());
    };

    if manifest.allows_canonical(&canonical) {
        return Ok(());
    }

    Err(format!(
        "Error: {role} source '{}' is not listed in source manifest '{}'\nManifest entries are allowed source reads, not extra semantic roots.",
        source_path,
        manifest.display_path()
    ))
}

pub(crate) struct SourceLoadRequest<'a> {
    pub(crate) root_source: &'a str,
    pub(crate) source_manifest_path: Option<&'a str>,
    pub(crate) std_root: Option<&'a Path>,
}

#[derive(Debug)]
pub enum SourceLoadError {
    Message(String),
    Compiler {
        snapshot: Option<SourceSnapshot>,
        errors: CompileErrors,
    },
    /// A trusted toolchain module a reached fallible intrinsic requires could
    /// not be acquired because the toolchain itself is broken (missing,
    /// unreadable, or malformed std). This is a broken-installation /
    /// environmental error, not a program error — surfaced loudly with its own
    /// message.
    Toolchain(ToolchainIntegrityError),
    /// A trusted toolchain module resolves to a path the hermetic build
    /// configuration forbids (RUE-1112). This is deterministically DISTINCT
    /// from a broken toolchain: the installation may be intact, but the sandbox
    /// policy denies the read, so the remedy is the source manifest, not the
    /// toolchain. It carries its own outer classification and presentation, never
    /// the "toolchain integrity" / broken-installation framing.
    HermeticDenial(HermeticDenialError),
}

pub(crate) fn load(
    request: SourceLoadRequest<'_>,
) -> Result<ImportDiscoveryResult, SourceLoadError> {
    // Source loading is the first half of the `compile` root: manifest policy,
    // then the demand-driven import-discovery frontier. It was previously
    // timed only through the `parse_file` spans it happens to contain, which
    // left the rest of discovery in the root's unattributed residual (RUE-786).
    let _span =
        tracing::info_span!("source_loading", phase = "source_discovery_and_parsing").entered();
    let manifest = {
        let _span = tracing::info_span!("source_manifest").entered();
        let manifest = request
            .source_manifest_path
            .map(SourceManifest::load)
            .transpose()
            .map_err(SourceLoadError::Message)?;
        validate_manifest_allows_source(manifest.as_ref(), request.root_source, "root")
            .map_err(SourceLoadError::Message)?;
        manifest
    };
    discover_and_load_imports(request.root_source, manifest, request.std_root)
}

#[derive(Debug)]
pub(crate) struct ImportDiscoveryResult {
    pub(crate) source_snapshot: SourceSnapshot,
    /// Immutable, normalized inputs used by every compiler discovery plan.
    pub(crate) resolution: SourceResolutionInputs,
    /// Canonical physical reads accepted while assembling `source_snapshot`.
    pub(crate) read_manifest: AcceptedReadManifest,
    /// Canonical import topology and diagnostics published by the compiler.
    pub(crate) revision: Arc<ImportDiscoveryView>,
    #[cfg(test)]
    pub(crate) input_revision: ImportInputRevision,
    /// How this discovery discharged its ADR-0075 closing witness. Read by the
    /// falsifier tests that pin both the cumulative proof and its fallback.
    #[cfg(test)]
    pub(crate) witness_discharge: WitnessDischarge,
    pub(crate) session: CompilerSession,
    /// Acquisition context threaded out for the host park/retry loop
    /// (`acquire_reached_toolchain_modules`). Host filesystem access lives
    /// outside snapshot/query evaluation, so the loop owns the assembler and read
    /// policy directly; the compiler only issues typed demands and verifies
    /// strictly-additive successors from records.
    assembler: DiscoverySourceAssembler,
    /// The standard-library root a demanded trusted module resolves against, or
    /// `None` when no toolchain std is configured.
    std_root: Option<PathBuf>,
    /// The read policy trusted-module acquisition obeys (same authority as an
    /// ordinary import read), or `None` when unrestricted.
    source_manifest: Option<SourceManifest>,
    /// The empty rooted closure witness of the current committed close — the
    /// frontier a same-generation trusted-toolchain successor continues from.
    witness: ImportDemandFrontier,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceResolutionInputs {
    pub(crate) root_path: PathBuf,
    pub(crate) context: ImportDiscoveryContext,
}

impl ImportDiscoveryResult {
    pub(crate) fn watch_inputs(&self) -> Vec<WatchInput> {
        let mut paths = Vec::with_capacity(self.read_manifest.len() + 1);
        for entry in self.read_manifest.iter() {
            let source = self
                .source_snapshot
                .files()
                .find(|source| {
                    self.source_snapshot.module_id(source.file_id) == Some(entry.module())
                })
                .expect("an accepted read has source bytes in the committed snapshot");
            let fingerprint = WatchFingerprint::from_bytes(source.source.as_bytes());
            paths.push(WatchInput::new(
                PathBuf::from(entry.requested_path()),
                PathBuf::from(entry.canonical_path()),
                fingerprint,
            ));
        }
        if let Some(manifest) = &self.source_manifest {
            paths.push(WatchInput::new(
                manifest.path.clone(),
                manifest.path.clone(),
                manifest.content_hash,
            ));
        }
        paths.sort_by(|left, right| {
            left.requested_path()
                .cmp(right.requested_path())
                .then_with(|| left.canonical_path().cmp(right.canonical_path()))
        });
        paths.dedup();
        paths
    }
}

/// The closed import-discovery revision produced by
/// [`drive_import_discovery_to_close`].
struct ClosedDiscovery {
    /// The closed source snapshot.
    snapshot: SourceSnapshot,
    /// The committed closed discovery view.
    closed: Arc<ImportDiscoveryView>,
    /// The final import input revision of this close. Read only by the test-only
    /// `ImportDiscoveryResult::input_revision` frontier-round assertions.
    #[cfg_attr(not(test), allow(dead_code))]
    input_revision: ImportInputRevision,
    /// The final empty rooted frontier that witnessed closure — the closure
    /// witness a same-generation trusted-toolchain successor continues from.
    witness: ImportDemandFrontier,
    /// How the ADR-0075 closing witness was discharged: proven from the
    /// cumulative coverage record, or fallen back to the whole-plan rooting.
    /// Read only by the falsifier tests that pin both paths as reachable.
    #[cfg_attr(not(test), allow(dead_code))]
    witness_discharge: WitnessDischarge,
}

// Test-only falsifier for the ADR-0075 cumulative witness: when armed, drop
// one occurrence from the coverage record just before the closing check, so
// the record can no longer prove it covers the plan.
//
// The witness is sound because the union of every round's roots is the whole
// final plan by construction, which means the fallback is unreachable in
// ordinary discovery — and an unreachable fallback is one nobody notices has
// rotted. This forges the one condition that reaches it.
//
// Deliberately not a `mod`: `cli_executes_only_revision_pinned_compiler_frontiers`
// reads this file's production half as everything before the first
// `#[cfg(test)] mod`, and a test module here would hide the host boundaries
// below it from that gate.
#[cfg(test)]
thread_local! {
    static FORGE_UNCOVERED_PLAN_SEGMENT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Arms the forge for the current thread; disarmed when dropped, so one test
/// cannot leak the condition into another.
#[cfg(test)]
struct ForgedUncoveredSegment;

#[cfg(test)]
impl ForgedUncoveredSegment {
    fn arm() -> Self {
        FORGE_UNCOVERED_PLAN_SEGMENT.with(|armed| armed.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for ForgedUncoveredSegment {
    fn drop(&mut self) {
        FORGE_UNCOVERED_PLAN_SEGMENT.with(|armed| armed.set(false));
    }
}

#[cfg(test)]
fn forge_uncovered_plan_segment(
    rooted: &mut std::collections::BTreeSet<rue_compiler::ImportOccurrenceKey>,
) {
    if FORGE_UNCOVERED_PLAN_SEGMENT.with(|armed| armed.replace(false))
        && let Some(first) = rooted.iter().next().cloned()
    {
        rooted.remove(&first);
    }
}

/// How one discovery discharged its closing witness (ADR-0075).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WitnessDischarge {
    /// Closures whose cumulative record already covered the plan, so the
    /// whole-plan rooting was not dispatched.
    pub(crate) cumulative: u32,
    /// Closures that fell back to the whole-plan rooting because the record
    /// could not prove coverage.
    pub(crate) reroot: u32,
}

/// Reach the parser-owned import fixed point by executing compiler-generated
/// filesystem requests. This driver owns only the read-policy checks and
/// physical observation transactions; the compiler owns recognition,
/// candidate order, identity assembly, closure, and canonical outcomes.
///
/// The CLI accepts exactly one root source (ADR-0046 / RUE-767); every other
/// file enters the compilation only through the root's `@import` graph, which
/// discovery loads below. There is therefore no set of "extra positional
/// sources" to reconcile against the import graph — the flat-mode surface that
/// once needed that reconciliation (RUE-434) was removed.
///
/// Drive parser-owned import discovery to its fixed point and close it against
/// the current `assembler` source set, committing a closed import-discovery
/// revision in `staging`. Returns the closed source snapshot, its committed
/// discovery view, the final import input revision, and the empty rooted closure
/// witness.
///
/// `continuation` selects the request generation. `None` begins a fresh
/// external-input observation generation (the initial close) and roots its FIRST
/// round in the whole plan. `Some(revision)` is a SAME-GENERATION re-close on a
/// strictly-additive successor already published into `revision`: it never begins
/// a new generation — doing so would defeat the same-generation guarantee the
/// trusted-toolchain continuation relies on.
///
/// `reclose` is `Some` (alongside a `continuation`) for a trusted-toolchain
/// re-close. It carries the opaque, compiler-derived successor-delta capability
/// (the host cannot choose or edit its module set) and the predecessor module
/// set. A trusted std leaf such as `strbuf.rue` DOES carry import edges (it
/// imports `option.rue`, `arraybuf.rue`, `rawbuf.rue`), so the re-close must
/// discover them; but the predecessor graph already closed valid and its ledger
/// is carried unchanged. The re-close therefore roots its discovery frontier ONLY
/// in import occurrences owned by modules added since the predecessor close
/// (`strbuf` and, transitively, `arraybuf`/`rawbuf`). A predecessor module's
/// import occurrences are never re-rooted or re-resolved: the re-close continues
/// from the verified closed predecessor graph rather than rebuilding it, so
/// acquisition cost is O(new leaves), not O(existing import topology).
///
/// This loop is the owner of "which occurrences are still open", because it is
/// the only place that knows the ROUND structure: it holds the plan sequence,
/// the previous round's frontier, and the decision to stop. The compiler owns
/// both halves of the answer as immutable derived values — the plan's own delta
/// segment and the frontier's own fanout (`plan_round_roots`) — so the host
/// contributes no membership claim of its own and the frontier's fail-closed
/// plan-membership guard still checks every root it is handed.
struct ReClose<'a> {
    delta: &'a TrustedSuccessorDelta,
}

/// How many times one ordinary discovery round may re-run its wave because a
/// source it read changed before the wave published.
///
/// A wave re-run is bounded rather than unlimited: a source being rewritten in a
/// loop must surface as a fail-closed error instead of spinning discovery
/// forever. The bound is generous — a real editor save races one wave at most.
const WAVE_STAMP_RETRIES: u32 = 4;

#[cfg(test)]
thread_local! {
    /// Test hook fired between a wave's last read and its stamp verification, so
    /// a test can rewrite a source inside the exact window the atomicity
    /// contract covers.
    static WAVE_PUBLISH_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
    /// Waves discarded by that verification and re-run.
    static WAVE_STAMP_RERUNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn fire_wave_publish_hook() {
    WAVE_PUBLISH_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

/// Whether every source the wave read still has the exact physical identity and
/// metadata stamp it was read with.
///
/// A wave publishes one revision covering reads taken over its whole closure, so
/// the window between its first read and its publication is wider than a single
/// hop's. Verifying the whole batch here, fail-closed, is what makes that window
/// safe: a revision can never mix a stale read with a fresh one, because a single
/// disagreement discards the wave and re-runs it.
fn wave_reads_are_stable(wave: &ImportDiscoveryWave) -> bool {
    wave.accepted_reads().iter().all(|source| {
        fs::metadata(Path::new(source.canonical_path())).is_ok_and(|metadata| {
            physical_file_identity(&metadata) == source.metadata_identity()
                && file_metadata_fingerprint(&metadata) == source.metadata_fingerprint()
        })
    })
}

/// Resolve one discovery wave to its fixed point and publish it as one revision.
///
/// Returns the successor revision and the batch frontier the next round
/// continues from — the wave's whole fanout, so the next round re-roots exactly
/// the occurrences the wave demanded answers for.
#[allow(clippy::too_many_arguments)]
fn run_import_wave(
    assembler: &mut DiscoverySourceAssembler,
    staging: &mut CompilerSession,
    input_revision: ImportInputRevision,
    plan: &ImportDiscoveryPlan,
    frontier: &ImportDemandFrontier,
    source_manifest: Option<&SourceManifest>,
    reobserved_reads: Option<&AHashMap<String, AcceptedImportSource>>,
) -> Result<(ImportInputRevision, ImportDemandFrontier), SourceLoadError> {
    for attempt in 0..=WAVE_STAMP_RETRIES {
        let mut wave = begin_import_wave(staging, input_revision, plan, frontier)
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        while !wave.is_complete() {
            let observations = wave
                .requests()
                .iter()
                .cloned()
                .map(|request| execute_import_request(request, source_manifest, reobserved_reads))
                .collect::<Vec<_>>();
            extend_import_wave(staging, &mut wave, observations)
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        }
        #[cfg(test)]
        fire_wave_publish_hook();
        if !wave_reads_are_stable(&wave) {
            #[cfg(test)]
            WAVE_STAMP_RERUNS.with(|count| count.set(count.get() + 1));
            if attempt == WAVE_STAMP_RETRIES {
                return Err(SourceLoadError::Message(format!(
                    "Error: a source read during import discovery kept changing across {} wave attempts",
                    WAVE_STAMP_RETRIES + 1
                )));
            }
            // Nothing was assembled or published, so the retry re-reads the same
            // compiler-produced operations against the settled filesystem.
            continue;
        }
        assembler
            .add_wave_reads(&wave)
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        let successor_snapshot = assembler
            .snapshot()
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        return publish_import_wave(
            staging,
            wave,
            &successor_snapshot,
            assembler.accepted_read_manifest(),
        )
        .map_err(|error| SourceLoadError::Message(format!("Error: {error}")));
    }
    unreachable!("the wave retry loop returns or fails on its last attempt")
}

fn drive_import_discovery_to_close(
    assembler: &mut DiscoverySourceAssembler,
    staging: &mut CompilerSession,
    context: &ImportDiscoveryContext,
    source_manifest: Option<&SourceManifest>,
    reobserved_reads: Option<&AHashMap<String, AcceptedImportSource>>,
    continuation: Option<ImportInputRevision>,
    reclose: Option<ReClose<'_>>,
) -> Result<ClosedDiscovery, SourceLoadError> {
    // Exactly one import-input request is opened per external request, here,
    // before the frontier loop below. Rounds inside that loop publish overlay
    // successors which inherit this request's compatibility token, so the whole
    // discovery of one program observes one filesystem regime. A long-lived
    // filesystem host must populate `reobserved_reads` from the previous rooted
    // closure before entering this function.
    let mut input_revision = match continuation {
        Some(revision) => revision,
        None => {
            let initial_snapshot = assembler
                .snapshot()
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
            begin_import_input_request(
                staging,
                &initial_snapshot,
                context.clone(),
                assembler.accepted_read_manifest(),
            )
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?
        }
    };
    let final_plan;
    let witness;
    // The previous round's frontier, once there is one. It names the exact
    // occurrences whose host answers arrive this round and which may therefore
    // demand another candidate; every other occurrence already in the plan was
    // conclusive when that frontier was built and stays conclusive for the whole
    // request generation.
    let mut previous_frontier: Option<ImportDemandFrontier> = None;
    // Every occurrence this discovery has rooted, across all its rounds. This is
    // the ADR-0075 cumulative coverage record that lets the closing witness be
    // proven rather than re-dispatched; see the closing debt below.
    let mut rooted_occurrences: std::collections::BTreeSet<rue_compiler::ImportOccurrenceKey> =
        std::collections::BTreeSet::new();
    // How the closing witness was discharged, for the falsifier: proven from the
    // record, or fallen back to the whole-plan rooting.
    let mut cumulative_witness_closures = 0_u32;
    let mut reroot_witness_closures = 0_u32;
    loop {
        // One frontier round: plan and frontier construction (which owns the
        // canonical parse of everything read so far), then the host reads that
        // answer the frontier's requests. Both halves are timed so a discovery
        // regression is localized to compiler planning or to filesystem I/O.
        let _round_span = tracing::info_span!("import_discovery_round").entered();
        let plan_span = tracing::info_span!("import_plan").entered();
        let (snapshot, ledger) = {
            let _span = tracing::info_span!("import_revision_inputs").entered();
            let snapshot = assembler
                .snapshot()
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
            let ledger = import_observation_ledger(staging, input_revision)
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
            (snapshot, ledger)
        };
        // A trusted-toolchain successor stages from the compiler-published view
        // itself; the host supplies only the opaque capability. The assembler's
        // snapshot/manifest reach the compiler solely through the verified
        // observation-batch publication.
        let staged = {
            let _span = tracing::info_span!("import_stage").entered();
            match &reclose {
                Some(reclose) => stage_import_discovery_successor(staging, reclose.delta),
                None => stage_import_input_request(staging, input_revision),
            }
        };
        let plan = match staged {
            Ok(plan) => plan,
            Err(_) => {
                let diagnostics = staging
                    .import_diagnostics()
                    .expect("failed staging publishes canonical import diagnostics");
                let errors = CompileErrors::from(diagnostics.errors().to_vec());
                return Err(SourceLoadError::Compiler {
                    snapshot: Some(snapshot),
                    errors,
                });
            }
        };
        // A trusted-toolchain re-close roots its frontier only in the plan's delta
        // occurrences — those owned by modules added since the predecessor close.
        // These come straight from the plan's delta segment, never by filtering the
        // merged predecessor plan.
        //
        // An ordinary round roots the same way once it has a predecessor round to
        // continue: the occurrences this round's stage added to the plan, plus the
        // ones the previous frontier demanded answers for. Rooting the WHOLE plan
        // every round instead re-dispatches a top-level `ResolveImport` request per
        // occurrence per round, which is quadratic in the depth of an import chain
        // while re-proving conclusions the ledger already fixed. The first round has
        // no predecessor, so it roots the whole plan.
        let mut roots = match (&reclose, &previous_frontier) {
            (Some(_), _) => plan_delta_roots(&plan),
            (None, None) => plan.demand_roots(),
            (None, Some(previous)) => plan_round_roots(&plan, previous),
        };
        // A round rooted in the open set proves only that the open set is
        // exhausted. The closure witness must mean more: it is what a
        // trusted-toolchain continuation verifies to accept that the predecessor
        // closed. So an ordinary discovery owes the whole plan exactly ONE more
        // rooting, taken when its open set first comes back empty; that whole-plan
        // frontier is the witness it closes on. A re-close owes nothing: its
        // predecessor graph is already closed and carried, and re-rooting it is
        // exactly the O(existing topology) work RUE-1112 removed.
        let mut closing_reroot_owed = reclose.is_none() && previous_frontier.is_some();
        let frontier = {
            let _span = tracing::info_span!("import_frontier").entered();
            loop {
                rooted_occurrences.extend(roots.occurrences().iter().cloned());
                let frontier = import_demand_frontier_for_roots(
                    staging,
                    input_revision,
                    &plan,
                    ImportDemandMode::Rooted,
                    &roots,
                )
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
                // Requests mean the round is not closing after all, so the debt
                // stands for a later round. Should the whole-plan rooting disagree
                // with the open set and produce requests, they are answered like
                // any other round's rather than dropped.
                if !frontier.requests().is_empty() || !closing_reroot_owed {
                    break frontier;
                }
                closing_reroot_owed = false;
                // ADR-0075: pay the closing debt from the cumulative record when
                // it already covers the plan. The first round roots the whole
                // plan, every later round roots what its stage added plus what
                // the previous frontier demanded, and the plan is append-only —
                // so the union across rounds is the whole final plan, and every
                // occurrence outside a round's roots was already conclusive for
                // the generation. Re-dispatching them proves nothing the record
                // does not already carry, and costs one root per plan occurrence
                // on every discovery.
                //
                // Fail-closed: if the record cannot prove coverage, the
                // whole-plan rooting runs exactly as before.
                let whole_plan = plan.demand_roots();
                #[cfg(test)]
                forge_uncovered_plan_segment(&mut rooted_occurrences);
                if whole_plan
                    .occurrences()
                    .iter()
                    .all(|occurrence| rooted_occurrences.contains(occurrence))
                {
                    cumulative_witness_closures += 1;
                    break frontier;
                }
                reroot_witness_closures += 1;
                roots = whole_plan;
            }
        };
        drop(plan_span);
        if frontier.requests().is_empty() {
            final_plan = plan;
            witness = frontier;
            break;
        }
        let _read_span =
            tracing::info_span!("import_read", phase = "source_discovery_and_parsing").entered();
        match &reclose {
            // A trusted-toolchain re-close keeps the hop-granular contract: its
            // frontier is rooted in the successor delta alone, its module set is
            // fixed by an opaque capability, and its failures are attributed per
            // leaf before the close folds them (RUE-1112).
            Some(_) => {
                let observations = frontier
                    .requests()
                    .iter()
                    .cloned()
                    .map(|request| {
                        execute_import_request(request, source_manifest, reobserved_reads)
                    })
                    .collect::<Vec<_>>();
                // In a trusted-toolchain re-close every frontier request resolves
                // an `@import` edge owned by an appended leaf or a leaf newly
                // discovered from it (e.g. strbuf → arraybuf/rawbuf). These edges
                // are toolchain-internal, so a non-accepted observation there is an
                // environmental failure of THAT transitive leaf, not a program
                // error — attribute it to the exact failing module before the
                // outcome is folded into an opaque close diagnostic.
                if let Some(error) = classify_trusted_transitive_failure(context, &observations) {
                    return Err(error);
                }
                let mut next_ledger = ledger;
                for observation in observations.iter().cloned() {
                    next_ledger
                        .record(observation)
                        .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
                }
                // A successor adds its delta groups' accepted sources, never
                // re-feeding the predecessor plan through the winner map.
                assembler
                    .add_successor_plan_reads(&plan, &next_ledger)
                    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
                let successor_snapshot = assembler
                    .snapshot()
                    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
                input_revision = publish_import_observation_batch(
                    staging,
                    &frontier,
                    &successor_snapshot,
                    assembler.accepted_read_manifest(),
                    observations,
                )
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
                previous_frontier = Some(frontier);
            }
            // An ordinary round resolves its whole wave before publishing
            // (ADR-0075): the sources this frontier discovers are read, their own
            // import occurrences are resolved against the wave's running ledger,
            // and that repeats until the closure raises no further demand. Each hop
            // emits exactly the operations, in exactly the order, the round it
            // replaces would have emitted, so the ledger records the same reads in
            // the same order — only the publication count changes.
            None => {
                let (revision, published) = run_import_wave(
                    assembler,
                    staging,
                    input_revision,
                    &plan,
                    &frontier,
                    source_manifest,
                    reobserved_reads,
                )?;
                input_revision = revision;
                previous_frontier = Some(published);
            }
        }
    }

    let _close_span = tracing::info_span!("import_discovery_close").entered();
    let snapshot = assembler
        .snapshot()
        .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    debug_assert_eq!(final_plan.source_revision(), snapshot.source_revision());
    // A trusted-toolchain successor closes over only the modules its opaque delta
    // capability authorizes, merging their topology into the committed
    // predecessor's closed graph; the initial close reduces the whole plan.
    let close_result = match &reclose {
        Some(reclose) => close_import_discovery_successor(staging, reclose.delta),
        None => close_import_input_request(staging, input_revision),
    };
    let closed = match close_result {
        Ok(closed) => closed,
        Err(errors) => {
            let attempted = discovery_attempt(staging)
                .expect("failed closure publishes an attempted import revision");
            if DependencyEnvelope::from_closed_revision(&attempted).is_some() {
                // Missing and ambiguous resolution are structurally closed and
                // therefore have canonical topology for `--emit deps`. The
                // caller owns their one diagnostic rendering so the envelope
                // can be written first. All other failures have no topology.
                attempted
            } else {
                return Err(SourceLoadError::Compiler {
                    snapshot: Some(snapshot),
                    errors,
                });
            }
        }
    };
    Ok(ClosedDiscovery {
        snapshot,
        closed,
        input_revision,
        witness,
        witness_discharge: WitnessDischarge {
            cumulative: cumulative_witness_closures,
            reroot: reroot_witness_closures,
        },
    })
}

pub(crate) fn discover_and_load_imports(
    root_source: &str,
    source_manifest: Option<SourceManifest>,
    std_root: Option<&Path>,
) -> Result<ImportDiscoveryResult, SourceLoadError> {
    // Validate the root source's span representability before discovery aliases
    // physical identities. With a single positional source there are no
    // duplicate CLI inputs left to detect here; discovery still recognizes
    // `main.rue` and `./main.rue` as the same physical source when one is
    // reached through the other's import graph.
    let physical_paths: AHashMap<_, _> =
        AHashMap::from([(FileId::new(1), root_source.to_string())]);
    let logical_paths: AHashMap<_, _> = AHashMap::from([(FileId::new(1), "root".to_string())]);
    if let Err(error) = SourceMetadata::new(FileId::new(1), physical_paths, logical_paths) {
        return Err(SourceLoadError::Compiler {
            snapshot: None,
            errors: CompileErrors::from(error),
        });
    }

    let root_path = normalize_lexical_path(Path::new(root_source));
    let root_dir = root_path
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .to_path_buf();
    let std_root = std_root.map(capture_std_root);
    let policy_revision = source_manifest
        .as_ref()
        .map(SourceManifest::policy_revision)
        .unwrap_or_else(|| "unrestricted".into());
    let context = ImportDiscoveryContext::new(
        1,
        root_dir.to_string_lossy(),
        std_root
            .as_deref()
            .map(|path| path.to_string_lossy())
            .as_deref(),
        policy_revision,
    )
    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    let root_canonical = fs::canonicalize(&root_path).map_err(|error| {
        SourceLoadError::Message(format!("Error reading {}: {error}", root_path.display()))
    })?;
    if source_manifest
        .as_ref()
        .is_some_and(|manifest| !manifest.allows_canonical(&root_canonical))
    {
        return Err(SourceLoadError::Message(
            "Error: root source escapes the source manifest after canonicalization".into(),
        ));
    }
    let root_read = stable_read_to_string(&root_canonical).map_err(|error| {
        SourceLoadError::Message(match error {
            StableReadError::Io(error) => format!("Error reading {}: {error}", root_path.display()),
            StableReadError::Changed => format!(
                "Error reading {}: source changed during read",
                root_path.display()
            ),
        })
    })?;
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        root_path.to_string_lossy(),
        root_canonical.to_string_lossy(),
        root_read.identity,
        root_read.fingerprint,
        Arc::new(root_read.source),
    )
    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;

    let mut staging = CompilerSession::new();
    // Reach the parser-owned import fixed point and close. Trusted
    // toolchain-module acquisition (the compiler-rooted std `Option`/`StrBuf` a
    // reached fallible intrinsic needs but no `@import` pulls) is NOT done here:
    // it is driven by the host park/retry loop (`acquire_reached_toolchain_modules`)
    // against real reached-body semantic demands, so an unreachable helper never
    // forces a std read. This close threads its assembler, read policy, std root,
    // and closure witness out so that loop can satisfy demands and re-close in the
    // same request generation.
    let close = drive_import_discovery_to_close(
        &mut assembler,
        &mut staging,
        &context,
        source_manifest.as_ref(),
        None,
        None,
        None,
    )?;

    Ok(ImportDiscoveryResult {
        source_snapshot: close.snapshot,
        resolution: SourceResolutionInputs { root_path, context },
        read_manifest: assembler.accepted_read_manifest(),
        revision: close.closed,
        #[cfg(test)]
        input_revision: close.input_revision,
        #[cfg(test)]
        witness_discharge: close.witness_discharge,
        session: staging,
        assembler,
        std_root,
        source_manifest,
        witness: close.witness,
    })
}

// Long-lived hosts begin each successor request by re-observing the exact
// accepted-read closure through this filesystem soundness boundary.
pub(crate) fn reload_from_filesystem(
    result: &mut ImportDiscoveryResult,
) -> Result<(), SourceLoadError> {
    let source_manifest = result
        .source_manifest
        .as_ref()
        .map(|manifest| SourceManifest::load(manifest.path.to_string_lossy().as_ref()))
        .transpose()
        .map_err(SourceLoadError::Message)?;
    validate_manifest_allows_source(
        source_manifest.as_ref(),
        result.resolution.root_path.to_string_lossy().as_ref(),
        "root",
    )
    .map_err(SourceLoadError::Message)?;
    let policy_revision = source_manifest
        .as_ref()
        .map(SourceManifest::policy_revision)
        .unwrap_or_else(|| "unrestricted".into());
    let context = ImportDiscoveryContext::new(
        result.resolution.context.epoch(),
        result.resolution.context.project_root(),
        result.resolution.context.std_root(),
        policy_revision,
    )
    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    let reobserved = reobserve_accepted_reads(
        &result.source_snapshot,
        &result.read_manifest,
        source_manifest.as_ref(),
    )?;
    let root_module = result.source_snapshot.source_revision().root();
    let root_entry = result
        .read_manifest
        .iter()
        .find(|entry| entry.module() == root_module)
        .expect("a closed read manifest contains its root");
    let root = reobserved.get(root_entry.requested_path()).ok_or_else(|| {
        SourceLoadError::Message(format!(
            "Error reading {}: source is no longer readable",
            root_entry.requested_path()
        ))
    })?;
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        root.requested_path(),
        root.canonical_path(),
        root.metadata_identity(),
        root.metadata_fingerprint(),
        root.source().clone(),
    )
    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    // Seed only the root. Reobserved predecessor reads are an observation cache
    // for requests the new rooted frontier actually issues; making every old
    // read explicit would keep modules that are no longer reachable after an
    // import-set edit and grow the retained snapshot monotonically.
    let close = drive_import_discovery_to_close(
        &mut assembler,
        &mut result.session,
        &context,
        source_manifest.as_ref(),
        Some(&reobserved),
        None,
        None,
    )?;
    result.source_snapshot = close.snapshot;
    result.read_manifest = assembler.accepted_read_manifest();
    result.revision = close.closed;
    result.assembler = assembler;
    result.witness = close.witness;
    result.resolution.context = context;
    result.source_manifest = source_manifest;
    #[cfg(test)]
    {
        result.input_revision = close.input_revision;
        #[cfg(test)]
        {
            result.witness_discharge = close.witness_discharge;
        }
    }
    Ok(())
}

/// Bounded rounds for reached-body trusted-toolchain acquisition. Each satisfied
/// round adds at least one demanded module and shrinks the outstanding demand
/// set, so the batched `Option`/`StrBuf` acquisition converges in one round; the
/// bound only guards against a compiler invariant violation that would spin.
const MAX_TOOLCHAIN_ACQUISITION_ROUNDS: usize = 4;

/// Drive reached-body trusted-toolchain acquisition to a fixed point on
/// `result`'s session (RUE-1112). Runs rooted, park-aware semantic analysis on
/// the committed closed revision: a reached body demanding an absent trusted std
/// module parks, the host satisfies exactly the parked demands under the B4
/// policy checks, publishes ONE strictly-additive successor in the same request
/// generation, and re-closes discovery on it before retrying.
///
/// Host filesystem access lives outside snapshot/query evaluation, so this loop
/// owns the physical reads and the assembler while the compiler only issues typed
/// demands and verifies each successor from records. A program with no reached
/// fallible intrinsic never parks and never reads std, so an unrelated program is
/// untouched even when std is malformed on disk. Returns `Ok(())` once semantic
/// analysis reaches a settled outcome — satisfied, or settled program
/// diagnostics that the driver's emit/compile surfaces report behind their own
/// preflight ordering. Only toolchain/hermetic acquisition failures surface
/// through [`SourceLoadError`].
pub(crate) fn acquire_reached_toolchain_modules(
    result: &mut ImportDiscoveryResult,
    options: &CompileOptions,
) -> Result<(), SourceLoadError> {
    // A discovery that did not close valid (missing or ambiguous imports) has no
    // queryable program, so semantic analysis cannot run and there is nothing to
    // acquire. Leave it untouched: the driver surfaces the canonical import
    // diagnostics through its closed-revision check, exactly as before this loop
    // existed. Running semantic here would preempt that with an opaque
    // "no successful parsed program" error.
    if result.revision.status() != ImportDiscoveryStatus::ClosedValid {
        return Ok(());
    }
    for _ in 0..MAX_TOOLCHAIN_ACQUISITION_ROUNDS {
        match rooted_or_toolchain_park(&mut result.session, options) {
            // Analysis satisfied every reached-body demand (or there were none).
            RootedParkOutcome::Ready => return Ok(()),
            // Deterministic program diagnostics — the source itself, not the
            // toolchain, is at fault, and an erroneous body raises no toolchain
            // park, so there is nothing here to acquire. Reporting stays with
            // the driver's emit/compile surfaces, which run their own preflight
            // checks (e.g. the output-alias guard) ahead of semantic
            // diagnostics; surfacing the errors from this loop would invert
            // that ordering. The semantic attempt is memoized, so the later
            // surface re-reads the cached outcome rather than re-analyzing.
            RootedParkOutcome::Errors(_) => return Ok(()),
            // A reached body demands trusted std modules absent from the current
            // revision. Satisfy exactly the parked demands, publish one successor,
            // and re-close so semantic can retry on it.
            RootedParkOutcome::Parked(park) => {
                // Attribute only the host work which satisfies the park. The
                // semantic request above already owns its own phase spans; wrapping
                // the whole fixed-point loop here would misreport that cached
                // semantic work as toolchain acquisition.
                let _span = tracing::info_span!("toolchain_acquisition").entered();
                for demand in park.demands() {
                    satisfy_toolchain_module_demand(
                        &mut result.assembler,
                        result.std_root.as_deref(),
                        result.source_manifest.as_ref(),
                        demand,
                    )
                    .map_err(SourceLoadError::from)?;
                }
                let successor = result
                    .assembler
                    .snapshot()
                    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
                let reads = result.assembler.accepted_read_manifest();
                // The park just attached its exact demanded set to the closed
                // continuation, so an authorizing token must be outstanding.
                let token = closed_discovery_continuation(&result.session).ok_or_else(|| {
                    SourceLoadError::Message(
                        "Error: internal compiler invariant — a reached-body toolchain park issued no authorizing continuation".into(),
                    )
                })?;
                let delta = publish_trusted_toolchain_successor(
                    &mut result.session,
                    token,
                    &result.witness,
                    &successor,
                    reads,
                )
                .map_err(|errors| SourceLoadError::Compiler {
                    snapshot: Some(successor.clone()),
                    errors,
                })?;
                // Same-generation re-close on the successor. Its module delta is
                // derived and verified from the opaque `delta` capability by the
                // compiler — the host cannot choose or omit it. The appended
                // leaves' `@import` edges (e.g. strbuf → option/arraybuf/rawbuf)
                // are discovered and read here; the predecessor graph is carried
                // closed and never re-rooted. A malformed or missing transitive
                // trusted leaf fails during this re-close, and classification
                // (below) attributes it to the actual failing module.
                let reclosed = drive_import_discovery_to_close(
                    &mut result.assembler,
                    &mut result.session,
                    &result.resolution.context,
                    result.source_manifest.as_ref(),
                    None,
                    Some(delta.revision()),
                    Some(ReClose { delta: &delta }),
                )
                .map_err(|error| {
                    reclassify_reclose_failure(error, result.std_root.as_deref(), park.demands())
                })?;
                result.source_snapshot = reclosed.snapshot;
                result.revision = reclosed.closed;
                result.read_manifest = result.assembler.accepted_read_manifest();
                result.witness = reclosed.witness;
                #[cfg(test)]
                {
                    result.input_revision = reclosed.input_revision;
                    result.witness_discharge = reclosed.witness_discharge;
                }
            }
        }
    }
    // Still parking after the bound: acquisition is not converging even though
    // every round published a successor. Surface as a toolchain-integrity error
    // rather than spinning.
    Err(SourceLoadError::Toolchain(
        ToolchainIntegrityError::UnsatisfiedAfterPublish {
            logical_path: "<reached-body trusted-toolchain acquisition did not converge>"
                .to_owned(),
        },
    ))
}

/// Derive the trusted logical path (`\0rue-std/<relative>`) for a std file the
/// re-close read against the configured std root, falling back to the requested
/// filesystem path when it lies outside the root's lexical prefix. Used only to
/// name the exact failing transitive leaf in an environmental error.
fn trusted_logical_path_for_requested(context: &ImportDiscoveryContext, requested: &str) -> String {
    const STD_NAMESPACE: &str = "\0rue-std/";
    match context.std_root() {
        Some(root) => match Path::new(requested).strip_prefix(root) {
            Ok(relative) => format!("{STD_NAMESPACE}{}", relative.to_string_lossy()),
            Err(_) => requested.to_owned(),
        },
        None => requested.to_owned(),
    }
}

/// Attribute a non-accepted observation for a toolchain-internal `@import` edge
/// (a transitive trusted leaf reached during the re-close) to the exact failing
/// module. A policy/containment denial is a hermetic build-configuration failure;
/// an absent, unreadable, or invalid-type module is a broken-installation
/// (toolchain-integrity) failure. An accepted observation returns `None`.
fn classify_trusted_transitive_failure(
    context: &ImportDiscoveryContext,
    observations: &[ImportObservation],
) -> Option<SourceLoadError> {
    for observation in observations {
        let requested = observation.request().requested_path().to_owned();
        let logical = trusted_logical_path_for_requested(context, &requested);
        let path = PathBuf::from(&requested);
        let error = match observation.status() {
            ImportObservationStatus::PresentReadable { .. } => continue,
            ImportObservationStatus::Absent => {
                SourceLoadError::Toolchain(ToolchainIntegrityError::Missing {
                    logical_path: logical,
                    path,
                })
            }
            ImportObservationStatus::InvalidPhysicalType { .. } => {
                SourceLoadError::Toolchain(ToolchainIntegrityError::Missing {
                    logical_path: logical,
                    path,
                })
            }
            ImportObservationStatus::PresentUnreadable(reason) => {
                SourceLoadError::Toolchain(ToolchainIntegrityError::Unreadable {
                    logical_path: logical,
                    path,
                    reason: reason.to_string(),
                })
            }
            ImportObservationStatus::UnstableRead(reason) => {
                SourceLoadError::Toolchain(ToolchainIntegrityError::Unreadable {
                    logical_path: logical,
                    path,
                    reason: reason.to_string(),
                })
            }
            ImportObservationStatus::DeniedLexical => {
                SourceLoadError::HermeticDenial(HermeticDenialError {
                    logical_path: logical,
                    path,
                    reason: "the source manifest does not declare this path".to_owned(),
                })
            }
            ImportObservationStatus::DeniedCanonical { canonical_path } => {
                SourceLoadError::HermeticDenial(HermeticDenialError {
                    logical_path: logical,
                    path: PathBuf::from(canonical_path.as_ref()),
                    reason: "its canonical path is not listed in the source manifest".to_owned(),
                })
            }
            ImportObservationStatus::Cancelled => continue,
        };
        return Some(error);
    }
    None
}

/// Classify a compiler failure from a trusted-toolchain re-close. The re-close's
/// only new modules are trusted std leaves (the appended leaves plus the
/// transitive helpers they import), so a parse/close compiler failure is a
/// malformed toolchain module — a broken installation, not a program error. The
/// failure is attributed to the module the diagnostics point at (via the error
/// span's file), so a malformed transitive leaf such as `arraybuf.rue` is named
/// as itself rather than the root demand. Denial, absence, and unreadable
/// transitive leaves are already classified from their observation earlier in the
/// re-close; only a malformed (read-but-unparseable) leaf reaches here.
/// Infrastructure failures (`Message`) and already-typed toolchain/hermetic
/// errors pass through unchanged.
fn reclassify_reclose_failure(
    error: SourceLoadError,
    std_root: Option<&Path>,
    demands: &[TrustedToolchainModuleDemand],
) -> SourceLoadError {
    match error {
        SourceLoadError::Compiler { snapshot, errors } => {
            // Attribute to the module the diagnostics locate, if any.
            let failing = snapshot.as_ref().and_then(|snapshot| {
                errors
                    .iter()
                    .find_map(|error| error.span())
                    .and_then(|span| {
                        let module = snapshot.module_id(span.file_id)?;
                        let path = snapshot
                            .metadata()
                            .physical_path(span.file_id)
                            .map(PathBuf::from)
                            .unwrap_or_default();
                        Some((module.as_str().to_owned(), path))
                    })
            });
            let (logical_path, path) = failing.unwrap_or_else(|| {
                // No locatable span: fall back to the first parked demand.
                let demand = demands.first();
                (
                    demand
                        .map(|demand| demand.logical_path().to_owned())
                        .unwrap_or_default(),
                    demand
                        .map(|demand| toolchain_module_path(std_root, demand))
                        .unwrap_or_default(),
                )
            });
            SourceLoadError::Toolchain(ToolchainIntegrityError::Malformed {
                logical_path,
                path,
                errors,
            })
        }
        other => other,
    }
}

/// A deterministic toolchain-integrity failure raised by the host while
/// satisfying a [`TrustedToolchainModuleDemand`].
///
/// The standard library exists as a toolchain guarantee: a missing, unreadable,
/// or malformed `option.rue` at the host read is a broken installation, like a
/// missing linker — a loud, typed, environmental failure, not a language-semantics
/// state. It is raised only for a program whose reached bodies actually demand the
/// module; programs that never demand it never observe it. It is folded into
/// [`SourceLoadError::Toolchain`] and kept distinct from a hermetic policy denial.
#[derive(Debug)]
pub enum ToolchainIntegrityError {
    /// A trusted toolchain module was demanded but no standard-library root is
    /// configured, so the host cannot resolve it at all.
    StdRootUnavailable { logical_path: String },
    /// The trusted module does not exist under the configured std root.
    Missing { logical_path: String, path: PathBuf },
    /// The trusted module exists but could not be read stably.
    Unreadable {
        logical_path: String,
        path: PathBuf,
        reason: String,
    },
    /// The trusted module was read but does not form a well-formed module.
    Malformed {
        logical_path: String,
        path: PathBuf,
        errors: CompileErrors,
    },
    /// The host published the successor but the demanded trusted module did not
    /// appear in it — an internal contradiction, surfaced loudly rather than
    /// looping forever.
    UnsatisfiedAfterPublish { logical_path: String },
}

impl std::fmt::Display for ToolchainIntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Toolchain integrity error: ")?;
        match self {
            Self::StdRootUnavailable { logical_path } => write!(
                f,
                "the program requires the trusted standard-library module '{logical_path}', but no standard-library path is configured (set RUE_STD_PATH). This is a broken toolchain installation, not a program error."
            ),
            Self::Missing { logical_path, path } => write!(
                f,
                "the trusted standard-library module '{logical_path}' is missing from the toolchain at '{}'. The standard library must exist as a toolchain guarantee; this is a broken installation.",
                path.display()
            ),
            Self::Unreadable {
                logical_path,
                path,
                reason,
            } => write!(
                f,
                "the trusted standard-library module '{logical_path}' at '{}' could not be read: {reason}. This is a broken toolchain installation.",
                path.display()
            ),
            Self::Malformed {
                logical_path,
                path,
                errors,
            } => write!(
                f,
                "the trusted standard-library module '{logical_path}' at '{}' is malformed: {errors}. This is a broken toolchain installation.",
                path.display()
            ),
            Self::UnsatisfiedAfterPublish { logical_path } => write!(
                f,
                "the trusted standard-library module '{logical_path}' was read and published but did not appear in the successor module set. This is a compiler invariant violation."
            ),
        }
    }
}

/// A hermetic build-configuration denial while satisfying a
/// [`TrustedToolchainModuleDemand`] (RUE-1112).
///
/// A trusted-module path the hermetic build configuration forbids — the
/// `--source-manifest` policy did not authorize the read, or the module
/// canonicalizes outside the standard-library root (a symlink escape). This is
/// deterministically DISTINCT from a missing or broken toolchain: the
/// installation may be intact, but the sandbox policy denies the read, so the
/// remedy is the source manifest, not the toolchain. It therefore carries its
/// own outer classification and presentation and never the "toolchain integrity"
/// / broken-installation framing. A manifest denial is enforced before any
/// filesystem probe, so a denied path is never even stat-ed.
#[derive(Debug)]
pub struct HermeticDenialError {
    pub(crate) logical_path: String,
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

impl std::fmt::Display for HermeticDenialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Hermetic build configuration error: the trusted standard-library module '{}' at '{}' is not permitted by the hermetic build configuration: {}. This is a build-configuration error (adjust the source manifest), not a broken toolchain.",
            self.logical_path,
            self.path.display(),
            self.reason
        )
    }
}

/// The outcome of a failed trusted-toolchain-module acquisition: either the
/// toolchain itself is broken ([`ToolchainIntegrityError`]) or the hermetic build
/// configuration denies the read ([`HermeticDenialError`]). These two classes are
/// kept distinct all the way to the CLI so a denied read is never presented as a
/// corrupt toolchain.
#[derive(Debug)]
pub(crate) enum ToolchainAcquisitionError {
    Toolchain(ToolchainIntegrityError),
    Hermetic(HermeticDenialError),
}

impl std::fmt::Display for ToolchainAcquisitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toolchain(error) => write!(f, "{error}"),
            Self::Hermetic(error) => write!(f, "{error}"),
        }
    }
}

impl From<ToolchainIntegrityError> for ToolchainAcquisitionError {
    fn from(error: ToolchainIntegrityError) -> Self {
        Self::Toolchain(error)
    }
}

impl From<HermeticDenialError> for ToolchainAcquisitionError {
    fn from(error: HermeticDenialError) -> Self {
        Self::Hermetic(error)
    }
}

impl From<ToolchainAcquisitionError> for SourceLoadError {
    fn from(error: ToolchainAcquisitionError) -> Self {
        match error {
            ToolchainAcquisitionError::Toolchain(error) => SourceLoadError::Toolchain(error),
            ToolchainAcquisitionError::Hermetic(error) => SourceLoadError::HermeticDenial(error),
        }
    }
}

/// Resolve one trusted module against the std root, read it under the same
/// stability contract as ordinary imports, and add it to the assembler through
/// the trusted classification. This never touches the import ledger.
///
/// A trusted-module read obeys the same manifest policy as an ordinary import
/// read: the manifest is the authority, consulted *before* any filesystem
/// probe. The lexical declaration check runs first so a denied path is never
/// even stat-ed; the resolved module must then canonicalize under the std root
/// (a symlink that escapes it is rejected) and satisfy the manifest's canonical
/// allow-list. A policy denial is a hermetic build-configuration failure,
/// deterministically distinct from a missing or malformed toolchain.
fn satisfy_toolchain_module_demand(
    assembler: &mut DiscoverySourceAssembler,
    std_root: Option<&Path>,
    source_manifest: Option<&SourceManifest>,
    demand: &TrustedToolchainModuleDemand,
) -> Result<(), ToolchainAcquisitionError> {
    let std_root = std_root.ok_or_else(|| ToolchainIntegrityError::StdRootUnavailable {
        logical_path: demand.logical_path().to_owned(),
    })?;
    let requested = normalize_lexical_path(&std_root.join(demand.std_relative_path()));

    // Manifest authority before any probe. A path the policy does not lexically
    // declare must not touch the filesystem at all, so this check precedes
    // `canonicalize`.
    if let Some(manifest) = source_manifest
        && !manifest.declares_path_without_probe(&requested)
    {
        return Err(HermeticDenialError {
            logical_path: demand.logical_path().to_owned(),
            path: requested,
            reason: format!(
                "the source manifest '{}' does not declare this path",
                manifest.display_path()
            ),
        }
        .into());
    }

    let canonical = match fs::canonicalize(&requested) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolchainIntegrityError::Missing {
                logical_path: demand.logical_path().to_owned(),
                path: requested,
            }
            .into());
        }
        Err(error) => {
            return Err(ToolchainIntegrityError::Unreadable {
                logical_path: demand.logical_path().to_owned(),
                path: requested,
                reason: error.to_string(),
            }
            .into());
        }
    };
    if !canonical.is_file() {
        return Err(ToolchainIntegrityError::Missing {
            logical_path: demand.logical_path().to_owned(),
            path: requested,
        }
        .into());
    }

    // Canonical containment: the resolved module must lie under the canonical
    // std root. Trusted classification grants standard-library provenance, so a
    // symlink that escapes the std root (pointing at an arbitrary file) can never
    // be admitted as trusted — that would let a hostile layout inject a "trusted"
    // module from outside the toolchain.
    let canonical_std_root = fs::canonicalize(std_root).map_err(|error| HermeticDenialError {
        logical_path: demand.logical_path().to_owned(),
        path: requested.clone(),
        reason: format!(
            "the standard-library root '{}' cannot be canonicalized: {error}",
            std_root.display()
        ),
    })?;
    if !canonical.starts_with(&canonical_std_root) {
        return Err(HermeticDenialError {
            logical_path: demand.logical_path().to_owned(),
            path: canonical,
            reason: format!(
                "it canonicalizes outside the standard-library root '{}'",
                canonical_std_root.display()
            ),
        }
        .into());
    }

    // Canonical manifest allow-list: after canonicalization the module must still
    // be an allowed read, matching the ordinary import policy's canonical arm.
    if let Some(manifest) = source_manifest
        && !manifest.allows_canonical(&canonical)
    {
        return Err(HermeticDenialError {
            logical_path: demand.logical_path().to_owned(),
            path: canonical,
            reason: format!(
                "its canonical path is not listed in the source manifest '{}'",
                manifest.display_path()
            ),
        }
        .into());
    }

    let read = stable_read_to_string(&canonical).map_err(|error| match error {
        StableReadError::Io(error) => ToolchainIntegrityError::Unreadable {
            logical_path: demand.logical_path().to_owned(),
            path: requested.clone(),
            reason: error.to_string(),
        },
        StableReadError::Changed => ToolchainIntegrityError::Unreadable {
            logical_path: demand.logical_path().to_owned(),
            path: requested.clone(),
            reason: "module metadata changed during read".to_owned(),
        },
    })?;

    assembler
        .add_explicit(
            requested.to_string_lossy().as_ref(),
            canonical.to_string_lossy().as_ref(),
            read.identity,
            read.fingerprint,
            Arc::new(read.source),
        )
        .map_err(|error| ToolchainIntegrityError::Unreadable {
            logical_path: demand.logical_path().to_owned(),
            path: requested,
            reason: format!("trusted classification rejected the module: {error}"),
        })?;
    Ok(())
}

fn toolchain_module_path(
    std_root: Option<&Path>,
    demand: &TrustedToolchainModuleDemand,
) -> PathBuf {
    match std_root {
        Some(root) => normalize_lexical_path(&root.join(demand.std_relative_path())),
        None => PathBuf::from(demand.logical_path()),
    }
}

#[cfg(test)]
mod architecture_tests {
    const IMPORT_DISCOVERY_AUTHORITIES: &[&str] = &[
        "stage_import_discovery",
        "close_import_discovery",
        "AcceptedImportSource::new",
        "ImportObservation::accepted",
        "ImportObservationLedger",
        "DiscoverySourceAssembler::new",
    ];

    fn production_before_tests(source: &str) -> &str {
        source
            .split("\n#[cfg(test)]\nmod ")
            .next()
            .unwrap_or(source)
    }

    fn forbidden_import_discovery_authority(source: &str) -> Option<&'static str> {
        let production = production_before_tests(source);
        IMPORT_DISCOVERY_AUTHORITIES
            .iter()
            .copied()
            .find(|authority| production.contains(authority))
    }

    #[test]
    fn source_loader_remains_the_only_cli_import_discovery_authority() {
        for (name, source) in [
            ("main.rs", include_str!("main.rs")),
            ("compile.rs", include_str!("compile.rs")),
            ("emit.rs", include_str!("emit.rs")),
            ("output.rs", include_str!("output.rs")),
        ] {
            if let Some(authority) = forbidden_import_discovery_authority(source) {
                panic!("{name} must delegate {authority} to source_loader.rs");
            }
        }
    }

    #[test]
    fn cli_executes_only_revision_pinned_compiler_frontiers() {
        let production = production_before_tests(include_str!("source_loader.rs"));
        assert!(!production.contains(".pending_requests("));
        for required_boundary in [
            "begin_import_input_request(",
            "import_demand_frontier_for_roots(",
            "publish_import_observation_batch(",
        ] {
            assert_eq!(
                production.matches(required_boundary).count(),
                1,
                "host boundary must have exactly one call: {required_boundary}"
            );
        }
    }

    #[test]
    fn authority_scan_looks_past_test_gated_imports_but_not_into_test_modules() {
        let peer = r#"
#[cfg(test)]
use crate::test_support;

fn duplicate_peer() {
    stage_import_discovery();
}

#[cfg(test)]
mod tests {
    fn test_only_reference() {
        close_import_discovery();
    }
}
"#;

        assert_eq!(
            forbidden_import_discovery_authority(peer),
            Some("stage_import_discovery")
        );
        assert!(!production_before_tests(peer).contains("close_import_discovery"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("rue-{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative: &str, source: &str) -> PathBuf {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, source).unwrap();
            path
        }
    }

    #[test]
    fn symbol_paths_are_root_relative_across_relocated_source_trees() {
        fn sources_at(root: &Path) -> Vec<(String, String)> {
            vec![
                (
                    root.join("project/main.rue").display().to_string(),
                    String::new(),
                ),
                (
                    root.join("project/./left/nested/../entry.rue")
                        .display()
                        .to_string(),
                    String::new(),
                ),
                (root.join("dep.rue").display().to_string(), String::new()),
                (
                    root.join("project/right/shared.rue").display().to_string(),
                    String::new(),
                ),
            ]
        }

        let base = std::env::temp_dir().join("rue-symbol-path-tests");
        let short = sources_at(&base.join("a"));
        let relocated = sources_at(&base.join("a-deliberately-much-longer-relocated-source-root"));
        let expected = vec![
            "main.rue",
            "left/entry.rue",
            "../dep.rue",
            "right/shared.rue",
        ];

        assert_eq!(
            derive_symbol_paths_with_std_root(&short, None).unwrap(),
            expected
        );
        assert_eq!(
            derive_symbol_paths_with_std_root(&relocated, None).unwrap(),
            expected
        );
    }

    #[test]
    fn symbol_paths_give_external_std_a_stable_namespace() {
        fn sources_at(project: &Path, std_root: &Path) -> Vec<(String, String)> {
            vec![
                (
                    project.join("main.rue").display().to_string(),
                    String::new(),
                ),
                (
                    std_root.join("_std.rue").display().to_string(),
                    String::new(),
                ),
                (
                    std_root.join("math/float.rue").display().to_string(),
                    String::new(),
                ),
                (
                    project
                        .join("@rue-std/math/float.rue")
                        .display()
                        .to_string(),
                    String::new(),
                ),
            ]
        }

        let base = std::env::temp_dir().join("rue-symbol-std-tests");
        let std_a = base.join("toolchain-a/std");
        let std_b = base.join("a-different-toolchain-location/std");
        let first = sources_at(&base.join("build-a/project"), &std_a);
        let second = sources_at(&base.join("different-depth/build-b/project"), &std_b);
        let expected = vec![
            "main.rue",
            "\0rue-std/_std.rue",
            "\0rue-std/math/float.rue",
            "@rue-std/math/float.rue",
        ];

        assert_eq!(
            derive_symbol_paths_with_std_root(&first, Some(&std_a)).unwrap(),
            expected
        );
        assert_eq!(
            derive_symbol_paths_with_std_root(&second, Some(&std_b)).unwrap(),
            expected
        );
    }

    #[cfg(windows)]
    #[test]
    fn symbol_paths_reject_unnamed_cross_volume_sources() {
        let sources = vec![
            (r"C:\project\main.rue".to_string(), String::new()),
            (r"D:\dependency\helper.rue".to_string(), String::new()),
        ];
        let error = derive_symbol_paths_with_std_root(&sources, None).unwrap_err();
        assert!(error.contains("another filesystem volume"));
    }

    #[test]
    fn source_manifest_entry_parses_comments_and_escaped_hashes() {
        assert_eq!(
            parse_source_manifest_entry("main.rue # comment"),
            "main.rue"
        );
        assert_eq!(
            parse_source_manifest_entry("dir/has\\#hash.rue # comment"),
            "dir/has#hash.rue"
        );
        assert_eq!(
            parse_source_manifest_entry("dir/has\\\\#comment.rue"),
            "dir/has\\\\"
        );
        assert_eq!(
            parse_source_manifest_entry("dir/trailing-backslash\\"),
            "dir/trailing-backslash\\"
        );
    }

    #[test]
    fn source_manifest_load_allows_escaped_hash_in_path() {
        let dir = TestDir::new("source-manifest-escaped-hash");
        let main = dir.write("main.rue", "fn main() -> i32 { 0 }\n");
        let hashed = dir.write("has#hash.rue", "pub fn answer() -> i32 { 42 }\n");
        let manifest = dir.write(
            "sources.manifest",
            "main.rue # normal comment\nhas\\#hash.rue # comment after escaped path\n",
        );

        let manifest = SourceManifest::load(manifest.to_str().unwrap()).unwrap();

        assert!(manifest.allows_canonical(&fs::canonicalize(main).unwrap()));
        assert!(manifest.allows_canonical(&fs::canonicalize(hashed).unwrap()));
    }

    #[test]
    fn unreadable_import_candidate_is_an_error_not_absence() {
        let dir = TestDir::new("unreadable-import");
        let main_path = dir.write(
            "main.rue",
            "const h = @import(\"helper.rue\");\nfn main() -> i32 { 0 }\n",
        );
        fs::write(dir.path.join("helper.rue"), [0xFFu8]).unwrap();

        let root_source = main_path.to_string_lossy().into_owned();
        assert!(discover_and_load_imports(&root_source, None, None).is_err());

        fs::write(dir.path.join("helper.rue"), "pub fn h() -> i32 { 1 }\n").unwrap();
        let result = discover_and_load_imports(&root_source, None, None).unwrap();
        assert_eq!(result.source_snapshot.len(), 2);
    }

    #[test]
    fn wide_same_depth_imports_use_one_host_frontier_round() {
        let single = TestDir::new("single-frontier");
        let single_main = single.write(
            "main.rue",
            r#"const a = @import("a.rue"); fn main() -> i32 { 0 }"#,
        );
        single.write("a.rue", "pub fn value() -> i32 { 1 }");
        let single_result =
            discover_and_load_imports(single_main.to_str().unwrap(), None, None).unwrap();

        let wide = TestDir::new("wide-frontier");
        let mut source = String::new();
        for index in 0..24 {
            source.push_str(&format!("const m{index} = @import(\"m{index}.rue\");\n"));
            wide.write(
                &format!("m{index}.rue"),
                &format!("pub fn value{index}() -> i32 {{ {index} }}"),
            );
        }
        source.push_str("fn main() -> i32 { 0 }\n");
        let wide_main = wide.write("main.rue", &source);
        let wide_result =
            discover_and_load_imports(wide_main.to_str().unwrap(), None, None).unwrap();

        assert_eq!(single_result.input_revision.frontier_round(), 1);
        assert_eq!(
            wide_result.input_revision.frontier_round(),
            single_result.input_revision.frontier_round(),
            "frontier count must depend on graph depth, not same-depth width"
        );
        assert_eq!(wide_result.read_manifest.len(), 25);
    }

    /// ADR-0075: depth costs hops inside one wave, not published revisions. This
    /// chain is three imports deep and still publishes exactly one discovery
    /// revision — the wave reads `a`, resolves ITS import in the same round,
    /// reads `b`, and so on to the fixed point before publishing once.
    #[test]
    fn import_chain_publishes_one_revision_per_wave_regardless_of_depth() {
        let dir = TestDir::new("depth-frontier");
        let main = dir.write(
            "main.rue",
            r#"const a = @import("a.rue"); fn main() -> i32 { a.b.c.value() }"#,
        );
        dir.write(
            "a.rue",
            r#"pub const b = @import("b.rue"); pub fn a() -> i32 { 1 }"#,
        );
        dir.write(
            "b.rue",
            r#"pub const c = @import("c.rue"); pub fn b() -> i32 { 2 }"#,
        );
        dir.write("c.rue", "pub fn value() -> i32 { 3 }");

        let result = discover_and_load_imports(main.to_str().unwrap(), None, None).unwrap();
        assert_eq!(result.input_revision.frontier_round(), 1);
        assert_eq!(result.read_manifest.len(), 4);
    }

    /// ADR-0075 witness soundness. The cumulative record is what lets an
    /// ordinary discovery skip the closing whole-plan rooting, so the fallback
    /// it replaces has to stay reachable and correct. Forge a record that
    /// cannot prove coverage and the whole-plan rooting must fire, producing
    /// the same close.
    #[test]
    fn a_record_that_cannot_prove_coverage_falls_back_to_the_whole_plan_rooting() {
        const MODULES: usize = 8;
        let dir = TestDir::new("witness-fallback");
        let main = dir.write(
            "main.rue",
            r#"const next = @import("m1.rue"); fn main() -> i32 { 0 }"#,
        );
        for index in 1..MODULES {
            let source = if index + 1 == MODULES {
                format!("pub fn value{index}() -> i32 {{ {index} }}")
            } else {
                format!(
                    "pub const next = @import(\"m{}.rue\"); pub fn value{index}() -> i32 {{ {index} }}",
                    index + 1
                )
            };
            dir.write(&format!("m{index}.rue"), &source);
        }
        let root = main.to_str().unwrap();

        let proven = discover_and_load_imports(root, None, None).unwrap();
        assert_eq!(
            proven.witness_discharge,
            WitnessDischarge {
                cumulative: 1,
                reroot: 0
            }
        );

        let forged = {
            let _armed = ForgedUncoveredSegment::arm();
            discover_and_load_imports(root, None, None).unwrap()
        };
        assert_eq!(
            forged.witness_discharge,
            WitnessDischarge {
                cumulative: 0,
                reroot: 1
            },
            "a record that cannot prove coverage must re-root the whole plan"
        );

        // The fallback is a proof strategy, not a different answer: same reads,
        // same closed plan, same revision count.
        assert_eq!(forged.read_manifest, proven.read_manifest);
        assert_eq!(
            forged.input_revision.frontier_round(),
            proven.input_revision.frontier_round()
        );
        // And it costs exactly what it used to: one extra rooting per plan
        // occurrence, which is the dispatch the cumulative record removes.
        assert_eq!(
            import_frontier_roots_requested(&forged.session),
            import_frontier_roots_requested(&proven.session) + (MODULES as u64 - 1),
            "the fallback re-dispatches the whole plan the record would have proven"
        );
    }

    #[test]
    fn import_chain_stages_only_each_new_module_and_import_group() {
        const MODULES: usize = 32;
        let dir = TestDir::new("linear-depth-staging");
        let main = dir.write(
            "main.rue",
            r#"const next = @import("m1.rue"); fn main() -> i32 { 0 }"#,
        );
        for index in 1..MODULES {
            let source = if index + 1 == MODULES {
                format!("pub fn value{index}() -> i32 {{ {index} }}")
            } else {
                format!(
                    "pub const next = @import(\"m{}.rue\"); pub fn value{index}() -> i32 {{ {index} }}",
                    index + 1
                )
            };
            dir.write(&format!("m{index}.rue"), &source);
        }

        let result = discover_and_load_imports(main.to_str().unwrap(), None, None).unwrap();

        // ADR-0075: the whole chain is ONE wave, so discovery publishes exactly
        // one input revision no matter how deep it runs. Publishing per import
        // hop instead cost MODULES-1 revisions here — 31 revision mints, ledger
        // appends, batch publications, and validation sweeps — and grew with
        // depth by contract.
        assert_eq!(
            result.input_revision.frontier_round(),
            1,
            "a chain of any depth publishes one discovery revision per wave"
        );
        assert_eq!(result.read_manifest.len(), MODULES);
        assert!(
            parse_sources_materialized(&result.session) <= (MODULES + 2) as u64,
            "frontier staging plus final presentation must remain linear in module count"
        );
        assert!(
            parse_modules_dispatched(&result.session) <= (MODULES * 2) as u64,
            "module parse dispatch must remain linear in module count"
        );
        assert_eq!(
            import_plan_groups_constructed(&result.session),
            (MODULES - 1) as u64,
            "each import occurrence must enter the plan exactly once"
        );
        // ADR-0073: every publication is an append-only overlay, so validation
        // certificates survive the whole discovery chain. Without that, each of
        // the ~MODULES hop-granular rounds expired every certificate and this
        // count grew as rounds times graph size — quadratic in chain depth (961+
        // at this shape). ADR-0075 leaves one publication to expire anything at
        // all, so the count is 1 either way; the bound stays loose against
        // scheduling variation and can only fall as waves remove publications.
        assert!(
            rue_compiler::unstable::validation_certificate_misses(&result.session)
                <= (MODULES as u64) * 8,
            "discovery certificate misses must stay linear in module count"
        );
        // The wave resolves the whole chain against its own running ledger, so
        // the query frontier is dispatched exactly twice: once for the round's
        // starting occurrence, and once more for the closing round, which roots
        // in the wave's fanout (MODULES-1 occurrences) and then owes the whole
        // plan one closure rooting (MODULES-1 again) — 1 + 31 + 31 = 63 here.
        // Publishing per hop instead cost 93 at this shape, and rooting the
        // whole plan every round cost 527 and grew quadratically with depth.
        // The wave resolves the whole chain against its own running ledger, so
        // the query frontier is dispatched exactly twice: once for the round's
        // starting occurrence, and once more for the closing round, which roots
        // in the wave's fanout (MODULES-1 occurrences) — 1 + 31 = 32 here.
        //
        // The closing round used to owe the whole plan one more rooting
        // (MODULES-1 again, 63 total). ADR-0075's cumulative witness discharges
        // that debt from the record instead, so the closing dispatch is gone
        // and this is now an exact count rather than a bound.
        assert_eq!(
            import_frontier_roots_requested(&result.session),
            MODULES as u64,
            "frontier dispatch must be the wave's roots alone, with the closing \
             witness proven from the cumulative record"
        );
        assert_eq!(
            result.witness_discharge,
            WitnessDischarge {
                cumulative: 1,
                reroot: 0
            },
            "an ordinary chain closes on the cumulative record"
        );
        // The read half of the same property: the wave reduces exactly the
        // occurrences each hop answered, one accepted source apiece, and the
        // closing round's frontier is empty so it assembles nothing — MODULES-1
        // reads offered in total. Publishing per hop cost 61 (each round also
        // re-reduced its predecessor's fanout occurrences), and re-reducing the
        // whole plan every round offered 528 and grew quadratically with depth.
        assert!(
            result.assembler.plan_reads_reduced() <= MODULES as u64,
            "per-round read volume must stay linear in chain depth, not rounds times plan"
        );
    }

    /// Deterministic per-size counters for the generated import chain below.
    ///
    /// Every field is a work counter published by the compiler, never a clock:
    /// at `-j1` a fresh build of a fixed chain performs a fixed sequence of
    /// query executions, so each field is exactly reproducible.
    #[derive(Clone, Copy)]
    struct ChainScalingCounters {
        module_resolutions: u64,
        module_resolution_visits: u64,
        identity_lookups: u64,
        identity_visits: u64,
        handoff_observations: u64,
        handoff_observation_visits: u64,
        parse_sources_materialized: u64,
        parse_key_entries_compared: u64,
        parse_modules_dispatched: u64,
        parse_invalidation_entries_compared: u64,
        plan_groups: u64,
        frontier_roots: u64,
        plan_reads_reduced: u64,
        close_records_reduced: u64,
        overlay_leaves_published: u64,
        ledger_entries_cloned: u64,
    }

    /// Compile a generated `modules`-deep import chain and read its counters.
    ///
    /// The chain is written programmatically rather than checked in: a fixture
    /// this size exists only to be scaled, and two committed copies of it would
    /// drift from each other and from the shape this gate describes.
    fn chain_scaling_counters(modules: usize) -> ChainScalingCounters {
        let dir = TestDir::new(&format!("chain-scaling-{modules}"));
        let main = dir.write(
            "main.rue",
            r#"const next = @import("m1.rue"); fn main() -> i32 { 0 }"#,
        );
        for index in 1..modules {
            let source = if index + 1 == modules {
                format!("pub fn value{index}() -> i32 {{ {index} }}")
            } else {
                format!(
                    "pub const next = @import(\"m{}.rue\"); pub fn value{index}() -> i32 {{ {index} }}",
                    index + 1
                )
            };
            dir.write(&format!("m{index}.rue"), &source);
        }

        let result = discover_and_load_imports(main.to_str().unwrap(), None, None).unwrap();
        assert_eq!(result.read_manifest.len(), modules);
        ChainScalingCounters {
            module_resolutions: snapshot_module_resolutions(&result.session),
            module_resolution_visits: snapshot_module_resolution_visits(&result.session),
            identity_lookups: accepted_read_identity_lookups(&result.session),
            identity_visits: accepted_read_identity_visits(&result.session),
            handoff_observations: handoff_observations(&result.session),
            handoff_observation_visits: handoff_observation_visits(&result.session),
            parse_sources_materialized: parse_sources_materialized(&result.session),
            parse_key_entries_compared: parse_key_entries_compared(&result.session),
            parse_modules_dispatched: parse_modules_dispatched(&result.session),
            parse_invalidation_entries_compared: parse_invalidation_entries_compared(
                &result.session,
            ),
            plan_groups: import_plan_groups_constructed(&result.session),
            frontier_roots: import_frontier_roots_requested(&result.session),
            plan_reads_reduced: result.assembler.plan_reads_reduced(),
            close_records_reduced: import_close_records_reduced(&result.session),
            overlay_leaves_published: import_view_overlay_leaves_published(&result.session),
            ledger_entries_cloned: import_view_ledger_entries_cloned(&result.session),
        }
    }

    /// Deep-chain scaling gate: no counter below may grow faster than the
    /// module count does, with headroom.
    ///
    /// WHY THIS EXISTS. The chain test above pins *dispatch* counts — frontier
    /// roots, plan groups, reduced reads, certificate misses — and those bounds
    /// caught the discovery-round regressions they were written for. They
    /// cannot see a regression that dispatches nothing. Two such regressions
    /// shipped and lived: projecting a parsed program resolved each module to
    /// its file by scanning every file in the snapshot, and authorizing a
    /// discovery batch resolved each accepted observation to its manifest entry
    /// by scanning every accepted read. Each is one scan per module over a set
    /// that grows with the program, so each is quadratic in chain depth — and
    /// every counter this compiler published was unchanged by them, at 32
    /// modules and at 1024. They were found by running callgrind over 256- and
    /// 1024-module chains and reading a 15x instruction ratio for 4x the
    /// modules. CI cannot run callgrind and wall clock on a shared host is far
    /// too noisy to gate, so the answer is to make scan-shaped work countable:
    /// `*_lookups` counts the identity questions asked, `*_visits` counts the
    /// positions examined answering them, and their ratio is exactly what an
    /// index-versus-scan regression moves.
    ///
    /// The handoff pair extends that shape past discovery into the query
    /// runtime (RUE-1579). Recording an attempt handoff deduplicates it by
    /// pointer identity against everything the same observation scope already
    /// holds, so a scope that starts retaining live lifecycles turns each
    /// observation into a walk over the ones before it — the same quadratic
    /// with no dispatch to show for it. That site measured superlinear once
    /// under callgrind and is linear now; counting it is what keeps it so.
    ///
    /// SIZES. 64 and 256 modules: a 4x span, wide enough that a quadratic term
    /// shows as ~16x against the ~4x asserted here, and cheap enough for the
    /// premerge tier — the two compiles together run in well under a second.
    ///
    /// HEADROOM. The bound is the size ratio times 1.5. Nothing here is
    /// expected to beat linear, but several of these counters carry an honest
    /// n-log-n term: the snapshot and the accepted-read manifest are
    /// size-tiered segment sequences whose segment count grows with log n, and
    /// discovery's ordered `BTreeMap`/`BTreeSet` bookkeeping costs log n per
    /// element. Across this span that factor is log2(256)/log2(64) = 1.33, so
    /// 1.5 admits n-log-n with margin while leaving a quadratic term — which
    /// needs 4.0 — nowhere to hide.
    #[test]
    fn import_chain_identity_resolution_stays_linear_in_depth() {
        const SMALL: usize = 64;
        const LARGE: usize = 256;
        const SIZE_RATIO: f64 = (LARGE / SMALL) as f64;
        /// See HEADROOM above: admits an n-log-n term, excludes a quadratic one.
        const HEADROOM: f64 = 1.5;

        let small = chain_scaling_counters(SMALL);
        let large = chain_scaling_counters(LARGE);

        // A counter that stopped being incremented would make every ratio below
        // pass for free, so require the two new counters to have observed at
        // least one question per module before reading their growth.
        for (label, counters, modules) in
            [("64-module", &small, SMALL), ("256-module", &large, LARGE)]
        {
            assert!(
                counters.module_resolutions >= modules as u64,
                "{label} chain: every module is resolved to its file at least once"
            );
            assert!(
                counters.module_resolution_visits >= counters.module_resolutions,
                "{label} chain: a resolution examines at least one snapshot position"
            );
            assert!(
                counters.identity_lookups >= modules as u64 - 1,
                "{label} chain: every accepted import observation is authorized"
            );
            assert!(
                counters.identity_visits >= counters.identity_lookups,
                "{label} chain: a lookup examines at least one manifest entry"
            );
            assert!(
                counters.handoff_observations >= modules as u64,
                "{label} chain: every module's published work is observed as a handoff"
            );
            assert!(
                counters.handoff_observation_visits >= counters.handoff_observations,
                "{label} chain: an observation examines at least one scope position"
            );
        }

        let mut violations = Vec::new();
        for (label, small_value, large_value) in [
            (
                "snapshot module resolutions",
                small.module_resolutions,
                large.module_resolutions,
            ),
            (
                "snapshot module resolution visits",
                small.module_resolution_visits,
                large.module_resolution_visits,
            ),
            (
                "accepted-read identity lookups",
                small.identity_lookups,
                large.identity_lookups,
            ),
            (
                "accepted-read identity visits",
                small.identity_visits,
                large.identity_visits,
            ),
            (
                "handoff observations",
                small.handoff_observations,
                large.handoff_observations,
            ),
            (
                "handoff observation visits",
                small.handoff_observation_visits,
                large.handoff_observation_visits,
            ),
            (
                "parse sources materialized",
                small.parse_sources_materialized,
                large.parse_sources_materialized,
            ),
            (
                "parse key entries compared",
                small.parse_key_entries_compared,
                large.parse_key_entries_compared,
            ),
            (
                "parse modules dispatched",
                small.parse_modules_dispatched,
                large.parse_modules_dispatched,
            ),
            (
                "parse invalidation entries compared",
                small.parse_invalidation_entries_compared,
                large.parse_invalidation_entries_compared,
            ),
            ("import plan groups", small.plan_groups, large.plan_groups),
            (
                "import frontier roots",
                small.frontier_roots,
                large.frontier_roots,
            ),
            (
                "plan reads reduced",
                small.plan_reads_reduced,
                large.plan_reads_reduced,
            ),
            (
                "close records reduced",
                small.close_records_reduced,
                large.close_records_reduced,
            ),
            (
                "overlay leaves published",
                small.overlay_leaves_published,
                large.overlay_leaves_published,
            ),
            (
                "ledger entries cloned",
                small.ledger_entries_cloned,
                large.ledger_entries_cloned,
            ),
        ] {
            let ratio = large_value as f64 / small_value.max(1) as f64;
            if ratio > SIZE_RATIO * HEADROOM {
                // Report every violating counter rather than only the first: a
                // regression usually moves several at once, and which ones move
                // together is what names the site.
                violations.push(format!(
                    "{label}: {small_value} at {SMALL} modules grew to {large_value} at \
                     {LARGE} ({ratio:.2}x for {SIZE_RATIO}x the modules)"
                ));
            }
        }
        assert!(
            violations.is_empty(),
            "deep-chain work must stay linear in module count; the bound is {:.2}x \
             ({SIZE_RATIO}x the modules times {HEADROOM} headroom):\n  {}",
            SIZE_RATIO * HEADROOM,
            violations.join("\n  ")
        );
    }

    /// A candidate that misses on its first group is answered but NOT concluded:
    /// the occurrence still owes its next candidate an operation. Under policy
    /// v2 the only multi-group occurrence is `std`: the vendored
    /// `{root}/std/_std.rue` is probed absent here, so the occurrence is
    /// answered but still owes the toolchain root's facade an operation.
    ///
    /// The contract this pins is unchanged by ADR-0075 — an occurrence that is
    /// answered but still open must be carried forward, and dropping it loses
    /// the standard library — the wave keeps the occurrence in its open set and
    /// derives its next candidate at the next hop, instead of a later round
    /// re-rooting it.
    #[test]
    fn import_occurrence_answered_but_still_open_is_carried_across_wave_hops() {
        let dir = TestDir::new("open-occurrence-reroot");
        let stdlib = TestDir::new("open-occurrence-reroot-std");
        let main = dir.write(
            "main.rue",
            r#"const leaf = @import("sub/leaf.rue"); fn main() -> i32 { leaf.value() }"#,
        );
        dir.write(
            "sub/leaf.rue",
            r#"const s = @import("std"); pub fn value() -> i32 { 7 }"#,
        );
        stdlib.write("_std.rue", "pub fn std_value() -> i32 { 1 }");
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let result =
            discover_and_load_imports(main.to_str().unwrap(), None, Some(&std_root)).unwrap();

        assert_eq!(result.read_manifest.len(), 3);
        assert!(
            result
                .read_manifest
                .iter()
                .any(|entry| entry.module().as_str().starts_with("\0rue-std/")),
            "the second candidate of the still-open occurrence must be resolved"
        );
        // One wave: the leaf, the absent vendored std probe, then the toolchain
        // facade the second candidate resolves to. Publishing per hop minted
        // separate revisions for the same reads.
        assert_eq!(result.input_revision.frontier_round(), 1);
    }

    /// ADR-0075 stamp atomicity: a wave publishes one revision covering reads
    /// taken across its whole closure, so a source rewritten after it was read
    /// and before the wave publishes must be caught as a batch, fail-closed.
    ///
    /// The hook fires in exactly that window — after the wave's last hop, before
    /// its stamp verification — and rewrites a source the wave read on its FIRST
    /// hop. The wave is discarded and re-run against the settled filesystem, and
    /// the revision that does publish carries the rewritten bytes with every
    /// recorded read verifying. No revision can mix a stale read with a fresh
    /// one, because one disagreement discards the whole wave.
    #[test]
    fn a_source_rewritten_mid_wave_forces_a_fail_closed_wave_rerun() {
        let dir = TestDir::new("wave-stamp-atomicity");
        let main = dir.write(
            "main.rue",
            r#"const a = @import("a.rue"); fn main() -> i32 { a.value() }"#,
        );
        dir.write(
            "a.rue",
            r#"pub const b = @import("b.rue"); pub fn value() -> i32 { b.value() + 1 }"#,
        );
        dir.write("b.rue", "pub fn value() -> i32 { 2 }");
        let rewritten =
            r#"pub const b = @import("b.rue"); pub fn value() -> i32 { b.value() + 9 }"#;

        let path = dir.path.join("a.rue");
        let fired = std::cell::Cell::new(false);
        WAVE_STAMP_RERUNS.with(|count| count.set(0));
        WAVE_PUBLISH_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                if !fired.replace(true) {
                    fs::write(&path, rewritten).unwrap();
                }
            }));
        });
        let result = discover_and_load_imports(main.to_str().unwrap(), None, None);
        WAVE_PUBLISH_HOOK.with(|hook| *hook.borrow_mut() = None);
        let result = result.expect("a re-run wave closes against the settled filesystem");

        assert_eq!(
            WAVE_STAMP_RERUNS.with(std::cell::Cell::get),
            1,
            "the mid-wave rewrite must discard exactly one wave"
        );
        assert_eq!(result.read_manifest.len(), 3);
        assert!(
            result
                .source_snapshot
                .files()
                .any(|source| source.source.contains("b.value() + 9")),
            "the published revision must carry the rewritten source, never the stale read"
        );
        for entry in result.read_manifest.iter() {
            let metadata = fs::metadata(entry.canonical_path()).expect("a published read exists");
            assert_eq!(
                physical_file_identity(&metadata),
                entry.metadata_identity(),
                "every recorded read of the published revision verifies"
            );
            assert_eq!(
                file_metadata_fingerprint(&metadata),
                entry.metadata_fingerprint(),
                "every recorded read of the published revision verifies"
            );
        }
    }

    /// ADR-0075 determinism: the ledger's read order and the revision's contents
    /// are properties of the compiler's candidate policy, not of how many query
    /// workers happen to be running. A wave dedupes host operations and derives
    /// each hop from its own ledger, so both must be identical across worker
    /// counts and across repeated runs.
    #[test]
    fn wave_ledger_order_and_contents_are_independent_of_worker_count() {
        fn discover(dir: &TestDir, jobs: usize) -> (String, Vec<String>, u64) {
            rue_compiler::configure_thread_pool(jobs);
            let result =
                discover_and_load_imports(dir.path.join("main.rue").to_str().unwrap(), None, None)
                    .unwrap();
            let modules = result
                .source_snapshot
                .source_revision()
                .modules()
                .iter()
                .map(|revision| revision.module.as_str().to_owned())
                .collect();
            (
                rue_compiler::unstable::import_discovery_observation_ledger_debug(&result.revision),
                modules,
                result.input_revision.frontier_round(),
            )
        }

        // A shape with real ordering pressure: same-depth siblings, a deeper
        // chain, and one occurrence whose first candidate misses beside its
        // importer and is answered at the project root.
        let dir = TestDir::new("wave-determinism");
        dir.write(
            "main.rue",
            r#"const a = @import("a.rue"); const d = @import("sub/d.rue"); fn main() -> i32 { a.value() + d.value() }"#,
        );
        dir.write(
            "a.rue",
            r#"pub const b = @import("b.rue"); pub const c = @import("c.rue"); pub fn value() -> i32 { b.value() + c.value() }"#,
        );
        dir.write(
            "b.rue",
            r#"pub const shared = @import("shared.rue"); pub fn value() -> i32 { shared.value() }"#,
        );
        dir.write("c.rue", "pub fn value() -> i32 { 3 }");
        dir.write(
            "sub/d.rue",
            r#"pub const shared = @import("shared.rue"); pub fn value() -> i32 { shared.value() }"#,
        );
        dir.write("shared.rue", "pub fn value() -> i32 { 7 }");

        let single = discover(&dir, 1);
        let repeat = discover(&dir, 1);
        let parallel = discover(&dir, 4);
        rue_compiler::configure_thread_pool(0);

        assert_eq!(single, repeat, "discovery must be reproducible run to run");
        assert_eq!(
            single, parallel,
            "ledger read order and revision contents must not depend on worker count"
        );
        assert_eq!(single.2, 1, "this closure is one wave");
        assert_eq!(single.1.len(), 6);
    }

    fn module_source_id(
        result: &ImportDiscoveryResult,
        logical_path: &str,
    ) -> rue_compiler::SourceId {
        result
            .source_snapshot
            .source_revision()
            .modules()
            .iter()
            .find(|module| module.module.as_str() == logical_path)
            .expect("test module is present")
            .source
            .clone()
    }

    #[test]
    fn tier_b_sweep_reuses_an_identical_out_of_band_rewrite() {
        let dir = TestDir::new("tier-b-identical");
        let main = dir.write(
            "main.rue",
            r#"const leaf = @import("leaf.rue"); fn main() -> i32 { leaf.value() }"#,
        );
        let leaf_source = "pub fn value() -> i32 { 1 }";
        let leaf = dir.write("leaf.rue", leaf_source);
        let mut result = discover_and_load_imports(main.to_str().unwrap(), None, None).unwrap();
        let source_id = module_source_id(&result, "leaf.rue");
        let semantic_before = rooted_cfg(&mut result.session, &CompileOptions::default()).unwrap();

        fs::write(&leaf, leaf_source).unwrap();
        reload_from_filesystem(&mut result).unwrap();
        let semantic_after = rooted_cfg(&mut result.session, &CompileOptions::default()).unwrap();

        assert_eq!(module_source_id(&result, "leaf.rue"), source_id);
        assert_eq!(
            semantic_before
                .functions()
                .iter()
                .map(|function| function.source_name().to_owned())
                .collect::<Vec<_>>(),
            semantic_after
                .functions()
                .iter()
                .map(|function| function.source_name().to_owned())
                .collect::<Vec<_>>(),
            "an identical rewrite must preserve the canonical semantic projection"
        );
    }

    #[test]
    fn tier_b_sweep_detects_a_changed_out_of_band_rewrite() {
        let dir = TestDir::new("tier-b-changed");
        let main = dir.write(
            "main.rue",
            r#"const leaf = @import("leaf.rue"); fn main() -> i32 { leaf.value() }"#,
        );
        let leaf = dir.write("leaf.rue", "pub fn value() -> i32 { 1 }");
        let mut result = discover_and_load_imports(main.to_str().unwrap(), None, None).unwrap();
        let source_id = module_source_id(&result, "leaf.rue");

        // Same-length bytes make size useless, and this write happens inside the
        // timestamp window in which an unchanged mtime cannot establish order.
        fs::write(&leaf, "pub fn value() -> i32 { 2 }").unwrap();
        reload_from_filesystem(&mut result).unwrap();

        assert_ne!(module_source_id(&result, "leaf.rue"), source_id);
        assert!(
            result
                .source_snapshot
                .files()
                .any(|source| source.source.contains("value() -> i32 { 2 }"))
        );
    }

    #[test]
    fn too_recent_mtime_forces_hashing_even_when_metadata_matches() {
        let dir = TestDir::new("tier-b-recent-mtime");
        let path = dir.write("leaf.rue", "pub fn value() -> i32 { 1 }");
        let metadata = fs::metadata(path).unwrap();
        let modified = metadata.modified().unwrap();

        assert!(metadata_requires_content_hash(
            physical_file_identity(&metadata),
            file_metadata_fingerprint(&metadata),
            &metadata,
            modified,
        ));
    }

    #[test]
    fn tier_b_reload_reloads_read_policy_and_fails_closed() {
        let dir = TestDir::new("tier-b-policy");
        let main = dir.write(
            "main.rue",
            r#"const leaf = @import("leaf.rue"); fn main() -> i32 { leaf.value() }"#,
        );
        dir.write("leaf.rue", "pub fn value() -> i32 { 1 }");
        let manifest_path = dir.write("sources.manifest", "main.rue\nleaf.rue\n");
        let manifest = SourceManifest::load(manifest_path.to_str().unwrap()).unwrap();
        let mut result =
            discover_and_load_imports(main.to_str().unwrap(), Some(manifest), None).unwrap();

        fs::write(&manifest_path, "main.rue\n").unwrap();
        match reload_from_filesystem(&mut result) {
            Err(SourceLoadError::Compiler { errors, .. }) => {
                let rendered = errors.to_string();
                assert!(rendered.contains("source manifest"), "{rendered}");
                assert!(rendered.contains("leaf.rue"), "{rendered}");
            }
            Err(other) => panic!("policy change escaped typed diagnostics: {other:?}"),
            Ok(()) => panic!("policy change reused a now-denied read"),
        }
    }

    #[test]
    fn rooted_std_discovery_does_not_read_unrelated_std_modules() {
        let project = TestDir::new("std-project");
        let stdlib = TestDir::new("std-library");
        let main = project.write(
            "main.rue",
            r#"const std = @import("std"); fn main() -> i32 { 0 }"#,
        );
        stdlib.write("_std.rue", r#"pub const math = @import("math.rue");"#);
        stdlib.write("math.rue", "pub fn answer() -> i32 { 42 }");
        let unrelated =
            fs::canonicalize(stdlib.write("unrelated.rue", "pub fn unused() -> i32 { 0 }"))
                .unwrap();
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let result =
            discover_and_load_imports(main.to_str().unwrap(), None, Some(&std_root)).unwrap();
        // `std` and the `math.rue` it re-exports are two hops of one wave.
        assert_eq!(result.input_revision.frontier_round(), 1);
        assert_eq!(result.read_manifest.len(), 3);
        assert!(
            result
                .read_manifest
                .iter()
                .all(|entry| entry.canonical_path() != unrelated.to_string_lossy())
        );
    }

    /// RUE-991: a std root reached through a symlinked prefix is the same
    /// location as its canonical spelling, so its modules must classify as
    /// standard-library rather than as escaping their own root. This is the
    /// portable form of the macOS `/var` -> `/private/var` alias that a
    /// `mktemp -d` std bundle hits on every run.
    #[cfg(unix)]
    #[test]
    fn symlinked_std_root_classifies_as_standard_library() {
        let project = TestDir::new("aliased-std-project");
        let stdlib = TestDir::new("aliased-std-library");
        let main = project.write(
            "main.rue",
            r#"const std = @import("std"); fn main() -> i32 { 0 }"#,
        );
        stdlib.write("_std.rue", r#"pub const math = @import("math.rue");"#);
        stdlib.write("math.rue", "pub fn answer() -> i32 { 42 }");
        let canonical_root = fs::canonicalize(&stdlib.path).unwrap();
        let alias = stdlib.path.with_extension("alias");
        std::os::unix::fs::symlink(&canonical_root, &alias).unwrap();

        let result = discover_and_load_imports(main.to_str().unwrap(), None, Some(&alias))
            .expect("an aliased std root resolves to the same bundle as its canonical spelling");

        assert_eq!(result.read_manifest.len(), 3);
        let std_modules: Vec<_> = result
            .read_manifest
            .iter()
            .filter(|entry| entry.module().is_trusted_standard_library())
            .map(|entry| entry.module().as_str().to_owned())
            .collect();
        assert_eq!(
            std_modules,
            vec!["\0rue-std/_std.rue", "\0rue-std/math.rue"],
            "aliased std modules keep their root-relative standard-library identity"
        );
        assert_eq!(
            result.std_root.as_deref(),
            Some(canonical_root.as_path()),
            "the captured std root is canonicalized once for the whole epoch"
        );

        fs::remove_file(&alias).unwrap();
    }

    #[test]
    fn manifest_denial_remains_a_typed_fail_closed_diagnostic() {
        let project = TestDir::new("manifest-denial");
        let main = project.write(
            "main.rue",
            r#"const missing = @import("missing"); fn main() -> i32 { 0 }"#,
        );
        let manifest_path = project.write("sources.manifest", "main.rue\n");
        let manifest = SourceManifest::load(manifest_path.to_str().unwrap()).unwrap();
        match discover_and_load_imports(main.to_str().unwrap(), Some(manifest), None) {
            Err(SourceLoadError::Compiler { errors, .. }) => {
                let rendered = errors.to_string();
                assert!(rendered.contains("source manifest"), "{rendered}");
                assert!(rendered.contains("missing.rue"), "{rendered}");
            }
            Err(SourceLoadError::Message(message)) => {
                panic!("policy denial escaped typed diagnostics: {message}")
            }
            Err(SourceLoadError::Toolchain(error)) => {
                panic!("policy denial escaped as a toolchain error: {error}")
            }
            Err(SourceLoadError::HermeticDenial(error)) => {
                panic!("import policy denial escaped as a hermetic toolchain denial: {error}")
            }
            Ok(_) => panic!("policy denial unexpectedly closed successfully"),
        }
    }

    // ---- RUE-1112 reached-body trusted-toolchain acquisition proofs ------
    //
    // These drive the whole host flow the driver runs: the initial import-discovery
    // close, then the reached-body park/retry acquisition loop
    // (`acquire_reached_toolchain_modules`). For a freestanding zero-import program
    // the initial close is the root alone; whether std is read is then decided by
    // real reached-body semantic demand, not a lexical scan — so an unreachable
    // helper never forces a std read. The observable guarantees (which trusted
    // module is acquired, and how a missing/malformed/denied one is classified)
    // are what these tests pin.

    /// Run the full host flow for `root`: initial close, then the reached-body
    /// park/retry acquisition loop, exactly as `main` orders them.
    fn load_and_acquire(
        root: &Path,
        source_manifest: Option<SourceManifest>,
        std_root: Option<&Path>,
    ) -> Result<ImportDiscoveryResult, SourceLoadError> {
        let mut result =
            discover_and_load_imports(root.to_str().unwrap(), source_manifest, std_root)?;
        acquire_reached_toolchain_modules(&mut result, &CompileOptions::default())?;
        Ok(result)
    }

    fn contains_trusted_option(snapshot: &SourceSnapshot) -> bool {
        snapshot.source_revision().modules().iter().any(|revision| {
            revision.module.is_trusted_standard_library()
                && revision.module.as_str() == rue_compiler::OPTION_MODULE_LOGICAL_PATH
        })
    }

    const FALLIBLE_ROOT: &str = "fn main() -> i32 { let _ = @parse_i64(\"1\"); 0 }";
    const VALID_OPTION: &str = "pub fn Option(comptime T: type) -> type { enum { Some(T), None } }";
    const MALFORMED_OPTION: &str = "this is not a valid rue module @@@ ???";

    /// t4: a freestanding fallible-intrinsic program acquires the trusted std
    /// `Option` its reached body demands. The acquired successor carries the
    /// module present and classified trusted alongside the root, and the assembler
    /// retains an accepted-read PROVENANCE leaf for it (never an import-ledger
    /// entry). Acquisition succeeding means the reached-body demand is satisfied.
    #[test]
    fn t4_freestanding_fallible_program_acquires_trusted_option() {
        let project = TestDir::new("t4-project");
        let stdlib = TestDir::new("t4-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        stdlib.write("option.rue", VALID_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let result =
            load_and_acquire(&main, None, Some(&std_root)).expect("host satisfies the demand");

        // The acquired snapshot contains the trusted Option, added exactly once
        // alongside the root.
        assert_eq!(result.source_snapshot.source_revision().modules().len(), 2);
        assert!(contains_trusted_option(&result.source_snapshot));

        // Provenance: an accepted-read leaf for the trusted module (a provenance
        // leaf, NOT an import-ledger entry).
        assert!(
            result
                .read_manifest
                .iter()
                .any(|entry| entry.module().is_trusted_standard_library()
                    && entry.module().as_str() == rue_compiler::OPTION_MODULE_LOGICAL_PATH)
        );
    }

    /// t1: a freestanding program with NO reached fallible intrinsic performs
    /// zero std reads and is untouched — even when the std path's option.rue is
    /// deliberately malformed. Unrelated programs never observe it.
    #[test]
    fn t1_no_fallible_intrinsic_never_reads_malformed_std() {
        let project = TestDir::new("t1-project");
        let stdlib = TestDir::new("t1-std");
        let main = project.write("main.rue", "fn main() -> i32 { let x = 1 + 2; x }");
        // Deliberately malformed; it must never be read.
        stdlib.write("option.rue", MALFORMED_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let result = load_and_acquire(&main, None, Some(&std_root))
            .expect("a program with no reached demand never touches std");

        // No std read happened: only the root module, no trusted module, and no
        // trusted leaf in the accepted-read manifest.
        assert_eq!(result.source_snapshot.source_revision().modules().len(), 1);
        assert!(!contains_trusted_option(&result.source_snapshot));
        assert!(
            !result
                .read_manifest
                .iter()
                .any(|entry| entry.module().is_trusted_standard_library())
        );
    }

    /// t5: a reached-body demand for an UNREACHABLE fallible intrinsic is never
    /// raised — a helper that mentions `@parse_i64` but is never called forces no
    /// std read even when std is malformed on disk. This is the reachability win
    /// the semantic-driven park has over a lexical scan.
    #[test]
    fn t5_unreachable_fallible_helper_never_reads_malformed_std() {
        let project = TestDir::new("t5-project");
        let stdlib = TestDir::new("t5-std");
        let main = project.write(
            "main.rue",
            "fn helper() -> i32 { let _ = @parse_i64(\"1\"); 0 }\n\
             fn main() -> i32 { 0 }",
        );
        // Malformed; it must never be read because `helper` is never reached.
        stdlib.write("option.rue", MALFORMED_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let result = load_and_acquire(&main, None, Some(&std_root))
            .expect("an unreachable fallible helper raises no demand and reads no std");
        assert!(!contains_trusted_option(&result.source_snapshot));
        assert!(
            !result
                .read_manifest
                .iter()
                .any(|entry| entry.module().is_trusted_standard_library())
        );
    }

    /// Toolchain-integrity arm: a reached fallible intrinsic with a MISSING std
    /// option.rue yields the deterministic environmental error.
    #[test]
    fn toolchain_integrity_missing_std_option_is_environmental_error() {
        let project = TestDir::new("integrity-missing-project");
        let stdlib = TestDir::new("integrity-missing-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        // No option.rue written under std_root.
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let error = load_and_acquire(&main, None, Some(&std_root))
            .expect_err("a missing trusted module is a toolchain-integrity error");
        assert!(
            matches!(
                error,
                SourceLoadError::Toolchain(ToolchainIntegrityError::Missing { .. })
            ),
            "expected Missing, got {error:?}"
        );
        let rendered = error_display(&error);
        assert!(rendered.contains("Toolchain integrity error"), "{rendered}");
        assert!(rendered.contains("broken"), "{rendered}");
    }

    /// Toolchain-integrity arm: a reached fallible intrinsic with a MALFORMED std
    /// option.rue yields the deterministic environmental error. A malformed
    /// trusted module fails to parse at the same-generation re-close (the only new
    /// module there is the one just acquired), so it is classified as a broken
    /// installation, not a program error.
    #[test]
    fn toolchain_integrity_malformed_std_option_is_environmental_error() {
        let project = TestDir::new("integrity-malformed-project");
        let stdlib = TestDir::new("integrity-malformed-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        stdlib.write("option.rue", MALFORMED_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let error = load_and_acquire(&main, None, Some(&std_root))
            .expect_err("a malformed trusted module is a toolchain-integrity error");
        assert!(
            matches!(
                error,
                SourceLoadError::Toolchain(ToolchainIntegrityError::Malformed { .. })
            ),
            "expected Malformed, got {error:?}"
        );
        assert!(error_display(&error).contains("malformed"), "{error:?}");
    }

    /// Toolchain-integrity arm: a reached fallible intrinsic with NO std root
    /// configured cannot resolve the demand at all.
    #[test]
    fn toolchain_integrity_absent_std_root_is_environmental_error() {
        let project = TestDir::new("integrity-nostd-project");
        let main = project.write("main.rue", FALLIBLE_ROOT);

        let error =
            load_and_acquire(&main, None, None).expect_err("no std root cannot satisfy the demand");
        assert!(
            matches!(
                error,
                SourceLoadError::Toolchain(ToolchainIntegrityError::StdRootUnavailable { .. })
            ),
            "expected StdRootUnavailable, got {error:?}"
        );
    }

    /// Render a `SourceLoadError`'s environmental/hermetic message for assertions.
    fn error_display(error: &SourceLoadError) -> String {
        match error {
            SourceLoadError::Toolchain(error) => error.to_string(),
            SourceLoadError::HermeticDenial(error) => error.to_string(),
            SourceLoadError::Message(message) => message.clone(),
            SourceLoadError::Compiler { errors, .. } => errors.to_string(),
        }
    }

    /// Load a `SourceManifest` from a written manifest file whose entries are the
    /// given absolute paths (one per line).
    fn manifest_allowing(project: &TestDir, entries: &[&Path]) -> SourceManifest {
        let body = entries
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let manifest_path = project.write("sources.manifest", &format!("{body}\n"));
        SourceManifest::load(manifest_path.to_str().unwrap()).unwrap()
    }

    /// Manifest authority: a std path the manifest does not declare is DENIED
    /// before any filesystem probe. The option.rue on disk is deliberately
    /// malformed; if acquisition read it the error would be `Malformed`, so a
    /// hermetic-denial result proves the read never happened.
    #[test]
    fn b4_manifest_denied_std_module_is_hermetic_denial_without_probe() {
        let project = TestDir::new("b4-denied-project");
        let stdlib = TestDir::new("b4-denied-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        stdlib.write("option.rue", MALFORMED_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();
        // The manifest authorizes only the root source, not the std module.
        let root_canonical = fs::canonicalize(&main).unwrap();
        let manifest = manifest_allowing(&project, &[&root_canonical]);

        let error = load_and_acquire(&main, Some(manifest), Some(&std_root))
            .expect_err("a manifest-denied std module cannot be acquired");
        assert!(
            matches!(error, SourceLoadError::HermeticDenial(_)),
            "expected a hermetic denial (never probed), got {error:?}"
        );
        assert!(
            error_display(&error).contains("hermetic build configuration"),
            "{error:?}"
        );
    }

    /// ADR-0078: under a source manifest an undeclared vendored
    /// `{root}/std/_std.rue` is SKIPPED, and `std` resolves to the declared
    /// toolchain facade.
    ///
    /// Hermetic denial is lexical and takes no probe, so the compiler cannot
    /// distinguish "absent" from "present but undeclared". Treating the denial
    /// as conclusive would fail every hermetic build whose program does not
    /// vendor std — the vendored candidate is probed first and denied even
    /// when nothing is there. The manifest is therefore the authority on which
    /// std is in the build: declare the vendored copy and it wins (that arm is
    /// covered by the CLI deps cases), omit it and the declared toolchain std
    /// resolves.
    #[test]
    fn undeclared_vendored_std_is_skipped_for_the_declared_toolchain_std() {
        let project = TestDir::new("vendored-skipped-project");
        let stdlib = TestDir::new("vendored-skipped-std");
        let main = project.write(
            "main.rue",
            "const s = @import(\"std\"); fn main() -> i32 { 0 }",
        );
        project.write("std/_std.rue", "pub fn vendored() -> i32 { 21 }");
        let env_std = stdlib.write("_std.rue", "pub fn env_std() -> i32 { 1 }");
        let std_root = fs::canonicalize(&stdlib.path).unwrap();
        let root_canonical = fs::canonicalize(&main).unwrap();
        let env_canonical = fs::canonicalize(&env_std).unwrap();
        let manifest = manifest_allowing(&project, &[&root_canonical, &env_canonical]);

        let result = load_and_acquire(&main, Some(manifest), Some(&std_root))
            .expect("the undeclared vendored candidate is skipped, not conclusive");
        assert!(
            result
                .read_manifest
                .iter()
                .any(|entry| entry.module().as_str().starts_with("\0rue-std/")),
            "std must resolve to the declared toolchain facade"
        );
        assert!(
            !result
                .read_manifest
                .iter()
                .any(|entry| entry.module().as_str() == "std/_std.rue"),
            "the undeclared vendored candidate must never be read"
        );
    }

    /// CLI classification: a hermetic denial is presented as a distinct
    /// build-configuration failure, NOT as toolchain corruption. The two error
    /// classes carry disjoint labels ("Hermetic build configuration error" vs
    /// "Toolchain integrity error") and disjoint remedies, so the CLI never tells
    /// a user their toolchain is broken when the sandbox policy denied the read.
    #[test]
    fn b4_hermetic_denial_label_is_distinct_from_toolchain_corruption() {
        let project = TestDir::new("b4-label-project");
        let stdlib = TestDir::new("b4-label-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        // Malformed on disk: if acquisition read it the class would be Toolchain
        // corruption. The manifest denies it, so the class must be Hermetic.
        stdlib.write("option.rue", MALFORMED_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();
        let root_canonical = fs::canonicalize(&main).unwrap();
        let manifest = manifest_allowing(&project, &[&root_canonical]);

        let denied = load_and_acquire(&main, Some(manifest), Some(&std_root))
            .expect_err("a manifest-denied std module is a hermetic denial");

        // Distinct outer classification.
        let SourceLoadError::HermeticDenial(hermetic) = &denied else {
            panic!(
                "a manifest denial must classify as SourceLoadError::HermeticDenial: {denied:?}"
            );
        };
        let hermetic_message = hermetic.to_string();
        assert!(
            hermetic_message.contains("Hermetic build configuration error"),
            "the hermetic denial must carry its own label: {hermetic_message}",
        );
        assert!(
            hermetic_message.contains("adjust the source manifest"),
            "the hermetic denial must point at the manifest remedy: {hermetic_message}",
        );
        assert!(
            !hermetic_message.contains("Toolchain integrity error"),
            "the hermetic denial must NOT be presented as toolchain corruption: {hermetic_message}",
        );
        assert!(
            !hermetic_message.contains("broken toolchain installation"),
            "the hermetic denial must NOT claim a broken installation: {hermetic_message}",
        );

        // The genuine toolchain-corruption class keeps the opposite label, so the
        // two are provably disjoint at the CLI boundary.
        let corruption = ToolchainIntegrityError::Missing {
            logical_path: rue_compiler::OPTION_MODULE_LOGICAL_PATH.to_owned(),
            path: std_root.join("option.rue"),
        }
        .to_string();
        assert!(
            corruption.contains("Toolchain integrity error")
                && !corruption.contains("Hermetic build configuration error"),
            "toolchain corruption must keep its distinct label: {corruption}",
        );
    }

    /// Manifest authority: when the manifest declares AND allows the canonical
    /// std module, acquisition proceeds exactly as an unrestricted read would.
    #[test]
    fn b4_manifest_allowed_std_module_acquires() {
        let project = TestDir::new("b4-allowed-project");
        let stdlib = TestDir::new("b4-allowed-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        let option = stdlib.write("option.rue", VALID_OPTION);
        let std_root = fs::canonicalize(&stdlib.path).unwrap();
        let root_canonical = fs::canonicalize(&main).unwrap();
        let option_canonical = fs::canonicalize(&option).unwrap();
        let manifest = manifest_allowing(&project, &[&root_canonical, &option_canonical]);

        let result = load_and_acquire(&main, Some(manifest), Some(&std_root))
            .expect("a manifest-allowed std module is acquired");
        assert!(contains_trusted_option(&result.source_snapshot));
    }

    /// Canonical containment: a std module that is a symlink escaping the std
    /// root is rejected as a hermetic denial. Trusted classification must never
    /// be granted to a file outside the toolchain, even one reachable through a
    /// symlink named `option.rue` under the std root.
    #[test]
    #[cfg(unix)]
    fn b4_symlink_escape_from_std_root_is_hermetic_denial() {
        let project = TestDir::new("b4-escape-project");
        let stdlib = TestDir::new("b4-escape-std");
        let main = project.write("main.rue", FALLIBLE_ROOT);
        // A perfectly valid Option module, but OUTSIDE the std root.
        let outside = project.write("escape_option.rue", VALID_OPTION);
        let outside_canonical = fs::canonicalize(&outside).unwrap();
        // `std_root/option.rue` is a symlink pointing at the outside file.
        let link = stdlib.path.join("option.rue");
        std::os::unix::fs::symlink(&outside_canonical, &link).unwrap();
        let std_root = fs::canonicalize(&stdlib.path).unwrap();

        let error = load_and_acquire(&main, None, Some(&std_root))
            .expect_err("a symlink escaping the std root cannot be trusted");
        assert!(
            matches!(error, SourceLoadError::HermeticDenial(_)),
            "expected a hermetic denial for the escape, got {error:?}"
        );
        assert!(
            error_display(&error).contains("outside the standard-library root"),
            "{error:?}"
        );
    }

    // ---- RUE-1112 leaf-only re-close acquisition proofs ------------------
    //
    // `@read_line` demands the trusted std `Option` and `StrBuf`. The real
    // `StrBuf` module imports `option.rue`, `arraybuf.rue`, and `rawbuf.rue`, and
    // `arraybuf.rue` imports `option.rue` and `rawbuf.rue`, so satisfying the
    // demand reads four trusted leaves in total. These tests build that exact
    // import shape with `TestDir` and pin that the re-close discovers it by
    // rooting ONLY in the newly appended leaves — never re-rooting the project's
    // pre-existing imports — and that a broken transitive leaf is attributed to
    // itself.

    /// A root whose reached body hits `@read_line`, demanding Option + StrBuf.
    const READ_LINE_ROOT: &str = "fn main() -> i32 { let _ = @read_line(); 0 }";
    const VALID_RAWBUF: &str = "pub struct RawBuf { cap: u64, }";
    const VALID_ARRAYBUF: &str = "const option = @import(\"option.rue\");\n\
         const rawbuf = @import(\"rawbuf.rue\");\n\
         pub struct ArrayBuf { len: u64, }";
    const VALID_STRBUF: &str = "const option = @import(\"option.rue\");\n\
         const arraybuf = @import(\"arraybuf.rue\");\n\
         const rawbuf = @import(\"rawbuf.rue\");\n\
         pub struct StrBuf { len: u64, }";

    /// Write the trusted std tree with the real `@read_line` topology: `strbuf`
    /// imports `option`/`arraybuf`/`rawbuf`, `arraybuf` imports `option`/`rawbuf`.
    /// Returns the canonicalized std root.
    fn write_read_line_std(stdlib: &TestDir) -> PathBuf {
        stdlib.write("option.rue", VALID_OPTION);
        stdlib.write("rawbuf.rue", VALID_RAWBUF);
        stdlib.write("arraybuf.rue", VALID_ARRAYBUF);
        stdlib.write("strbuf.rue", VALID_STRBUF);
        fs::canonicalize(&stdlib.path).unwrap()
    }

    /// Number of trusted std leaves in an accepted-read manifest.
    fn trusted_read_count(manifest: &AcceptedReadManifest) -> usize {
        manifest
            .iter()
            .filter(|entry| entry.module().is_trusted_standard_library())
            .count()
    }

    /// Number of ordinary (non-trusted) reads in an accepted-read manifest.
    fn project_read_count(manifest: &AcceptedReadManifest) -> usize {
        manifest
            .iter()
            .filter(|entry| !entry.module().is_trusted_standard_library())
            .count()
    }

    /// The re-close that acquires `@read_line`'s trusted leaves roots its demand
    /// frontier ONLY in the newly appended leaves and the transitive helpers they
    /// import — never in the project's pre-existing imports. Proven by driving two
    /// projects whose predecessor import topologies differ sharply (one with no
    /// project imports, one with several project modules importing each other) and
    /// asserting the re-close's frontier-root count is IDENTICAL, while the
    /// initial close's frontier-root count differs (it is topology-sensitive). The
    /// four trusted leaves are each read exactly once and no predecessor import is
    /// re-read.
    /// The four predecessor-sensitive work counters instrumented at each import
    /// discovery layer: host frontier roots, plan-group construction, close-time
    /// `ResolveImport` projection dispatch, and canonical graph reduction.
    #[derive(Clone, Copy)]
    struct ImportWorkProfile {
        frontier_roots: u64,
        plan_groups: u64,
        resolve_dispatched: u64,
        records_reduced: u64,
        full_leaves_published: u64,
        overlay_leaves_published: u64,
        ledger_entries_cloned: u64,
        source_entries_compared: u64,
        read_entries_compared: u64,
        snapshot_sources_rebuilt: u64,
        snapshot_sources_appended: u64,
        manifest_entries_rebuilt: u64,
        manifest_entries_appended: u64,
        parse_sources_materialized: u64,
        parse_key_entries_compared: u64,
        parse_modules_dispatched: u64,
        parse_invalidation_entries_compared: u64,
        frontend_invalidations: u64,
    }

    fn import_work_profile(result: &ImportDiscoveryResult) -> ImportWorkProfile {
        ImportWorkProfile {
            frontier_roots: import_frontier_roots_requested(&result.session),
            plan_groups: import_plan_groups_constructed(&result.session),
            resolve_dispatched: exact_import_groups_dispatched(&result.session),
            records_reduced: import_close_records_reduced(&result.session),
            full_leaves_published: import_view_full_leaves_published(&result.session),
            overlay_leaves_published: import_view_overlay_leaves_published(&result.session),
            ledger_entries_cloned: import_view_ledger_entries_cloned(&result.session),
            source_entries_compared: import_view_source_entries_compared(&result.session),
            read_entries_compared: import_view_read_entries_compared(&result.session),
            snapshot_sources_rebuilt: result.assembler.snapshot_sources_rebuilt(),
            snapshot_sources_appended: result.assembler.snapshot_sources_appended(),
            manifest_entries_rebuilt: result.assembler.manifest_entries_rebuilt(),
            manifest_entries_appended: result.assembler.manifest_entries_appended(),
            parse_sources_materialized: parse_sources_materialized(&result.session),
            parse_key_entries_compared: parse_key_entries_compared(&result.session),
            parse_modules_dispatched: parse_modules_dispatched(&result.session),
            parse_invalidation_entries_compared: parse_invalidation_entries_compared(
                &result.session,
            ),
            frontend_invalidations: frontend_query_invalidations(&result.session),
        }
    }

    /// The result of driving one project through acquisition, with the per-layer
    /// re-close work delta and the graph record-sharing witnesses before and after
    /// acquisition.
    struct Acquired {
        result: ImportDiscoveryResult,
        initial: ImportWorkProfile,
        reclose: ImportWorkProfile,
        /// Structural-sharing witnesses `[graph_records, plan_groups,
        /// resolution_modules]`, each `(predecessor_segment_address, delta_len)`,
        /// for the committed discovery before acquisition (the flat initial close)
        /// and after (the shared successor).
        sharing_before: Option<[(usize, usize); 3]>,
        sharing_after: Option<[(usize, usize); 3]>,
    }

    /// The work each layer performed during acquisition (the re-close delta),
    /// isolated from the initial close by differencing, plus the structural-sharing
    /// witnesses.
    fn acquisition_work(root: &Path, std_root: &Path) -> Acquired {
        let mut result =
            discover_and_load_imports(root.to_str().unwrap(), None, Some(std_root)).unwrap();
        let initial = import_work_profile(&result);
        let sharing_before = committed_successor_sharing(&result.session);
        acquire_reached_toolchain_modules(&mut result, &CompileOptions::default())
            .expect("the project acquires its trusted leaves");
        let after = import_work_profile(&result);
        let sharing_after = committed_successor_sharing(&result.session);
        let reclose = ImportWorkProfile {
            frontier_roots: after.frontier_roots - initial.frontier_roots,
            plan_groups: after.plan_groups - initial.plan_groups,
            resolve_dispatched: after.resolve_dispatched - initial.resolve_dispatched,
            records_reduced: after.records_reduced - initial.records_reduced,
            full_leaves_published: after.full_leaves_published - initial.full_leaves_published,
            overlay_leaves_published: after.overlay_leaves_published
                - initial.overlay_leaves_published,
            ledger_entries_cloned: after.ledger_entries_cloned - initial.ledger_entries_cloned,
            source_entries_compared: after.source_entries_compared
                - initial.source_entries_compared,
            read_entries_compared: after.read_entries_compared - initial.read_entries_compared,
            snapshot_sources_rebuilt: after.snapshot_sources_rebuilt
                - initial.snapshot_sources_rebuilt,
            snapshot_sources_appended: after.snapshot_sources_appended
                - initial.snapshot_sources_appended,
            manifest_entries_rebuilt: after.manifest_entries_rebuilt
                - initial.manifest_entries_rebuilt,
            manifest_entries_appended: after.manifest_entries_appended
                - initial.manifest_entries_appended,
            parse_sources_materialized: after.parse_sources_materialized
                - initial.parse_sources_materialized,
            parse_key_entries_compared: after.parse_key_entries_compared
                - initial.parse_key_entries_compared,
            parse_modules_dispatched: after.parse_modules_dispatched
                - initial.parse_modules_dispatched,
            parse_invalidation_entries_compared: after.parse_invalidation_entries_compared
                - initial.parse_invalidation_entries_compared,
            frontend_invalidations: after.frontend_invalidations - initial.frontend_invalidations,
        };
        Acquired {
            result,
            initial,
            reclose,
            sharing_before,
            sharing_after,
        }
    }

    #[test]
    fn read_line_reclose_roots_only_new_leaves_independent_of_project_topology() {
        // Small project: the root has no project imports at all.
        let small_project = TestDir::new("leaf-small-project");
        let small_stdlib = TestDir::new("leaf-small-std");
        let small_main = small_project.write("main.rue", READ_LINE_ROOT);
        let small_std_root = write_read_line_std(&small_stdlib);
        let small_acq = acquisition_work(&small_main, &small_std_root);
        let (small, small_initial, small_reclose) =
            (&small_acq.result, small_acq.initial, small_acq.reclose);

        // Big project: several project modules importing each other, so the
        // initial close does much more work at every layer.
        let big_project = TestDir::new("leaf-big-project");
        let big_stdlib = TestDir::new("leaf-big-std");
        big_project.write("c.rue", "pub fn cv() -> i32 { 3 }");
        big_project.write(
            "b.rue",
            "const c = @import(\"c.rue\"); pub fn bv() -> i32 { 2 }",
        );
        big_project.write(
            "a.rue",
            "const b = @import(\"b.rue\");\nconst c = @import(\"c.rue\");\npub fn av() -> i32 { 1 }",
        );
        let big_main = big_project.write(
            "main.rue",
            "const a = @import(\"a.rue\");\nconst b = @import(\"b.rue\");\n\
             fn main() -> i32 { let _ = @read_line(); 0 }",
        );
        let big_std_root = write_read_line_std(&big_stdlib);
        let big_acq = acquisition_work(&big_main, &big_std_root);
        let (big, big_initial, big_reclose) = (&big_acq.result, big_acq.initial, big_acq.reclose);

        // Both acquire the demanded Option and StrBuf.
        assert!(contains_trusted_option(&small.source_snapshot));
        assert!(contains_trusted_option(&big.source_snapshot));

        // The INITIAL close is topology-sensitive at every layer: the big project
        // roots, constructs, projects, and reduces strictly more than the
        // import-free small project. (The small project has no project imports, so
        // its initial frontier roots and dispatch are zero.)
        assert_eq!(small_initial.frontier_roots, 0);
        assert!(
            big_initial.frontier_roots > small_initial.frontier_roots,
            "initial frontier roots must scale with the predecessor topology: big={} small={}",
            big_initial.frontier_roots,
            small_initial.frontier_roots
        );
        assert!(
            big_initial.plan_groups > small_initial.plan_groups,
            "initial plan-group construction must scale with the predecessor topology: big={} small={}",
            big_initial.plan_groups,
            small_initial.plan_groups
        );
        assert!(
            big_initial.resolve_dispatched > small_initial.resolve_dispatched,
            "initial ResolveImport dispatch must scale with the predecessor topology: big={} small={}",
            big_initial.resolve_dispatched,
            small_initial.resolve_dispatched
        );
        assert!(
            big_initial.records_reduced > small_initial.records_reduced,
            "initial graph reduction must scale with the predecessor topology: big={} small={}",
            big_initial.records_reduced,
            small_initial.records_reduced
        );

        // The RE-CLOSE is topology-INDEPENDENT at EVERY layer: it roots,
        // constructs, projects, and reduces only the newly appended leaves and the
        // transitive helpers discovered from them, so every counter's acquisition
        // delta is identical across the two sharply different predecessor graphs.
        assert_eq!(
            small_reclose.frontier_roots, big_reclose.frontier_roots,
            "re-close frontier roots must not depend on the predecessor topology: small={} big={}",
            small_reclose.frontier_roots, big_reclose.frontier_roots
        );
        assert_eq!(
            small_reclose.plan_groups, big_reclose.plan_groups,
            "re-close plan-group construction must not depend on the predecessor topology: small={} big={}",
            small_reclose.plan_groups, big_reclose.plan_groups
        );
        assert_eq!(
            small_reclose.resolve_dispatched, big_reclose.resolve_dispatched,
            "re-close ResolveImport dispatch must not depend on the predecessor topology: small={} big={}",
            small_reclose.resolve_dispatched, big_reclose.resolve_dispatched
        );
        assert_eq!(
            small_reclose.records_reduced, big_reclose.records_reduced,
            "re-close graph reduction must not depend on the predecessor topology: small={} big={}",
            small_reclose.records_reduced, big_reclose.records_reduced
        );
        // PUBLICATION is O(delta) too: acquisition publishes ONLY through the
        // sparse successor overlay (zero complete-path leaves), and the overlay
        // leaf count — the new leaves' sources/provenance/observations plus the
        // re-stamped aggregate topology — is identical across the two predecessor
        // topologies. Predecessor leaves are structurally inherited, never
        // rehashed, revalidated, or republished.
        assert_eq!(
            small_reclose.full_leaves_published, 0,
            "acquisition must never publish through the complete path"
        );
        assert_eq!(
            big_reclose.full_leaves_published, 0,
            "acquisition must never publish through the complete path"
        );
        assert_eq!(
            small_reclose.overlay_leaves_published, big_reclose.overlay_leaves_published,
            "re-close overlay publication must not depend on the predecessor topology: small={} big={}",
            small_reclose.overlay_leaves_published, big_reclose.overlay_leaves_published
        );
        // The persistent ledger deep-copies only per-step recorded deltas on
        // clone (frozen predecessor segments are shared by Arc), so the
        // acquisition's total deep-copied observation count is identical across
        // the two predecessor topologies.
        assert_eq!(
            small_reclose.ledger_entries_cloned, big_reclose.ledger_entries_cloned,
            "re-close ledger cloning must not depend on the predecessor topology: small={} big={}",
            small_reclose.ledger_entries_cloned, big_reclose.ledger_entries_cloned
        );
        // The assembler seam is O(delta): once a lineage's first snapshot exists,
        // every successor snapshot is an extension appending only the newly read
        // sources — never a rebuild of all prior modules — so acquisition
        // materializes ZERO full-path sources and a topology-independent appended
        // count. The publication's source additions likewise flow from structural
        // authority (successor segments sharing the parent view by Arc identity),
        // so its fallback comparison path never touches a predecessor entry.
        assert_eq!(
            small_reclose.snapshot_sources_rebuilt, 0,
            "acquisition must never rebuild a full snapshot"
        );
        assert_eq!(
            big_reclose.snapshot_sources_rebuilt, 0,
            "acquisition must never rebuild a full snapshot"
        );
        assert_eq!(
            small_reclose.snapshot_sources_appended, big_reclose.snapshot_sources_appended,
            "snapshot extension must not depend on the predecessor topology: small={} big={}",
            small_reclose.snapshot_sources_appended, big_reclose.snapshot_sources_appended
        );
        assert!(small_reclose.snapshot_sources_appended > 0);
        assert_eq!(
            small_reclose.source_entries_compared, 0,
            "structural authority must publish source additions without comparing predecessor entries"
        );
        assert_eq!(
            big_reclose.source_entries_compared, 0,
            "structural authority must publish source additions without comparing predecessor entries"
        );
        // The accepted-read provenance manifest is O(delta) through the same
        // mechanism: acquisition extends the assembler's cached manifest with
        // only the newly accepted entries (never a full rebuild), and the
        // publication takes the shared segment identity as its authority for the
        // parent's provenance, so the fallback comparison path never touches a
        // predecessor read entry.
        assert_eq!(
            small_reclose.manifest_entries_rebuilt, 0,
            "acquisition must never rebuild a full accepted-read manifest"
        );
        assert_eq!(
            big_reclose.manifest_entries_rebuilt, 0,
            "acquisition must never rebuild a full accepted-read manifest"
        );
        assert_eq!(
            small_reclose.manifest_entries_appended, big_reclose.manifest_entries_appended,
            "manifest extension must not depend on the predecessor topology: small={} big={}",
            small_reclose.manifest_entries_appended, big_reclose.manifest_entries_appended
        );
        assert!(small_reclose.manifest_entries_appended > 0);
        assert_eq!(
            small_reclose.read_entries_compared, 0,
            "structural authority must publish provenance additions without comparing predecessor entries"
        );
        assert_eq!(
            big_reclose.read_entries_compared, 0,
            "structural authority must publish provenance additions without comparing predecessor entries"
        );
        // The PARSE projection is O(delta) on the successor path too: each
        // re-close stage extends the retained predecessor parsed program and
        // presentation order (never materializing a whole-program view), keys
        // on the published lineage identity plus the appended segment (never
        // re-hashing predecessor content), dispatches only the appended
        // modules' parse queries, and classifies invalidation over the delta
        // alone.
        assert_eq!(
            small_reclose.parse_sources_materialized, 0,
            "a successor stage must never materialize a whole-program parse projection"
        );
        assert_eq!(
            big_reclose.parse_sources_materialized, 0,
            "a successor stage must never materialize a whole-program parse projection"
        );
        assert_eq!(
            small_reclose.parse_key_entries_compared, big_reclose.parse_key_entries_compared,
            "successor parse keys must not depend on the predecessor topology: small={} big={}",
            small_reclose.parse_key_entries_compared, big_reclose.parse_key_entries_compared
        );
        // This fixture has two exact parse stages: Option+StrBuf, followed by
        // ArrayBuf+RawBuf. Each stage keys only its own two-module segment, for
        // 2+2=4 key entries. A cumulative lineage key would charge 2+4=6 here
        // while dispatching the same four modules.
        assert_eq!(
            small_reclose.parse_key_entries_compared, 4,
            "successor parse key work must be exact per-stage segment, not cumulative lineage"
        );
        assert_eq!(
            small_reclose.parse_modules_dispatched, big_reclose.parse_modules_dispatched,
            "successor parse dispatch must not depend on the predecessor topology: small={} big={}",
            small_reclose.parse_modules_dispatched, big_reclose.parse_modules_dispatched
        );
        assert_eq!(
            small_reclose.parse_modules_dispatched, 4,
            "the two exact stages dispatch the four newly acquired trusted modules once"
        );
        assert_eq!(
            small_reclose.parse_invalidation_entries_compared,
            big_reclose.parse_invalidation_entries_compared,
            "successor parse invalidation must not depend on the predecessor topology: small={} big={}",
            small_reclose.parse_invalidation_entries_compared,
            big_reclose.parse_invalidation_entries_compared
        );
        assert!(small_reclose.parse_invalidation_entries_compared > 0);
        // The successor observes its predecessor parse through the
        // exact-terminal adoption capability — by node incarnation, never by
        // key — proven mechanically by the rue-query frozen-key regression
        // (`adoption_never_hashes_or_compares_the_predecessor_key`).
        // Additive successor adoption keeps the predecessor's immutable source
        // leaf live, so acquisition invalidates NO retained frontend terminal —
        // the predecessor's retained downstream is never walked.
        assert_eq!(
            small_reclose.frontend_invalidations, 0,
            "additive successor adoption must not invalidate retained frontend terminals"
        );
        assert_eq!(
            big_reclose.frontend_invalidations, 0,
            "additive successor adoption must not invalidate retained frontend terminals"
        );
        // The re-close genuinely did work (it discovered and reduced the new
        // leaves' edges); the flat deltas are not a vacuous zero.
        assert!(small_reclose.frontier_roots > 0);
        assert!(small_reclose.records_reduced > 0);
        assert!(small_reclose.overlay_leaves_published > 0);

        // Each of the four trusted leaves is read exactly once, and the project
        // reads are exactly the project's own modules — nothing re-read: the small
        // project has only its root, the big project its root plus a/b/c.
        assert_eq!(trusted_read_count(&small.read_manifest), 4);
        assert_eq!(trusted_read_count(&big.read_manifest), 4);
        assert_eq!(project_read_count(&small.read_manifest), 1);
        assert_eq!(project_read_count(&big.read_manifest), 4);

        // STRUCTURAL LINEAGE: the committed successor retains the predecessor
        // close's root segment `Arc` as a stable witness for all three additive
        // artifacts, even if bounded size-tiered storage compacted. Each exact
        // delta is non-vacuous and independent of predecessor topology.
        let artifacts = ["graph records", "plan groups", "resolution modules"];
        for acq in [&small_acq, &big_acq] {
            let before = acq
                .sharing_before
                .expect("initial close has committed artifacts");
            let after = acq
                .sharing_after
                .expect("successor close has committed artifacts");
            for (index, name) in artifacts.iter().enumerate() {
                let (before_ptr, _) = before[index];
                let (after_ptr, after_delta) = after[index];
                assert_eq!(
                    before_ptr, after_ptr,
                    "the successor must retain the predecessor {name} lineage witness"
                );
                assert!(after_delta > 0, "the successor delta must carry new {name}");
            }
        }
        let small_before = small_acq.sharing_before.unwrap();
        let big_before = big_acq.sharing_before.unwrap();
        let small_after = small_acq.sharing_after.unwrap();
        let big_after = big_acq.sharing_after.unwrap();
        for (index, name) in artifacts.iter().enumerate() {
            // The initial close's own delta IS the project's discovery wave: one
            // publication carries every module the closure reached, so the
            // closing stage of a fresh close extends the first round's flat plan
            // by whatever the wave found (ADR-0075). The small project's root
            // imports nothing and closes on its first round, so its delta is
            // empty; the big project's wave carries `a`, `b`, and `c`. That the
            // predecessor delta tracks project topology this way is what makes
            // the equal successor deltas below a real independence proof rather
            // than a comparison of two zeroes.
            assert_eq!(
                small_before[index].1, 0,
                "a close with nothing to discover has no {name} delta"
            );
            assert_eq!(
                small_after[index].1, big_after[index].1,
                "the successor {name} delta size must be independent of the predecessor topology"
            );
        }
        assert!(
            big_before.iter().any(|(_, delta)| *delta > 0),
            "the big project's initial close carries its own discovery wave's delta"
        );
    }

    /// A manifest that denies a newly introduced transitive helper (`arraybuf.rue`,
    /// pulled in by `strbuf.rue`) surfaces as a typed hermetic denial attributed to
    /// that helper — not as toolchain corruption, and not attributed to the first
    /// parked demand (`option`).
    #[test]
    fn read_line_transitive_manifest_denial_is_hermetic_and_attributed_to_helper() {
        let project = TestDir::new("transitive-denied-project");
        let stdlib = TestDir::new("transitive-denied-std");
        let main = project.write("main.rue", READ_LINE_ROOT);
        let std_root = write_read_line_std(&stdlib);
        // Allow the root and every trusted leaf EXCEPT arraybuf.rue.
        let root_canonical = fs::canonicalize(&main).unwrap();
        let option_canonical = fs::canonicalize(std_root.join("option.rue")).unwrap();
        let strbuf_canonical = fs::canonicalize(std_root.join("strbuf.rue")).unwrap();
        let rawbuf_canonical = fs::canonicalize(std_root.join("rawbuf.rue")).unwrap();
        let manifest = manifest_allowing(
            &project,
            &[
                &root_canonical,
                &option_canonical,
                &strbuf_canonical,
                &rawbuf_canonical,
            ],
        );

        let error = load_and_acquire(&main, Some(manifest), Some(&std_root))
            .expect_err("a manifest-denied transitive helper cannot be acquired");
        let SourceLoadError::HermeticDenial(denial) = &error else {
            panic!("a transitive manifest denial must be a hermetic denial: {error:?}");
        };
        let message = denial.to_string();
        assert!(
            message.contains("arraybuf.rue"),
            "the denial must name the denied helper: {message}"
        );
        assert!(
            !message.contains("option.rue"),
            "the denial must not be attributed to the first demand: {message}"
        );
    }

    /// A malformed transitive helper (`arraybuf.rue` with garbage, imported by
    /// `strbuf.rue`) is classified as a malformed toolchain module attributed to
    /// `arraybuf.rue` — not to `option.rue` or `strbuf.rue`.
    #[test]
    fn read_line_malformed_transitive_helper_is_attributed_to_that_helper() {
        let project = TestDir::new("transitive-malformed-project");
        let stdlib = TestDir::new("transitive-malformed-std");
        let main = project.write("main.rue", READ_LINE_ROOT);
        let std_root = write_read_line_std(&stdlib);
        // Corrupt only the transitive helper; option and strbuf remain valid.
        stdlib.write("arraybuf.rue", MALFORMED_OPTION);

        let error = load_and_acquire(&main, None, Some(&std_root))
            .expect_err("a malformed transitive helper is a toolchain-integrity error");
        let SourceLoadError::Toolchain(ToolchainIntegrityError::Malformed {
            logical_path,
            path,
            ..
        }) = &error
        else {
            panic!("a malformed transitive helper must classify as Malformed: {error:?}");
        };
        assert!(
            logical_path.contains("arraybuf.rue"),
            "the malformed leaf must be named by its logical path: {logical_path}"
        );
        assert!(
            path.to_string_lossy().contains("arraybuf.rue"),
            "the malformed leaf must be named by its filesystem path: {}",
            path.display()
        );
        assert!(
            !logical_path.contains("option.rue") && !logical_path.contains("strbuf.rue"),
            "the malformed leaf must not be attributed to option or strbuf: {logical_path}"
        );
    }

    /// A missing transitive helper (`arraybuf.rue` absent though `strbuf.rue`
    /// imports it) is a toolchain-integrity error identifying that leaf.
    #[test]
    fn read_line_missing_transitive_helper_identifies_that_helper() {
        let project = TestDir::new("transitive-missing-project");
        let stdlib = TestDir::new("transitive-missing-std");
        let main = project.write("main.rue", READ_LINE_ROOT);
        // Write the tree, then remove the transitive helper strbuf imports.
        let std_root = write_read_line_std(&stdlib);
        fs::remove_file(std_root.join("arraybuf.rue")).unwrap();

        let error = load_and_acquire(&main, None, Some(&std_root))
            .expect_err("a missing transitive helper is a toolchain-integrity error");
        let SourceLoadError::Toolchain(ToolchainIntegrityError::Missing { logical_path, path }) =
            &error
        else {
            panic!("a missing transitive helper must classify as Missing: {error:?}");
        };
        assert!(
            logical_path.contains("arraybuf.rue")
                || path.to_string_lossy().contains("arraybuf.rue"),
            "the missing leaf must be named: logical={logical_path} path={}",
            path.display()
        );
        assert!(
            !logical_path.contains("option.rue"),
            "the missing leaf must not be attributed to the first demand: {logical_path}"
        );
    }
}
