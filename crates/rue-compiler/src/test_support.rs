use crate::queries::CompileState;
use crate::*;
use ahash::AHashMap;

// Test-only projections. Production consumers query CompilerSession artifacts.

/// Owned semantic projection used by compiler unit tests.
///
/// This struct provides access to the typed IR (AIR) for each function,
/// useful for testing semantic analysis without generating machine code.
#[derive(Debug)]
pub struct AirOutput {
    /// Named and reached anonymous struct declarations from the rooted graph.
    pub struct_count: usize,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

pub(crate) fn test_air(source: &str) -> MultiErrorResult<AirOutput> {
    let snapshot = SourceSnapshot::single("<test>", source).map_err(CompileErrors::from)?;
    let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())?;
    let named_structs = semantic
        .declarations()
        .iter()
        .filter(|declaration| {
            matches!(
                &declaration.payload,
                crate::durable_semantics::DurableDeclarationPayload::Struct { .. }
            )
        })
        .count();
    let anonymous_structs = semantic
        .anonymous_nominals()
        .iter()
        .filter(|nominal| {
            matches!(
                &nominal.shape,
                crate::durable_semantics::DurableAnonymousNominalShape::Struct { .. }
            )
        })
        .count();
    Ok(AirOutput {
        struct_count: named_structs + anonymous_structs,
        warnings: semantic.warnings.clone(),
    })
}

pub(crate) fn assert_rooted_cfg_value_parity(
    label: &str,
    actual: &crate::session::RootedCfgOutput,
    expected: &crate::session::RootedCfgOutput,
) {
    assert_eq!(
        format!("{:?}", actual.functions()),
        format!("{:?}", expected.functions()),
        "{label}: rooted functions differ",
    );
    assert_eq!(
        format!("{:?}", actual.warnings()),
        format!("{:?}", expected.warnings()),
        "{label}: warnings differ",
    );
    assert_eq!(
        actual.string_domains().collect::<Vec<_>>(),
        expected.string_domains().collect::<Vec<_>>(),
        "{label}: function-owned string domains differ",
    );
    assert_eq!(
        format!("{:?}", actual.declarations()),
        format!("{:?}", expected.declarations()),
        "{label}: declaration semantics differ",
    );
    assert_eq!(
        format!("{:?}", actual.anonymous_nominals()),
        format!("{:?}", expected.anonymous_nominals()),
        "{label}: anonymous nominal semantics differ",
    );
}

pub(crate) fn test_cfg(source: &str) -> MultiErrorResult<CompileState> {
    let snapshot = SourceSnapshot::single("<test>", source).map_err(CompileErrors::from)?;
    let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())?;
    Ok(CompileState {
        functions: semantic.cfgs.clone(),
        warnings: semantic.warnings.clone(),
        snapshot,
    })
}

pub(crate) fn test_codegen_state(state: &CompileState, target: Target) -> MultiErrorResult<()> {
    let options = CompileOptions {
        target,
        ..Default::default()
    };
    let mut session = CompilerSession::new();
    publish_test_snapshot(&mut session, &state.snapshot)?;
    let semantic = session.rooted_cfg(&options)?;
    session
        .codegen_units(
            &semantic,
            &options,
            rue_codegen::BackendArtifactRequest::default(),
        )
        .map(drop)
}

/// The canonical merge and RIR artifacts for `snapshot`.
///
/// Lower-layer unit tests that need a merged program or a whole-program RIR
/// take them from the session queries production uses, so a fixture never
/// exercises an assembly production does not perform. Both come from one
/// session because the RIR query consumes the merge terminal.
///
/// The fixture is published as declared, not through a discovery epoch: merge
/// and lowering do not consume the import graph, and a fixture that names its
/// own physical paths must keep them for identity-sensitive assertions.
pub(crate) struct TestFrontendStages {
    pub(crate) merged: std::sync::Arc<CanonicalMergedProgram>,
    pub(crate) rir: std::sync::Arc<CanonicalRirOutput>,
}

