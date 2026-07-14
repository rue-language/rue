use rayon::prelude::*;
use tracing::{info, info_span};

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
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host()
                .expect("Rue cannot choose a default compile target on this unsupported host"),
            linker: LinkerMode::Internal,
            opt_level: OptLevel::default(),
            preview_features: PreviewFeatures::new(),
        }
    }
}

/// A function with its typed IR (AIR) and control flow graph (CFG).
///
/// This combines the output of semantic analysis with CFG construction.
#[derive(Debug)]
pub struct FunctionWithCfg {
    /// The analyzed function from semantic analysis.
    pub analyzed: AnalyzedFunction,
    /// The control flow graph built from the AIR.
    pub cfg: Cfg,
}

/// Intermediate compilation state after frontend processing.
///
/// This allows inspection of the IR at each stage, useful for
/// debugging and the `--emit` CLI flags.
#[cfg(test)]
pub(crate) struct CompileState {
    /// String interner used during compilation.
    pub interner: ThreadedRodeo,
    /// Analyzed functions with typed IR and control flow graphs.
    pub functions: Vec<FunctionWithCfg>,
    /// Type intern pool containing all struct and enum definitions.
    pub type_pool: TypeInternPool,
    /// Warnings collected during compilation.
    pub warnings: Vec<CompileWarning>,
}

/// Frontend artifacts after semantic analysis has been lowered to CFGs.
pub(crate) struct CfgFrontendOutput {
    /// Analyzed functions paired with their optimized CFGs.
    pub(crate) functions: Vec<FunctionWithCfg>,
    /// Type intern pool containing all struct and enum definitions.
    pub(crate) type_pool: TypeInternPool,
    /// String literals indexed by their AIR string_const index.
    pub(crate) strings: Vec<String>,
    /// Warnings collected during semantic analysis and CFG construction.
    pub(crate) warnings: Vec<CompileWarning>,
    pub(crate) implicit_named_destructor_dependencies:
        Vec<rue_air::ImplicitNamedDestructorDependencyEvent>,
    pub(crate) implicit_named_destructor_dependencies_complete: bool,
    pub(crate) work: canonical_semantic::CfgConstructionWork,
}

pub(crate) struct CfgConstructionFailure {
    pub(crate) errors: CompileErrors,
    pub(crate) work: canonical_semantic::CfgConstructionWork,
}

