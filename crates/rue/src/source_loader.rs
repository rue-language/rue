use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use rue_compiler::unstable::frontend_query_invalidations;
use rue_compiler::unstable::{
    DiscoverySourceAssembler, ImportDemandFrontier, ImportDemandMode, ImportInputRevision,
    SemanticParkOutcome, TrustedSuccessorDelta, begin_import_input_request,
    close_import_discovery_successor, closed_discovery_continuation, discovery_attempt,
    import_demand_frontier_for_roots, import_observation_ledger, plan_delta_roots,
    publish_import_observation_batch, publish_trusted_toolchain_successor,
    semantic_or_toolchain_park, stage_import_discovery_successor,
};
#[cfg(test)]
use rue_compiler::unstable::{
    committed_successor_sharing, exact_import_groups_dispatched, import_close_records_reduced,
    import_frontier_roots_requested, import_plan_groups_constructed,
    import_view_full_leaves_published, import_view_ledger_entries_cloned,
    import_view_overlay_leaves_published, import_view_read_entries_compared,
    import_view_source_entries_compared, parse_invalidation_entries_compared,
    parse_key_entries_compared, parse_modules_dispatched, parse_sources_materialized,
};
use rue_compiler::{
    AcceptedImportSource, AcceptedReadManifest, CompileErrors, CompileOptions, CompilerSession,
    DependencyEnvelope, FileId, FileMetadataFingerprint, ImportDiscoveryContext,
    ImportDiscoveryRequest, ImportDiscoveryStatus, ImportDiscoveryView, ImportObservation,
    ImportObservationStatus, PhysicalFileIdentity, SourceMetadata, SourceSnapshot,
    TrustedToolchainModuleDemand,
};

#[derive(Debug)]
pub(crate) struct SourceManifest {
    path: PathBuf,
    allowed: std::collections::HashSet<PathBuf>,
    declared_paths: std::collections::HashSet<PathBuf>,
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

