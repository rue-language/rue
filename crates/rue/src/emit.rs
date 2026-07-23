#[cfg(test)]
use rue_compiler::unstable::MetricsSnapshot;
#[cfg(test)]
use rue_compiler::unstable::update_for_presentation;
use rue_compiler::unstable::{
    LowerMetrics, ParseMetrics, PresentationRequest, PresentationStage, SemanticMetrics,
    semantic_metrics,
};
use rue_compiler::{
    CompileErrors, CompileOptions, CompileWarning, CompilerSession, DependencyEnvelope,
    DependencyEnvelopeStatus, ImportDiscoveryStatus, ImportDiscoveryView, RirView, SemanticView,
    SourceSnapshot, SyntaxView,
};
use rue_error::{CompileError, ErrorKind};
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
    /// Emit the source dependency graph discovered while loading imports.
    Deps,
}

pub(crate) struct EmitFrontend {
    parsed: SyntaxView,
    _rir: std::sync::Arc<RirView>,
    semantic: std::sync::Arc<SemanticView>,
    pub(crate) work: EmitWork,
    #[cfg(test)]
    pub(crate) session_work: MetricsSnapshot,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EmitWork {
    pub(crate) parsed: ParseMetrics,
    pub(crate) lowered: LowerMetrics,
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

pub(crate) fn build_emit_frontend_in_session(
    session: &mut CompilerSession,
    options: CompileOptions,
) -> Result<EmitFrontend, CompileErrors> {
    let parsed = session.published().ok_or_else(|| {
        CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
            "emit requires a published closed discovery revision".into(),
        )))
    })?;
    let rir = {
        let _span = info_span!("semantic_astgen").entered();
        session.rir()?
    };
    let semantic = session.semantic(&options)?;
    let session_work = session.unstable_metrics();
    Ok(EmitFrontend {
        parsed,
        _rir: rir,
        semantic: semantic.clone(),
        work: EmitWork {
            parsed: session_work.parse_metrics(),
            lowered: session_work.lower_metrics(),
            semantic: semantic_metrics(&semantic),
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

impl EmitFrontend {
    fn warnings(&self) -> &[CompileWarning] {
        self.semantic.warnings()
    }

    fn query_work(&self) -> EmitWork {
        self.work
    }
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
            "deps" => Ok(EmitStage::Deps),
            _ => Err(ParseEmitStageError(s.to_string())),
        }
    }
}

impl EmitStage {
    pub(crate) fn all_names() -> &'static str {
        "tokens, ast, rir, air, cfg, lowering, mir, liveness, regalloc, asm, stackframe, deps"
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
    pub(crate) source_snapshot: &'a SourceSnapshot,
    pub(crate) session: &'a mut CompilerSession,
    pub(crate) stages: &'a [EmitStage],
    pub(crate) discovery_revision: &'a ImportDiscoveryView,
    pub(crate) compile_options: CompileOptions,
    pub(crate) diagnostics: &'a DiagnosticOutput<'diagnostics>,
}

pub(crate) fn execute(request: EmitRequest<'_, '_>) -> Result<(), ()> {
    let EmitRequest {
        source_snapshot,
        session,
        stages,
        discovery_revision,
        compile_options,
        diagnostics,
    } = request;

    if stages.contains(&EmitStage::Deps) {
        // `validate_output_modes` already rejected `--emit deps` mixed with any
        // other stage before this point, so only the sole-deps case reaches here.
        debug_assert_eq!(
            stages.len(),
            1,
            "validate_output_modes must reject --emit deps combined with other stages"
        );
        let dependency_envelope = DependencyEnvelope::from_closed_revision(discovery_revision)
            .expect("closed valid or resolution-incomplete discovery has dependency topology");
        let incomplete = dependency_envelope.status == DependencyEnvelopeStatus::Incomplete;
        match serde_json::to_string_pretty(&dependency_envelope) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("Error emitting dependency envelope: {error}");
                return Err(());
            }
        }
        if incomplete {
            diagnostics.print_errors(discovery_revision.diagnostics());
            return Err(());
        }
        return Ok(());
    }

    if discovery_revision.status() != ImportDiscoveryStatus::ClosedValid {
        diagnostics.print_errors(discovery_revision.diagnostics());
        return Err(());
    }
    let frontend_route = emit_frontend_route(stages);
    let frontend_state = match frontend_route {
        EmitFrontendRoute::SessionQuery => {
            let frontend = match build_emit_frontend_in_session(session, compile_options.clone()) {
                Ok(frontend) => frontend,
                Err(errors) => {
                    diagnostics.print_errors(&errors);
                    return Err(());
                }
            };
            Some(frontend)
        }
        EmitFrontendRoute::RirOnly => {
            // RIR is a pre-semantic presentation: force the RIR terminal so its
            // parse/lowering diagnostics are attributed canonically, but never run
            // semantic body analysis. Because the trusted-toolchain park is raised
            // only by reached-body semantic analysis, this performs no park and no
            // std acquisition — an `--emit rir` run reads zero std modules even for
            // a reached fallible intrinsic. Semantic-only diagnostics (e.g. an
            // undefined variable) belong to a normal build or a later `--emit`
            // stage, not to the untyped-IR presentation.
            if let Err(errors) = session.rir() {
                diagnostics.print_errors(&errors);
                return Err(());
            }
            None
        }
        EmitFrontendRoute::AstOnlySyntax | EmitFrontendRoute::None => None,
    };
    if let Some(state) = &frontend_state {
        // Every semantic-frontend --emit mode must surface frontend warnings
        // (RUE-130). Pre-semantic presentations (tokens/ast/rir) have no semantic
        // warnings to surface.
        diagnostics.print_warnings(state.warnings());
        let work = state.query_work();
        debug_assert_eq!(state.parsed.modules().len(), source_snapshot.len());
        debug_assert!(work.parsed.parser_invocations <= source_snapshot.len());
        debug_assert_eq!(work.lowered.parser_invocations, 0);
        debug_assert_eq!(work.semantic.binding.bind_invocations, 1);
        debug_assert_eq!(work.semantic.manifest.build_invocations, 1);
    }

    let file_order = source_snapshot
        .files()
        .map(|source| source.file_id)
        .collect::<Vec<_>>();

    for stage in stages {
        let unstable_stage = match stage {
            EmitStage::Tokens => PresentationStage::Tokens,
            EmitStage::Ast => PresentationStage::Ast,
            EmitStage::Rir => PresentationStage::Rir,
            EmitStage::Air => PresentationStage::Air,
            EmitStage::Cfg => PresentationStage::Cfg,
            EmitStage::Lowering => PresentationStage::Lowering,
            EmitStage::Mir => PresentationStage::Mir,
            EmitStage::Liveness => PresentationStage::Liveness,
            EmitStage::RegAlloc => PresentationStage::RegAlloc,
            EmitStage::Asm => PresentationStage::Asm,
            EmitStage::StackFrame => PresentationStage::StackFrame,
            EmitStage::Deps => continue,
        };
        if matches!(stage, EmitStage::Tokens | EmitStage::Ast) {
            for source in source_snapshot.files() {
                match stage {
                    EmitStage::Tokens => println!("=== Tokens ({}) ===", source.path),
                    EmitStage::Ast => println!("=== AST ({}) ===", source.path),
                    _ => unreachable!(),
                }
                let output = match session.unstable_present(PresentationRequest {
                    stage: unstable_stage,
                    options: &compile_options,
                    file_order: &[source.file_id],
                }) {
                    Ok(output) => output,
                    Err(errors) => {
                        diagnostics.print_errors(&errors);
                        return Err(());
                    }
                };
                print!("{}", output.as_str());
                println!();
            }
            continue;
        }
        let output = match session.unstable_present(PresentationRequest {
            stage: unstable_stage,
            options: &compile_options,
            file_order: &file_order,
        }) {
            Ok(output) => output,
            Err(errors) => {
                diagnostics.print_errors(&errors);
                return Err(());
            }
        };
        match stage {
            EmitStage::Tokens | EmitStage::Ast => unreachable!(),
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
            EmitStage::Lowering => {
                println!("{}", output.as_str());
            }
            EmitStage::Mir => {
                println!("=== MIR ({}) ===", compile_options.target);
                println!("{}", output.as_str());
            }
            EmitStage::Liveness => {
                println!("=== Liveness Analysis ({}) ===", compile_options.target);
                println!("{}", output.as_str());
            }
            EmitStage::RegAlloc => {
                println!("=== Register Allocation ({}) ===", compile_options.target);
                println!("{}", output.as_str());
            }
            EmitStage::Asm => {
                println!("=== Assembly ({}) ===", compile_options.target);
                println!("{}", output.as_str());
            }
            EmitStage::StackFrame => {
                print!("{}", output.as_str());
            }
            // Dependency presentation returns before frontend planning above.
            EmitStage::Deps => {}
        }
    }

    Ok(())
}

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