/// Lower semantic-analysis output through drop-glue synthesis, comptime filtering,
/// CFG construction, and CFG optimization.
///
/// This is shared by `CompilerSession` queries and `--emit` presentation
/// helpers so the live semantic tail stays in one place.
pub(crate) fn build_functions_and_cfgs(
    sema_output: SemaOutput,
    opt_level: OptLevel,
    interner: &ThreadedRodeo,
) -> Result<CfgFrontendOutput, CfgConstructionFailure> {
    let SemaOutput {
        functions,
        strings,
        mut warnings,
        type_pool,
        body_analysis_work: _,
        ..
    } = sema_output;

    // Synthesize drop glue functions.
    let drop_glue_functions = drop_glue::synthesize_drop_glue(&type_pool);
    let mut work = canonical_semantic::CfgConstructionWork {
        drop_glue_functions_synthesized: drop_glue_functions.len(),
        functions_considered: functions.len() + drop_glue_functions.len(),
        comptime_functions_filtered: functions
            .iter()
            .filter(|f| f.air.return_type() == Type::COMPTIME_TYPE)
            .count(),
        ..Default::default()
    };

    // Combine user functions with drop glue, filtering out comptime-only functions.
    let mut all_functions: Vec<_> = functions
        .into_iter()
        .filter(|f| f.air.return_type() != Type::COMPTIME_TYPE)
        .chain(drop_glue_functions)
        .collect();
    // Function order controls indexed Rayon collection, object-file order,
    // and final linker layout. Machine symbols are the stable semantic
    // identity shared by user, specialized, destructor, and glue functions.
    all_functions.sort_by(|left, right| left.name.cmp(&right.name));

    let _span = info_span!("cfg_construction").entered();

    let results: Vec<_> = all_functions
        .into_par_iter()
        .map(|func| {
            let dependency_source = func.implicit_drop_source.clone();
            let mut function_work = canonical_semantic::CfgConstructionWork {
                cfg_builds_attempted: 1,
                air_instructions_consumed: func.air.instructions().len(),
                ..Default::default()
            };
            let cfg_output = CfgBuilder::build(
                &func.air,
                func.num_locals,
                func.num_param_slots,
                &func.name,
                &type_pool,
                func.param_modes.clone(),
                interner,
                func.allow_unreachable_code,
            );

            // A non-empty `errors` means the CFG builder hit malformed AIR
            // (an internal compiler error, RUE-7). Abort before optimizing
            // the discarded CFG rather than working on it.
            if !cfg_output.errors.is_empty() {
                function_work.cfg_builds_failed = 1;
                let mut errs = CompileErrors::new();
                for e in cfg_output.errors {
                    errs.push(e);
                }
                return Err((errs, function_work));
            }

            function_work.cfg_builds_succeeded = 1;
            function_work.optimization_attempts = 1;
            function_work.optimized_level_attempts = usize::from(opt_level != OptLevel::O0);
            let mut cfg = cfg_output.cfg;
            rue_cfg::opt::optimize(&mut cfg, opt_level, &type_pool);
            function_work.optimization_completions = 1;
            function_work.cfg_warnings_emitted = cfg_output.warnings.len();
            function_work.implicit_destructor_targets_emitted =
                cfg_output.implicit_named_destructors.len();

            let mut implicit_edges = Vec::new();
            let mut complete = !cfg_output.anonymous_destructor_dependency_incomplete;
            // A named struct definition globally owns its synthesized glue.
            // Its own destructor is emitted as a direct AIR call rather than a
            // CFG Drop, so retain that definition -> destructor edge here.
            if let Some(rue_air::ImplicitDropDependencySourceEvent::NamedStruct { file, name }) =
                &dependency_source
            {
                for struct_id in type_pool.all_struct_ids() {
                    let target = type_pool.struct_def(struct_id);
                    if target.file_id.index() == *file
                        && target.name == *name
                        && target.destructor.is_some()
                        && !target.is_builtin
                    {
                        implicit_edges.push(rue_air::ImplicitNamedDestructorDependencyEvent {
                            source: dependency_source.clone().unwrap(),
                            target_file: *file,
                            target_owner_name: name.clone(),
                        });
                    }
                }
            }
            if !cfg_output.implicit_named_destructors.is_empty() {
                if matches!(
                    dependency_source,
                    Some(rue_air::ImplicitDropDependencySourceEvent::Anonymous)
                ) {
                    complete = false;
                } else if let Some(source) = dependency_source {
                    for struct_id in cfg_output.implicit_named_destructors {
                        let target = type_pool.struct_def(struct_id);
                        implicit_edges.push(rue_air::ImplicitNamedDestructorDependencyEvent {
                            source: source.clone(),
                            target_file: target.file_id.index(),
                            target_owner_name: target.name,
                        });
                    }
                }
            }

            Ok((
                (
                    FunctionWithCfg {
                        analyzed: func,
                        cfg,
                    },
                    cfg_output.warnings,
                    implicit_edges,
                    complete,
                ),
                function_work,
            ))
        })
        .collect();

    let mut functions = Vec::with_capacity(results.len());
    let mut implicit_named_destructor_dependencies = Vec::new();
    let mut implicit_named_destructor_dependencies_complete = true;
    let mut first_errors = None;
    for result in results {
        let (output, function_work) = match result {
            Ok((output, function_work)) => (Some(output), function_work),
            Err((errors, function_work)) => {
                if first_errors.is_none() {
                    first_errors = Some(errors);
                }
                (None, function_work)
            }
        };
        work.cfg_builds_attempted += function_work.cfg_builds_attempted;
        work.cfg_builds_succeeded += function_work.cfg_builds_succeeded;
        work.cfg_builds_failed += function_work.cfg_builds_failed;
        work.air_instructions_consumed += function_work.air_instructions_consumed;
        work.optimization_attempts += function_work.optimization_attempts;
        work.optimization_completions += function_work.optimization_completions;
        work.optimized_level_attempts += function_work.optimized_level_attempts;
        work.cfg_warnings_emitted += function_work.cfg_warnings_emitted;
        work.implicit_destructor_targets_emitted +=
            function_work.implicit_destructor_targets_emitted;
        if let Some((func, func_warnings, mut implicit_edges, complete)) = output {
            functions.push(func);
            warnings.extend(func_warnings);
            implicit_named_destructor_dependencies.append(&mut implicit_edges);
            implicit_named_destructor_dependencies_complete &= complete;
        }
    }
    if let Some(errors) = first_errors {
        return Err(CfgConstructionFailure { errors, work });
    }
    implicit_named_destructor_dependencies.sort();
    implicit_named_destructor_dependencies.dedup();

    info!(
        function_count = functions.len(),
        "CFG construction complete"
    );

    Ok(CfgFrontendOutput {
        functions,
        type_pool,
        strings,
        warnings,
        implicit_named_destructor_dependencies,
        implicit_named_destructor_dependencies_complete,
        work,
    })
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
    pub source_stats: SourceStats,
    /// Structural work performed by the session query graph.
    pub work: PipelineWork,
}

/// Compile an immutable owned source snapshot through a one-shot canonical
/// frontend session, then through the existing backend and linker boundary.
pub fn compile_snapshot(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    compile_snapshot_impl(snapshot, options)
}