pub(crate) fn test_frontend_stages(
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<TestFrontendStages> {
    let mut session = CompilerSession::new();
    session.update(snapshot).into_owner_result()?;
    let merged = session.merge()?;
    let rir = session.canonical_rir()?;
    Ok(TestFrontendStages { merged, rir })
}

/// The canonical merged program for `snapshot`.
pub(crate) fn test_merged_program(
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<std::sync::Arc<CanonicalMergedProgram>> {
    let mut session = CompilerSession::new();
    session.update(snapshot).into_owner_result()?;
    session.merge()
}

/// The canonical whole-program RIR for `snapshot`.
pub(crate) fn test_canonical_rir(
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<std::sync::Arc<CanonicalRirOutput>> {
    let mut session = CompilerSession::new();
    session.update(snapshot).into_owner_result()?;
    session.canonical_rir()
}

pub(crate) fn test_frontend_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<(
    std::sync::Arc<CanonicalRirOutput>,
    crate::session::RootedCfgOutput,
    CompilerSessionWork,
)> {
    let mut session = CompilerSession::new();
    publish_test_snapshot(&mut session, snapshot)?;
    let rir = session.canonical_rir()?;
    let semantic = session.rooted_cfg(options)?;
    let work = session.work().clone();
    drop(session);
    Ok((rir, semantic, work))
}

pub(crate) fn test_compile_source(source: &str) -> MultiErrorResult<CompileOutput> {
    let snapshot = SourceSnapshot::single("<test>", source).map_err(CompileErrors::from)?;
    test_compile_snapshot(&snapshot, &CompileOptions::default())
}

pub(crate) fn test_compile_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let mut session = CompilerSession::new();
    let published = publish_test_snapshot(&mut session, snapshot)?;
    crate::queries::compile_with_session(&mut session, &published, options)
}

pub(crate) fn test_compile_sources(
    sources: &[SourceView<'_>],
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let root = sources
        .first()
        .map(|source| source.file_id)
        .unwrap_or(FileId::DEFAULT);
    let metadata = SourceMetadata::from_sources(sources, root, AHashMap::new())
        .map_err(CompileErrors::from)?;
    test_compile_sources_with_metadata(sources, &metadata, options)
}

pub(crate) fn test_compile_sources_with_metadata(
    sources: &[SourceView<'_>],
    metadata: &SourceMetadata,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let snapshot =
        SourceSnapshot::from_sources(sources, metadata.clone()).map_err(CompileErrors::from)?;
    test_compile_snapshot(&snapshot, options)
}

/// Publish `snapshot` into `session` and return the snapshot the session
/// actually holds.
///
/// An import-bearing fixture is republished by its discovery epoch, whose
/// snapshot holds the modules reached from the root under their canonical
/// identities — so consumers that must agree with the published program compile
/// against the returned snapshot, not the fixture's declared one.
pub(crate) fn publish_test_snapshot(
    session: &mut CompilerSession,
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<SourceSnapshot> {
    if !fixture_has_imports(snapshot)? {
        session
            .update_for_presentation(snapshot)
            .into_owner_result()?;
        return Ok(snapshot.clone());
    }
    Ok(TestDiscoveryHost::new(snapshot)?.drive(session)?.snapshot)
}

/// Whether `snapshot` declares any import directive, decided by a standalone
/// parse so the answer costs the session nothing when discovery follows.
pub(crate) fn fixture_has_imports(snapshot: &SourceSnapshot) -> MultiErrorResult<bool> {
    let parsed = crate::parsed_modules::parse_source_snapshot_modules(snapshot)?;
    Ok(!parsed.import_directives().is_empty())
}

/// The canonical import graph `snapshot` commits, for lower-layer unit tests
/// that analyze a fixture without owning a session.
///
/// Import-bearing fixtures resolve through a full discovery epoch; import-free
/// ones take production's own uniquely valid empty graph over their declared
/// module set.
pub(crate) fn test_import_graph(
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<CanonicalImportGraph> {
    if !fixture_has_imports(snapshot)? {
        let parsed = crate::parsed_modules::parse_source_snapshot_modules(snapshot)?;
        return crate::import_graph::import_free_canonical_graph(parsed.as_ref());
    }
    let mut session = CompilerSession::new();
    let discovery = TestDiscoveryHost::new(snapshot)?.drive(&mut session)?;
    Ok(discovery.graph)
}

/// What one in-memory discovery epoch produced.
pub(crate) struct TestDiscovery {
    /// The snapshot discovery assembled and the session published — the modules
    /// actually reached from the root, not the fixture's declared set.
    pub(crate) snapshot: SourceSnapshot,
    /// Parse work accumulated across the epoch's staging rounds.
    pub(crate) parse_work: crate::ParsedModulesWork,
    pub(crate) graph: CanonicalImportGraph,
}

/// One file the fixture can serve, keyed by the path discovery would request.
struct TestFixtureFile {
    canonical_path: std::sync::Arc<str>,
    identity: PhysicalFileIdentity,
    fingerprint: FileMetadataFingerprint,
    source: std::sync::Arc<String>,
}

/// An in-memory host for the real import-discovery protocol (ADR-0051).
///
/// A fixture's *logical* module paths describe a virtual project tree: each
/// module is served at exactly the path production would request for that
/// module identity, under a synthetic project root. Its *physical* paths carry
/// over as canonical identity, which is their production role — a canonical
/// path that differs from the requested one is the relocated-or-linked file
/// case, not a second spelling the resolver is allowed to match against.
///
/// The host answers one question per compiler-issued frontier request: does
/// this exact requested path exist, and what are its bytes. Candidate
/// precedence, canonical identity, provenance, and read policy stay in the
/// compiler, so a fixture cannot resolve an import the real resolver would not.
pub(crate) struct TestDiscoveryHost {
    context: ImportDiscoveryContext,
    root_requested_path: String,
    files: std::collections::BTreeMap<String, TestFixtureFile>,
}

/// The synthetic project root every fixture's virtual tree hangs from. Fixtures
/// name modules logically, so the tree needs a root but never a real directory.
const FIXTURE_PROJECT_ROOT: &str = "/rue-fixture";
const FIXTURE_STD_ROOT: &str = "/rue-fixture/std";
const TRUSTED_MODULE_PREFIX: &str = "\0rue-std/";

impl TestDiscoveryHost {
    pub(crate) fn new(snapshot: &SourceSnapshot) -> MultiErrorResult<Self> {
        let metadata = snapshot.metadata();
        let mut trusted = false;
        let mut files = std::collections::BTreeMap::new();
        for source in snapshot.files() {
            let module = metadata
                .module_id(source.file_id)
                .expect("validated snapshot metadata names every file");
            let requested = fixture_requested_path(&module)?;
            trusted |= module.is_trusted_standard_library();
            // A trusted module's canonical path must stay under the captured
            // std root, so only project modules can carry a fixture-declared
            // relocation. A relative fixture spelling names no on-disk file at
            // all and is canonically its own requested path.
            let canonical = if module.is_trusted_standard_library()
                || !std::path::Path::new(source.path).is_absolute()
            {
                requested.clone()
            } else {
                source.path.to_owned()
            };
            let text = snapshot
                .shared_source_text(source.file_id)
                .expect("validated snapshot retains every file's text");
            files.insert(
                requested,
                TestFixtureFile {
                    identity: PhysicalFileIdentity::new(
                        1,
                        crate::import_discovery::source_fingerprint(&canonical),
                    ),
                    fingerprint: FileMetadataFingerprint::new(text.len() as u64, 0, 0),
                    canonical_path: std::sync::Arc::from(canonical.as_str()),
                    source: text,
                },
            );
        }
        let root_module = metadata.root_module_id();
        if root_module.is_trusted_standard_library() {
            return Err(invalid_fixture(
                "a discovery root cannot be a trusted standard-library module",
            ));
        }
        let context = ImportDiscoveryContext::new(
            1,
            FIXTURE_PROJECT_ROOT,
            trusted.then_some(FIXTURE_STD_ROOT),
            "test-fixture-discovery",
        )
        .map_err(CompileErrors::from)?;
        Ok(Self {
            context,
            root_requested_path: fixture_requested_path(&root_module)?,
            files,
        })
    }

    fn assembler(&self) -> MultiErrorResult<DiscoverySourceAssembler> {
        let root = self
            .files
            .get(&self.root_requested_path)
            .expect("the root module is one of the fixture's own files");
        DiscoverySourceAssembler::new(
            self.context.clone(),
            self.root_requested_path.as_str(),
            root.canonical_path.as_ref(),
            root.identity,
            root.fingerprint,
            root.source.clone(),
        )
        .map_err(CompileErrors::from)
    }

    /// Answer one compiler-issued probe: the fixture reports existence and
    /// bytes for the exact requested path, and nothing else.
    fn serve(&self, request: crate::ImportDiscoveryRequest) -> MultiErrorResult<ImportObservation> {
        let Some(file) = self.files.get(request.requested_path()) else {
            return Ok(ImportObservation::absent(request));
        };
        let accepted = AcceptedImportSource::new(
            request.requested_path(),
            file.canonical_path.as_ref(),
            file.identity,
            file.fingerprint,
            file.source.clone(),
        )
        .map_err(CompileErrors::from)?;
        ImportObservation::accepted(request, accepted).map_err(CompileErrors::from)
    }

    /// Run one complete discovery epoch against `session` and commit its graph.
    pub(crate) fn drive(&self, session: &mut CompilerSession) -> MultiErrorResult<TestDiscovery> {
        let mut assembler = self.assembler()?;
        let initial = assembler.snapshot().map_err(CompileErrors::from)?;
        let mut revision = session
            .begin_import_input_request(
                &initial,
                self.context.clone(),
                assembler.accepted_read_manifest(),
            )
            .map_err(CompileErrors::from)?;
        loop {
            let snapshot = assembler.snapshot().map_err(CompileErrors::from)?;
            let ledger = session
                .import_observation_ledger(revision)
                .map_err(CompileErrors::from)?;
            let plan = session.stage_import_discovery(
                &snapshot,
                self.context.clone(),
                assembler.accepted_read_manifest().shared_slice(),
                ledger.clone(),
            )?;
            let frontier = session
                .import_demand_frontier_for_roots(
                    revision,
                    &plan,
                    ImportDemandMode::Rooted,
                    &plan.demand_roots(),
                )
                .map_err(CompileErrors::from)?;
            if frontier.requests().is_empty() {
                let artifact = session.close_import_discovery(ledger)?;
                let graph = artifact
                    .graph()
                    .expect("a closed-valid discovery revision retains its canonical graph")
                    .graph()
                    .clone();
                return Ok(TestDiscovery {
                    snapshot: artifact.snapshot().clone(),
                    parse_work: artifact.parse_work(),
                    graph,
                });
            }
            let observations = frontier
                .requests()
                .iter()
                .cloned()
                .map(|request| self.serve(request))
                .collect::<MultiErrorResult<Vec<_>>>()?;
            let mut assembly_ledger = ledger;
            for observation in observations.iter().cloned() {
                assembly_ledger
                    .record(observation)
                    .map_err(CompileErrors::from)?;
            }
            assembler
                .add_plan_reads(&plan, &assembly_ledger)
                .map_err(CompileErrors::from)?;
            let successor = assembler.snapshot().map_err(CompileErrors::from)?;
            revision = session
                .publish_import_observation_batch(
                    &frontier,
                    &successor,
                    assembler.accepted_read_manifest(),
                    observations,
                )
                .map_err(CompileErrors::from)?;
        }
    }
}

/// The path production would request for `module` inside the fixture tree —
/// the compiler's own module-identity-to-path inverse, not a search.
fn fixture_requested_path(module: &ModuleId) -> MultiErrorResult<String> {
    let (root, relative) = match module.as_str().strip_prefix(TRUSTED_MODULE_PREFIX) {
        Some(relative) => (FIXTURE_STD_ROOT, relative),
        None => (FIXTURE_PROJECT_ROOT, module.as_str()),
    };
    if relative.is_empty() || relative.starts_with('/') {
        return Err(invalid_fixture(&format!(
            "fixture module {module} has no project-relative logical path"
        )));
    }
    Ok(format!("{root}/{relative}"))
}

fn invalid_fixture(message: &str) -> CompileErrors {
    CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
        message.to_owned(),
    )))
}
/// Test-only sink for proving that provider type facts materialize each nominal
/// at most once. Production body analysis consumes the durable facts directly.
#[derive(Default)]
pub(crate) struct ProviderMaterialization {
    nominals: AHashMap<crate::StableDefinitionKey, crate::DurableDeclarationPayload>,
}

impl ProviderMaterialization {
    pub(crate) fn materialize_nominal(
        &mut self,
        key: &crate::StableDefinitionKey,
        payload: &crate::DurableDeclarationPayload,
    ) {
        self.nominals
            .entry(key.clone())
            .or_insert_with(|| payload.clone());
    }

    pub(crate) fn materialized_nominal(
        &self,
        key: &crate::StableDefinitionKey,
    ) -> Option<&crate::DurableDeclarationPayload> {
        self.nominals.get(key)
    }

    pub(crate) fn materialized_nominal_count(&self) -> usize {
        self.nominals.len()
    }
}
