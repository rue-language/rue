#[cfg(test)]
use rue_compiler::unstable::MetricsSnapshot;
#[cfg(test)]
use rue_compiler::unstable::update_for_presentation;
use rue_compiler::unstable::{
    ImportDiscoveryRevision, ImportDiscoveryStatus, LowerMetrics, ParseMetrics,
    PresentationRequest, PresentationStage, SemanticMetrics, semantic_metrics,
};
use rue_compiler::{
    CompileError, CompileErrors, CompileOptions, CompileWarning, CompilerSession,
    DependencyEnvelope, DependencyEnvelopeStatus, ErrorKind, RirView, SemanticView, SourceSnapshot,
    SyntaxView,
};
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
    None,
    AstOnlySyntax,
    SessionQuery,
}

pub(crate) fn emit_frontend_route(stages: &[EmitStage]) -> EmitFrontendRoute {
    if stages.iter().any(|stage| {
        matches!(
            stage,
            EmitStage::Rir
                | EmitStage::Air
                | EmitStage::Cfg
                | EmitStage::Lowering
                | EmitStage::Mir
                | EmitStage::Liveness
                | EmitStage::RegAlloc
                | EmitStage::Asm
                | EmitStage::StackFrame
        )
    }) {
        EmitFrontendRoute::SessionQuery
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

/// Handle emit stages for multi-file compilation.
///
/// For early stages (tokens, ast), each file is processed and labeled individually.
/// For later stages (rir, air, cfg, etc.), the merged program is used.
pub(crate) struct EmitRequest<'a, 'diagnostics> {
    pub(crate) source_snapshot: &'a SourceSnapshot,
    pub(crate) session: &'a mut CompilerSession,
    pub(crate) stages: &'a [EmitStage],
    pub(crate) discovery_revision: &'a ImportDiscoveryRevision,
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
        if stages.len() != 1 {
            eprintln!("Error: --emit deps cannot be combined with other --emit stages");
            return Err(());
        }
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
    let frontend_state = if frontend_route == EmitFrontendRoute::SessionQuery {
        let frontend = match build_emit_frontend_in_session(session, compile_options.clone()) {
            Ok(frontend) => frontend,
            Err(errors) => {
                diagnostics.print_errors(&errors);
                return Err(());
            }
        };
        Some(frontend)
    } else {
        None
    };
    if let Some(state) = &frontend_state {
        // Every --emit mode must surface frontend warnings (RUE-130).
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
