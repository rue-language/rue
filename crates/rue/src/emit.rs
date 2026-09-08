#[cfg(test)]
use rue_compiler::unstable::MetricsSnapshot;
use rue_compiler::unstable::PresentationOutput;
#[cfg(test)]
use rue_compiler::unstable::update_for_presentation;
#[cfg(test)]
use rue_compiler::unstable::{
    CanonicalRirPresentationMetrics, ParseMetrics, SemanticMetrics, rooted_cfg,
};
use rue_compiler::unstable::{PresentationBatchRequest, PresentationRequest, PresentationStage};
use rue_compiler::{
    AcceptedReadManifest, CompileErrors, CompileOptions, DependencyEnvelope,
    DependencyEnvelopeStatus, ImportDiscoveryStatus, SourceSnapshot,
};
#[cfg(test)]
use rue_compiler::{CompilerSession, RirView};
use rue_driver::{AttemptedRead, FilesystemCompilerHost, WatchInput};
#[cfg(test)]
use rue_error::{CompileError, ErrorKind};
#[cfg(test)]
use tracing::info_span;

use crate::DiagnosticOutput;

/// Compilation stages that can be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmitStage {
    /// Emit tokens from the lexer.
    Tokens,
    /// Emit the abstract syntax tree.
    Ast,
    /// Emit RIR (untyped intermediate representation).
    Rir,
    /// Emit AIR (typed intermediate representation).
    Air,
    /// Emit CFG (control flow graph).
    Cfg,
    /// Emit lowering (CFG to MIR instruction selection).
    Lowering,
    /// Emit MIR (machine intermediate representation).
    Mir,
    /// Emit liveness analysis information.
    Liveness,
    /// Emit register allocation debug info.
    RegAlloc,
    /// Emit assembly text.
    Asm,
    /// Emit stack frame layout per function.
    StackFrame,
    /// Emit the calling convention and per-value placement of every reachable
    /// function's signature.
    Abi,
    /// Emit the source dependency graph discovered while loading imports.
    Deps,
}

#[cfg(test)]
pub(crate) struct EmitFrontend {
    _rir: std::sync::Arc<RirView>,
    pub(crate) work: EmitWork,
    #[cfg(test)]
    pub(crate) session_work: MetricsSnapshot,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct EmitWork {
    pub(crate) parsed: ParseMetrics,
    pub(crate) canonical_rir_presentation: CanonicalRirPresentationMetrics,
    pub(crate) semantic: SemanticMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmitFrontendRoute {
    /// Tokens — presented directly from parsed modules; no frontend query.
    None,
    /// AST — presented from the parse terminal; no lowering, no semantics.
    AstOnlySyntax,
    /// RIR — presented from the RIR terminal (parse + lowering) but WITHOUT any
    /// semantic body analysis. Because the trusted-toolchain park is raised only
    /// by reached-body semantic analysis, an RIR emit runs no park and no std
    /// acquisition: it is a pre-semantic presentation of the untyped IR.
    RirOnly,
    /// A backend stage (AIR and later) that requires semantic body analysis.
    SessionQuery,
}

/// Whether any requested stage requires semantic body analysis (AIR and later).
/// RIR, AST, tokens, and deps are all pre-semantic presentations, so an emit made
/// up only of those never analyzes a body — and therefore never parks on a
/// trusted-toolchain demand or acquires std. The host acquisition loop is gated on
/// this so an `--emit rir`/`--emit ast` run performs zero std reads.
pub(crate) fn emit_requires_semantic(stages: &[EmitStage]) -> bool {
    stages.iter().any(|stage| {
        matches!(
            stage,
            EmitStage::Air
                | EmitStage::Cfg
                | EmitStage::Lowering
                | EmitStage::Mir
                | EmitStage::Liveness
                | EmitStage::RegAlloc
                | EmitStage::Asm
                | EmitStage::StackFrame
                | EmitStage::Abi
        )
    })
}

pub(crate) fn emit_frontend_route(stages: &[EmitStage]) -> EmitFrontendRoute {
    if emit_requires_semantic(stages) {
        EmitFrontendRoute::SessionQuery
    } else if stages.contains(&EmitStage::Rir) {
        EmitFrontendRoute::RirOnly
    } else if stages.contains(&EmitStage::Ast) {
        EmitFrontendRoute::AstOnlySyntax
    } else {
        EmitFrontendRoute::None
    }
}

#[cfg(test)]
pub(crate) fn build_emit_frontend_in_session(
    session: &mut CompilerSession,
    options: CompileOptions,
) -> Result<EmitFrontend, CompileErrors> {
    session.published().ok_or_else(|| {
        CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
            "emit requires a published closed discovery revision".into(),
        )))
    })?;
    let rir = {
        let _span = info_span!("semantic_astgen").entered();
        session.rir()?
    };
    let rooted = rooted_cfg(session, &options)?;
    let session_work = session.unstable_metrics();
    Ok(EmitFrontend {
        _rir: rir,
        work: EmitWork {
            parsed: session_work.parse_metrics(),
            canonical_rir_presentation: session_work.canonical_rir_presentation(),
            semantic: rooted.metrics(),
        },
        #[cfg(test)]
        session_work,
    })
}

