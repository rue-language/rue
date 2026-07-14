use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::Arc;

use tracing::{Level, info_span};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{EnvFilter, Layer as _, fmt};

mod timing;

#[cfg(test)]
use rue_compiler::CompilerSessionWork;
use rue_compiler::{
    AcceptedImportSource, Ast, CanonicalRirOutput, CanonicalSemanticOutput, CompileError,
    CompileErrors, CompileOptions, CompileWarning, CompilerSession, DiscoverySourceAssembler,
    ErrorKind, FileId, FileMetadataFingerprint, ImportDiscoveryContext,
    ImportDiscoveryRevisionArtifact, ImportObservation, ImportObservationLedger,
    ImportObservationStatus, Lexer, LinkerMode, MultiFileFormatter, MultiFileJsonFormatter,
    OptLevel, PhysicalFileIdentity, PipelineWork, PreviewFeature, PreviewFeatures, SourceInfo,
    SourceMetadata, SourceSnapshot, Token, TypeInternPool, configure_thread_pool,
    generate_emitted_asm, generate_liveness_info, generate_lowering_info, generate_mir,
    generate_regalloc_info, generate_stack_frame_info, parse_source_snapshot_for_ast_presentation,
};
use rue_rir::RirPrinter;
use rue_target::Target;
use serde::Serialize;

/// Compilation stages that can be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmitStage {
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

struct EmitFrontend {
    parsed: Arc<rue_compiler::ParsedProgram>,
    rir: Arc<CanonicalRirOutput>,
    semantic: Arc<CanonicalSemanticOutput>,
    work: PipelineWork,
    #[cfg(test)]
    session_work: CompilerSessionWork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmitFrontendRoute {
    None,
    AstOnlySyntax,
    SessionQuery,
}

fn emit_frontend_route(stages: &[EmitStage]) -> EmitFrontendRoute {
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

fn build_emit_frontend_in_session(
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
    let session_work = session.work().clone();
    Ok(EmitFrontend {
        parsed,
        rir,
        semantic: semantic.clone(),
        work: PipelineWork {
            parsed: session_work.last_parse,
            merged: session_work.last_merge,
            lowered: session_work.last_rir,
            semantic: semantic.work(),
        },
        #[cfg(test)]
        session_work,
    })
}

#[cfg(test)]
fn build_emit_frontend(
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

    fn type_pool(&self) -> &TypeInternPool {
        self.semantic.type_pool()
    }

    fn strings(&self) -> &[String] {
        self.semantic.strings()
    }

    fn warnings(&self) -> &[CompileWarning] {
        self.semantic.warnings()
    }

    fn query_work(&self) -> PipelineWork {
        self.work
    }
}

/// Error returned when parsing an emit stage name fails.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParseEmitStageError(String);

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
    fn all_names() -> &'static str {
        "tokens, ast, rir, air, cfg, lowering, mir, liveness, regalloc, asm, stackframe, deps"
    }
}

/// Log level for tracing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LogLevel {
    /// No logging output (default).
    #[default]
    Off,
    /// Only errors.
    Error,
    /// Errors and warnings.
    Warn,
    /// Errors, warnings, and info.
    Info,
    /// Errors, warnings, info, and debug.
    Debug,
    /// All logging including trace.
    Trace,
}

/// Error returned when parsing a log level fails.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParseLogLevelError(String);

impl std::fmt::Display for ParseLogLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown log level '{}'", self.0)
    }
}

impl std::error::Error for ParseLogLevelError {}

impl std::str::FromStr for LogLevel {
    type Err = ParseLogLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "off" => Ok(LogLevel::Off),
            "error" => Ok(LogLevel::Error),
            "warn" => Ok(LogLevel::Warn),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            "trace" => Ok(LogLevel::Trace),
            _ => Err(ParseLogLevelError(s.to_string())),
        }
    }
}

impl LogLevel {
    fn all_names() -> &'static str {
        "off, error, warn, info, debug, trace"
    }

    /// Convert to tracing Level, returns None for Off.
    fn to_tracing_level(self) -> Option<Level> {
        match self {
            LogLevel::Off => None,
            LogLevel::Error => Some(Level::ERROR),
            LogLevel::Warn => Some(Level::WARN),
            LogLevel::Info => Some(Level::INFO),
            LogLevel::Debug => Some(Level::DEBUG),
            LogLevel::Trace => Some(Level::TRACE),
        }
    }
}

/// Log format for tracing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LogFormat {
    /// Human-readable text format (default).
    #[default]
    Text,
    /// Machine-readable JSON format.
    Json,
}

/// Error returned when parsing a log format fails.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParseLogFormatError(String);

impl std::fmt::Display for ParseLogFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown log format '{}'", self.0)
    }
}

impl std::error::Error for ParseLogFormatError {}

impl std::str::FromStr for LogFormat {
    type Err = ParseLogFormatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(LogFormat::Text),
            "json" => Ok(LogFormat::Json),
            _ => Err(ParseLogFormatError(s.to_string())),
        }
    }
}

impl LogFormat {
    fn all_names() -> &'static str {
        "text, json"
    }
}

/// Format for compiler diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ErrorFormat {
    /// Human-readable diagnostics with source snippets.
    #[default]
    Text,
    /// Machine-readable JSON diagnostics.
    Json,
}

/// Error returned when parsing a diagnostic format fails.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParseErrorFormatError(String);

impl std::fmt::Display for ParseErrorFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown error format '{}'", self.0)
    }
}

impl std::error::Error for ParseErrorFormatError {}

impl std::str::FromStr for ErrorFormat {
    type Err = ParseErrorFormatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(ErrorFormat::Text),
            "json" => Ok(ErrorFormat::Json),
            _ => Err(ParseErrorFormatError(s.to_string())),
        }
    }
}

impl ErrorFormat {
    fn all_names() -> &'static str {
        "text, json"
    }
}

struct Options {
    /// Source files named positionally on the command line. The first path is
    /// the root source; additional paths are legacy flat-mode inputs and must
    /// not also be reachable through `@import`.
    source_paths: Vec<String>,
    /// Optional build-system-facing manifest of source files the compiler may
    /// read while resolving the root module's import graph.
    source_manifest_path: Option<String>,
    output_path: String,
    emit_stages: Vec<EmitStage>,
    target: Target,
    linker: LinkerMode,
    opt_level: OptLevel,
    preview_features: PreviewFeatures,
    log_level: LogLevel,
    log_format: LogFormat,
    error_format: ErrorFormat,
    time_passes: bool,
    benchmark_json: bool,
    /// Number of parallel jobs (0 = auto-detect, use all cores).
    jobs: usize,
}

/// Version string for the rue compiler (single-sourced from rue-error so the
/// `--version` banner and ICE reports can never drift apart).
const VERSION: &str = rue_compiler::VERSION;

/// Highest explicit `-j`/`--jobs` value accepted by the CLI.
///
/// `0` still means auto-detect. A fixed generous ceiling catches accidental
/// values like `-j 100000` before Rayon tries to spawn an impractical number
/// of worker threads, while still leaving ample room above current CI and
/// workstation core counts.
const MAX_EXPLICIT_JOBS: usize = 256;

fn print_version() {
    println!("rue {}", VERSION);
}

/// Render the usage/help text. Kept as a builder so the caller picks the
/// stream: an explicit `--help` request writes to stdout (normal CLI
/// convention, like `--version`), while usage attached to an argument error
/// writes to stderr (RUE-518).
fn usage_text() -> String {
    format!(
        "\
Usage: rue [options] <source.rue> [output]
       rue [options] <root.rue> -o <output>

Options:
  -o, --output <path>  Set output path
  --source-manifest <path>
                       Restrict source imports to a line-oriented manifest
  --target <target>    Set compilation target (default: host)
                       Valid targets: {targets}
  --linker <linker>    Set linker to use (default: internal)
                       Use 'internal' for built-in linker, or a command
                       like 'clang', 'gcc', or 'ld' for system linker
  -O<level>            Set optimization level (default: -O0)
                       Levels: {opt_levels}
  -j, --jobs <N>       Set number of parallel jobs (default: 0 = auto; max: {MAX_EXPLICIT_JOBS})
                       Use -j1 for single-threaded compilation
  --emit <stage>       Emit intermediate representation and exit
                       Can be specified multiple times for multiple outputs
                       Stages: {emit_stages}
  --preview <feature>  Enable a preview feature (can be repeated)
                       Features: {preview_features}
  --log-level <level>  Set logging level (default: off)
                       Levels: {log_levels}
                       Can also use RUST_LOG environment variable
  --log-format <fmt>   Set logging format (default: text)
                       Formats: {log_formats}
  --error-format <fmt> Set diagnostic format (default: text)
                       Formats: {error_formats}
  --time-passes        Show timing for each compilation pass
  --benchmark-json     Output timing as JSON (for benchmarking)
  --version            Show version information
  --help               Show this help message",
        targets = Target::all_names(),
        opt_levels = OptLevel::all_names(),
        emit_stages = EmitStage::all_names(),
        preview_features = PreviewFeature::all_names(),
        log_levels = LogLevel::all_names(),
        log_formats = LogFormat::all_names(),
        error_formats = ErrorFormat::all_names(),
    )
}

/// Write usage to stderr, for the argument-error paths.
fn print_usage() {
    eprintln!("{}", usage_text());
}

/// Write help to stdout, for an explicit successful `--help` request (RUE-518).
fn print_help() {
    println!("{}", usage_text());
}

/// Result of parsing command-line arguments.
enum ParseResult {
    /// Successfully parsed options.
    Options(Options),
    /// Parsing failed with an error.
    Error,
    /// User requested help or version (already printed, should exit 0).
    Exit,
}

/// Resolve a path to a canonical key for the output-clobber comparison.
///
/// Sources always exist, so they canonicalize directly. The output file
/// usually does NOT exist yet, so `canonicalize` fails on it; in that case we
/// canonicalize the parent directory and re-attach the file name, which still
/// collapses `./prog` and `prog` (or `dir/../prog`) to the same key. When even
/// the parent can't be resolved — as in unit tests with fake paths that touch
/// no real files — we fall back to the raw path, preserving the old
/// exact-string behavior. Extension is irrelevant; only the resolved location
/// matters (RUE-351).
fn clobber_key(path: &str) -> PathBuf {
    let p = Path::new(path);
    if let Ok(canon) = fs::canonicalize(p) {
        return canon;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => {
            // An empty parent means the path is a bare file name in the cwd.
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            match fs::canonicalize(parent) {
                Ok(canon_parent) => canon_parent.join(name),
                Err(_) => p.to_path_buf(),
            }
        }
        _ => p.to_path_buf(),
    }
}

