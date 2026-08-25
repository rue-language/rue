use tracing::info_span;

use crate::*;

/// A borrowed source record returned by [`SourceSnapshot::files`].
///
/// Snapshot construction uses owned source buffers; this view exists for
/// diagnostics and presentation consumers that need the associated path and
/// request-local file ID without copying source text.
pub struct SourceView<'a> {
    /// Path to the source file (used for error messages).
    pub path: &'a str,
    /// Source code content.
    pub source: &'a str,
    /// Unique identifier for this file.
    pub file_id: FileId,
}

impl<'a> SourceView<'a> {
    pub(crate) fn new(path: &'a str, source: &'a str, file_id: FileId) -> Self {
        Self {
            path,
            source,
            file_id,
        }
    }
}

/// Which linker to use for the final linking phase.
///
/// The Rue compiler can either use its built-in ELF linker or delegate to
/// an external system linker like `clang`, `gcc`, or `ld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkerMode {
    /// Use the internal linker (default).
    Internal,
    /// Use an external system linker (e.g., "clang", "ld", "gcc").
    System(String),
}

impl Default for LinkerMode {
    fn default() -> Self {
        LinkerMode::Internal
    }
}

/// Configuration options for compilation.
///
/// Controls target architecture, linker selection, optimization level, and feature flags.
///
/// # Example
///
/// ```ignore
/// let options = CompileOptions {
///     target: Target::host().unwrap(),
///     linker: LinkerMode::Internal,
///     opt_level: OptLevel::O1,
///     preview_features: PreviewFeatures::new(),
/// };
/// let snapshot = SourceSnapshot::single("main.rue", source)?;
/// let output = compile_snapshot(&snapshot, &options)?;
/// ```
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// The target architecture and OS.
    pub target: Target,
    /// Which linker to use.
    pub linker: LinkerMode,
    /// Optimization level.
    pub opt_level: OptLevel,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// Static archives (`.a`) supplied on the command line with
    /// `--link-archive` (ADR-0064 C FFI). The linker resolves undefined
    /// `extern "C"` symbols from these members. Linking-only: this does not
    /// participate in the semantic or codegen cache identity.
    pub link_archives: Vec<std::path::PathBuf>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host()
                .expect("Rue cannot choose a default compile target on this unsupported host"),
            linker: LinkerMode::Internal,
            opt_level: OptLevel::default(),
            preview_features: PreviewFeatures::new(),
            link_archives: Vec::new(),
        }
    }
}

/// Intermediate compilation state after frontend processing.
///
/// This allows inspection of the IR at each stage, useful for
/// debugging and the `--emit` CLI flags.
#[cfg(test)]
pub(crate) struct CompileState {
    /// Exact rooted functions and their function-local semantic domains.
    pub functions: Vec<crate::session::RootedCfgUnit>,
    /// Warnings collected during compilation.
    pub warnings: Vec<CompileWarning>,
    /// Source retained so target-specific backend tests can query the same
    /// production graph instead of rebuilding a peer frontend state.
    pub snapshot: SourceSnapshot,
}

/// Source-volume and phase-work metrics collected by one-shot compilation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    pub files: usize,
    pub bytes: usize,
    pub lines: usize,
    pub tokens: usize,
}

/// Composable structural work from the session query graph.
///
/// The phase-shaped fields remain stable for metrics consumers during the
/// query-native cutover. A zero for a retired whole-program phase means that
/// compilation deliberately bypassed that phase; rooted body-analysis and CFG
/// query executions are reported in `semantic` rather than being hidden behind
/// a default value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineWork {
    pub parsed: ParsedModulesWork,
    pub merged: CanonicalMergeWork,
    pub lowered: CanonicalRirWork,
    pub semantic: CanonicalSemanticWork,
}