#[cfg(test)]
pub(crate) fn build_emit_frontend(
    source_snapshot: &SourceSnapshot,
    options: CompileOptions,
) -> Result<EmitFrontend, CompileErrors> {
    let mut session = CompilerSession::new();
    update_for_presentation(&mut session, source_snapshot).into_result()?;
    build_emit_frontend_in_session(&mut session, options)
}

/// Error returned when parsing an emit stage name fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseEmitStageError(String);

impl std::fmt::Display for ParseEmitStageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown emit stage '{}'", self.0)
    }
}

impl std::error::Error for ParseEmitStageError {}

impl std::str::FromStr for EmitStage {
    type Err = ParseEmitStageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tokens" => Ok(EmitStage::Tokens),
            "ast" => Ok(EmitStage::Ast),
            "rir" => Ok(EmitStage::Rir),
            "air" => Ok(EmitStage::Air),
            "cfg" => Ok(EmitStage::Cfg),
            "lowering" => Ok(EmitStage::Lowering),
            "mir" => Ok(EmitStage::Mir),
            "liveness" => Ok(EmitStage::Liveness),
            "regalloc" => Ok(EmitStage::RegAlloc),
            "asm" => Ok(EmitStage::Asm),
            "stackframe" => Ok(EmitStage::StackFrame),
            "abi" => Ok(EmitStage::Abi),
            "deps" => Ok(EmitStage::Deps),
            _ => Err(ParseEmitStageError(s.to_string())),
        }
    }
}

impl EmitStage {
    pub(crate) fn all_names() -> &'static str {
        "tokens, ast, rir, air, cfg, lowering, mir, liveness, regalloc, asm, stackframe, abi, \
         deps"
    }
}

/// Reject output-mode combinations that cannot coexist, independent of any
/// filesystem or session state.
///
/// This is the single authority for output-mode compatibility. The driver
/// evaluates it immediately after argument parsing — before tracing, thread-pool
/// configuration, manifest loading, or any source I/O — so an options error is
/// never masked by an unrelated missing-file or manifest failure, and is never
/// nondeterministically subordinate to it (RUE-798).
///
/// Two combinations are incompatible:
///
/// * `--emit deps` writes a dependency graph to stdout and cannot share the run
///   with any other `--emit` stage.
/// * `--emit` and `--benchmark-json` both own stdout, so their outputs would
///   interleave and corrupt each other.
pub(crate) fn validate_output_modes(
    emit_stages: &[EmitStage],
    benchmark_json: bool,
) -> Result<(), String> {
    if emit_stages.contains(&EmitStage::Deps) && emit_stages.len() != 1 {
        return Err("Error: --emit deps cannot be combined with other --emit stages".to_string());
    }
    if benchmark_json && !emit_stages.is_empty() {
        return Err(
            "Error: --emit cannot be combined with --benchmark-json (both write to stdout)"
                .to_string(),
        );
    }
    Ok(())
}

/// Handle emit stages for multi-file compilation.
///
/// For early stages (tokens, ast), each file is processed and labeled individually.
/// For later stages (rir, air, cfg, etc.), the merged program is used.
pub(crate) struct EmitRequest<'a, 'diagnostics> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) stages: &'a [EmitStage],
    pub(crate) compile_options: CompileOptions,
    pub(crate) diagnostics: &'a DiagnosticOutput<'diagnostics>,
}

