use crate::queries::CompileState;
use crate::*;

// Test-only projections. Production consumers query CompilerSession artifacts.

/// Owned semantic projection used by compiler unit tests.
///
/// This struct provides access to the typed IR (AIR) for each function,
/// useful for testing semantic analysis without generating machine code.
#[derive(Debug)]
pub struct AirOutput {
    /// Type intern pool containing all struct and enum definitions.
    pub type_pool: FrozenTypeInternPool,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

pub(crate) fn test_air(source: &str) -> MultiErrorResult<AirOutput> {
    let snapshot = SourceSnapshot::single("<test>", source).map_err(CompileErrors::from)?;
    let (_, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())?;
    let semantic = std::sync::Arc::try_unwrap(semantic)
        .expect("test session uniquely owns its semantic output after return");
    let (_, type_pool, _, warnings) = semantic.into_parts();
    Ok(AirOutput {
        type_pool,
        warnings,
    })
}

pub(crate) fn test_cfg(source: &str) -> MultiErrorResult<CompileState> {
    let snapshot = SourceSnapshot::single("<test>", source).map_err(CompileErrors::from)?;
    let (rir, semantic, _) = test_frontend_snapshot(&snapshot, &CompileOptions::default())?;
    let rir =
        std::sync::Arc::try_unwrap(rir).expect("test session uniquely owns its RIR after return");
    let semantic = std::sync::Arc::try_unwrap(semantic)
        .expect("test session uniquely owns its semantic output after return");
    let (_, symbols) = rir.into_parts();
    let (functions, type_pool, strings, warnings) = semantic.into_parts();
    Ok(CompileState {
        interner: symbols.into_interner(),
        functions,
        type_pool,
        strings,
        warnings,
    })
}

pub(crate) fn test_codegen_state(state: &CompileState, target: Target) -> MultiErrorResult<()> {
    let options = CompileOptions {
        target,
        ..Default::default()
    };
    crate::backend::generate_backend_products(
        &state.functions,
        &state.type_pool,
        &state.strings,
        &state.interner,
        &options,
        &[],
        rue_codegen::BackendArtifactRequest::default(),
    )
    .map(drop)
}

pub(crate) fn test_frontend_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<(
    std::sync::Arc<CanonicalRirOutput>,
    std::sync::Arc<CanonicalSemanticOutput>,
    CompilerSessionWork,
)> {
    let mut session = CompilerSession::new();
    publish_test_snapshot(&mut session, snapshot)?;
    let rir = session.canonical_rir()?;
    let semantic = session.canonical_semantic(options)?;
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
    publish_test_snapshot(&mut session, snapshot)?;
    crate::queries::compile_with_session(&mut session, snapshot, options)
}

pub(crate) fn test_compile_sources(
    sources: &[SourceView<'_>],
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let root = sources
        .first()
        .map(|source| source.file_id)
        .unwrap_or(FileId::DEFAULT);
    let metadata = SourceMetadata::from_sources(sources, root, std::collections::HashMap::new())
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

fn publish_test_snapshot(
    session: &mut CompilerSession,
    snapshot: &SourceSnapshot,
) -> MultiErrorResult<()> {
    let program = session
        .update_for_presentation(snapshot)
        .into_owner_result()?;
    if !program.import_directives().is_empty() {
        let graph = test_fixture_import_graph(&program)?;
        session.adopt_test_import_graph(graph);
    }
    Ok(())
}

/// Build explicit canonical records for in-memory fixtures whose import
/// spellings name one loaded logical or physical module spelling. This is
/// deliberately a test-data convention, not a path-resolution implementation.
pub(crate) fn test_fixture_import_graph(
    program: &ParsedProgram,
) -> MultiErrorResult<CanonicalImportGraph> {
    test_fixture_import_graph_parts(
        program.root(),
        program
            .modules()
            .iter()
            .map(|module| {
                (
                    module.module_id().clone(),
                    std::sync::Arc::from(module.physical_path()),
                )
            })
            .collect(),
        program.import_directives(),
    )
}

pub(crate) fn test_fixture_import_graph_parts(
    root: &ModuleId,
    modules: Vec<(ModuleId, std::sync::Arc<str>)>,
    directives: &ImportDirectives,
) -> MultiErrorResult<CanonicalImportGraph> {
    let inputs = ModuleResolutionInputs::new(
        root.clone(),
        modules
            .iter()
            .map(|(module, physical_path)| ModuleResolutionInput {
                module: module.clone(),
                physical_path: physical_path.clone(),
            })
            .collect(),
    )
    .map_err(CompileErrors::from)?;
    let records = directives
        .iter()
        .map(|directive| {
            let target = modules
                .iter()
                .find(|(module, physical_path)| {
                    [module.as_str(), physical_path.as_ref()]
                        .into_iter()
                        .any(|spelling| {
                            spelling == directive.specifier()
                                || spelling.strip_suffix(directive.specifier()).is_some_and(
                                    |prefix| prefix.is_empty() || prefix.ends_with('/'),
                                )
                        })
                })
                .ok_or_else(|| {
                    CompileErrors::from(CompileError::without_span(
                        ErrorKind::InvalidCompilerInput(format!(
                            "test fixture has no logical module matching import {:?}",
                            directive.specifier()
                        )),
                    ))
                })?;
            Ok(CanonicalImportRecord::new(
                directive.importer().clone(),
                directive.specifier(),
                CanonicalImportResolution::Resolved(target.0.clone()),
            ))
        })
        .collect::<MultiErrorResult<Vec<_>>>()?;
    CanonicalImportGraph::from_supplied(root.clone(), records, &inputs).map_err(|validation| {
        CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
            format!("test fixture supplied an invalid import graph: {validation:?}"),
        )))
    })
}
