use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rue_compiler::unstable::ImportDiscoveryRevision;
use rue_compiler::{
    AcceptedImportSource, AcceptedReadManifestEntry, CompileErrors, CompilerSession,
    DependencyEnvelope, DiscoverySourceAssembler, FileId, FileMetadataFingerprint,
    ImportDiscoveryContext, ImportObservation, ImportObservationLedger, ImportObservationStatus,
    PhysicalFileIdentity, SourceMetadata, SourceSnapshot,
};

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
}

pub(crate) fn load(
    request: SourceLoadRequest<'_>,
) -> Result<ImportDiscoveryResult, SourceLoadError> {
    let manifest = request
        .source_manifest_path
        .map(SourceManifest::load)
        .transpose()
        .map_err(SourceLoadError::Message)?;
    validate_manifest_allows_source(manifest.as_ref(), request.root_source, "root")
        .map_err(SourceLoadError::Message)?;
    discover_and_load_imports(request.root_source, manifest.as_ref(), request.std_root)
}

#[derive(Debug)]
pub(crate) struct ImportDiscoveryResult {
    pub(crate) source_snapshot: SourceSnapshot,
    /// Immutable, normalized inputs used by every compiler discovery plan.
    pub(crate) resolution: SourceResolutionInputs,
    /// Canonical physical reads accepted while assembling `source_snapshot`.
    pub(crate) read_manifest: Arc<[AcceptedReadManifestEntry]>,
    /// Canonical import topology and diagnostics published by the compiler.
    pub(crate) revision: Arc<ImportDiscoveryRevision>,
    pub(crate) session: CompilerSession,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceResolutionInputs {
    pub(crate) root_path: PathBuf,
    pub(crate) context: ImportDiscoveryContext,
}

/// Reject a source before discovery lexing if Rue's span representation cannot
/// describe its byte offsets.
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
pub(crate) fn discover_and_load_imports(
    root_source: &str,
    source_manifest: Option<&SourceManifest>,
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
    if source_manifest.is_some_and(|manifest| !manifest.allows_canonical(&root_canonical)) {
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

    let mut ledger = ImportObservationLedger::default();
    let mut staging = CompilerSession::new();
    let final_plan;
    loop {
        let snapshot = assembler
            .snapshot()
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        let plan = match staging.stage_import_discovery(
            &snapshot,
            context.clone(),
            assembler.accepted_read_manifest(),
            ledger.clone(),
        ) {
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
        loop {
            let pending = plan.pending_requests(&ledger);
            if pending.is_empty() {
                break;
            }
            for request in pending {
                let candidate = Path::new(request.requested_path());
                let observation = if let Some(manifest) = source_manifest
                    && !manifest.declares_path_without_probe(candidate)
                {
                    ImportObservation::failure(request, ImportObservationStatus::DeniedLexical)
                        .unwrap()
                } else {
                    match fs::canonicalize(candidate) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            ImportObservation::absent(request)
                        }
                        Err(error) => ImportObservation::failure(
                            request,
                            ImportObservationStatus::PresentUnreadable(Arc::from(
                                error.to_string(),
                            )),
                        )
                        .unwrap(),
                        Ok(canonical)
                            if source_manifest
                                .is_some_and(|manifest| !manifest.allows_canonical(&canonical)) =>
                        {
                            ImportObservation::failure(
                                request,
                                ImportObservationStatus::DeniedCanonical {
                                    canonical_path: Arc::from(
                                        canonical.to_string_lossy().into_owned(),
                                    ),
                                },
                            )
                            .unwrap()
                        }
                        Ok(canonical) if !canonical.is_file() => ImportObservation::failure(
                            request,
                            ImportObservationStatus::InvalidPhysicalType {
                                canonical_path: Arc::from(canonical.to_string_lossy().into_owned()),
                            },
                        )
                        .unwrap(),
                        Ok(canonical) => match stable_read_to_string(&canonical) {
                            Ok(read) => {
                                let accepted = AcceptedImportSource::new(
                                    Arc::from(request.requested_path()),
                                    Arc::from(canonical.to_string_lossy().into_owned()),
                                    read.identity,
                                    read.fingerprint,
                                    Arc::new(read.source),
                                )
                                .unwrap();
                                ImportObservation::accepted(request, accepted).unwrap()
                            }
                            Err(StableReadError::Io(error)) => ImportObservation::failure(
                                request,
                                ImportObservationStatus::PresentUnreadable(Arc::from(
                                    error.to_string(),
                                )),
                            )
                            .unwrap(),
                            Err(StableReadError::Changed) => ImportObservation::failure(
                                request,
                                ImportObservationStatus::UnstableRead(Arc::from(
                                    "candidate metadata changed during read",
                                )),
                            )
                            .unwrap(),
                        },
                    }
                };
                ledger
                    .record(observation)
                    .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
            }
        }
        if plan.failures(&ledger).next().is_some() {
            final_plan = plan;
            break;
        }
        let added = assembler
            .add_plan_reads(&plan, &ledger)
            .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
        if added == 0 {
            final_plan = plan;
            break;
        }
    }

    let snapshot = assembler
        .snapshot()
        .map_err(|error| SourceLoadError::Message(format!("Error: {error}")))?;
    let read_manifest = assembler.accepted_read_manifest();
    debug_assert_eq!(final_plan.source_revision(), snapshot.source_revision());
    let closed = match staging.close_import_discovery(ledger) {
        Ok(closed) => closed,
        Err(_) => {
            let attempted = staging
                .discovery_attempt()
                .expect("failed closure publishes an attempted import revision")
                .clone();
            if DependencyEnvelope::from_closed_revision(&attempted).is_some() {
                // Missing and ambiguous resolution are structurally closed and
                // therefore have canonical topology for `--emit deps`. The
                // caller owns their one diagnostic rendering so the envelope
                // can be written first. All other failures have no topology.
                attempted
            } else {
                let diagnostics = staging
                    .import_diagnostics()
                    .expect("failed closure publishes canonical import diagnostics");
                let errors = CompileErrors::from(diagnostics.errors().to_vec());
                return Err(SourceLoadError::Compiler {
                    snapshot: Some(snapshot),
                    errors,
                });
            }
        }
    };
    Ok(ImportDiscoveryResult {
        source_snapshot: snapshot,
        resolution: SourceResolutionInputs { root_path, context },
        read_manifest,
        revision: closed,
        session: staging,
    })
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
            .split("\n#[cfg(test)]\nmod tests {")
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