struct OwnedEmitResponse {
    source_snapshot: SourceSnapshot,
    accepted_reads: AcceptedReadManifest,
    attempted_reads: Vec<AttemptedRead>,
    watch_inputs: Vec<WatchInput>,
    error_format: crate::ErrorFormat,
    target: rue_target::Target,
    result: OwnedEmitResult,
}

enum OwnedEmitResult {
    Dependencies {
        json: String,
        errors: Option<CompileErrors>,
    },
    Stages(Vec<OwnedEmitStage>),
    Failed(CompileErrors),
    InternalFailure(String),
}

struct OwnedEmitStage {
    stage: EmitStage,
    file: Option<String>,
    output: PresentationOutput,
}

fn produce(request: EmitRequest<'_, '_>) -> OwnedEmitResponse {
    let EmitRequest {
        host,
        stages,
        compile_options,
        diagnostics,
    } = request;
    let source_snapshot = host.source_snapshot().clone();
    let accepted_reads = host.accepted_reads().clone();
    let attempted_reads = host.attempted_reads().to_vec();
    let watch_inputs = host.watch_inputs();
    let error_format = diagnostics.format();
    let target = compile_options.target;
    let discovery_revision = host.discovery_revision().clone();

    if stages.contains(&EmitStage::Deps) {
        debug_assert_eq!(stages.len(), 1);
        let dependency_envelope =
            match DependencyEnvelope::from_closed_revision(&discovery_revision) {
                Some(envelope) => envelope,
                None => {
                    return OwnedEmitResponse {
                        source_snapshot,
                        accepted_reads,
                        attempted_reads,
                        watch_inputs,
                        error_format,
                        target,
                        result: OwnedEmitResult::Failed(rue_driver::with_import_migration_helps(
                            discovery_revision.diagnostics(),
                        )),
                    };
                }
            };
        let incomplete = dependency_envelope.status == DependencyEnvelopeStatus::Incomplete;
        let result = match serde_json::to_string_pretty(&dependency_envelope) {
            Ok(json) => OwnedEmitResult::Dependencies {
                json,
                errors: incomplete.then(|| {
                    rue_driver::with_import_migration_helps(discovery_revision.diagnostics())
                }),
            },
            Err(error) => OwnedEmitResult::InternalFailure(format!(
                "Error emitting dependency envelope: {error}"
            )),
        };
        return OwnedEmitResponse {
            source_snapshot,
            accepted_reads,
            attempted_reads,
            watch_inputs,
            error_format,
            target,
            result,
        };
    }

    if discovery_revision.status() != ImportDiscoveryStatus::ClosedValid {
        return OwnedEmitResponse {
            source_snapshot,
            accepted_reads,
            attempted_reads,
            watch_inputs,
            error_format,
            target,
            result: OwnedEmitResult::Failed(rue_driver::with_import_migration_helps(
                discovery_revision.diagnostics(),
            )),
        };
    }

    match emit_frontend_route(stages) {
        EmitFrontendRoute::SessionQuery
        | EmitFrontendRoute::AstOnlySyntax
        | EmitFrontendRoute::None => {}
        EmitFrontendRoute::RirOnly => {
            if let Err(errors) = host.rir() {
                return OwnedEmitResponse {
                    source_snapshot,
                    accepted_reads,
                    attempted_reads,
                    watch_inputs,
                    error_format,
                    target,
                    result: OwnedEmitResult::Failed(rue_driver::with_import_migration_helps(
                        &errors,
                    )),
                };
            }
        }
    }

    let file_order = source_snapshot
        .files()
        .map(|source| source.file_id)
        .collect::<Vec<_>>();
    let whole_program_stages: Vec<PresentationStage> = stages
        .iter()
        .filter_map(|stage| match stage {
            EmitStage::Rir => Some(PresentationStage::Rir),
            EmitStage::Air => Some(PresentationStage::Air),
            EmitStage::Cfg => Some(PresentationStage::Cfg),
            EmitStage::Lowering => Some(PresentationStage::Lowering),
            EmitStage::Mir => Some(PresentationStage::Mir),
            EmitStage::Liveness => Some(PresentationStage::Liveness),
            EmitStage::RegAlloc => Some(PresentationStage::RegAlloc),
            EmitStage::Asm => Some(PresentationStage::Asm),
            EmitStage::StackFrame => Some(PresentationStage::StackFrame),
            EmitStage::Abi => Some(PresentationStage::Abi),
            EmitStage::Tokens | EmitStage::Ast | EmitStage::Deps => None,
        })
        .collect();
    let whole_program_outputs = if whole_program_stages.is_empty() {
        Vec::new()
    } else {
        match host.present_many(PresentationBatchRequest {
            stages: &whole_program_stages,
            options: &compile_options,
            file_order: &file_order,
        }) {
            Ok(outputs) => outputs,
            Err(errors) => {
                return OwnedEmitResponse {
                    source_snapshot,
                    accepted_reads,
                    attempted_reads,
                    watch_inputs,
                    error_format,
                    target,
                    result: OwnedEmitResult::Failed(rue_driver::with_import_migration_helps(
                        &errors,
                    )),
                };
            }
        }
    };

    let mut whole_program_index = 0;
    let mut outputs = Vec::new();
    for stage in stages {
        if matches!(stage, EmitStage::Tokens | EmitStage::Ast) {
            let unstable_stage = match stage {
                EmitStage::Tokens => PresentationStage::Tokens,
                EmitStage::Ast => PresentationStage::Ast,
                _ => unreachable!(),
            };
            let files = source_snapshot
                .files()
                .map(|source| (source.file_id, source.path.to_owned()))
                .collect::<Vec<_>>();
            for (file_id, file_path) in files {
                let output = match host.present(PresentationRequest {
                    stage: unstable_stage,
                    options: &compile_options,
                    file_order: &[file_id],
                }) {
                    Ok(output) => output,
                    Err(errors) => {
                        return OwnedEmitResponse {
                            source_snapshot,
                            accepted_reads,
                            attempted_reads,
                            watch_inputs,
                            error_format,
                            target,
                            result: OwnedEmitResult::Failed(
                                rue_driver::with_import_migration_helps(&errors),
                            ),
                        };
                    }
                };
                outputs.push(OwnedEmitStage {
                    stage: *stage,
                    file: Some(file_path),
                    output,
                });
            }
            continue;
        }
        if matches!(stage, EmitStage::Deps) {
            continue;
        }
        let output = whole_program_outputs
            .get(whole_program_index)
            .expect("one whole-program output per whole-program stage")
            .clone();
        whole_program_index += 1;
        outputs.push(OwnedEmitStage {
            stage: *stage,
            file: None,
            output,
        });
    }
    OwnedEmitResponse {
        source_snapshot,
        accepted_reads,
        attempted_reads,
        watch_inputs,
        error_format,
        target,
        result: OwnedEmitResult::Stages(outputs),
    }
}