/// Output from successful one-shot compilation.
///
/// Contains the compiled executable binary and any warnings generated
/// during compilation. The binary format depends on the target platform
/// (ELF for Linux, Mach-O for macOS).
#[derive(Debug)]
pub struct CompileOutput {
    /// The compiled ELF binary.
    pub elf: Vec<u8>,
    /// Warnings generated during compilation.
    pub warnings: Vec<CompileWarning>,
    /// Source volume measured while executing the session queries.
    pub(crate) source_stats: SourceStats,
    /// Structural work performed by the session query graph.
    pub(crate) work: PipelineWork,
    /// Live query-runtime work observed after the rooted compiler queries.
    pub(crate) query_runtime: crate::unstable::QueryRuntimeMetrics,
    pub(crate) semantic_reachability: crate::unstable::SemanticReachabilityMetrics,
    pub(crate) provider_observations: crate::unstable::ProviderObservationMetrics,
    pub(crate) publication: crate::unstable::PublicationMetrics,
}

impl CompileOutput {
    /// Return instrumentation from this one-shot compilation without exposing
    /// query-engine work records.
    pub fn unstable_metrics(&self) -> crate::unstable::OneShotMetrics {
        crate::unstable::OneShotMetrics::new(
            self.source_stats,
            self.work,
            self.query_runtime,
            self.semantic_reachability,
            self.provider_observations,
            self.publication,
        )
    }
}

/// Compile an immutable owned source snapshot through a one-shot canonical
/// frontend session, then through the existing backend and linker boundary.
pub fn compile_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let total_source_bytes: usize = snapshot.files().map(|source| source.source.len()).sum();
    let _span = info_span!(
        "compile",
        target = %options.target,
        file_count = snapshot.len(),
        source_bytes = total_source_bytes
    )
    .entered();
    compile_snapshot_impl(snapshot, options)
}

/// Compile the closed-valid discovery revision already adopted by `session`.
///
/// This preserves the compiler-owned import graph and captured discovery
/// context instead of reconstructing a peer frontend from source bytes.
impl CompilerSession {
    /// Run the fresh backend tail for the exact published snapshot used by the
    /// cold-versus-reused differential oracle, including direct no-discovery
    /// sessions. Production callers reach the same tail through
    /// [`compile_snapshot`] or the driver's compile-scope adapter.
    pub(crate) fn oracle_executable(
        &mut self,
        snapshot: &SourceSnapshot,
        options: &CompileOptions,
    ) -> MultiErrorResult<CompileOutput> {
        compile_with_session(self, snapshot, options)
    }

    /// Produce an executable while the caller's canonical `compile` span is
    /// entered.
    ///
    /// The filesystem driver uses this after import discovery so the exact
    /// discovery parse and the later query pipeline share one timing root.
    /// One-shot callers use [`compile_snapshot`], which owns that root.
    pub(crate) fn executable_in_compile_scope(
        &mut self,
        options: &CompileOptions,
    ) -> MultiErrorResult<CompileOutput> {
        let snapshot = self.committed_snapshot_for_executable()?;
        compile_with_session(self, &snapshot, options)
    }

    pub(crate) fn committed_snapshot_for_executable(&self) -> MultiErrorResult<SourceSnapshot> {
        let snapshot = self
            .committed_import_discovery_artifact()
            .ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "compilation requires a closed-valid import discovery revision".into(),
                )))
            })?
            .snapshot()
            .clone();
        Ok(snapshot)
    }
}

fn compile_snapshot_impl(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let mut session = CompilerSession::new();
    session.update_for_presentation(snapshot).into_result()?;
    compile_with_session(&mut session, snapshot, options)
}

pub(crate) fn compile_with_session(
    session: &mut CompilerSession,
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("compile_pipeline").entered();
    let rooted = if matches!(options.linker, LinkerMode::Internal) {
        session
            .rooted_codegen_internal_with_cancellation(
                options,
                rue_codegen::BackendArtifactRequest::default(),
                rue_query::CancellationToken::new(),
            )
            .map_err(|control| match control {
                crate::session::PipelineRequestControl::Compile(errors) => errors,
                crate::session::PipelineRequestControl::Abort(abort) => {
                    crate::session::pipeline_abort_errors("internal codegen", abort)
                }
                crate::session::PipelineRequestControl::Parked(park) => {
                    crate::session::unresolved_toolchain_park_errors(&park)
                }
            })?
    } else {
        session.rooted_codegen(options, rue_codegen::BackendArtifactRequest::default())?
    };
    compile_rooted_with_session(session, snapshot, options, rooted)
}