/// Compile the closed-valid discovery revision already adopted by `session`.
///
/// This preserves the compiler-owned import graph and captured discovery
/// context instead of reconstructing a peer frontend from source bytes.
impl CompilerSession {
    /// Produce an executable from this session's closed-valid discovery revision.
    pub fn executable(&mut self, options: &CompileOptions) -> MultiErrorResult<CompileOutput> {
        let snapshot = self
            .committed_import_discovery()
            .ok_or_else(|| {
                CompileErrors::from(CompileError::without_span(ErrorKind::InvalidCompilerInput(
                    "compilation requires a closed-valid import discovery revision".into(),
                )))
            })?
            .snapshot()
            .clone();
        compile_with_session(self, &snapshot, options)
    }
}

fn compile_snapshot_impl(
    snapshot: &SourceSnapshot,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    // NOTE: the Rayon global thread pool is deliberately NOT configured here.
    // `build_global()` panics if called twice, and the `--emit` driver path
    // never reaches this function, so it was silently ignoring `-j`/`--jobs`
    // (RUE-352). The jobs setting is now applied once in the driver's `main()`
    // via `configure_thread_pool` before dispatching to any path.

    let mut session = CompilerSession::new();
    session.update_for_presentation(snapshot).into_result()?;
    compile_with_session(&mut session, snapshot, options)
}

pub(crate) fn compile_with_session(
    session: &mut CompilerSession,
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

    let rir = {
        let _span = info_span!("semantic_astgen").entered();
        session.rir()?
    };
    let semantic = session.semantic(options)?;
    let session_work = session.work();
    let mut output = crate::backend::compile_backend(
        semantic.functions(),
        semantic.type_pool(),
        semantic.strings(),
        rir.semantic_symbols().interner(),
        options,
        semantic.warnings(),
    )?;
    output.source_stats = SourceStats {
        files: snapshot.len(),
        bytes: total_source_bytes,
        lines: snapshot
            .files()
            .map(|source| source.source.lines().count())
            .sum(),
        tokens: session_work.last_parse.syntax.tokens,
    };
    output.work = PipelineWork {
        parsed: session_work.last_parse,
        merged: session_work.last_merge,
        lowered: session_work.last_rir,
        semantic: semantic.work(),
    };
    Ok(output)
}

#[cfg(test)]
mod failure_work_tests {
    use lasso::ThreadedRodeo;
    use rue_air::{Air, AirInst, AirInstData, Sema, Type};
    use rue_error::PreviewFeatures;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;
    use rue_span::Span;

    use super::*;

    fn malformed_cfg_input() -> (SemaOutput, ThreadedRodeo) {
        let source = "fn alpha() -> i32 { 1 }\nfn broken() -> i32 { 2 }\nfn main() -> i32 { alpha() + broken() + zeta() }\nfn zeta() -> i32 { 3 }";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let mut output = Sema::new(&rir, &interner, PreviewFeatures::new())
            .analyze_all()
            .unwrap();

        for (name, start) in [("broken", 10), ("zeta", 20)] {
            let generic = interner.get_or_intern(format!("unrewritten_{name}"));
            let function = output
                .functions
                .iter_mut()
                .find(|function| function.name == name)
                .unwrap();
            let mut air = Air::new(Type::I32);
            let call = air.add_inst(AirInst {
                data: AirInstData::CallGeneric {
                    name: generic,
                    type_args_start: 0,
                    type_args_len: 0,
                    value_args_start: 0,
                    value_args_len: 0,
                    args_start: 0,
                    args_len: 0,
                },
                ty: Type::I32,
                span: Span::new(start, start + 1),
            });
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(call)),
                ty: Type::I32,
                span: Span::new(start, start + 1),
            });
            function.air = air;
        }
        (output, interner)
    }

    #[test]
    fn malformed_air_retains_deterministic_work_from_every_cfg_builder() {
        let run = || {
            let (output, interner) = malformed_cfg_input();
            match build_functions_and_cfgs(output, OptLevel::O1, &interner) {
                Ok(_) => panic!("malformed AIR unexpectedly built a CFG"),
                Err(failure) => failure,
            }
        };
        let first = run();
        let second = run();
        assert_eq!(first.work, second.work);
        assert_eq!(first.work.functions_considered, 4);
        assert_eq!(first.work.cfg_builds_attempted, 4);
        assert_eq!(first.work.cfg_builds_succeeded, 2);
        assert_eq!(first.work.cfg_builds_failed, 2);
        assert_eq!(first.work.optimization_attempts, 2);
        assert_eq!(first.work.optimization_completions, 2);
        assert_eq!(first.work.optimized_level_attempts, 2);
        assert_eq!(
            format!("{:?}", first.errors),
            format!("{:?}", second.errors)
        );
        assert_eq!(
            first.errors.iter().next().unwrap().span().unwrap().start,
            10,
            "the first sorted failing function supplies the published diagnostic"
        );
    }
}
