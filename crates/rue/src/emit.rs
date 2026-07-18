use std::fmt::Write as _;
use std::sync::Arc;

#[cfg(test)]
use rue_compiler::unstable::MetricsSnapshot;
use rue_compiler::unstable::{
    ImportDiscoveryRevision, ImportDiscoveryStatus, LowerMetrics, ParseMetrics, SemanticMetrics,
};
use rue_compiler::{
    Ast, CanonicalRirOutput, CanonicalSemanticOutput, CompileError, CompileErrors, CompileOptions,
    CompileWarning, CompilerSession, DependencyEnvelope, DependencyEnvelopeStatus, ErrorKind,
    FrozenTypeInternPool, Lexer, SourceSnapshot, Token, generate_emitted_asm,
    generate_liveness_info, generate_lowering_info, generate_mir, generate_regalloc_info,
    generate_stack_frame_info, parse_source_snapshot_for_ast_presentation,
};
use rue_rir::RirPrinter;
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
    parsed: Arc<rue_compiler::ParsedProgram>,
    rir: Arc<CanonicalRirOutput>,
    semantic: Arc<CanonicalSemanticOutput>,
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
    let parsed = session.published().cloned().ok_or_else(|| {
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
        rir,
        semantic: semantic.clone(),
        work: EmitWork {
            parsed: session_work.parse_metrics(),
            lowered: session_work.lower_metrics(),
            semantic: semantic.unstable_metrics(),
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
    session
        .update_for_presentation(source_snapshot)
        .into_result()?;
    build_emit_frontend_in_session(&mut session, options)
}

impl EmitFrontend {
    fn rir(&self) -> &rue_compiler::Rir {
        self.rir.rir()
    }

    fn interner(&self) -> &rue_compiler::ThreadedRodeo {
        self.rir.semantic_symbols().interner()
    }

    fn functions(&self) -> &[rue_compiler::FunctionWithCfg] {
        self.semantic.functions()
    }

    fn type_pool(&self) -> &FrozenTypeInternPool {
        self.semantic.type_pool()
    }

    fn strings(&self) -> &[String] {
        self.semantic.strings()
    }

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
    // Determine which stages we need
    let needs_tokens = stages.contains(&EmitStage::Tokens);
    let needs_ast = stages.contains(&EmitStage::Ast);

    // For tokens, we need to lex each file separately (before parsing merges interners)
    // We'll collect per-file tokens if needed
    let per_file_tokens: Option<Vec<(String, Vec<Token>)>> = if needs_tokens {
        let mut file_tokens = Vec::with_capacity(source_snapshot.len());
        for source in source_snapshot.files() {
            // Lex with the file's real FileId so a lex error in the Nth file
            // is attributed to that file, not to the first one (RUE-38).
            let lexer = Lexer::with_file_id(source.source, source.file_id);
            match lexer.tokenize_preserving_interner() {
                Ok((tokens, _interner)) => {
                    file_tokens.push((source.path.to_string(), tokens));
                }
                Err((errors, _interner)) => {
                    diagnostics.print_errors(&errors);
                    return Err(());
                }
            }
        }
        Some(file_tokens)
    } else {
        None
    };

    let frontend_route = emit_frontend_route(stages);
    // AST-only preserves its syntax-only behavior (duplicates are printable).
    // Combined AST+later canonical modes reuse the unit's once-only projection.
    let mut per_file_asts: Option<Vec<(String, std::sync::Arc<Ast>)>> =
        if frontend_route == EmitFrontendRoute::AstOnlySyntax {
            match parse_source_snapshot_for_ast_presentation(source_snapshot) {
                Ok(presentation) => {
                    debug_assert_eq!(
                        presentation.work().parsed.syntax.parser_invocations,
                        source_snapshot.len()
                    );
                    Some(presentation.files().to_vec())
                }
                Err(errors) => {
                    diagnostics.print_errors(&errors);
                    return Err(());
                }
            }
        } else {
            None
        };

    let frontend_state = if frontend_route == EmitFrontendRoute::SessionQuery {
        let frontend = match build_emit_frontend_in_session(session, compile_options.clone()) {
            Ok(frontend) => frontend,
            Err(errors) => {
                diagnostics.print_errors(&errors);
                return Err(());
            }
        };
        if needs_ast {
            per_file_asts = Some(
                source_snapshot
                    .files()
                    .map(|source| {
                        let module = frontend
                            .parsed
                            .modules()
                            .iter()
                            .find(|module| module.file_id() == source.file_id)
                            .expect("frontend parsed every snapshot source");
                        (source.path.to_string(), module.shared_ast())
                    })
                    .collect(),
            );
        }
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

    // Now emit in order
    for stage in stages {
        match stage {
            EmitStage::Tokens => {
                if let Some(ref file_tokens) = per_file_tokens {
                    for (path, tokens) in file_tokens {
                        println!("=== Tokens ({}) ===", path);
                        for token in tokens {
                            println!("{}", token);
                        }
                        println!();
                    }
                }
            }
            EmitStage::Ast => {
                if let Some(ref asts) = per_file_asts {
                    for (path, ast) in asts {
                        println!("=== AST ({}) ===", path);
                        print!("{}", ast);
                        println!();
                    }
                }
            }
            EmitStage::Rir => {
                println!("=== RIR ===");
                if let Some(ref state) = frontend_state {
                    let order = state
                        .rir
                        .presentation_order(source_snapshot.files().map(|source| source.file_id));
                    let printer = RirPrinter::with_presentation_order(
                        state.rir(),
                        state.interner(),
                        order.instructions,
                        order.extra,
                    );
                    println!("{}", printer);
                }
                println!();
            }
            EmitStage::Air => {
                println!("=== AIR ===");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        println!("function {}:", func.analyzed.name);
                        println!(
                            "{}",
                            func.analyzed.air.display_with_interner(state.interner())
                        );
                    }
                }
                println!();
            }
            EmitStage::Cfg => {
                println!("=== CFG ===");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        println!("{}", func.cfg.display_with_interner(state.interner()));
                    }
                }
                println!();
            }
            EmitStage::Lowering => {
                let mut output = String::new();
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let lowering_info = match generate_lowering_info(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            compile_options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                diagnostics.print_error(&e);
                                return Err(());
                            }
                        };
                        write!(&mut output, "{}", lowering_info).expect("write to String");
                    }
                }
                output.push('\n');
                print!("{}", output);
            }
            EmitStage::Mir => {
                let mut output = String::new();
                writeln!(&mut output, "=== MIR ({}) ===", compile_options.target)
                    .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let mir = match generate_mir(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            compile_options.target,
                        ) {
                            Ok(mir) => mir,
                            Err(e) => {
                                diagnostics.print_error(&e);
                                return Err(());
                            }
                        };
                        writeln!(&mut output, "function {}:", func.analyzed.name)
                            .expect("write to String");
                        writeln!(&mut output, "{}", mir).expect("write to String");
                    }
                }
                output.push('\n');
                print!("{}", output);
            }
            EmitStage::Liveness => {
                let mut output = String::new();
                writeln!(
                    &mut output,
                    "=== Liveness Analysis ({}) ===",
                    compile_options.target
                )
                .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let liveness_info = match generate_liveness_info(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            compile_options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                diagnostics.print_error(&e);
                                return Err(());
                            }
                        };
                        writeln!(&mut output, "function {}:", func.analyzed.name)
                            .expect("write to String");
                        writeln!(&mut output, "{}", liveness_info).expect("write to String");
                    }
                }
                output.push('\n');
                print!("{}", output);
            }
            EmitStage::RegAlloc => {
                let mut output = String::new();
                writeln!(
                    &mut output,
                    "=== Register Allocation ({}) ===",
                    compile_options.target
                )
                .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let regalloc_info = match generate_regalloc_info(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            compile_options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                diagnostics.print_error(&e);
                                return Err(());
                            }
                        };
                        writeln!(&mut output, "function {}:", func.analyzed.name)
                            .expect("write to String");
                        write!(&mut output, "{}", regalloc_info).expect("write to String");
                    }
                }
                output.push('\n');
                print!("{}", output);
            }
            EmitStage::Asm => {
                let mut output = String::new();
                writeln!(&mut output, "=== Assembly ({}) ===", compile_options.target)
                    .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let asm = match generate_emitted_asm(
                            &func.cfg,
                            state.type_pool(),
                            state.strings(),
                            state.interner(),
                            compile_options.target,
                        ) {
                            Ok(asm) => asm,
                            Err(e) => {
                                diagnostics.print_error(&e);
                                return Err(());
                            }
                        };
                        writeln!(&mut output, ".globl {}", func.analyzed.name)
                            .expect("write to String");
                        writeln!(&mut output, "{}:", func.analyzed.name).expect("write to String");
                        write!(&mut output, "{}", asm).expect("write to String");
                    }
                }
                output.push('\n');
                print!("{}", output);
            }
            EmitStage::StackFrame => {
                let mut output = String::new();
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let frame_info = match generate_stack_frame_info(
                            &func.cfg,
                            &func.analyzed.name,
                            state.type_pool(),
                            state.interner(),
                            compile_options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                diagnostics.print_error(&e);
                                return Err(());
                            }
                        };
                        writeln!(&mut output, "{}", frame_info).expect("write to String");
                    }
                }
                print!("{}", output);
            }
            // Dependency presentation returns before frontend planning above.
            EmitStage::Deps => {}
        }
    }

    Ok(())
}