/// Would writing the output destroy the source file at `source`?
///
/// Two complementary checks (RUE-527):
/// - resolved-path equality via [`clobber_key`], which collapses spellings
///   and symlinks — and works when the output does not exist yet;
/// - device+inode equality, which catches HARD links: `ln main.rue program`
///   gives two distinct canonical names for one shared inode, so writing
///   `program` destroys `main.rue`. Only checkable when the output exists;
///   `output_meta` is its metadata (`None` when it doesn't exist).
fn output_would_clobber(
    output_key: &Path,
    output_meta: Option<&fs::Metadata>,
    source: &str,
) -> bool {
    if clobber_key(source) == *output_key {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Some(out_meta), Ok(src_meta)) = (output_meta, fs::metadata(source)) {
            return out_meta.dev() == src_meta.dev() && out_meta.ino() == src_meta.ino();
        }
    }
    false
}

/// Refuse an output path that names (or hard-links) any source of the
/// compilation. `sources` is whatever set is known at the call site: the
/// positional paths at argument-parse time, and the full import-discovered
/// set later — imports are appended after parsing, so the guard must run
/// again once they are known (RUE-527).
fn check_output_clobbers_source<'a>(
    output_path: &str,
    sources: impl IntoIterator<Item = &'a str>,
) -> Result<(), ()> {
    let output_key = clobber_key(output_path);
    let output_meta = fs::metadata(output_path).ok();
    for source in sources {
        if output_would_clobber(&output_key, output_meta.as_ref(), source) {
            eprintln!(
                "Error: output path '{output_path}' is also an input source file; \
                 refusing to overwrite it"
            );
            return Err(());
        }
    }
    Ok(())
}

fn parse_jobs_value(jobs_str: &str) -> Option<usize> {
    let jobs = match jobs_str.parse::<usize>() {
        Ok(jobs) => jobs,
        Err(_) => {
            eprintln!("Error: --jobs value must be a non-negative integer");
            return None;
        }
    };

    if jobs > MAX_EXPLICIT_JOBS {
        eprintln!(
            "Error: --jobs value {jobs} is too large; maximum explicit value is {MAX_EXPLICIT_JOBS} (use 0 for auto-detect)"
        );
        return None;
    }

    Some(jobs)
}

/// Parse arguments from a slice of strings (for testing).
fn parse_args_from(args: &[&str]) -> ParseResult {
    if args.is_empty() {
        print_usage();
        return ParseResult::Error;
    }

    let mut emit_stages = Vec::new();
    let mut target: Option<Target> = None;
    let mut linker: Option<LinkerMode> = None;
    let mut opt_level: Option<OptLevel> = None;
    let mut preview_features = PreviewFeatures::new();
    let mut log_level: Option<LogLevel> = None;
    let mut log_format: Option<LogFormat> = None;
    let mut error_format: Option<ErrorFormat> = None;
    let mut time_passes = false;
    let mut benchmark_json = false;
    let mut jobs: Option<usize> = None;
    let mut source_manifest_path: Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut positional = Vec::new();
    let mut args_iter = args.iter().peekable();

    while let Some(arg) = args_iter.next() {
        match *arg {
            "--emit" => {
                let Some(stage_str) = args_iter.next() else {
                    eprintln!("Error: --emit requires a value");
                    eprintln!("Valid stages: {}", EmitStage::all_names());
                    return ParseResult::Error;
                };
                match stage_str.parse::<EmitStage>() {
                    Ok(stage) => emit_stages.push(stage),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid stages: {}", EmitStage::all_names());
                        return ParseResult::Error;
                    }
                }
            }
            "--target" => {
                let Some(target_str) = args_iter.next() else {
                    eprintln!("Error: --target requires a value");
                    eprintln!("Valid targets: {}", Target::all_names());
                    return ParseResult::Error;
                };
                match target_str.parse::<Target>() {
                    Ok(t) => target = Some(t),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return ParseResult::Error;
                    }
                }
            }
            "--linker" => {
                let Some(linker_str) = args_iter.next() else {
                    eprintln!("Error: --linker requires a value");
                    eprintln!("Use 'internal' or a system linker command like 'clang'");
                    return ParseResult::Error;
                };
                linker = Some(if *linker_str == "internal" {
                    LinkerMode::Internal
                } else {
                    LinkerMode::System(linker_str.to_string())
                });
            }
            "--preview" => {
                let Some(feature_str) = args_iter.next() else {
                    eprintln!("Error: --preview requires a feature name");
                    eprintln!("Available features: {}", PreviewFeature::all_names());
                    return ParseResult::Error;
                };
                match feature_str.parse::<PreviewFeature>() {
                    Ok(feature) => {
                        preview_features.insert(feature);
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Available features: {}", PreviewFeature::all_names());
                        return ParseResult::Error;
                    }
                }
            }
            "--log-level" => {
                let Some(level_str) = args_iter.next() else {
                    eprintln!("Error: --log-level requires a value");
                    eprintln!("Valid levels: {}", LogLevel::all_names());
                    return ParseResult::Error;
                };
                match level_str.parse::<LogLevel>() {
                    Ok(level) => log_level = Some(level),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid levels: {}", LogLevel::all_names());
                        return ParseResult::Error;
                    }
                }
            }
            "--log-format" => {
                let Some(format_str) = args_iter.next() else {
                    eprintln!("Error: --log-format requires a value");
                    eprintln!("Valid formats: {}", LogFormat::all_names());
                    return ParseResult::Error;
                };
                match format_str.parse::<LogFormat>() {
                    Ok(format) => log_format = Some(format),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid formats: {}", LogFormat::all_names());
                        return ParseResult::Error;
                    }
                }
            }
            "--error-format" => {
                let Some(format_str) = args_iter.next() else {
                    eprintln!("Error: --error-format requires a value");
                    eprintln!("Valid formats: {}", ErrorFormat::all_names());
                    return ParseResult::Error;
                };
                match format_str.parse::<ErrorFormat>() {
                    Ok(format) => error_format = Some(format),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid formats: {}", ErrorFormat::all_names());
                        return ParseResult::Error;
                    }
                }
            }
            "--jobs" | "-j" => {
                let Some(jobs_str) = args_iter.next() else {
                    eprintln!("Error: --jobs requires a value");
                    return ParseResult::Error;
                };
                let Some(parsed_jobs) = parse_jobs_value(jobs_str) else {
                    return ParseResult::Error;
                };
                jobs = Some(parsed_jobs);
            }
            "-o" | "--output" => {
                let Some(out_str) = args_iter.next() else {
                    eprintln!("Error: -o requires an output path");
                    return ParseResult::Error;
                };
                output_path = Some(out_str.to_string());
            }
            "--source-manifest" => {
                let Some(path) = args_iter.next() else {
                    eprintln!("Error: --source-manifest requires a path");
                    return ParseResult::Error;
                };
                source_manifest_path = Some(path.to_string());
            }
            "--time-passes" => {
                time_passes = true;
            }
            "--benchmark-json" => {
                benchmark_json = true;
            }
            "--help" | "-h" => {
                // Explicit help request: success, so write to stdout (RUE-518).
                print_help();
                return ParseResult::Exit;
            }
            "--version" | "-V" => {
                print_version();
                return ParseResult::Exit;
            }
            _ if arg.starts_with("-O") => {
                // Parse -O0, -O1, -O2, -O3
                let level_str = &arg[2..];
                match level_str.parse::<OptLevel>() {
                    Ok(level) => opt_level = Some(level),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid levels: {}", OptLevel::all_names());
                        return ParseResult::Error;
                    }
                }
            }
            _ if arg.starts_with("-j") && arg.len() > 2 => {
                // Parse -j1, -j4, etc. (attached form)
                let jobs_str = &arg[2..];
                let Some(parsed_jobs) = parse_jobs_value(jobs_str) else {
                    return ParseResult::Error;
                };
                jobs = Some(parsed_jobs);
            }
            _ if arg.starts_with('-') => {
                eprintln!("Unknown option: {}", arg);
                print_usage();
                return ParseResult::Error;
            }
            _ => positional.push(arg.to_string()),
        }
    }

    if positional.is_empty() {
        eprintln!("Error: No source file specified");
        print_usage();
        return ParseResult::Error;
    }

    // Determine source files and output path based on argument count and -o flag
    let explicit_output = output_path.is_some();
    let (source_paths, final_output_path) = if let Some(out) = output_path {
        // -o was specified: all positional args are source files
        (positional, out)
    } else if !emit_stages.is_empty() {
        // --emit produces no executable, so there is no output positional:
        // every positional arg is a source file, including the second path
        // that legacy two-positional mode would otherwise treat as output
        // (RUE-130).
        (positional, "a.out".to_string())
    } else if positional.len() == 1 {
        // Single source file, no -o: default output to a.out
        (positional, "a.out".to_string())
    } else if positional.len() == 2 {
        // Two positional args, no -o: backwards compatible mode
        // First is source, second is output — but NEVER treat a .rue file as
        // the output. Refusing `rue a.rue b.rue` protects the second source
        // from being overwritten by the compiled binary (RUE-130).
        if positional[1].ends_with(".rue") {
            eprintln!(
                "Error: refusing to use '{}' as the output path: it looks like a source file",
                positional[1]
            );
            eprintln!("Compile the root source and import helper modules with @import:");
            eprintln!("       rue {} -o <output>", positional[0]);
            return ParseResult::Error;
        }
        let mut pos = positional;
        let out = pos.pop().unwrap();
        (pos, out)
    } else {
        // Multiple source files without -o: error. The root-module workflow is
        // `rue main.rue -o output`; helper modules are discovered through
        // `@import`, not by listing them positionally.
        eprintln!("Error: multiple source files require an explicit root-module compile");
        eprintln!("Compile the root source and import helper modules with @import:");
        eprintln!("       rue {} -o <output>", positional[0]);
        return ParseResult::Error;
    };

    if !emit_stages.is_empty() {
        // --emit prints IR to stdout and never writes the output path, so the
        // clobber guard below does not apply (nothing can be clobbered) — but
        // an explicit -o deserves a warning, since it is silently ignored.
        if explicit_output {
            eprintln!("Warning: -o is ignored with --emit; IR goes to stdout");
        }
    } else {
        // In every executable-producing mode: the output must not clobber an
        // input. Compare RESOLVED filesystem paths, not raw strings, so
        // different spellings of the same file are all caught — `rue a.rue -o
        // a.rue`, `rue ./prog -o prog`, or an extensionless source `rue prog
        // -o prog`. The earlier guard keyed off a `.rue` suffix, so
        // extensionless sources slipped through and the compiled output
        // silently overwrote the source (RUE-351). Hard links are caught by
        // inode, and the guard re-runs after @import discovery for sources
        // that are not known yet at parse time (RUE-527).
        if check_output_clobbers_source(&final_output_path, source_paths.iter().map(|s| s.as_str()))
            .is_err()
        {
            return ParseResult::Error;
        }
    }

    if log_format.is_some() && log_level.is_none() {
        eprintln!(
            "Warning: --log-format has no effect without --log-level (logging is off by default)"
        );
    }

    let Some(final_target) = target.or_else(Target::host) else {
        eprintln!(
            "Error: no --target specified and this host ({}) is not a supported Rue target",
            Target::host_description()
        );
        eprintln!("Specify an explicit target with --target <target>.");
        eprintln!("Valid targets: {}", Target::all_names());
        return ParseResult::Error;
    };

    ParseResult::Options(Options {
        source_paths,
        source_manifest_path,
        output_path: final_output_path,
        emit_stages,
        target: final_target,
        linker: linker.unwrap_or_default(),
        opt_level: opt_level.unwrap_or_default(),
        preview_features,
        log_level: log_level.unwrap_or_default(),
        log_format: log_format.unwrap_or_default(),
        error_format: error_format.unwrap_or_default(),
        time_passes,
        benchmark_json,
        jobs: jobs.unwrap_or(0),
    })
}