fn complete(response: OwnedEmitResponse) -> Result<(), ()> {
    let OwnedEmitResponse {
        source_snapshot,
        accepted_reads,
        attempted_reads,
        watch_inputs,
        error_format,
        target,
        result,
    } = response;
    consume_emit_observations(accepted_reads, attempted_reads, watch_inputs);
    let sources = source_snapshot
        .files()
        .map(|source| {
            (
                source.file_id,
                rue_compiler::unstable::SourceInfo::new(source.source, source.path),
            )
        })
        .collect();
    let diagnostics = DiagnosticOutput::new(error_format, sources);
    match result {
        OwnedEmitResult::InternalFailure(message) => {
            eprintln!("{message}");
            Err(())
        }
        OwnedEmitResult::Failed(errors) => {
            diagnostics.print_prepared_errors(&errors);
            Err(())
        }
        OwnedEmitResult::Dependencies { json, errors } => {
            println!("{json}");
            if let Some(errors) = errors {
                diagnostics.print_prepared_errors(&errors);
                Err(())
            } else {
                Ok(())
            }
        }
        OwnedEmitResult::Stages(outputs) => {
            let mut warnings_printed = false;
            for owned in outputs {
                let output = owned.output;
                if let Some(file) = owned.file {
                    match owned.stage {
                        EmitStage::Tokens => println!("=== Tokens ({file}) ==="),
                        EmitStage::Ast => println!("=== AST ({file}) ==="),
                        _ => unreachable!(),
                    }
                    print!("{}", output.as_str());
                    println!();
                    continue;
                }
                if !warnings_printed && emit_requires_semantic(&[owned.stage]) {
                    diagnostics.print_warnings(output.warnings());
                    warnings_printed = true;
                }
                match owned.stage {
                    EmitStage::Rir => {
                        println!("=== RIR ===");
                        println!("{}", output.as_str());
                        println!();
                    }
                    EmitStage::Air => {
                        println!("=== AIR ===");
                        print!("{}", output.as_str());
                        println!();
                    }
                    EmitStage::Cfg => {
                        println!("=== CFG ===");
                        print!("{}", output.as_str());
                        println!();
                    }
                    EmitStage::Lowering => println!("{}", output.as_str()),
                    EmitStage::Mir => {
                        println!("=== MIR ({target}) ===");
                        println!("{}", output.as_str());
                    }
                    EmitStage::Liveness => {
                        println!("=== Liveness Analysis ({target}) ===");
                        println!("{}", output.as_str());
                    }
                    EmitStage::RegAlloc => {
                        println!("=== Register Allocation ({target}) ===");
                        println!("{}", output.as_str());
                    }
                    EmitStage::Asm => {
                        println!("=== Assembly ({target}) ===");
                        println!("{}", output.as_str());
                    }
                    EmitStage::StackFrame => print!("{}", output.as_str()),
                    EmitStage::Abi => {
                        println!("=== ABI ({target}) ===");
                        print!("{}", output.as_str());
                    }
                    EmitStage::Tokens | EmitStage::Ast | EmitStage::Deps => unreachable!(),
                }
            }
            Ok(())
        }
    }
}