        let mut allowed = std::collections::HashSet::new();
        let mut declared_paths = std::collections::HashSet::new();
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

fn execute_import_request(
    request: ImportDiscoveryRequest,
    source_manifest: Option<&SourceManifest>,
) -> ImportObservation {
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
pub(crate) enum SourceLoadError {
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
    let _span = tracing::info_span!("source_loading").entered();
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
/// external-input observation generation (the initial close) and roots discovery
/// in the whole plan. `Some(revision)` is a SAME-GENERATION re-close on a
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
struct ReClose<'a> {
    delta: &'a TrustedSuccessorDelta,
}

fn drive_import_discovery_to_close(
    assembler: &mut DiscoverySourceAssembler,
    staging: &mut CompilerSession,
    context: &ImportDiscoveryContext,
    source_manifest: Option<&SourceManifest>,
    continuation: Option<ImportInputRevision>,
    reclose: Option<ReClose<'_>>,
) -> Result<ClosedDiscovery, SourceLoadError> {
    // Exactly one import-input request is opened per invocation, here, before
    // the frontier loop below. Rounds inside that loop publish overlay
    // successors which inherit this request's compatibility token, so the whole
    // discovery of one program observes one filesystem regime.
    //
    // That single-request shape is load-bearing for soundness, not just tidy.
    // Since RUE-1137 a successor request under an unchanged regime reuses its
    // predecessor's retained terminals, which asserts that inputs it did not
    // re-observe are unchanged. Nothing verifies that assertion yet — the Tier B
    // re-observation sweep required by ADR-0063 §2.1 is unimplemented.
    //
    // So: do not add a second request per process, and do not make this driver
    // long-lived or filesystem-watching, until RUE-1148 lands. A `--watch` mode
    // or LSP host belongs behind that issue, not in front of it.
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
                None => staging.stage_import_discovery(
                    &snapshot,
                    context.clone(),
                    assembler.accepted_read_manifest().shared_slice(),
                    ledger.clone(),
                ),
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
        let roots = match &reclose {
            Some(_) => plan_delta_roots(&plan),
            None => plan.demand_roots(),
        };
        let frontier = {
            let _span = tracing::info_span!("import_frontier").entered();
            import_demand_frontier_for_roots(
                staging,
                input_revision,
                &plan,
                ImportDemandMode::Rooted,
                &roots,
            )
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?
        };
        drop(plan_span);
        if frontier.requests().is_empty() {
            final_plan = plan;
            witness = frontier;
            break;
        }
        let _read_span = tracing::info_span!("import_read").entered();
        let observations = frontier
            .requests()
            .iter()
            .cloned()
            .map(|request| execute_import_request(request, source_manifest))
            .collect::<Vec<_>>();
        // In a trusted-toolchain re-close every frontier request resolves an
        // `@import` edge owned by an appended leaf or a leaf newly discovered from
        // it (e.g. strbuf → arraybuf/rawbuf). These edges are toolchain-internal,
        // so a non-accepted observation there is an environmental failure of THAT
        // transitive leaf, not a program error — attribute it to the exact failing
        // module before the outcome is folded into an opaque close diagnostic.
        if reclose.is_some()
            && let Some(error) = classify_trusted_transitive_failure(context, &observations)
        {
            return Err(error);
        }
        let mut next_ledger = ledger;
        for observation in observations.iter().cloned() {
            next_ledger
                .record(observation)
                .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        }
        // Assemble only the newly read leaves: a successor adds its delta groups'
        // accepted sources, never re-feeding the predecessor plan through the
        // winner map.
        let assembled = match &reclose {
            Some(_) => assembler.add_successor_plan_reads(&plan, &next_ledger),
            None => assembler.add_plan_reads(&plan, &next_ledger),
        };
        let _ = assembled.map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
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
    }

    let _close_span = tracing::info_span!("import_discovery_close").entered();
    let snapshot = assembler
        .snapshot()
        .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    debug_assert_eq!(final_plan.source_revision(), snapshot.source_revision());
    let ledger = import_observation_ledger(staging, input_revision)
        .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    // A trusted-toolchain successor closes over only the modules its opaque delta
    // capability authorizes, merging their topology into the committed
    // predecessor's closed graph; the initial close reduces the whole plan.
    let close_result = match &reclose {
        Some(reclose) => close_import_discovery_successor(staging, reclose.delta),
        None => staging.close_import_discovery(ledger),
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
    let physical_paths: HashMap<_, _> = HashMap::from([(FileId::new(1), root_source.to_string())]);
    let logical_paths: HashMap<_, _> = HashMap::from([(FileId::new(1), "root".to_string())]);
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
    let std_root = std_root.map(normalize_lexical_path);
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
    )?;

    Ok(ImportDiscoveryResult {
        source_snapshot: close.snapshot,
        resolution: SourceResolutionInputs { root_path, context },
        read_manifest: assembler.accepted_read_manifest(),
        revision: close.closed,
        #[cfg(test)]
        input_revision: close.input_revision,
        session: staging,
        assembler,
        std_root,
        source_manifest,
        witness: close.witness,
    })
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
    let _span = tracing::info_span!("toolchain_acquisition").entered();
    for _ in 0..MAX_TOOLCHAIN_ACQUISITION_ROUNDS {
        match semantic_or_toolchain_park(&mut result.session, options) {
            // Analysis satisfied every reached-body demand (or there were none).
            SemanticParkOutcome::Ready(_) => return Ok(()),
            // Deterministic program diagnostics — the source itself, not the
            // toolchain, is at fault, and an erroneous body raises no toolchain
            // park, so there is nothing here to acquire. Reporting stays with
            // the driver's emit/compile surfaces, which run their own preflight
            // checks (e.g. the output-alias guard) ahead of semantic
            // diagnostics; surfacing the errors from this loop would invert
            // that ordering. The semantic attempt is memoized, so the later
            // surface re-reads the cached outcome rather than re-analyzing.
            SemanticParkOutcome::Errors(_) => return Ok(()),
            // A reached body demands trusted std modules absent from the current
            // revision. Satisfy exactly the parked demands, publish one successor,
            // and re-close so semantic can retry on it.
            SemanticParkOutcome::Parked(park) => {
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
pub(crate) enum ToolchainIntegrityError {
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
pub(crate) struct HermeticDenialError {
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

    #[test]
    fn import_chain_adds_one_frontier_round_per_depth() {
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
        assert_eq!(result.input_revision.frontier_round(), 3);
        assert_eq!(result.read_manifest.len(), 4);
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
        assert_eq!(result.input_revision.frontier_round(), 2);
        assert_eq!(result.read_manifest.len(), 3);
        assert!(
            result
                .read_manifest
                .iter()
                .all(|entry| entry.canonical_path() != unrelated.to_string_lossy())
        );
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

        // STRUCTURAL SHARING: the committed successor carries the predecessor
        // close's segment `Arc` by reference for ALL THREE additive artifacts —
        // canonical graph records, plan request groups, and the module-resolution
        // table. Each artifact's shared predecessor-segment address is
        // byte-identical to the predecessor close's flat address (so no predecessor
        // entry was copied, re-sorted, or reallocated), and each delta size is
        // non-vacuous and identical across the two sharply different predecessor
        // topologies (allocation independent of predecessor size).
        let artifacts = ["graph records", "plan groups", "resolution modules"];
        for acq in [&small_acq, &big_acq] {
            let before = acq
                .sharing_before
                .expect("initial close has committed artifacts");
            let after = acq
                .sharing_after
                .expect("successor close has committed artifacts");
            for (index, name) in artifacts.iter().enumerate() {
                let (before_ptr, before_delta) = before[index];
                let (after_ptr, after_delta) = after[index];
                assert_eq!(before_delta, 0, "the initial close is flat for {name}");
                assert_eq!(
                    before_ptr, after_ptr,
                    "the successor must share the predecessor {name} Arc by reference (no copy)"
                );
                assert!(after_delta > 0, "the successor delta must carry new {name}");
            }
        }
        let small_after = small_acq.sharing_after.unwrap();
        let big_after = big_acq.sharing_after.unwrap();
        for (index, name) in artifacts.iter().enumerate() {
            assert_eq!(
                small_after[index].1, big_after[index].1,
                "the successor {name} delta size must be independent of the predecessor topology"
            );
        }
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