fn parse_args() -> Option<Options> {
    let args: Vec<String> = env::args().skip(1).collect();
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    match parse_args_from(&args_refs) {
        ParseResult::Options(opts) => Some(opts),
        ParseResult::Error => None,
        ParseResult::Exit => std::process::exit(0),
    }
}

struct DiagnosticOutput<'a> {
    format: ErrorFormat,
    text: MultiFileFormatter<'a>,
    json: MultiFileJsonFormatter<'a>,
}

impl<'a> DiagnosticOutput<'a> {
    fn new(format: ErrorFormat, sources: Vec<(FileId, SourceInfo<'a>)>) -> Self {
        Self {
            format,
            text: MultiFileFormatter::new(sources.clone()),
            json: MultiFileJsonFormatter::new(sources),
        }
    }

    fn print_error(&self, error: &CompileError) {
        match self.format {
            ErrorFormat::Text => eprintln!("{}", self.text.format_error(error)),
            ErrorFormat::Json => eprintln!("{}", self.json.format_error(error).to_json()),
        }
    }

    fn print_errors(&self, errors: &CompileErrors) {
        match self.format {
            ErrorFormat::Text => eprintln!("{}", self.text.format_errors(errors)),
            ErrorFormat::Json => eprintln!("{}", self.json.format_errors(errors)),
        }
    }

    fn print_warnings(&self, warnings: &[CompileWarning]) {
        if warnings.is_empty() {
            return;
        }

        match self.format {
            ErrorFormat::Text => eprintln!("{}", self.text.format_warnings(warnings)),
            ErrorFormat::Json => eprintln!("{}", self.json.format_warnings(warnings)),
        }
    }
}

/// Initialize the tracing subscriber based on CLI options and RUST_LOG.
///
/// Priority: RUST_LOG environment variable takes precedence over --log-level flag.
/// If neither is set and log_level is Off, no subscriber is installed (unless
/// `time_passes` or `benchmark_json` is true, in which case a timing-only subscriber is installed).
///
/// Returns `Some(TimingData)` if `time_passes` or `benchmark_json` is true, which can be used to
/// retrieve the timing report after compilation completes.
fn init_tracing(
    log_level: LogLevel,
    log_format: LogFormat,
    time_passes: bool,
    benchmark_json: bool,
) -> Option<timing::TimingData> {
    use tracing_subscriber::layer::SubscriberExt;

    // Check if RUST_LOG is set - it takes priority
    let rust_log = env::var("RUST_LOG").ok();

    // Determine if we should enable logging
    let effective_level = if rust_log.is_some() {
        // RUST_LOG is set, we'll use it for filtering
        Some(Level::TRACE) // Allow all, let EnvFilter handle it
    } else {
        log_level.to_tracing_level()
    };

    let logging_enabled = effective_level.is_some();

    // Need timing data if either --time-passes or --benchmark-json is specified
    let needs_timing = time_passes || benchmark_json;

    // If neither logging nor timing is enabled, don't install a subscriber
    if !logging_enabled && !needs_timing {
        return None;
    }

    // Create timing data if timing is needed
    let timing_data = if needs_timing {
        Some(timing::TimingData::new())
    } else {
        None
    };

    // Build the filter (only used if logging is enabled)
    let filter = if logging_enabled {
        let f = if let Some(rust_log) = rust_log {
            // Use RUST_LOG value
            EnvFilter::try_new(rust_log).unwrap_or_else(|e| {
                eprintln!("Warning: invalid RUST_LOG value, using default: {}", e);
                EnvFilter::new(format!(
                    "{}",
                    log_level.to_tracing_level().unwrap_or(Level::INFO)
                ))
            })
        } else {
            // Use --log-level value
            EnvFilter::new(format!(
                "{}",
                log_level.to_tracing_level().unwrap_or(Level::INFO)
            ))
        };
        Some(f)
    } else {
        None
    };

    // Build and install the subscriber
    // We need to handle all combinations of timing + logging
    match (needs_timing, logging_enabled, log_format) {
        // Timing only (no logging)
        (true, false, _) => {
            let timing_layer = timing::TimingLayer::new(timing_data.clone().unwrap());
            let subscriber = tracing_subscriber::registry().with(timing_layer);
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // Timing + text logging
        (true, true, LogFormat::Text) => {
            let timing_layer = timing::TimingLayer::new(timing_data.clone().unwrap());
            let subscriber = tracing_subscriber::registry().with(timing_layer).with(
                fmt::layer()
                    .with_target(true)
                    .with_span_events(FmtSpan::CLOSE)
                    .with_writer(std::io::stderr)
                    .with_filter(filter.unwrap()),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // Timing + JSON logging
        (true, true, LogFormat::Json) => {
            let timing_layer = timing::TimingLayer::new(timing_data.clone().unwrap());
            let subscriber = tracing_subscriber::registry().with(timing_layer).with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_span_events(FmtSpan::CLOSE)
                    .with_writer(std::io::stderr)
                    .with_filter(filter.unwrap()),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // Text logging only (no timing)
        (false, true, LogFormat::Text) => {
            let subscriber = tracing_subscriber::registry().with(
                fmt::layer()
                    .with_target(true)
                    .with_span_events(FmtSpan::CLOSE)
                    .with_writer(std::io::stderr)
                    .with_filter(filter.unwrap()),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // JSON logging only (no timing)
        (false, true, LogFormat::Json) => {
            let subscriber = tracing_subscriber::registry().with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_span_events(FmtSpan::CLOSE)
                    .with_writer(std::io::stderr)
                    .with_filter(filter.unwrap()),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // Neither timing nor logging - already handled above
        (false, false, _) => unreachable!(),
    }

    timing_data
}

/// Print timing output based on CLI flags.
fn print_timing_output(
    timing_data: &Option<timing::TimingData>,
    time_passes: bool,
    benchmark_json: bool,
    target: &Target,
    source_metrics: Option<timing::SourceMetrics>,
) {
    if let Some(timing) = timing_data {
        if benchmark_json {
            // JSON output goes to stdout for easy capture
            // Include metadata and source metrics for historical analysis
            println!(
                "{}",
                timing.to_json_with_metrics(
                    &target.to_string(),
                    VERSION,
                    source_metrics,
                    get_peak_memory_bytes(),
                )
            );
        } else if time_passes {
            // Human-readable output goes to stderr
            eprintln!("{}", timing.report());
        }
    }
}

/// Get peak memory usage in bytes (platform-specific).
///
/// Returns None if memory usage cannot be determined.
fn get_peak_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // On Linux, read from /proc/self/status
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmHWM:") {
                    // VmHWM is "high water mark" - peak resident set size
                    // Format: "VmHWM:     12345 kB"
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return Some(kb * 1024);
                        }
                    }
                }
            }
        }
        None
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, use rusage
        use std::mem::MaybeUninit;
        let mut rusage = MaybeUninit::uninit();
        // SAFETY: rusage is properly aligned and getrusage is a standard POSIX call
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, rusage.as_mut_ptr()) };
        if result == 0 {
            // SAFETY: getrusage succeeded, so rusage is initialized
            let rusage = unsafe { rusage.assume_init() };
            // ru_maxrss is in bytes on macOS (unlike Linux where it's in KB)
            Some(rusage.ru_maxrss as u64)
        } else {
            None
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

struct SourceManifest {
    path: PathBuf,
    allowed: std::collections::HashSet<PathBuf>,
    declared_paths: std::collections::HashSet<PathBuf>,
}

impl SourceManifest {
    fn load(path: &str) -> Result<Self, String> {
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

    fn allows_canonical(&self, canonical: &Path) -> bool {
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
fn derive_symbol_paths_with_std_root(
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

fn parse_source_manifest_entry(raw_line: &str) -> String {
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

fn validate_manifest_allows_source(
    manifest: Option<&SourceManifest>,
    source_path: &str,
    role: &str,
) -> Result<(), ()> {
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

    eprintln!(
        "Error: {role} source '{}' is not listed in source manifest '{}'",
        source_path,
        manifest.display_path()
    );
    eprintln!("Manifest entries are allowed source reads, not extra semantic roots.");
    Err(())
}

#[derive(Debug, Clone, Serialize)]
struct DependencySource {
    path: String,
    kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct DependencyImport {
    from: String,
    specifier: String,
    resolved: String,
}

#[derive(Debug, Clone, Serialize)]
struct DependencyOutput {
    version: u32,
    root: String,
    sources: Vec<DependencySource>,
    imports: Vec<DependencyImport>,
}

#[derive(Debug)]
struct ImportDiscoveryResult {
    mixed_imports: Vec<PathBuf>,
    source_snapshot: SourceSnapshot,
    revision: Arc<ImportDiscoveryRevisionArtifact>,
    session: CompilerSession,
}

fn dependency_output_from_canonical_graph(
    snapshot: &SourceSnapshot,
    graph: &rue_compiler::CanonicalImportGraph,
    positional_sources: &HashSet<PathBuf>,
) -> DependencyOutput {
    let physical_for = |module: &rue_compiler::ModuleId| {
        snapshot
            .metadata()
            .logical_paths()
            .find_map(|(file_id, logical)| {
                (logical == module.as_str())
                    .then(|| snapshot.metadata().physical_path(file_id))
                    .flatten()
            })
            .and_then(|path| fs::canonicalize(path).ok())
    };
    let root = physical_for(graph.root()).unwrap_or_default();
    let sources = snapshot
        .files()
        .filter_map(|source| {
            fs::canonicalize(source.path).ok().map(|path| {
                let kind = if source.file_id == snapshot.metadata().root_file_id() {
                    "root"
                } else if positional_sources.contains(&path) {
                    "positional"
                } else {
                    "import"
                };
                DependencySource {
                    path: path.display().to_string(),
                    kind,
                }
            })
        })
        .collect();
    let mut imports = Vec::new();
    for record in graph.records() {
        let Some(from) = physical_for(record.importer()) else {
            continue;
        };
        let targets: Vec<_> = match record.resolution() {
            rue_compiler::CanonicalImportResolution::Resolved(target) => vec![target],
            rue_compiler::CanonicalImportResolution::Ambiguous {
                file_module,
                directory_module,
            } => vec![file_module, directory_module],
            rue_compiler::CanonicalImportResolution::Missing => Vec::new(),
        };
        for target in targets {
            if let Some(resolved) = physical_for(target) {
                imports.push(DependencyImport {
                    from: from.display().to_string(),
                    specifier: record.normalized_specifier().to_owned(),
                    resolved: resolved.display().to_string(),
                });
            }
        }
    }
    DependencyOutput {
        version: 1,
        root: root.display().to_string(),
        sources,
        imports,
    }
}

/// Reject a source before discovery lexing if Rue's span representation cannot
/// describe its byte offsets.
/// Reach the parser-owned import fixed point by executing compiler-generated
/// filesystem requests. This driver owns only the read-policy checks and
/// physical observation transactions; the compiler owns recognition,
/// candidate order, identity assembly, closure, and canonical outcomes.
///
/// Returns non-root positional sources that were also reached through an
/// import. That mixed mode is rejected by the CLI after discovery (RUE-434),
/// because the file should be part of the root import graph, not also an
/// additional flat positional input.
fn discover_and_load_imports(
    sources: &[(String, String)],
    source_manifest: Option<&SourceManifest>,
    std_root: Option<&Path>,
    error_format: ErrorFormat,
) -> Result<ImportDiscoveryResult, ()> {
    use std::collections::HashSet;

    // Preserve the compiler API boundary's positional-source validation before
    // discovery aliases physical identities. In particular, `main.rue` and
    // `./main.rue` are duplicate CLI inputs even though discovery would
    // correctly recognize them as the same physical source.
    let physical_paths: HashMap<_, _> = sources
        .iter()
        .enumerate()
        .map(|(index, (path, _))| (FileId::new((index + 1) as u32), path.clone()))
        .collect();
    let logical_paths: HashMap<_, _> = sources
        .iter()
        .enumerate()
        .map(|(index, _)| {
            (
                FileId::new((index + 1) as u32),
                format!("positional-{}", index + 1),
            )
        })
        .collect();
    if let Err(error) = SourceMetadata::new(FileId::new(1), physical_paths, logical_paths) {
        DiagnosticOutput::new(error_format, Vec::new()).print_errors(&CompileErrors::from(error));
        return Err(());
    }

    let root_path = normalize_lexical_path(Path::new(&sources[0].0));
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
    .map_err(|error| {
        eprintln!("Error: {error}");
    })?;
    let root_canonical = fs::canonicalize(&root_path).map_err(|error| {
        eprintln!("Error reading {}: {error}", root_path.display());
    })?;
    if source_manifest.is_some_and(|manifest| !manifest.allows_canonical(&root_canonical)) {
        eprintln!("Error: root source escapes the source manifest after canonicalization");
        return Err(());
    }
    let root_read = stable_read_to_string(&root_canonical).map_err(|error| match error {
        StableReadError::Io(error) => eprintln!("Error reading {}: {error}", root_path.display()),
        StableReadError::Changed => eprintln!(
            "Error reading {}: source changed during read",
            root_path.display()
        ),
    })?;
    let mut assembler = DiscoverySourceAssembler::new(
        context.clone(),
        root_path.to_string_lossy(),
        root_canonical.to_string_lossy(),
        root_read.identity,
        root_read.fingerprint,
        Arc::new(root_read.source),
    )
    .map_err(|error| {
        eprintln!("Error: {error}");
    })?;
    let explicit_non_root: HashSet<PathBuf> = sources
        .iter()
        .skip(1)
        .filter_map(|(path, _)| fs::canonicalize(path).ok())
        .collect();
    for (path, _) in sources.iter().skip(1) {
        let canonical = fs::canonicalize(path).map_err(|error| {
            eprintln!("Error reading {path}: {error}");
        })?;
        if source_manifest.is_some_and(|manifest| !manifest.allows_canonical(&canonical)) {
            eprintln!(
                "Error: positional source escapes the source manifest after canonicalization: {path}"
            );
            return Err(());
        }
        let read = stable_read_to_string(&canonical).map_err(|error| match error {
            StableReadError::Io(error) => eprintln!("Error reading {path}: {error}"),
            StableReadError::Changed => {
                eprintln!("Error reading {path}: source changed during read")
            }
        })?;
        let requested = normalize_lexical_path(Path::new(path));
        assembler
            .add_explicit(
                requested.to_string_lossy().as_ref(),
                canonical.to_string_lossy().as_ref(),
                read.identity,
                read.fingerprint,
                Arc::new(read.source),
            )
            .map_err(|error| {
                eprintln!("Error: {error}");
            })?;
    }

    let mut ledger = ImportObservationLedger::default();
    let mut staging = CompilerSession::new();
    let mut mixed_imports = Vec::new();
    let mut mixed_seen = HashSet::new();
    let final_plan;
    loop {
        let snapshot = assembler.snapshot().map_err(|error| {
            eprintln!("Error: {error}");
        })?;
        let plan = match staging.stage_import_discovery(
            &snapshot,
            context.clone(),
            assembler.accepted_read_manifest(),
            ledger.clone(),
        ) {
            Ok(plan) => plan,
            Err(errors) => {
                let infos = snapshot
                    .files()
                    .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
                    .collect();
                DiagnosticOutput::new(error_format, infos).print_errors(&errors);
                return Err(());
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
                ledger.record(observation).map_err(|error| {
                    eprintln!("Error: {error}");
                })?;
            }
        }
        if plan.failures(&ledger).next().is_some() {
            final_plan = plan;
            break;
        }
        for accepted in plan.accepted_sources(&ledger) {
            let canonical = PathBuf::from(accepted.canonical_path());
            if explicit_non_root.contains(&canonical) && mixed_seen.insert(canonical.clone()) {
                mixed_imports.push(canonical);
            }
        }
        let added = assembler.add_plan_reads(&plan, &ledger).map_err(|error| {
            eprintln!("Error: {error}");
        })?;
        if added == 0 {
            final_plan = plan;
            break;
        }
    }

    let snapshot = assembler.snapshot().map_err(|error| {
        eprintln!("Error: {error}");
    })?;
    debug_assert_eq!(final_plan.source_revision(), snapshot.source_revision());
    let closed = staging.close_import_discovery(ledger).map_err(|errors| {
        let infos = snapshot
            .files()
            .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
            .collect();
        DiagnosticOutput::new(error_format, infos).print_errors(&errors);
    })?;
    Ok(ImportDiscoveryResult {
        mixed_imports,
        source_snapshot: snapshot,
        revision: closed,
        session: staging,
    })
}

fn main() {
    // Rust's startup ignores SIGPIPE, so a write to a closed pipe
    // (`rue --emit tokens x.rue | head`) returns EPIPE and `println!` panics
    // — which the ICE hook below then mislabels as a compiler bug asking the
    // user to file an issue. Restore the conventional Unix behavior: die
    // silently with SIGPIPE (exit 141), like every other CLI in a pipeline.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Present compiler panics as internal compiler errors with a report
    // banner instead of a raw Rust backtrace pointer (RUE-130). The default
    // hook still runs first so RUST_BACKTRACE=1 output is preserved.
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_panic_hook(info);
        eprintln!();
        eprintln!("error: internal compiler error: the compiler panicked; this is a bug in rue");
        eprintln!("note: rue version {}", VERSION);
        eprintln!("note: please report this at https://github.com/rue-language/rue/issues");
        eprintln!("note: re-run with RUST_BACKTRACE=1 for a backtrace");
    }));

    let options = match parse_args() {
        Some(opts) => opts,
        None => std::process::exit(1),
    };

    // Initialize tracing based on CLI options
    // Returns timing data if --time-passes or --benchmark-json was specified
    let timing_data = init_tracing(
        options.log_level,
        options.log_format,
        options.time_passes,
        options.benchmark_json,
    );

    // Configure Rayon's global thread pool ONCE, before dispatching to either
    // the `--emit` path or the normal compile path. `build_global()` panics if
    // called twice, so it lives here rather than inside a per-compilation entry
    // point — the previous placement meant `--emit` ignored `-j`/`--jobs`
    // entirely (RUE-352).
    configure_thread_pool(options.jobs);

    let source_manifest = match options.source_manifest_path.as_deref() {
        Some(path) => match SourceManifest::load(path) {
            Ok(manifest) => Some(manifest),
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        },
        None => None,
    };

    for (index, path) in options.source_paths.iter().enumerate() {
        let role = if index == 0 { "root" } else { "positional" };
        if validate_manifest_allows_source(source_manifest.as_ref(), path, role).is_err() {
            std::process::exit(1);
        }
    }

    // Discovery performs the accepted stable-read transaction for every root
    // and positional source; keep only the requested spellings here.
    let sources: Vec<(String, String)> = options
        .source_paths
        .iter()
        .map(|path| (path.clone(), String::new()))
        .collect();

    // Discover and load @import-ed modules from disk, transitively. Sema
    // resolves imports only against already-loaded files, so without this
    // step `const utils = @import("utils")` fails with E0704 unless the
    // user hand-lists every module on the command line (RUE-14).
    // Capture environment-derived resolution context exactly once for this
    // discovery epoch. Every compiler plan and identity decision receives this
    // immutable value; no discovery iteration rereads the environment.
    let captured_std_root = env::var_os("RUE_STD_PATH").map(PathBuf::from);
    let mut import_discovery = match discover_and_load_imports(
        &sources,
        source_manifest.as_ref(),
        captured_std_root.as_deref(),
        options.error_format,
    ) {
        Ok(result) => result,
        Err(()) => std::process::exit(1),
    };
    if !import_discovery.mixed_imports.is_empty() {
        eprintln!(
            "Error: source files listed after the root must not also be loaded through @import"
        );
        for path in &import_discovery.mixed_imports {
            eprintln!("  imported and explicitly listed: {}", path.display());
        }
        eprintln!("Compile the root source only and let @import discover helper modules:");
        eprintln!(
            "       rue {} -o {}",
            options.source_paths[0], options.output_path
        );
        eprintln!("Build-system source manifests are tracked separately from positional inputs.");
        std::process::exit(1);
    }
    let source_snapshot = import_discovery.source_snapshot.clone();

    // Re-run the output-clobber guard now that @import discovery has appended
    // the full source set: the parse-time guard only saw positional paths, so
    // `rue main.rue -o helper.rue` (helper loaded via @import) silently
    // replaced helper.rue with the executable (RUE-527). --emit never writes
    // the output path, so it is exempt, matching the parse-time guard.
    if options.emit_stages.is_empty()
        && check_output_clobbers_source(
            &options.output_path,
            source_snapshot.files().map(|source| source.path),
        )
        .is_err()
    {
        std::process::exit(1);
    }

    // Create multi-file diagnostic formatters from the snapshot's borrowed
    // views so diagnostics and compilation necessarily observe the same input.
    let source_infos = source_snapshot
        .files()
        .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
        .collect();
    let diagnostics = DiagnosticOutput::new(options.error_format, source_infos);

    if options.emit_stages.contains(&EmitStage::Deps) {
        if options.emit_stages.len() != 1 {
            eprintln!("Error: --emit deps cannot be combined with other --emit stages");
            std::process::exit(1);
        }
        if options.benchmark_json {
            eprintln!(
                "Error: --emit cannot be combined with --benchmark-json (both write to stdout)"
            );
            std::process::exit(1);
        }
        let positional_sources = options
            .source_paths
            .iter()
            .skip(1)
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect();
        let dependency_output = dependency_output_from_canonical_graph(
            &source_snapshot,
            import_discovery
                .revision
                .graph()
                .expect("closed valid discovery has graph")
                .graph(),
            &positional_sources,
        );
        match serde_json::to_string_pretty(&dependency_output) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("Error emitting dependency graph: {e}");
                std::process::exit(1);
            }
        }
        print_timing_output(
            &timing_data,
            options.time_passes,
            options.benchmark_json,
            &options.target,
            None,
        );
        return;
    }

    // --emit and --benchmark-json both own stdout, so combining them would
    // interleave IR text with the benchmark JSON and corrupt it — reject early.
    if options.benchmark_json && !options.emit_stages.is_empty() {
        eprintln!("Error: --emit cannot be combined with --benchmark-json (both write to stdout)");
        std::process::exit(1);
    }

    // Handle emit modes with multi-file support
    if !options.emit_stages.is_empty() {
        if let Err(()) = handle_emit_multi_file(
            &source_snapshot,
            &mut import_discovery.session,
            &options,
            &diagnostics,
        ) {
            std::process::exit(1);
        }
        print_timing_output(
            &timing_data,
            options.time_passes,
            options.benchmark_json,
            &options.target,
            None,
        );
        return;
    }

    // Normal compilation - uses multi-file compilation for all source files
    let compile_options = CompileOptions {
        target: options.target,
        linker: options.linker.clone(),
        opt_level: options.opt_level,
        preview_features: options.preview_features.clone(),
    };
    let compile_result = import_discovery.session.executable(&compile_options);
    match compile_result {
        Ok(output) => {
            // Print warnings using the diagnostic formatter
            diagnostics.print_warnings(&output.warnings);

            // Write output
            if let Err(e) = fs::write(&options.output_path, &output.elf) {
                eprintln!("Error writing {}: {}", options.output_path, e);
                std::process::exit(1);
            }

            // Make executable (Unix only)
            #[cfg(unix)]
            {
                let path = Path::new(&options.output_path);
                match fs::metadata(path) {
                    Ok(metadata) => {
                        let mut perms = metadata.permissions();
                        perms.set_mode(0o755);
                        if let Err(e) = fs::set_permissions(path, perms) {
                            eprintln!(
                                "Warning: could not set executable permissions on {}: {}",
                                options.output_path, e
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: could not read file metadata for {}: {}",
                            options.output_path, e
                        );
                    }
                }
            }

            // Ad-hoc codesign for macOS (required for executables to run on ARM64)
            #[cfg(target_os = "macos")]
            {
                // Only codesign if target is macOS (cross-compilation check)
                if compile_options.target.is_macho() {
                    // A command-line Mach-O has no Info.plist, so codesign's
                    // default identifier comes from the output filename. Pin
                    // both identity and timestamp policy: choosing a different
                    // destination path must not change the program bytes
                    // (RUE-619).
                    let result = Command::new("codesign")
                        .args([
                            "-f",
                            "-s",
                            "-",
                            "--identifier",
                            "dev.rue-lang.program",
                            "--timestamp=none",
                            &options.output_path,
                        ])
                        .output();
                    match result {
                        Ok(output) => {
                            if !output.status.success() {
                                eprintln!(
                                    "Warning: codesign failed: {}",
                                    String::from_utf8_lossy(&output.stderr)
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: could not run codesign: {}", e);
                        }
                    }
                }
            }

            // Don't print normal compilation message when using --benchmark-json
            // as it would interfere with JSON parsing
            if !options.benchmark_json {
                let linker_str = match &options.linker {
                    LinkerMode::Internal => "internal".to_string(),
                    LinkerMode::System(cmd) => cmd.clone(),
                };
                let source_str = if options.source_paths.len() == 1 {
                    options.source_paths[0].clone()
                } else {
                    format!("{} files", options.source_paths.len())
                };
                println!(
                    "Compiled {} -> {} (target: {}, linker: {})",
                    source_str, options.output_path, options.target, linker_str
                );
            }

            print_timing_output(
                &timing_data,
                options.time_passes,
                options.benchmark_json,
                &options.target,
                options.benchmark_json.then(|| {
                    let source_stats = output.source_stats;
                    timing::SourceMetrics {
                        files: source_stats.files,
                        bytes: source_stats.bytes,
                        lines: source_stats.lines,
                        tokens: source_stats.tokens,
                    }
                }),
            );
        }
        Err(errors) => {
            diagnostics.print_errors(&errors);
            std::process::exit(1);
        }
    }
}

/// Handle emit stages for multi-file compilation.
///
/// For early stages (tokens, ast), each file is processed and labeled individually.
/// For later stages (rir, air, cfg, etc.), the merged program is used.
fn handle_emit_multi_file(
    source_snapshot: &SourceSnapshot,
    session: &mut CompilerSession,
    options: &Options,
    diagnostics: &DiagnosticOutput<'_>,
) -> Result<(), ()> {
    // Determine which stages we need
    let needs_tokens = options.emit_stages.contains(&EmitStage::Tokens);
    let needs_ast = options.emit_stages.contains(&EmitStage::Ast);

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

    let frontend_route = emit_frontend_route(&options.emit_stages);
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
        let compile_options = CompileOptions {
            target: options.target,
            linker: options.linker.clone(),
            opt_level: options.opt_level,
            preview_features: options.preview_features.clone(),
        };
        let frontend = match build_emit_frontend_in_session(session, compile_options) {
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
        debug_assert_eq!(work.parsed.syntax.parser_invocations, source_snapshot.len());
        debug_assert_eq!(work.lowered.parser_invocations, 0);
        debug_assert_eq!(work.semantic.binding.bind_invocations, 1);
        debug_assert_eq!(work.semantic.manifest.build_invocations, 1);
    }

    use std::fmt::Write as _;

    // Now emit in order
    for stage in &options.emit_stages {
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
                            options.target,
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
                writeln!(&mut output, "=== MIR ({}) ===", options.target).expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let mir = match generate_mir(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            options.target,
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
                    options.target
                )
                .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let liveness_info = match generate_liveness_info(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            options.target,
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
                    options.target
                )
                .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let regalloc_info = match generate_regalloc_info(
                            &func.cfg,
                            state.type_pool(),
                            state.interner(),
                            options.target,
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
                writeln!(&mut output, "=== Assembly ({}) ===", options.target)
                    .expect("write to String");
                if let Some(ref state) = frontend_state {
                    for func in state.functions() {
                        let asm = match generate_emitted_asm(
                            &func.cfg,
                            state.type_pool(),
                            state.strings(),
                            state.interner(),
                            options.target,
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
                            options.target,
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
            EmitStage::Deps => unreachable!("--emit deps is handled before frontend emission"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_snapshot(root: FileId, sources: &[(FileId, &str, &str, &str)]) -> SourceSnapshot {
        let physical_paths = sources
            .iter()
            .map(|(file_id, physical, _, _)| (*file_id, (*physical).to_owned()))
            .collect();
        let logical_paths = sources
            .iter()
            .map(|(file_id, _, logical, _)| (*file_id, (*logical).to_owned()))
            .collect();
        let contents = sources
            .iter()
            .map(|(file_id, _, _, source)| (*file_id, Arc::new((*source).to_owned())))
            .collect();
        let metadata = SourceMetadata::new(root, physical_paths, logical_paths).unwrap();
        SourceSnapshot::new(metadata, contents).unwrap()
    }

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

        fn write(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
            path
        }
    }

    #[test]
    fn symbol_paths_are_root_relative_across_relocated_source_trees() {
        fn sources_at(root: &Path) -> Vec<(String, String)> {
            vec![
                (
                    root.join("project/main.rue").display().to_string(),
                    String::new(),
                ),
                (
                    root.join("project/./left/nested/../entry.rue")
                        .display()
                        .to_string(),
                    String::new(),
                ),
                (root.join("dep.rue").display().to_string(), String::new()),
                (
                    root.join("project/right/shared.rue").display().to_string(),
                    String::new(),
                ),
            ]
        }

        let base = std::env::temp_dir().join("rue-symbol-path-tests");
        let short = sources_at(&base.join("a"));
        let relocated = sources_at(&base.join("a-deliberately-much-longer-relocated-source-root"));
        let expected = vec![
            "main.rue",
            "left/entry.rue",
            "../dep.rue",
            "right/shared.rue",
        ];

        assert_eq!(
            derive_symbol_paths_with_std_root(&short, None).unwrap(),
            expected
        );
        assert_eq!(
            derive_symbol_paths_with_std_root(&relocated, None).unwrap(),
            expected
        );
    }

    #[test]
    fn symbol_paths_give_external_std_a_stable_namespace() {
        fn sources_at(project: &Path, std_root: &Path) -> Vec<(String, String)> {
            vec![
                (
                    project.join("main.rue").display().to_string(),
                    String::new(),
                ),
                (
                    std_root.join("_std.rue").display().to_string(),
                    String::new(),
                ),
                (
                    std_root.join("math/float.rue").display().to_string(),
                    String::new(),
                ),
                (
                    project
                        .join("@rue-std/math/float.rue")
                        .display()
                        .to_string(),
                    String::new(),
                ),
            ]
        }

        let base = std::env::temp_dir().join("rue-symbol-std-tests");
        let std_a = base.join("toolchain-a/std");
        let std_b = base.join("a-different-toolchain-location/std");
        let first = sources_at(&base.join("build-a/project"), &std_a);
        let second = sources_at(&base.join("different-depth/build-b/project"), &std_b);
        let expected = vec![
            "main.rue",
            "\0rue-std/_std.rue",
            "\0rue-std/math/float.rue",
            "@rue-std/math/float.rue",
        ];

        assert_eq!(
            derive_symbol_paths_with_std_root(&first, Some(&std_a)).unwrap(),
            expected
        );
        assert_eq!(
            derive_symbol_paths_with_std_root(&second, Some(&std_b)).unwrap(),
            expected
        );
    }

    #[cfg(windows)]
    #[test]
    fn symbol_paths_reject_unnamed_cross_volume_sources() {
        let sources = vec![
            (r"C:\project\main.rue".to_string(), String::new()),
            (r"D:\dependency\helper.rue".to_string(), String::new()),
        ];
        let error = derive_symbol_paths_with_std_root(&sources, None).unwrap_err();
        assert!(error.contains("another filesystem volume"));
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Helper to extract Options from ParseResult, panicking if not Options.
    fn unwrap_options(result: ParseResult) -> Options {
        match result {
            ParseResult::Options(opts) => opts,
            ParseResult::Error => panic!("Expected Options, got Error"),
            ParseResult::Exit => panic!("Expected Options, got Exit"),
        }
    }

    /// Helper to check if result is an error.
    fn is_error(result: &ParseResult) -> bool {
        matches!(result, ParseResult::Error)
    }

    /// Helper to check if result is an exit.
    fn is_exit(result: &ParseResult) -> bool {
        matches!(result, ParseResult::Exit)
    }

    #[test]
    fn source_manifest_entry_parses_comments_and_escaped_hashes() {
        assert_eq!(
            parse_source_manifest_entry("main.rue # comment"),
            "main.rue"
        );
        assert_eq!(
            parse_source_manifest_entry("dir/has\\#hash.rue # comment"),
            "dir/has#hash.rue"
        );
        assert_eq!(
            parse_source_manifest_entry("dir/has\\\\#comment.rue"),
            "dir/has\\\\"
        );
        assert_eq!(
            parse_source_manifest_entry("dir/trailing-backslash\\"),
            "dir/trailing-backslash\\"
        );
    }

    #[test]
    fn source_manifest_load_allows_escaped_hash_in_path() {
        let dir = TestDir::new("source-manifest-escaped-hash");
        let main = dir.write("main.rue", "fn main() -> i32 { 0 }\n");
        let hashed = dir.write("has#hash.rue", "pub fn answer() -> i32 { 42 }\n");
        let manifest = dir.write(
            "sources.manifest",
            "main.rue # normal comment\nhas\\#hash.rue # comment after escaped path\n",
        );

        let manifest = SourceManifest::load(manifest.to_str().unwrap()).unwrap();

        assert!(manifest.allows_canonical(&fs::canonicalize(main).unwrap()));
        assert!(manifest.allows_canonical(&fs::canonicalize(hashed).unwrap()));
    }

    // ========== Basic parsing tests ==========

    #[test]
    fn parse_source_file_only() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.source_paths, vec!["source.rue"]);
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_source_and_output() {
        let opts = unwrap_options(parse_args_from(&["source.rue", "output"]));
        assert_eq!(opts.source_paths, vec!["source.rue"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_source_manifest() {
        let opts = unwrap_options(parse_args_from(&[
            "--source-manifest",
            "sources.manifest",
            "source.rue",
        ]));
        assert_eq!(opts.source_paths, vec!["source.rue"]);
        assert_eq!(
            opts.source_manifest_path.as_deref(),
            Some("sources.manifest")
        );
    }

    #[test]
    fn parse_source_manifest_missing_value() {
        assert!(is_error(&parse_args_from(&[
            "source.rue",
            "--source-manifest",
        ])));
    }

    #[test]
    fn parse_no_args_returns_error() {
        assert!(is_error(&parse_args_from(&[])));
    }

    // ========== Multi-file argument parsing tests ==========

    #[test]
    fn parse_multi_file_with_output_flag() {
        let opts = unwrap_options(parse_args_from(&["a.rue", "b.rue", "-o", "output"]));
        assert_eq!(opts.source_paths, vec!["a.rue", "b.rue"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_multi_file_with_output_long_flag() {
        let opts = unwrap_options(parse_args_from(&["a.rue", "b.rue", "--output", "out"]));
        assert_eq!(opts.source_paths, vec!["a.rue", "b.rue"]);
        assert_eq!(opts.output_path, "out");
    }

    #[test]
    fn parse_multi_file_without_output_flag_error() {
        // Three positional args without -o should error
        assert!(is_error(&parse_args_from(&["a.rue", "b.rue", "c.rue"])));
    }

    #[test]
    fn parse_output_equals_extensionless_input_error() {
        // RUE-351: `-o` targeting an extensionless input source is refused.
        // Resolved-path identity, rather than a `.rue` suffix, protects the
        // input; neither file needs to exist for the keys to match.
        assert!(is_error(&parse_args_from(&["prog", "-o", "prog"])));
    }

    #[test]
    fn parse_output_equals_input_different_spelling_error() {
        // RUE-351: resolved-path comparison catches `./prog` vs `prog` — the
        // same file spelled two ways — not just a byte-for-byte string match.
        assert!(is_error(&parse_args_from(&["./prog", "-o", "prog"])));
    }

    #[test]
    fn parse_distinct_output_and_input_ok() {
        // A genuinely different output path must still be accepted.
        let opts = unwrap_options(parse_args_from(&["prog.rue", "-o", "prog"]));
        assert_eq!(opts.source_paths, vec!["prog.rue"]);
        assert_eq!(opts.output_path, "prog");
    }

    #[test]
    fn parse_multi_file_with_options() {
        let opts = unwrap_options(parse_args_from(&[
            "-O2",
            "main.rue",
            "utils.rue",
            "lib.rue",
            "-o",
            "program",
        ]));
        assert_eq!(opts.source_paths, vec!["main.rue", "utils.rue", "lib.rue"]);
        assert_eq!(opts.output_path, "program");
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    #[test]
    fn parse_output_flag_before_sources() {
        let opts = unwrap_options(parse_args_from(&["-o", "output", "a.rue", "b.rue"]));
        assert_eq!(opts.source_paths, vec!["a.rue", "b.rue"]);
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_single_file_with_output_flag() {
        // Even single file can use -o explicitly
        let opts = unwrap_options(parse_args_from(&["source.rue", "-o", "myprogram"]));
        assert_eq!(opts.source_paths, vec!["source.rue"]);
        assert_eq!(opts.output_path, "myprogram");
    }

    #[test]
    fn parse_output_flag_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "-o"])));
    }

    #[test]
    fn parse_output_long_flag_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--output"])));
    }

    // ========== --emit tests ==========

    #[test]
    fn rir_and_later_emits_share_the_canonical_frontend_route() {
        for stage in [
            EmitStage::Rir,
            EmitStage::Air,
            EmitStage::Cfg,
            EmitStage::Lowering,
            EmitStage::Mir,
            EmitStage::Liveness,
            EmitStage::RegAlloc,
            EmitStage::Asm,
            EmitStage::StackFrame,
        ] {
            assert_eq!(
                emit_frontend_route(&[stage]),
                EmitFrontendRoute::SessionQuery
            );
        }
        assert_eq!(
            emit_frontend_route(&[EmitStage::Ast, EmitStage::Air]),
            EmitFrontendRoute::SessionQuery
        );
        assert_eq!(
            emit_frontend_route(&[EmitStage::Rir, EmitStage::Air]),
            EmitFrontendRoute::SessionQuery
        );
        assert_eq!(
            emit_frontend_route(&[EmitStage::Ast]),
            EmitFrontendRoute::AstOnlySyntax
        );
        assert_eq!(
            emit_frontend_route(&[EmitStage::Tokens]),
            EmitFrontendRoute::None
        );
    }

    #[test]
    fn session_emit_frontend_performs_one_parse_lower_and_bind() {
        let root = FileId::new(9);
        let helper = FileId::new(2);
        let snapshot = test_snapshot(
            root,
            &[
                (
                    root,
                    "/checkout/main.rue",
                    "main.rue",
                    "const helper = @import(\"helper.rue\"); fn main() -> i32 { helper.answer() }",
                ),
                (
                    helper,
                    "/checkout/helper.rue",
                    "helper.rue",
                    "pub fn answer() -> i32 { 42 }",
                ),
            ],
        );
        let frontend = build_emit_frontend(&snapshot, CompileOptions::default()).unwrap();
        let work = frontend.work;

        assert_eq!(work.parsed.syntax.lexer_invocations, 2);
        assert_eq!(work.parsed.syntax.parser_invocations, 2);
        assert_eq!(work.lowered.parser_invocations, 0);
        assert_eq!(work.lowered.ast_payload_clones, 0);
        assert_eq!(work.semantic.binding.bind_invocations, 1);
        assert_eq!(work.semantic.manifest.build_invocations, 1);
        assert_eq!(work.semantic.cfg.cfg_builds_attempted, 2);
        assert_eq!(work.semantic.cfg.cfg_builds_succeeded, 2);
        assert_eq!(work.semantic.cfg.cfg_builds_failed, 0);
        let session_work = &frontend.session_work;
        assert_eq!(session_work.updates, 1);
        assert_eq!(session_work.merge.executions, 1);
        assert_eq!(session_work.rir.executions, 1);
        assert_eq!(session_work.semantic.executions, 1);

        let presentation = parse_source_snapshot_for_ast_presentation(&snapshot).unwrap();
        assert_eq!(
            presentation
                .files()
                .iter()
                .map(|(path, ast)| (path.as_str(), ast.items.len()))
                .collect::<Vec<_>>(),
            [("/checkout/main.rue", 2), ("/checkout/helper.rue", 1)]
        );
        let ast_work = presentation.work();
        assert_eq!(ast_work.parsed.syntax.parser_invocations, 2);
        assert_eq!(ast_work.merge_invocations, 0);
        assert_eq!(ast_work.astgen_invocations, 0);
        assert_eq!(ast_work.bind_invocations, 0);
        assert_eq!(ast_work.manifest_invocations, 0);
    }

    #[test]
    fn session_emit_frontend_preserves_discovery_order_for_multifile_errors() {
        let root = FileId::new(9);
        let helper = FileId::new(2);
        let snapshot = test_snapshot(
            root,
            &[
                (
                    root,
                    "/checkout/z-root.rue",
                    "z-root.rue",
                    "fn main() { let # = 1; }",
                ),
                (
                    helper,
                    "/checkout/a-helper.rue",
                    "a-helper.rue",
                    "fn helper() { let # = 2; }",
                ),
            ],
        );

        let errors = match build_emit_frontend(&snapshot, CompileOptions::default()) {
            Err(errors) => errors,
            Ok(_) => panic!("invalid sources unexpectedly produced emit artifacts"),
        };
        let mut files = errors
            .iter()
            .filter_map(|error| error.span().map(|span| span.file_id))
            .collect::<Vec<_>>();
        files.dedup();
        assert_eq!(files, [root, helper]);
    }

    #[test]
    fn ast_presentation_prints_duplicates_without_merging() {
        let snapshot =
            SourceSnapshot::single("main.rue", "fn duplicate() {} fn duplicate() {}").unwrap();
        let presentation = parse_source_snapshot_for_ast_presentation(&snapshot).unwrap();

        assert_eq!(presentation.files()[0].1.items.len(), 2);
        assert_eq!(presentation.work().merge_invocations, 0);
    }

    #[test]
    fn parse_emit_tokens() {
        let opts = unwrap_options(parse_args_from(&["--emit", "tokens", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Tokens]);
    }

    #[test]
    fn emit_mode_skips_output_clobber_guard() {
        // --emit writes nothing to the output path, so -o naming a source
        // file is NOT a clobber (it is merely ignored, with a warning); the
        // same spelling without --emit is still refused.
        let opts = unwrap_options(parse_args_from(&["--emit", "ast", "x.rue", "-o", "x.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Ast]);
        assert!(is_error(&parse_args_from(&["x.rue", "-o", "x.rue"])));
    }

    #[test]
    fn unreadable_import_candidate_is_an_error_not_absence() {
        // RUE-529: an import candidate that EXISTS but cannot be read
        // (invalid UTF-8 here) must be a hard error, not treated as absent —
        // absence misreported the import as E0704 "cannot find module" and
        // silently erased one arm of a file-vs-directory ambiguity. (Unit
        // test rather than a CLI case: TOML case sources cannot express
        // invalid-UTF-8 file content.)
        let dir =
            std::env::temp_dir().join(format!("rue-unreadable-import-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join("main.rue");
        fs::write(
            &main_path,
            "const h = @import(\"helper.rue\");\nfn main() -> i32 { 0 }\n",
        )
        .unwrap();
        fs::write(dir.join("helper.rue"), [0xFFu8]).unwrap();

        let sources = vec![(
            main_path.to_string_lossy().into_owned(),
            fs::read_to_string(&main_path).unwrap(),
        )];
        let result = discover_and_load_imports(&sources, None, None, ErrorFormat::Text);
        assert!(
            result.is_err(),
            "unreadable existing candidate must error, not resolve or vanish"
        );

        // Control: once the candidate is valid text, discovery loads it.
        fs::write(dir.join("helper.rue"), "pub fn h() -> i32 {{ 1 }}\n").unwrap();
        let sources = vec![(
            main_path.to_string_lossy().into_owned(),
            fs::read_to_string(&main_path).unwrap(),
        )];
        let result = discover_and_load_imports(&sources, None, None, ErrorFormat::Text).unwrap();
        assert_eq!(
            result.source_snapshot.len(),
            2,
            "helper must be discovered and loaded"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn clobber_guard_catches_hard_link_alias() {
        // RUE-527: a hard link gives the output a DIFFERENT canonical path
        // from the source while sharing its inode — `ln main.rue program;
        // rue main.rue -o program` destroyed main.rue. The guard must compare
        // device+inode, not just resolved paths.
        let dir =
            std::env::temp_dir().join(format!("rue-clobber-hardlink-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("main.rue");
        let link = dir.join("program");
        let _ = fs::remove_file(&link);
        fs::write(&source, "fn main() -> i32 { 0 }\n").unwrap();
        fs::hard_link(&source, &link).unwrap();

        let source_str = source.to_str().unwrap();
        let link_str = link.to_str().unwrap();
        let output_key = clobber_key(link_str);
        let output_meta = fs::metadata(link_str).ok();
        assert!(output_would_clobber(
            &output_key,
            output_meta.as_ref(),
            source_str
        ));

        // A distinct file in the same directory is NOT a clobber.
        let other = dir.join("other.rue");
        fs::write(&other, "fn main() -> i32 { 1 }\n").unwrap();
        assert!(!output_would_clobber(
            &output_key,
            output_meta.as_ref(),
            other.to_str().unwrap()
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_emit_ast() {
        let opts = unwrap_options(parse_args_from(&["--emit", "ast", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Ast]);
    }

    #[test]
    fn parse_emit_rir() {
        let opts = unwrap_options(parse_args_from(&["--emit", "rir", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Rir]);
    }

    #[test]
    fn parse_emit_air() {
        let opts = unwrap_options(parse_args_from(&["--emit", "air", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_emit_cfg() {
        let opts = unwrap_options(parse_args_from(&["--emit", "cfg", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Cfg]);
    }

    #[test]
    fn parse_emit_mir() {
        let opts = unwrap_options(parse_args_from(&["--emit", "mir", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Mir]);
    }

    #[test]
    fn parse_emit_asm() {
        let opts = unwrap_options(parse_args_from(&["--emit", "asm", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Asm]);
    }

    #[test]
    fn parse_emit_deps() {
        let opts = unwrap_options(parse_args_from(&["--emit", "deps", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Deps]);
    }

    #[test]
    fn parse_multiple_emit_stages() {
        let opts = unwrap_options(parse_args_from(&[
            "--emit",
            "tokens",
            "--emit",
            "ast",
            "--emit",
            "air",
            "source.rue",
        ]));
        assert_eq!(
            opts.emit_stages,
            vec![EmitStage::Tokens, EmitStage::Ast, EmitStage::Air]
        );
    }

    #[test]
    fn parse_emit_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--emit"])));
    }

    #[test]
    fn parse_emit_invalid_stage() {
        assert!(is_error(&parse_args_from(&[
            "--emit",
            "invalid",
            "source.rue"
        ])));
    }

    // ========== --target tests ==========

    #[test]
    fn parse_target_x86_64_linux() {
        let opts = unwrap_options(parse_args_from(&["--target", "x86_64-linux", "source.rue"]));
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    #[test]
    fn parse_target_aarch64_macos() {
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "aarch64-macos",
            "source.rue",
        ]));
        assert_eq!(opts.target, Target::Aarch64Macos);
    }

    #[test]
    fn parse_target_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--target"])));
    }

    #[test]
    fn parse_target_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--target",
            "invalid",
            "source.rue"
        ])));
    }

    // ========== --linker tests ==========

    #[test]
    fn parse_linker_internal() {
        let opts = unwrap_options(parse_args_from(&["--linker", "internal", "source.rue"]));
        assert_eq!(opts.linker, LinkerMode::Internal);
    }

    #[test]
    fn parse_linker_system_clang() {
        let opts = unwrap_options(parse_args_from(&["--linker", "clang", "source.rue"]));
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
    }

    #[test]
    fn parse_linker_system_gcc() {
        let opts = unwrap_options(parse_args_from(&["--linker", "gcc", "source.rue"]));
        assert_eq!(opts.linker, LinkerMode::System("gcc".to_string()));
    }

    #[test]
    fn parse_linker_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--linker"])));
    }

    // ========== Optimization level tests ==========

    #[test]
    fn parse_opt_level_0() {
        let opts = unwrap_options(parse_args_from(&["-O0", "source.rue"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_opt_level_1() {
        let opts = unwrap_options(parse_args_from(&["-O1", "source.rue"]));
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn parse_opt_level_2() {
        let opts = unwrap_options(parse_args_from(&["-O2", "source.rue"]));
        assert_eq!(opts.opt_level, OptLevel::O2);
    }

    #[test]
    fn parse_opt_level_3() {
        let opts = unwrap_options(parse_args_from(&["-O3", "source.rue"]));
        assert_eq!(opts.opt_level, OptLevel::O3);
    }

    #[test]
    fn parse_opt_level_invalid() {
        assert!(is_error(&parse_args_from(&["-O9", "source.rue"])));
    }

    // ========== --preview tests ==========

    #[test]
    fn parse_preview_valid_feature() {
        let opts = unwrap_options(parse_args_from(&["--preview", "test_infra", "source.rue"]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
    }

    #[test]
    fn parse_preview_multiple_flags() {
        // Test that --preview can be specified multiple times
        // (currently only one feature exists, but the flag can still be repeated)
        let opts = unwrap_options(parse_args_from(&[
            "--preview",
            "test_infra",
            "--preview",
            "test_infra",
            "source.rue",
        ]));
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
        assert_eq!(opts.preview_features.len(), 1);
    }

    #[test]
    fn parse_preview_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--preview"])));
    }

    #[test]
    fn parse_preview_invalid_feature() {
        assert!(is_error(&parse_args_from(&[
            "--preview",
            "nonexistent",
            "source.rue"
        ])));
    }

    // ========== --log-level tests ==========

    #[test]
    fn parse_log_level_off() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "off", "source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Off);
    }

    #[test]
    fn parse_log_level_error() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "error", "source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Error);
    }

    #[test]
    fn parse_log_level_warn() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "warn", "source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Warn);
    }

    #[test]
    fn parse_log_level_info() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "info", "source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Info);
    }

    #[test]
    fn parse_log_level_debug() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "debug", "source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Debug);
    }

    #[test]
    fn parse_log_level_trace() {
        let opts = unwrap_options(parse_args_from(&["--log-level", "trace", "source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Trace);
    }

    #[test]
    fn parse_log_level_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--log-level"])));
    }

    #[test]
    fn parse_log_level_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--log-level",
            "invalid",
            "source.rue"
        ])));
    }

    // ========== --log-format tests ==========

    #[test]
    fn parse_log_format_text() {
        let opts = unwrap_options(parse_args_from(&["--log-format", "text", "source.rue"]));
        assert_eq!(opts.log_format, LogFormat::Text);
    }

    #[test]
    fn parse_log_format_json() {
        let opts = unwrap_options(parse_args_from(&["--log-format", "json", "source.rue"]));
        assert_eq!(opts.log_format, LogFormat::Json);
    }

    #[test]
    fn parse_log_format_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--log-format"])));
    }

    #[test]
    fn parse_log_format_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--log-format",
            "invalid",
            "source.rue"
        ])));
    }

    // ========== --error-format tests ==========

    #[test]
    fn parse_error_format_text() {
        let opts = unwrap_options(parse_args_from(&["--error-format", "text", "source.rue"]));
        assert_eq!(opts.error_format, ErrorFormat::Text);
    }

    #[test]
    fn parse_error_format_json() {
        let opts = unwrap_options(parse_args_from(&["--error-format", "json", "source.rue"]));
        assert_eq!(opts.error_format, ErrorFormat::Json);
    }

    #[test]
    fn parse_error_format_missing_value() {
        assert!(is_error(&parse_args_from(&[
            "source.rue",
            "--error-format"
        ])));
    }

    #[test]
    fn parse_error_format_invalid() {
        assert!(is_error(&parse_args_from(&[
            "--error-format",
            "invalid",
            "source.rue"
        ])));
    }

    // ========== --help and --version tests ==========

    #[test]
    fn parse_help_long() {
        assert!(is_exit(&parse_args_from(&["--help"])));
    }

    #[test]
    fn parse_help_short() {
        assert!(is_exit(&parse_args_from(&["-h"])));
    }

    #[test]
    fn parse_version_long() {
        assert!(is_exit(&parse_args_from(&["--version"])));
    }

    #[test]
    fn parse_version_short() {
        assert!(is_exit(&parse_args_from(&["-V"])));
    }

    // ========== Unknown option tests ==========

    #[test]
    fn parse_unknown_option() {
        assert!(is_error(&parse_args_from(&["--unknown", "source.rue"])));
    }

    #[test]
    fn parse_unknown_short_option() {
        assert!(is_error(&parse_args_from(&["-x", "source.rue"])));
    }

    // ========== Combined options tests ==========

    #[test]
    fn parse_all_options_combined() {
        // Under --emit no executable is produced, so there is no output
        // positional: every positional is a source file (RUE-130). The old
        // behavior claimed the second positional as a (dead) output path,
        // which made multi-file --emit impossible.
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "x86_64-linux",
            "--linker",
            "clang",
            "-O2",
            "--emit",
            "air",
            "source.rue",
            "other.rue",
        ]));
        assert_eq!(opts.source_paths, vec!["source.rue", "other.rue"]);
        assert_eq!(opts.target, Target::X86_64Linux);
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit_stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_options_after_source() {
        // Options can appear after the source file
        let opts = unwrap_options(parse_args_from(&["source.rue", "-O1"]));
        assert_eq!(opts.source_paths, vec!["source.rue"]);
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn parse_mixed_option_positions() {
        let opts = unwrap_options(parse_args_from(&[
            "-O1",
            "source.rue",
            "--target",
            "x86_64-linux",
            "output",
        ]));
        assert_eq!(opts.source_paths, vec!["source.rue"]);
        assert_eq!(opts.output_path, "output");
        assert_eq!(opts.opt_level, OptLevel::O1);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    // ========== Default values tests ==========

    #[test]
    fn parse_defaults_output_path() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_defaults_opt_level() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.opt_level, OptLevel::O0);
    }

    #[test]
    fn parse_defaults_linker() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.linker, LinkerMode::Internal);
    }

    #[test]
    fn parse_defaults_emit_stages_empty() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert!(opts.emit_stages.is_empty());
    }

    #[test]
    fn parse_defaults_log_level() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.log_level, LogLevel::Off);
    }

    #[test]
    fn parse_defaults_log_format() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.log_format, LogFormat::Text);
    }

    #[test]
    fn parse_defaults_error_format() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.error_format, ErrorFormat::Text);
    }

    #[test]
    fn parse_defaults_time_passes() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert!(!opts.time_passes);
    }

    // ========== --time-passes tests ==========

    #[test]
    fn parse_time_passes() {
        let opts = unwrap_options(parse_args_from(&["--time-passes", "source.rue"]));
        assert!(opts.time_passes);
    }

    #[test]
    fn parse_time_passes_with_other_options() {
        let opts = unwrap_options(parse_args_from(&[
            "--time-passes",
            "-O2",
            "--target",
            "x86_64-linux",
            "source.rue",
        ]));
        assert!(opts.time_passes);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    // ========== --benchmark-json tests ==========

    #[test]
    fn parse_benchmark_json() {
        let opts = unwrap_options(parse_args_from(&["--benchmark-json", "source.rue"]));
        assert!(opts.benchmark_json);
    }

    #[test]
    fn parse_benchmark_json_with_other_options() {
        let opts = unwrap_options(parse_args_from(&[
            "--benchmark-json",
            "-O2",
            "--target",
            "x86_64-linux",
            "source.rue",
        ]));
        assert!(opts.benchmark_json);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    #[test]
    fn parse_defaults_benchmark_json() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert!(!opts.benchmark_json);
    }

    #[test]
    fn parse_both_time_passes_and_benchmark_json() {
        // When both are specified, benchmark_json takes precedence (JSON output)
        let opts = unwrap_options(parse_args_from(&[
            "--time-passes",
            "--benchmark-json",
            "source.rue",
        ]));
        assert!(opts.time_passes);
        assert!(opts.benchmark_json);
    }

    // ========== --jobs tests ==========

    #[test]
    fn parse_jobs_long_form() {
        let opts = unwrap_options(parse_args_from(&["--jobs", "4", "source.rue"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_short_form() {
        let opts = unwrap_options(parse_args_from(&["-j", "4", "source.rue"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_attached_form() {
        let opts = unwrap_options(parse_args_from(&["-j4", "source.rue"]));
        assert_eq!(opts.jobs, 4);
    }

    #[test]
    fn parse_jobs_single_thread() {
        let opts = unwrap_options(parse_args_from(&["-j1", "source.rue"]));
        assert_eq!(opts.jobs, 1);
    }

    #[test]
    fn parse_jobs_auto_detect() {
        let opts = unwrap_options(parse_args_from(&["--jobs", "0", "source.rue"]));
        assert_eq!(opts.jobs, 0);
    }

    #[test]
    fn parse_jobs_accepts_max_explicit_value() {
        let max_jobs = MAX_EXPLICIT_JOBS.to_string();
        let opts = unwrap_options(parse_args_from(&["--jobs", &max_jobs, "source.rue"]));
        assert_eq!(opts.jobs, MAX_EXPLICIT_JOBS);
    }

    #[test]
    fn parse_jobs_missing_value() {
        assert!(is_error(&parse_args_from(&["source.rue", "--jobs"])));
    }

    #[test]
    fn parse_jobs_missing_value_short() {
        assert!(is_error(&parse_args_from(&["source.rue", "-j"])));
    }

    #[test]
    fn parse_jobs_invalid_value() {
        assert!(is_error(&parse_args_from(&["--jobs", "abc", "source.rue"])));
    }

    #[test]
    fn parse_jobs_negative_value() {
        // Negative values should fail to parse as usize
        assert!(is_error(&parse_args_from(&["--jobs", "-1", "source.rue"])));
    }

    #[test]
    fn parse_jobs_rejects_excessive_value() {
        let excessive = (MAX_EXPLICIT_JOBS + 1).to_string();
        assert!(is_error(&parse_args_from(&[
            "--jobs",
            &excessive,
            "source.rue"
        ])));
    }

    #[test]
    fn parse_jobs_rejects_excessive_attached_value() {
        let excessive = format!("-j{}", MAX_EXPLICIT_JOBS + 1);
        assert!(is_error(&parse_args_from(&[&excessive, "source.rue"])));
    }

    #[test]
    fn parse_jobs_with_other_options() {
        let opts = unwrap_options(parse_args_from(&[
            "-j4",
            "-O2",
            "--target",
            "x86_64-linux",
            "source.rue",
        ]));
        assert_eq!(opts.jobs, 4);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.target, Target::X86_64Linux);
    }

    #[test]
    fn parse_defaults_jobs() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.jobs, 0);
    }

    // ========== EmitStage FromStr tests ==========

    #[test]
    fn emit_stage_from_str_all_valid() {
        assert_eq!("tokens".parse::<EmitStage>().unwrap(), EmitStage::Tokens);
        assert_eq!("ast".parse::<EmitStage>().unwrap(), EmitStage::Ast);
        assert_eq!("rir".parse::<EmitStage>().unwrap(), EmitStage::Rir);
        assert_eq!("air".parse::<EmitStage>().unwrap(), EmitStage::Air);
        assert_eq!("cfg".parse::<EmitStage>().unwrap(), EmitStage::Cfg);
        assert_eq!(
            "lowering".parse::<EmitStage>().unwrap(),
            EmitStage::Lowering
        );
        assert_eq!("mir".parse::<EmitStage>().unwrap(), EmitStage::Mir);
        assert_eq!(
            "liveness".parse::<EmitStage>().unwrap(),
            EmitStage::Liveness
        );
        assert_eq!(
            "regalloc".parse::<EmitStage>().unwrap(),
            EmitStage::RegAlloc
        );
        assert_eq!("asm".parse::<EmitStage>().unwrap(), EmitStage::Asm);
        assert_eq!(
            "stackframe".parse::<EmitStage>().unwrap(),
            EmitStage::StackFrame
        );
        assert_eq!("deps".parse::<EmitStage>().unwrap(), EmitStage::Deps);
    }

    #[test]
    fn emit_stage_from_str_invalid() {
        let err = "invalid".parse::<EmitStage>().unwrap_err();
        assert_eq!(err.to_string(), "unknown emit stage 'invalid'");
    }

    #[test]
    fn emit_stage_all_names() {
        assert_eq!(
            EmitStage::all_names(),
            "tokens, ast, rir, air, cfg, lowering, mir, liveness, regalloc, asm, stackframe, deps"
        );
    }

    #[test]
    fn parse_emit_lowering() {
        let opts = unwrap_options(parse_args_from(&["--emit", "lowering", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Lowering]);
    }

    #[test]
    fn parse_emit_regalloc() {
        let opts = unwrap_options(parse_args_from(&["--emit", "regalloc", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::RegAlloc]);
    }

    #[test]
    fn parse_emit_stackframe() {
        let opts = unwrap_options(parse_args_from(&["--emit", "stackframe", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::StackFrame]);
    }

    #[test]
    fn parse_emit_liveness() {
        let opts = unwrap_options(parse_args_from(&["--emit", "liveness", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Liveness]);
    }

    // ========== LogLevel FromStr tests ==========

    #[test]
    fn log_level_from_str_all_valid() {
        assert_eq!("off".parse::<LogLevel>().unwrap(), LogLevel::Off);
        assert_eq!("error".parse::<LogLevel>().unwrap(), LogLevel::Error);
        assert_eq!("warn".parse::<LogLevel>().unwrap(), LogLevel::Warn);
        assert_eq!("info".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!("debug".parse::<LogLevel>().unwrap(), LogLevel::Debug);
        assert_eq!("trace".parse::<LogLevel>().unwrap(), LogLevel::Trace);
    }

    #[test]
    fn log_level_from_str_invalid() {
        let err = "invalid".parse::<LogLevel>().unwrap_err();
        assert_eq!(err.to_string(), "unknown log level 'invalid'");
    }

    #[test]
    fn log_level_all_names() {
        assert_eq!(
            LogLevel::all_names(),
            "off, error, warn, info, debug, trace"
        );
    }

    #[test]
    fn log_level_to_tracing_level() {
        assert!(LogLevel::Off.to_tracing_level().is_none());
        assert_eq!(LogLevel::Error.to_tracing_level(), Some(Level::ERROR));
        assert_eq!(LogLevel::Warn.to_tracing_level(), Some(Level::WARN));
        assert_eq!(LogLevel::Info.to_tracing_level(), Some(Level::INFO));
        assert_eq!(LogLevel::Debug.to_tracing_level(), Some(Level::DEBUG));
        assert_eq!(LogLevel::Trace.to_tracing_level(), Some(Level::TRACE));
    }

    // ========== LogFormat FromStr tests ==========

    #[test]
    fn log_format_from_str_all_valid() {
        assert_eq!("text".parse::<LogFormat>().unwrap(), LogFormat::Text);
        assert_eq!("json".parse::<LogFormat>().unwrap(), LogFormat::Json);
    }

    #[test]
    fn log_format_from_str_invalid() {
        let err = "invalid".parse::<LogFormat>().unwrap_err();
        assert_eq!(err.to_string(), "unknown log format 'invalid'");
    }

    #[test]
    fn log_format_all_names() {
        assert_eq!(LogFormat::all_names(), "text, json");
    }
}