fn consume_emit_observations(
    accepted_reads: AcceptedReadManifest,
    attempted_reads: Vec<AttemptedRead>,
    watch_inputs: Vec<WatchInput>,
) {
    // The response owns the exact closure and last attempt even though emit's
    // presentation surface has no event field for them. Consume them at the
    // completion boundary rather than consulting a successor host state.
    drop((accepted_reads, attempted_reads, watch_inputs));
}

pub(crate) fn execute(request: EmitRequest<'_, '_>) -> Result<(), ()> {
    complete(produce(request))
}

#[cfg(test)]
#[path = "emit_owned_tests.rs"]
mod owned_tests;

#[cfg(test)]
mod output_mode_tests {
    use super::{EmitStage, validate_output_modes};

    #[test]
    fn accepts_sole_deps_stage() {
        assert!(validate_output_modes(&[EmitStage::Deps], false).is_ok());
    }

    #[test]
    fn accepts_single_ir_stage() {
        assert!(validate_output_modes(&[EmitStage::Air], false).is_ok());
    }

    #[test]
    fn accepts_multiple_non_deps_stages() {
        assert!(validate_output_modes(&[EmitStage::Air, EmitStage::Cfg], false).is_ok());
    }

    #[test]
    fn accepts_benchmark_json_without_emit() {
        assert!(validate_output_modes(&[], true).is_ok());
    }

    #[test]
    fn accepts_no_options() {
        assert!(validate_output_modes(&[], false).is_ok());
    }

    #[test]
    fn rejects_deps_with_other_stage() {
        let error = validate_output_modes(&[EmitStage::Deps, EmitStage::Air], false).unwrap_err();
        assert!(error.contains("--emit deps cannot be combined"));
    }

    #[test]
    fn rejects_other_stage_before_deps() {
        // Order-independent: deps discovered second must still be rejected.
        let error = validate_output_modes(&[EmitStage::Air, EmitStage::Deps], false).unwrap_err();
        assert!(error.contains("--emit deps cannot be combined"));
    }

    #[test]
    fn rejects_benchmark_json_with_emit() {
        let error = validate_output_modes(&[EmitStage::Air], true).unwrap_err();
        assert!(error.contains("--emit cannot be combined with --benchmark-json"));
    }

    #[test]
    fn rejects_benchmark_json_with_deps() {
        // A sole-deps stage is fine on its own but still conflicts with
        // --benchmark-json, since both write to stdout.
        let error = validate_output_modes(&[EmitStage::Deps], true).unwrap_err();
        assert!(error.contains("--emit cannot be combined with --benchmark-json"));
    }

    #[test]
    fn deps_conflict_wins_over_benchmark_json() {
        // When both rules apply, the mixed-deps message is reported first,
        // matching the pre-RUE-798 ordering in the driver.
        let error = validate_output_modes(&[EmitStage::Deps, EmitStage::Air], true).unwrap_err();
        assert!(error.contains("--emit deps cannot be combined"));
    }
}