pub(crate) fn compile_with_session_with_cancellation(
    session: &mut CompilerSession,
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
    cancellation: rue_query::CancellationToken,
) -> Result<CompileOutput, crate::session::PipelineRequestControl> {
    let _span = info_span!("compile_pipeline").entered();
    if cancellation.is_canceled() {
        return Err(crate::session::PipelineRequestControl::Abort(
            rue_query::QueryAbort::Canceled,
        ));
    }
    let rooted = if matches!(options.linker, LinkerMode::Internal) {
        session.rooted_codegen_internal_with_cancellation(
            options,
            rue_codegen::BackendArtifactRequest::default(),
            cancellation.clone(),
        )?
    } else {
        session.rooted_codegen_with_cancellation(
            options,
            rue_codegen::BackendArtifactRequest::default(),
            cancellation.clone(),
        )?
    };
    if cancellation.is_canceled() {
        return Err(crate::session::PipelineRequestControl::Abort(
            rue_query::QueryAbort::Canceled,
        ));
    }
    let output = compile_rooted_with_session(session, snapshot, options, rooted)
        .map_err(crate::session::PipelineRequestControl::Compile)?;
    if cancellation.is_canceled() {
        return Err(crate::session::PipelineRequestControl::Abort(
            rue_query::QueryAbort::Canceled,
        ));
    }
    Ok(output)
}

/// Finish a canonical compilation from the exact objects-ready continuation
/// issued by this session. Used by the retained host's unstable endpoint API;
/// the ordinary one-shot adapter reaches the same helper through
/// [`compile_with_session`].
pub(crate) fn compile_rooted_with_session(
    session: &mut CompilerSession,
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
    rooted: crate::session::RootedCodegenOutput,
) -> MultiErrorResult<CompileOutput> {
    let source_tokens = session
        .published_owner()
        .filter(|program| program.belongs_to_exact_snapshot(snapshot))
        .map(|program| program.token_count())
        .ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                "compilation snapshot differs from the published parsed program".into(),
            )))
        })?;
    let total_source_bytes: usize = snapshot.files().map(|source| source.source.len()).sum();
    let session_work = session.work().clone();
    let metrics = session.unstable_metrics();
    let query_runtime = metrics.query_runtime();
    let semantic_reachability = metrics.semantic_reachability();
    let provider_observations = crate::unstable::provider_observation_metrics(session);
    let mut output = match rooted.input {
        crate::session::RootedCodegenInput::Structured => {
            let export_thunk_objects = rooted
                .exports
                .iter()
                .map(|export| {
                    crate::backend::generate_export_thunk_object(
                        options.target,
                        &export.exported_symbol,
                        &export.native_symbol,
                        &export.param_types,
                    )
                })
                .collect::<Vec<_>>();
            crate::linking::link_internal_structured_units_with_warnings(
                options,
                &rooted.units,
                &export_thunk_objects,
                &rooted.warnings,
            )?
        }
        crate::session::RootedCodegenInput::Projected => {
            let image = crate::program_image_plan::ProgramImage::from_rooted(
                rooted.objects,
                rooted.exports,
                options,
            )?;
            image.fresh_link(options, &rooted.warnings)?
        }
    };
    output.source_stats = SourceStats {
        files: snapshot.len(),
        bytes: total_source_bytes,
        lines: snapshot
            .files()
            .map(|source| source.source.lines().count())
            .sum(),
        tokens: source_tokens,
    };
    output.work = PipelineWork {
        parsed: session_work.last_parse,
        merged: Default::default(),
        lowered: Default::default(),
        semantic: rooted.work,
    };
    output.query_runtime = query_runtime;
    output.semantic_reachability = semantic_reachability;
    output.provider_observations = provider_observations;
    output.publication = crate::unstable::PublicationMetrics {
        cone_retention_failures: session.publication_cone_retention_failures(),
    };
    Ok(output)
}
