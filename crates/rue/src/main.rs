use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::{fs, sync::Arc};

use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{EnvFilter, Layer as _, fmt};

#[cfg(rue_benchmark_allocations)]
mod allocation;
mod compile;
mod emit;
mod output;
mod platform_signing;
mod timing;
mod watch;

use emit::EmitStage;
#[cfg(test)]
use emit::{EmitFrontendRoute, build_emit_frontend, emit_frontend_route, emit_requires_semantic};
#[cfg(test)]
use rue_compiler::unstable::update_for_presentation;
use rue_compiler::unstable::{
    JsonDiagnostic, MultiFileFormatter, MultiFileJsonFormatter, SourceInfo,
};
use rue_driver::{FilesystemCompilerHost, HostOpenRequest, SourceLoadError};

use rue_compiler::{
    CompileErrors, CompileOptions, CompileWarning, FileId, ImportDiscoveryStatus, LinkerMode,
    OptLevel, PreviewFeature, PreviewFeatures, configure_thread_pool,
};
#[cfg(test)]
use rue_compiler::{CompilerSession, SourceMetadata, SourceSnapshot};
use rue_error::{CompileError, ErrorCode};
use rue_perf_schema::{
    ArtifactHitEvidence, BuildBoundary, CompilerBoundaryEvidence, CompilerBuildProfile,
    CompilerConfigurationEvidence, CompilerCriticalPathEvidence, CompilerInputClass,
    CompilerInputEvidence, CompilerPipeline, CompilerStage, EmbeddedAssetClass,
    EmbeddedAssetEvidence, LinkPolicy, OptimizationLevel, OutputKind, WorkerSetting,
};
use rue_target::Target;
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
    /// The single root source file named positionally on the command line. The
    /// compiler builds exactly one root module; every other file is reached
    /// through its `@import` graph. The legacy flat-mode surface, where extra
    /// positional paths seeded a shared namespace, was removed (ADR-0046 /
    /// RUE-767) — the driver now refuses additional positional sources.
    source_path: String,
    /// Optional build-system-facing manifest of source files the compiler may
    /// read while resolving the root module's import graph.
    source_manifest_path: Option<String>,
    output_path: String,
    emit_stages: Vec<EmitStage>,
    target: Target,
    linker: LinkerMode,
    opt_level: OptLevel,
    preview_features: PreviewFeatures,
    /// Static archives (`.a`) supplied with `--link-archive`, resolved for
    /// undefined `extern "C"` symbols at link time (ADR-0064 C FFI).
    link_archives: Vec<std::path::PathBuf>,
    log_level: LogLevel,
    log_format: LogFormat,
    error_format: ErrorFormat,
    time_passes: bool,
    benchmark_json: bool,
    watch: bool,
    /// Number of parallel jobs (0 = auto-detect, use all cores).
    jobs: usize,
}

/// Version string for the rue compiler (single-sourced from rue-error so the
/// `--version` banner and ICE reports can never drift apart).
const VERSION: &str = rue_compiler::VERSION;

/// Highest explicit `-j`/`--jobs` value accepted by the CLI.
///
/// `0` still means auto-detect. A fixed generous ceiling catches accidental
/// values like `-j 100000` before the compiler schedules an impractical number
/// of workers, while still leaving ample room above current CI and
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
Usage: rue [options] <root.rue> [output]
       rue [options] <root.rue> -o <output>

The compiler takes exactly one root source file and discovers every other
file through its @import graph; pass build-system inputs with --source-manifest.

Options:
  -o, --output <path>  Set output path
  --source-manifest <path>
                       Restrict source imports to a line-oriented manifest
  --link-archive <path>
                       Link a static archive (.a) resolving extern \"C\" symbols
                       (ADR-0064 C FFI); can be repeated
  --target <target>    Set compilation target (default: host)
                       Valid targets: {targets}
  --linker <linker>    Set linker to use (default: internal)
                       Use 'internal' for built-in linker, or a command
                       like 'clang', 'gcc', or 'ld' for system linker
                       A system linker keeps function symbols for native
                       profilers (see docs/process/profiling.md)
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
                       'json' writes one JSON array of diagnostic objects per
                       stderr line, compiler panics (ICEs) included
                       (schema: docs/process/diagnostics.md)
  --time-passes        Show timing for each compilation pass
  --benchmark-json     Output timing as JSON (for benchmarking)
  --watch              Recompile when the accepted source closure changes
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

// CAUTION: `rue_test_runner::ice_message` treats the substring
// "internal compiler error" anywhere on the compiler's stderr as proof the
// compiler crashed, and usage text goes to stderr on an argument error. Never
// spell that phrase out in this help text — an ordinary `rue --bogus-flag`
// would then be reported as an ICE and fail every case that pins it.
//
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
    /// Successfully parsed options (boxed: Options dwarfs the other variants).
    Options(Box<Options>),
    /// Parsing failed with an error.
    Error,
    /// User requested help or version (already printed, should exit 0).
    Exit,
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

/// The sole accepted positional source: the root module. Returns `None` when
/// the caller passed additional positional source files — the removed flat-mode
/// input surface (ADR-0046 / RUE-767) — so the caller can emit the migration
/// diagnostic. `positional` is never empty at the call sites (checked earlier).
fn single_root_source(positional: &[String]) -> Option<String> {
    match positional {
        [root] => Some(root.clone()),
        _ => None,
    }
}

/// The migration diagnostic for the removed flat-mode positional input surface.
/// The compiler builds exactly one root module and reaches every other file
/// through its `@import` graph; a build system that needs to bound the readable
/// file set passes `--source-manifest` (ADR-0046 / RUE-767). Names the first
/// offending extra argument and points at the single-root invocation. Kept pure
/// (returns the text) so its exact wording can be pinned by a unit test.
fn extra_positional_sources_diagnostic(root: &str, extra: &str) -> String {
    format!(
        "Error: unexpected extra source file '{extra}': the compiler builds one root module and its @import graph\n\
         Compile the root source only; reach helper modules with @import, or list build inputs with --source-manifest:\n\
         \x20      rue {root} -o <output>"
    )
}

/// Emit the removed-flat-mode migration diagnostic for `positional`, whose first
/// element is the root source and whose second is the first offending extra.
fn refuse_extra_positional_sources(positional: &[String]) {
    eprintln!(
        "{}",
        extra_positional_sources_diagnostic(&positional[0], &positional[1])
    );
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
    let mut watch = false;
    let mut jobs: Option<usize> = None;
    let mut source_manifest_path: Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut link_archives: Vec<std::path::PathBuf> = Vec::new();
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
            "--link-archive" => {
                let Some(path) = args_iter.next() else {
                    eprintln!("Error: --link-archive requires a path");
                    return ParseResult::Error;
                };
                link_archives.push(std::path::PathBuf::from(path));
            }
            "--time-passes" => {
                time_passes = true;
            }
            "--benchmark-json" => {
                benchmark_json = true;
            }
            "--watch" => {
                watch = true;
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

    // Determine the single root source and output path from the positional
    // args and the -o flag. Every driver form accepts exactly one positional
    // source: the compiler builds one root module and reaches the rest through
    // @import (ADR-0046 / RUE-767). Additional positional .rue arguments are
    // the removed flat-mode input surface and are refused.
    let explicit_output = output_path.is_some();
    let (source_path, final_output_path) = if let Some(out) = output_path {
        // -o names the output explicitly, so every positional is a source.
        match single_root_source(&positional) {
            Some(root) => (root, out),
            None => {
                refuse_extra_positional_sources(&positional);
                return ParseResult::Error;
            }
        }
    } else if !emit_stages.is_empty() {
        // --emit produces no executable, so there is no output positional:
        // every positional is a source file.
        match single_root_source(&positional) {
            Some(root) => (root, "a.out".to_string()),
            None => {
                refuse_extra_positional_sources(&positional);
                return ParseResult::Error;
            }
        }
    } else if positional.len() == 1 {
        // Single source file, no -o: default output to a.out.
        (positional.into_iter().next().unwrap(), "a.out".to_string())
    } else if positional.len() == 2 {
        // Two positional args, no -o: the backwards-compatible
        // `rue <source> <output>` form. First is source, second is output —
        // but NEVER treat a .rue file as the output. Refusing `rue a.rue b.rue`
        // protects the second source from being overwritten by the compiled
        // binary (RUE-130); helper modules are reached through @import, never a
        // second positional source (RUE-767).
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
        (pos.pop().unwrap(), out)
    } else {
        // Three or more positional args without -o: the extra sources are the
        // removed flat-mode input surface (RUE-767).
        refuse_extra_positional_sources(&positional);
        return ParseResult::Error;
    };

    if !emit_stages.is_empty() {
        // --emit prints IR to stdout and never writes the output path, so the
        // clobber guard below does not apply (nothing can be clobbered) — but
        // an explicit -o deserves a warning, since it is silently ignored.
        if explicit_output {
            eprintln!("Warning: -o is ignored with --emit; IR goes to stdout");
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

    ParseResult::Options(Box::new(Options {
        source_path,
        source_manifest_path,
        output_path: final_output_path,
        emit_stages,
        target: final_target,
        linker: linker.unwrap_or_default(),
        opt_level: opt_level.unwrap_or_default(),
        preview_features,
        link_archives,
        log_level: log_level.unwrap_or_default(),
        log_format: log_format.unwrap_or_default(),
        error_format: error_format.unwrap_or_default(),
        time_passes,
        benchmark_json,
        watch,
        jobs: jobs.unwrap_or(0),
    }))
}

fn parse_args() -> Option<Options> {
    let args: Vec<String> = env::args().skip(1).collect();
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    match parse_args_from(&args_refs) {
        ParseResult::Options(opts) => Some(*opts),
        ParseResult::Error => None,
        ParseResult::Exit => std::process::exit(0),
    }
}

fn validate_watch_modes(options: &Options) -> Result<(), &'static str> {
    if options.watch
        && (!options.emit_stages.is_empty() || options.benchmark_json || options.time_passes)
    {
        Err("Error: --watch cannot be combined with --emit, --benchmark-json, or --time-passes")
    } else {
        Ok(())
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

    /// Render one error. Under `--error-format json` a lone error is still
    /// wrapped in a one-element array: every JSON diagnostic line the CLI
    /// writes is an array of diagnostic objects, so a consumer parses one
    /// shape rather than switching on object-vs-array (RUE-436). The
    /// single-error paths (output publication, watch-cycle publication) used
    /// to emit a bare object here while every batch path emitted an array.
    fn render_error(&self, error: &CompileError) -> String {
        match self.format {
            ErrorFormat::Text => self.text.format_error(error).to_string(),
            ErrorFormat::Json => format!("[{}]", self.json.format_error(error).to_json()),
        }
    }

    fn render_errors(&self, errors: &CompileErrors) -> String {
        match self.format {
            ErrorFormat::Text => self.text.format_errors(errors),
            ErrorFormat::Json => self.json.format_errors(errors),
        }
    }

    fn render_warnings(&self, warnings: &[CompileWarning]) -> String {
        match self.format {
            ErrorFormat::Text => self.text.format_warnings(warnings),
            ErrorFormat::Json => self.json.format_warnings(warnings),
        }
    }

    fn print_error(&self, error: &CompileError) {
        eprintln!("{}", self.render_error(error));
    }

    fn print_errors(&self, errors: &CompileErrors) {
        eprintln!("{}", self.render_errors(errors));
    }

    fn print_warnings(&self, warnings: &[CompileWarning]) {
        if warnings.is_empty() {
            return;
        }

        eprintln!("{}", self.render_warnings(warnings));
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
    compiler_work: Option<rue_perf_schema::CompilerWork>,
    emitted_output: Option<&[u8]>,
    compiler_boundary: Option<&CompilerBoundaryEvidence>,
    critical_path: Option<&CompilerCriticalPathEvidence>,
) {
    if let Some(timing) = timing_data {
        if benchmark_json {
            // JSON output goes to stdout for easy capture
            // Include metadata and source metrics for historical analysis
            let mut payload: serde_json::Value =
                serde_json::from_str(&timing.to_json_with_metrics(
                    &target.to_string(),
                    VERSION,
                    source_metrics,
                    compiler_work,
                    get_peak_memory_bytes(),
                ))
                .unwrap();
            if let Some(bytes) = emitted_output {
                use sha2::{Digest, Sha256};
                payload["emitted_output"] = serde_json::json!({
                    "sha256": format!("{:x}", Sha256::digest(bytes)),
                    "size_bytes": bytes.len(),
                });
            }
            if let Some(boundary) = compiler_boundary {
                payload["compiler_boundary"] = serde_json::to_value(boundary).unwrap();
            }
            if let Some(critical_path) = critical_path {
                payload["critical_path"] = serde_json::to_value(critical_path).unwrap();
            }
            #[cfg(rue_benchmark_allocations)]
            {
                let metrics = allocation::snapshot();
                payload["compiler_allocations"] = serde_json::json!({
                    "count": metrics.allocations,
                    "requested_bytes": metrics.allocated_bytes,
                    "boundary": "canonical compile root including discovery and backend",
                });
            }
            println!("{payload}");
        } else if time_passes {
            // Human-readable output goes to stderr
            eprintln!("{}", timing.report());
        }
    }
}

fn compiler_build_profile() -> CompilerBuildProfile {
    #[cfg(rue_release_build)]
    {
        CompilerBuildProfile::ReleaseThinLto
    }
    #[cfg(not(rue_release_build))]
    {
        CompilerBuildProfile::Debug
    }
}

fn compiler_boundary_evidence(
    options: &Options,
    resolved_workers: usize,
    snapshot: &rue_compiler::SourceSnapshot,
    emitted_output: &[u8],
) -> Option<CompilerBoundaryEvidence> {
    let requested_workers = WorkerSetting::from_jobs(options.jobs)?;
    let mut accepted_inputs = snapshot
        .files()
        .map(|source| {
            use sha2::{Digest, Sha256};
            let module = snapshot
                .module_id(source.file_id)
                .expect("source views originate from this snapshot");
            CompilerInputEvidence {
                class: if module.is_trusted_standard_library() {
                    CompilerInputClass::TrustedStandardLibrarySource
                } else {
                    CompilerInputClass::WorkloadSource
                },
                logical_identity: module.as_str().to_owned(),
                sha256: format!("{:x}", Sha256::digest(source.source.as_bytes())),
            }
        })
        .collect::<Vec<_>>();
    accepted_inputs.sort_by(|left, right| {
        (&left.class, left.logical_identity.as_str())
            .cmp(&(&right.class, right.logical_identity.as_str()))
    });

    Some(CompilerBoundaryEvidence {
        boundary: BuildBoundary::FreshSourceToNativeV1,
        pipeline: CompilerPipeline::CanonicalRootedQueryGraphV1,
        session_count: 1,
        root_request_count: 1,
        configuration: CompilerConfigurationEvidence {
            target: options.target.to_string(),
            compiler_build_profile: compiler_build_profile(),
            optimization: match options.opt_level {
                OptLevel::O0 => OptimizationLevel::O0,
                OptLevel::O1 => OptimizationLevel::O1,
                OptLevel::O2 => OptimizationLevel::O2,
                OptLevel::O3 => OptimizationLevel::O3,
            },
            linker: match &options.linker {
                LinkerMode::Internal => LinkPolicy::Internal,
                LinkerMode::System(_) => LinkPolicy::System,
            },
            output_kind: OutputKind::NativeExecutable,
            requested_workers,
            resolved_workers: u32::try_from(resolved_workers).unwrap_or(u32::MAX),
            preview_features: {
                let mut features = options
                    .preview_features
                    .iter()
                    .map(|feature| feature.name().to_owned())
                    .collect::<Vec<_>>();
                features.sort();
                features
            },
            external_link_archives: u32::try_from(options.link_archives.len()).unwrap_or(u32::MAX),
        },
        completed_stages: CompilerStage::FRESH_SOURCE_TO_NATIVE_V1.to_vec(),
        accepted_inputs,
        embedded_assets: vec![EmbeddedAssetEvidence {
            class: EmbeddedAssetClass::BundledRuntimeArchive,
            logical_identity: "rue-runtime".to_string(),
            target: options.target.to_string(),
        }],
        artifact_hits: ArtifactHitEvidence::default(),
        emitted_output_sha256: {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(emitted_output))
        },
        emitted_output_size_bytes: u64::try_from(emitted_output.len()).unwrap_or(u64::MAX),
    })
}

fn compiler_critical_path_evidence(
    timing: &timing::TimingData,
    metrics: rue_compiler::unstable::OneShotMetrics,
) -> CompilerCriticalPathEvidence {
    let runtime = metrics.query_runtime;
    CompilerCriticalPathEvidence {
        query_worker_active_ns: runtime.query_worker_active_ns,
        ready_items: runtime.ready_items,
        ready_wait_ns: runtime.ready_wait_ns,
        max_ready_wait_ns: runtime.max_ready_wait_ns,
        longest_query_dependency_chain: runtime.longest_query_dependency_chain,
        peak_query_workers: runtime.peak_query_workers,
        toolchain_acquisition_ns: timing.pass_total_ns("toolchain_acquisition"),
        semantic_bodies: timing.pass_duration_distribution("body_analysis"),
        semantic_prerequisite_bodies: timing.pass_duration_distribution("body_query_prerequisites"),
        semantic_declaration_graph: timing
            .pass_duration_distribution("declaration_graph_collection"),
        semantic_declaration_occurrence_indexes: timing
            .pass_duration_distribution("declaration_occurrence_index"),
        semantic_declaration_nuclei: timing.pass_duration_distribution("declaration_nucleus"),
        semantic_declaration_signature_parsing: timing
            .pass_duration_distribution("declaration_signature_parsing"),
        semantic_body_closure: timing.pass_duration_distribution("body_closure_collection"),
        semantic_body_graph_projection: timing.pass_duration_distribution("body_graph_projection"),
        semantic_body_input_lowering: timing.pass_duration_distribution("body_input_lowering"),
        semantic_provider_analysis: timing.pass_duration_distribution("semantic_provider_analysis"),
        semantic_provider_host_setup: timing
            .pass_duration_distribution("semantic_provider_host_setup"),
        semantic_provider_expression_engine: timing
            .pass_duration_distribution("semantic_provider_expression_engine"),
        semantic_expression_setup: timing.pass_duration_distribution("semantic_expression_setup"),
        semantic_inference_precompute: timing
            .pass_duration_distribution("semantic_inference_precompute"),
        semantic_constraint_generation: timing
            .pass_duration_distribution("semantic_constraint_generation"),
        semantic_unification_resolution: timing
            .pass_duration_distribution("semantic_unification_resolution"),
        semantic_air_emission_validation: timing
            .pass_duration_distribution("semantic_air_emission_validation"),
        semantic_provider_specialization_selection: timing
            .pass_duration_distribution("semantic_provider_specialization_selection"),
        semantic_provider_body_export: timing
            .pass_duration_distribution("semantic_provider_body_export"),
        semantic_provider_result_projection: timing
            .pass_duration_distribution("semantic_provider_result_projection"),
        cfg_input_preparation_bodies: timing.pass_duration_distribution("cfg_input_preparation"),
        semantic_materialization_bodies: timing
            .pass_duration_distribution("semantic_materialization"),
        cfg_domain_prerequisite_bodies: timing
            .pass_duration_distribution("cfg_domain_prerequisites"),
        cfg_domain_projection_bodies: timing.pass_duration_distribution("cfg_domain_projection"),
        cfg_prerequisite_collection_bodies: timing
            .pass_duration_distribution("cfg_prerequisite_collection"),
        cfg_prerequisite_query_bodies: timing
            .pass_duration_distribution("cfg_prerequisite_queries"),
        cfg_builder_bodies: timing.pass_duration_distribution("cfg_builder"),
        cfg_publication_bodies: timing.pass_duration_distribution("cfg_publication"),
        cfg_construction_bodies: timing.pass_duration_distribution("cfg_construction"),
        cfg_optimization_bodies: timing.pass_duration_distribution("cfg_optimization"),
        joins: runtime.joins,
        declined_joins: runtime.declined_joins,
        donated_permits: runtime.donated_permits,
    }
}

fn benchmark_compiler_work(
    metrics: rue_compiler::unstable::OneShotMetrics,
) -> rue_perf_schema::CompilerWork {
    let runtime = metrics.query_runtime;
    let validation = runtime.validation;
    let reachability = metrics.semantic_reachability;
    let provider = metrics.provider_observations;
    rue_perf_schema::CompilerWork {
        semantic_provider: rue_perf_schema::SemanticProviderWork {
            name_lookups: provider.name_lookups,
            import_lookups: provider.import_lookups,
            method_candidates: provider.method_candidates,
            operator_candidates: provider.operator_candidates,
            declaration_facts: provider.declaration_facts,
            identity_facts: provider.identity_facts,
            signature_facts: provider.signature_facts,
            type_facts: provider.type_facts,
            const_facts: provider.const_facts,
            materializations: provider.materializations,
            shared_payload_materializations: provider.shared_payload_materializations,
            owned_payload_materializations: provider.owned_payload_materializations,
            const_materializations: provider.const_materializations,
            nominal_materializations: provider.nominal_materializations,
            function_materializations: provider.function_materializations,
            method_materializations: provider.method_materializations,
            anonymous_facts: provider.anonymous_facts,
            producer_facts: provider.producer_facts,
            toolchain_facts: provider.toolchain_facts,
        },
        semantic_reachability: rue_perf_schema::SemanticReachabilityWork {
            frontier_scans: reachability.frontier_scans,
            frontier_scan_keys: reachability.frontier_scan_keys,
            frontier_batches: reachability.frontier_batches,
            frontier_keys: reachability.frontier_keys,
            frontier_width_one: reachability.frontier_width_one,
            frontier_width_two_to_three: reachability.frontier_width_two_to_three,
            frontier_width_four_to_seven: reachability.frontier_width_four_to_seven,
            frontier_width_eight_or_more: reachability.frontier_width_eight_or_more,
            transactions_prefetched: reachability.transactions_prefetched,
            transactions_serial: reachability.transactions_serial,
        },
        cfg_materialization: rue_perf_schema::CfgMaterializationWork {
            index_builds: metrics.semantic.cfg.materialization_index_builds as u64,
            declarations_scanned: metrics.semantic.cfg.materialization_declarations_scanned as u64,
            anonymous_nominals_scanned: metrics
                .semantic
                .cfg
                .materialization_anonymous_nominals_scanned
                as u64,
            type_nodes_scanned: metrics.semantic.cfg.materialization_type_nodes_scanned as u64,
            fact_selections: metrics.semantic.cfg.materialization_fact_selections as u64,
            declarations_selected: metrics.semantic.cfg.materialization_declarations_selected
                as u64,
            anonymous_nominals_selected: metrics
                .semantic
                .cfg
                .materialization_anonymous_nominals_selected
                as u64,
            callables_selected: metrics.semantic.cfg.materialization_callables_selected as u64,
            nominal_metadata_selected: metrics
                .semantic
                .cfg
                .materialization_nominal_metadata_selected
                as u64,
            modules_selected: metrics.semantic.cfg.materialization_modules_selected as u64,
            builtin_nominals_selected: metrics
                .semantic
                .cfg
                .materialization_builtin_nominals_selected
                as u64,
            required_types_selected: metrics.semantic.cfg.materialization_required_types_selected
                as u64,
        },
        cfg_prerequisites: rue_perf_schema::CfgPrerequisiteWork {
            stable_types_scanned: metrics.semantic.cfg.prerequisite_stable_types_scanned as u64,
            layout_requests: metrics.semantic.cfg.prerequisite_layout_requests as u64,
            type_fact_requests: metrics.semantic.cfg.prerequisite_type_fact_requests as u64,
            drop_glue_requests: metrics.semantic.cfg.prerequisite_drop_glue_requests as u64,
        },
        cfg_retained_charge: rue_perf_schema::CfgRetainedChargeWork {
            interner_scans: metrics.semantic.cfg.retained_interner_charge_scans as u64,
            interner_entries_scanned: metrics.semantic.cfg.retained_interner_entries_scanned as u64,
            interner_utf8_bytes_scanned: metrics.semantic.cfg.retained_interner_utf8_bytes_scanned
                as u64,
        },
        cfg_local_epoch: rue_perf_schema::CfgLocalEpochWork {
            epochs: metrics.semantic.cfg.local_epochs as u64,
            air_instructions: metrics.semantic.cfg.local_air_instructions as u64,
            air_payload_bytes: metrics.semantic.cfg.local_air_payload_bytes as u64,
            type_entries: metrics.semantic.cfg.local_type_entries as u64,
            aggregate_type_aliases: metrics.semantic.cfg.local_aggregate_type_aliases as u64,
            materialized_type_handles: metrics.semantic.cfg.local_materialized_type_handles as u64,
            interner_entries: metrics.semantic.cfg.local_interner_entries as u64,
            interner_utf8_bytes: metrics.semantic.cfg.local_interner_utf8_bytes as u64,
            strings: metrics.semantic.cfg.local_strings as u64,
            local_atoms: metrics.semantic.cfg.local_atoms as u64,
        },
        query_runtime: rue_perf_schema::QueryRuntimeWork {
            claims: runtime.claims,
            reuses: runtime.reuses,
            joins: runtime.joins,
            declined_joins: runtime.declined_joins,
            body_completions: runtime.body_completions,
            red_publications: runtime.red_publications,
            green_publications: runtime.green_publications,
            cancellations: runtime.cancellations,
            cycles: runtime.cycles,
            validation: rue_perf_schema::ValidationWork {
                traversals: validation.traversals,
                successful_traversals: validation.successful_traversals,
                dirty_traversals: validation.dirty_traversals,
                aborted_traversals: validation.aborted_traversals,
                input_observations: validation.input_observations,
                dependency_observations: validation.dependency_observations,
                registry_probes: validation.registry_probes,
                registry_index_lookups: validation.registry_index_lookups,
                registry_misses: validation.registry_misses,
                node_visits: validation.node_visits,
                active_cycle_prunes: validation.active_cycle_prunes,
                memo_hits: validation.memo_hits,
                memo_misses: validation.memo_misses,
                certificate_misses: validation.certificate_misses,
                proof_reacquisition_misses: validation.proof_reacquisition_misses,
                endorsement_probes: validation.endorsement_probes,
                endorsement_hits: validation.endorsement_hits,
                terminal_lease_observations: validation.terminal_lease_observations,
                duplicate_terminal_lease_observations: validation
                    .duplicate_terminal_lease_observations,
                demands: validation.demands,
                demand_reuses: validation.demand_reuses,
                demand_computes: validation.demand_computes,
                demand_joins: validation.demand_joins,
                demand_aborts: validation.demand_aborts,
                superseded: validation.superseded,
                certificates_published: validation.certificates_published,
            },
            display_identities: rue_perf_schema::DisplayIdentityWork {
                memo_node_materializations: runtime.display_identities.memo_node_materializations,
                memo_node_bytes: runtime.display_identities.memo_node_bytes,
                structured_wait_materializations: runtime
                    .display_identities
                    .structured_wait_materializations,
                structured_wait_bytes: runtime.display_identities.structured_wait_bytes,
                abort_fallback_materializations: runtime
                    .display_identities
                    .abort_fallback_materializations,
                abort_fallback_bytes: runtime.display_identities.abort_fallback_bytes,
            },
            retention_enforcements: runtime.retention_enforcements,
            retention_scan_entries: runtime.retention_scan_entries,
        },
    }
}

/// Get peak memory usage in bytes (platform-specific).
///
/// Returns None if memory usage cannot be determined.
fn get_peak_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // On Linux, read from /proc/self/status
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
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

/// Present a source-loading failure and exit. The four classes carry disjoint
/// presentations: an ordinary message; a broken-toolchain (environmental) error;
/// a hermetic build-configuration denial (distinct from a broken toolchain — the
/// remedy is the source manifest, not the installation); and program diagnostics
/// rendered against the failing snapshot's source views.
fn report_source_load_error(error: SourceLoadError, error_format: ErrorFormat) -> ! {
    match error {
        SourceLoadError::Message(message) => {
            eprintln!("{message}");
        }
        SourceLoadError::Toolchain(error) => {
            eprintln!("{error}");
        }
        SourceLoadError::HermeticDenial(error) => {
            eprintln!("{error}");
        }
        SourceLoadError::Compiler { snapshot, errors } => {
            let infos = snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .files()
                        .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
                        .collect()
                })
                .unwrap_or_default();
            DiagnosticOutput::new(error_format, infos).print_errors(&errors);
        }
    }
    std::process::exit(1);
}

/// Diagnostic format the ICE panic hook renders with.
///
/// The hook is installed before `--error-format` has been parsed (so a panic
/// inside argument parsing is still an ICE report rather than a raw Rust
/// backtrace), and a `&'static` panic hook cannot borrow the parsed options.
/// The driver publishes the chosen format here exactly once, immediately after
/// parsing; `false` — the initial value — means the `text` default.
static ICE_FORMAT_IS_JSON: AtomicBool = AtomicBool::new(false);

/// Publish the diagnostic format the ICE panic hook must render with.
fn set_ice_error_format(format: ErrorFormat) {
    ICE_FORMAT_IS_JSON.store(format == ErrorFormat::Json, Ordering::Relaxed);
}

/// The diagnostic format the ICE panic hook renders with. `Text` until the
/// driver publishes a parsed `--error-format`.
fn ice_error_format() -> ErrorFormat {
    if ICE_FORMAT_IS_JSON.load(Ordering::Relaxed) {
        ErrorFormat::Json
    } else {
        ErrorFormat::Text
    }
}

/// Render a compiler panic as a structured diagnostic.
///
/// A thin adapter over [`ice_diagnostic`] and [`panic_payload_message`]: this
/// crate is compiled with `panic = "abort"` (see `toolchains/rust/defs.bzl`),
/// so no test can raise a catchable panic to obtain a real `PanicHookInfo`.
/// Splitting the two testable halves out keeps everything but this three-line
/// projection under unit test.
fn ice_panic_diagnostic(info: &std::panic::PanicHookInfo<'_>) -> JsonDiagnostic {
    ice_diagnostic(
        &panic_payload_message(info.payload()),
        info.location().map(|location| location.to_string()),
    )
}

/// Build the structured diagnostic for a compiler panic.
///
/// The code is [`ErrorCode::INTERNAL_ERROR`] (`E9000`) — the same code a
/// graceful `ice_error!` ICE carries — so a consumer filters both halves of
/// the ICE surface on one code. The diagnostic has no spans: a panic has no
/// source location in the *user's* program, and inventing one would be a lie.
fn ice_diagnostic(payload: &str, panic_site: Option<String>) -> JsonDiagnostic {
    let mut notes = vec![format!("rue version {VERSION}")];
    if let Some(site) = panic_site {
        notes.push(format!("panic at {site}"));
    }
    // `Backtrace::capture` is a no-op unless RUST_BACKTRACE is set, matching
    // the text hook's behavior — the JSON report is not more verbose by
    // default, it is only parseable.
    let backtrace = std::backtrace::Backtrace::capture();
    if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
        notes.push(format!("backtrace:\n{backtrace}"));
    }

    JsonDiagnostic {
        code: ErrorCode::INTERNAL_ERROR.to_string(),
        message: format!(
            "internal compiler error: the compiler panicked; this is a bug in rue: {payload}"
        ),
        severity: "error",
        spans: Vec::new(),
        suggestions: Vec::new(),
        notes,
        helps: vec![
            "please report this at https://github.com/rue-language/rue/issues".to_string(),
            "re-run with RUST_BACKTRACE=1 for a backtrace".to_string(),
        ],
    }
}

/// Extract a panic's message payload. `panic!` produces either a `&'static str`
/// (a literal) or a formatted `String`; anything else is an opaque payload.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic with a non-string payload".to_string()
    }
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
    //
    // Under `--error-format json` the same ICE is instead published as an
    // ordinary structured diagnostic (RUE-436): a machine consumer that asked
    // for JSON must be able to parse EVERY line the compiler writes to stderr,
    // and the default hook's `thread 'main' panicked at ...` banner is not
    // JSON. Nothing is lost — the panic payload, panic location, and (when
    // `RUST_BACKTRACE` is set) the backtrace all move into the diagnostic's
    // notes. Graceful ICEs (`ice_error!` -> `ErrorKind::InternalError`) are
    // ordinary `CompileError`s and already travel the normal JSON path; this
    // closes the panic half of the same surface.
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if ice_error_format() == ErrorFormat::Json {
            eprintln!("[{}]", ice_panic_diagnostic(info).to_json());
            return;
        }
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

    // The hook above is installed BEFORE argument parsing, so a panic inside
    // the parser is still reported as an ICE. Publish the requested format now
    // that it is known; until this point the hook uses the `text` default.
    set_ice_error_format(options.error_format);

    // Reject incompatible output modes before any tracing, thread-pool, manifest,
    // or source I/O work. This is a pure options check, so surfacing it first
    // keeps an options error from being masked by — or nondeterministically
    // ordered behind — an unrelated missing-file or manifest failure (RUE-798).
    if let Err(message) = emit::validate_output_modes(&options.emit_stages, options.benchmark_json)
    {
        eprintln!("{message}");
        std::process::exit(1);
    }
    if let Err(message) = validate_watch_modes(&options) {
        eprintln!("{message}");
        std::process::exit(1);
    }

    // Initialize tracing based on CLI options
    // Returns timing data if --time-passes or --benchmark-json was specified
    let timing_data = init_tracing(
        options.log_level,
        options.log_format,
        options.time_passes,
        options.benchmark_json,
    );

    // Configure the compiler's shared structured-query budget before
    // dispatching to either the `--emit` path or the normal compile path, so
    // every driver path honors `-j`/`--jobs` (RUE-352).
    let resolved_workers = configure_thread_pool(options.jobs);

    // Discover and load @import-ed modules from disk, transitively. Sema
    // resolves imports only against already-loaded files, so without this
    // step `const utils = @import("utils")` fails with E0704 unless the
    // user hand-lists every module on the command line (RUE-14).
    // Capture environment-derived resolution context exactly once for this
    // discovery epoch. Every compiler plan and identity decision receives this
    // immutable value; no discovery iteration rereads the environment.
    // One root spans the disjoint intervals that perform canonical compiler
    // work. RUE-890 made discovery's exact parse publishable, so leaving this
    // boundary inside `CompilerSession::executable` would incorrectly report
    // the discovery parse as a second timing root.
    let compile_span = tracing::info_span!("compile", target = %options.target);
    let captured_std_root = env::var_os("RUE_STD_PATH").map(PathBuf::from);
    #[cfg(rue_benchmark_allocations)]
    if options.benchmark_json {
        allocation::begin();
    }
    let mut compiler_host = {
        let _compile = compile_span.enter();
        match FilesystemCompilerHost::open(HostOpenRequest {
            root_source: &options.source_path,
            source_manifest_path: options.source_manifest_path.as_deref(),
            std_root: captured_std_root.as_deref(),
        }) {
            Ok(result) => result,
            Err(error) => report_source_load_error(error, options.error_format),
        }
    };

    let compile_options = CompileOptions {
        target: options.target,
        linker: options.linker.clone(),
        opt_level: options.opt_level,
        preview_features: options.preview_features.clone(),
        link_archives: options.link_archives.clone(),
    };

    // Acquire any trusted toolchain modules a reached fallible intrinsic requires
    // but no `@import` pulled — the compiler-rooted std `Option` for a fallible
    // intrinsic, plus std `StrBuf` for `@read_line`'s `Option(StrBuf)` payload.
    // This runs the rooted, park-aware semantic attempt and satisfies exactly the
    // demands it parks on, so an unreachable helper never forces a std read; a
    // reached-body demand a broken or policy-denied toolchain cannot satisfy
    // surfaces as its own environmental / build-configuration error. Host
    // filesystem access lives outside the compiler's snapshot/query evaluation,
    // which is why the driver owns this loop rather than the session.
    //
    // INVARIANT: acquisition runs only when the run will analyze bodies — a normal
    // compile, or an `--emit` of a semantic stage (AIR and later). The park is
    // raised only by reached-body semantic analysis, so a pre-semantic `--emit`
    // (tokens/ast/rir/deps) presents its artifact without ever parking; gating the
    // loop out keeps such an emit at ZERO std reads even for a reached fallible
    // intrinsic with a broken std on disk. When it does run, it runs before both
    // emit and compile, matching the acquire-before-everything ordering.
    if options.emit_stages.is_empty() || emit::emit_requires_semantic(&options.emit_stages) {
        let _compile = compile_span.enter();
        if let Err(error) = compiler_host.acquire_reached_toolchain_modules(&compile_options) {
            report_source_load_error(error, options.error_format);
        }
    }

    if options.watch {
        watch::run(watch::WatchRequest {
            host: compiler_host,
            compile_options,
            source_path: options.source_path,
            output_path: options.output_path,
            error_format: options.error_format,
        });
    }

    #[cfg(rue_benchmark_allocations)]
    if options.benchmark_json {
        allocation::pause();
    }
    let source_snapshot = compiler_host.source_snapshot().clone();
    tracing::debug!(
        root = %compiler_host.root_path().display(),
        project_root = compiler_host.discovery_context().project_root(),
        std_root = compiler_host.discovery_context().std_root(),
        read_policy_revision = compiler_host.discovery_context().read_policy_revision(),
        accepted_reads = compiler_host.accepted_reads().len(),
        "source loading complete"
    );

    // Create multi-file diagnostic formatters from the snapshot's borrowed
    // views so diagnostics and compilation necessarily observe the same input.
    let source_infos = source_snapshot
        .files()
        .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
        .collect();
    let diagnostics = DiagnosticOutput::new(options.error_format, source_infos);

    // Output-mode compatibility (including `--emit` + `--benchmark-json`) was
    // already validated before any I/O by `emit::validate_output_modes` (RUE-798).

    // Handle emit modes with multi-file support
    if !options.emit_stages.is_empty() {
        {
            let _compile = compile_span.enter();
            if let Err(()) = emit::execute(emit::EmitRequest {
                host: &mut compiler_host,
                stages: &options.emit_stages,
                compile_options: compile_options.clone(),
                diagnostics: &diagnostics,
            }) {
                std::process::exit(1);
            }
        }
        drop(compile_span);
        print_timing_output(
            &timing_data,
            options.time_passes,
            options.benchmark_json,
            &options.target,
            None,
            None,
            None,
            None,
            None,
        );
        return;
    }

    if compiler_host.discovery_status() != ImportDiscoveryStatus::ClosedValid {
        diagnostics.print_errors(compiler_host.discovery_revision().diagnostics());
        std::process::exit(1);
    }

    // Closed discovery fixes the complete source identity set. Validate the
    // destination before semantic/codegen/link work, then retain that set for
    // mandatory revalidation immediately before atomic publication.
    let publication_destination = match output::preflight_destination(
        Path::new(&options.output_path),
        source_snapshot.files().map(|source| source.path),
    ) {
        Ok(destination) => destination,
        Err(output::PublishError::WouldClobberSource) => {
            eprintln!(
                "Error: output path '{}' is also an input source file; refusing to overwrite it",
                options.output_path
            );
            std::process::exit(1);
        }
        Err(error) => {
            diagnostics.print_error(&error.into_compile_error());
            std::process::exit(1);
        }
    };

    // Normal compilation - uses multi-file compilation for all source files
    #[cfg(rue_benchmark_allocations)]
    if options.benchmark_json {
        allocation::resume();
    }
    let compile_result = {
        let _compile = compile_span.enter();
        compile::execute(compile::CompileRequest {
            host: &mut compiler_host,
            options: compile_options,
            destination: publication_destination,
        })
    };
    drop(compile_span);
    #[cfg(rue_benchmark_allocations)]
    if options.benchmark_json {
        allocation::finish();
    }
    match compile_result {
        Ok(output) => {
            // Publication runs after the compiler's timing root closes, so it
            // is measured as a driver phase: it breaks down process-minus-root
            // overhead without becoming a second timing root (RUE-786).
            let publication = {
                let _span = tracing::info_span!("output_write", driver_phase = true).entered();
                output.publish()
            };
            // Warnings live outside the publication result so failures cannot
            // discard them; present them before inspecting and reporting the
            // publication outcome.
            diagnostics.print_warnings(&publication.warnings);
            let output = match publication.result {
                Ok(output) => output,
                Err(output::PublishError::WouldClobberSource) => {
                    eprintln!(
                        "Error: output path '{}' is also an input source file; refusing to overwrite it",
                        options.output_path
                    );
                    std::process::exit(1);
                }
                Err(error) => {
                    diagnostics.print_error(&error.into_compile_error());
                    std::process::exit(1);
                }
            };

            // Don't print normal compilation message when using --benchmark-json
            // as it would interfere with JSON parsing
            if !options.benchmark_json {
                let linker_str = match &options.linker {
                    LinkerMode::Internal => "internal".to_string(),
                    LinkerMode::System(cmd) => cmd.clone(),
                };
                println!(
                    "Compiled {} -> {} (target: {}, linker: {})",
                    options.source_path, options.output_path, options.target, linker_str
                );
            }

            // Publication may perform target-specific finalization (notably
            // ad-hoc Mach-O signing) after the linker produced its byte
            // buffer. Benchmark evidence must describe the artifact users can
            // actually execute, so both compiler and runner hash the published
            // file rather than the pre-publication linker buffer.
            let benchmark_emitted_output = if options.benchmark_json {
                match std::fs::read(&options.output_path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) => {
                        eprintln!(
                            "Error: could not verify published benchmark output '{}': {error}",
                            options.output_path
                        );
                        std::process::exit(1);
                    }
                }
            } else {
                None
            };

            let benchmark_metrics = options.benchmark_json.then(|| output.unstable_metrics());
            let compiler_boundary = options
                .benchmark_json
                .then(|| {
                    compiler_boundary_evidence(
                        &options,
                        resolved_workers,
                        &source_snapshot,
                        benchmark_emitted_output
                            .as_deref()
                            .expect("benchmark output was read after publication"),
                    )
                })
                .flatten();
            let critical_path = benchmark_metrics.and_then(|metrics| {
                timing_data
                    .as_ref()
                    .map(|timing| compiler_critical_path_evidence(timing, metrics))
            });
            print_timing_output(
                &timing_data,
                options.time_passes,
                options.benchmark_json,
                &options.target,
                benchmark_metrics.map(|source_stats| {
                    timing::SourceMetrics {
                        files: source_stats.files,
                        // Every accepted source file is one module in the
                        // canonical import graph. Query-work counters do not
                        // include the root module, so `files` is the exact
                        // one-shot module count rather than a work estimate.
                        modules: source_stats.files,
                        bytes: source_stats.bytes,
                        lines: source_stats.lines,
                        tokens: source_stats.tokens,
                        functions: source_stats.semantic.cfg.functions_considered,
                    }
                }),
                benchmark_metrics.map(benchmark_compiler_work),
                benchmark_emitted_output.as_deref(),
                compiler_boundary.as_ref(),
                critical_path.as_ref(),
            );
        }
        Err(errors) => {
            diagnostics.print_errors(&errors);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_work_projection_preserves_structural_counters() {
        let metrics = rue_compiler::unstable::OneShotMetrics {
            provider_observations: rue_compiler::unstable::ProviderObservationMetrics {
                name_lookups: 2,
                import_lookups: 3,
                method_candidates: 5,
                operator_candidates: 7,
                identity_facts: 11,
                signature_facts: 13,
                type_facts: 17,
                const_facts: 19,
                materializations: 23,
                shared_payload_materializations: 3,
                owned_payload_materializations: 20,
                const_materializations: 2,
                nominal_materializations: 3,
                function_materializations: 11,
                method_materializations: 7,
                anonymous_facts: 29,
                producer_facts: 31,
                toolchain_facts: 37,
                declaration_facts: 60,
            },
            semantic: rue_compiler::unstable::SemanticMetrics {
                cfg: rue_compiler::unstable::SemanticCfgMetrics {
                    materialization_index_builds: 1,
                    materialization_declarations_scanned: 31,
                    materialization_anonymous_nominals_scanned: 7,
                    materialization_type_nodes_scanned: 47,
                    materialization_fact_selections: 11,
                    materialization_declarations_selected: 13,
                    materialization_anonymous_nominals_selected: 17,
                    materialization_callables_selected: 19,
                    materialization_nominal_metadata_selected: 21,
                    materialization_modules_selected: 23,
                    materialization_builtin_nominals_selected: 29,
                    materialization_required_types_selected: 31,
                    prerequisite_stable_types_scanned: 37,
                    prerequisite_layout_requests: 41,
                    prerequisite_type_fact_requests: 43,
                    prerequisite_drop_glue_requests: 47,
                    ..Default::default()
                },
                ..Default::default()
            },
            query_runtime: rue_compiler::unstable::QueryRuntimeMetrics {
                claims: 41,
                reuses: 43,
                joins: 47,
                declined_joins: 2,
                body_completions: 53,
                red_publications: 59,
                green_publications: 61,
                cancellations: 67,
                cycles: 71,
                validation: rue_compiler::unstable::QueryValidationMetrics {
                    traversals: 3,
                    dependency_observations: 11,
                    memo_hits: 7,
                    memo_misses: 2,
                    duplicate_terminal_lease_observations: 5,
                    ..Default::default()
                },
                display_identities: rue_compiler::unstable::QueryDisplayIdentityMetrics {
                    memo_node_materializations: 19,
                    memo_node_bytes: 113,
                    structured_wait_materializations: 23,
                    structured_wait_bytes: 211,
                    abort_fallback_materializations: 2,
                    abort_fallback_bytes: 17,
                },
                retention_enforcements: 13,
                retention_scan_entries: 17,
                ..Default::default()
            },
            ..Default::default()
        };
        let projected = benchmark_compiler_work(metrics);
        assert_eq!(projected.semantic_provider.name_lookups, 2);
        assert_eq!(projected.semantic_provider.import_lookups, 3);
        assert_eq!(projected.semantic_provider.method_candidates, 5);
        assert_eq!(projected.semantic_provider.operator_candidates, 7);
        assert_eq!(projected.semantic_provider.declaration_facts, 60);
        assert_eq!(projected.semantic_provider.identity_facts, 11);
        assert_eq!(projected.semantic_provider.signature_facts, 13);
        assert_eq!(projected.semantic_provider.type_facts, 17);
        assert_eq!(projected.semantic_provider.const_facts, 19);
        assert_eq!(
            projected.semantic_provider.declaration_facts,
            projected.semantic_provider.identity_facts
                + projected.semantic_provider.signature_facts
                + projected.semantic_provider.type_facts
                + projected.semantic_provider.const_facts
        );
        assert_eq!(projected.semantic_provider.materializations, 23);
        assert_eq!(
            projected.semantic_provider.materializations,
            projected.semantic_provider.shared_payload_materializations
                + projected.semantic_provider.owned_payload_materializations
        );
        assert_eq!(projected.semantic_provider.const_materializations, 2);
        assert_eq!(projected.semantic_provider.nominal_materializations, 3);
        assert_eq!(projected.semantic_provider.function_materializations, 11);
        assert_eq!(projected.semantic_provider.method_materializations, 7);
        assert_eq!(
            projected.semantic_provider.materializations,
            projected.semantic_provider.const_materializations
                + projected.semantic_provider.nominal_materializations
                + projected.semantic_provider.function_materializations
                + projected.semantic_provider.method_materializations
        );
        assert_eq!(projected.semantic_provider.anonymous_facts, 29);
        assert_eq!(projected.semantic_provider.producer_facts, 31);
        assert_eq!(projected.semantic_provider.toolchain_facts, 37);
        assert_eq!(projected.cfg_materialization.index_builds, 1);
        assert_eq!(projected.cfg_materialization.declarations_scanned, 31);
        assert_eq!(projected.cfg_materialization.anonymous_nominals_scanned, 7);
        assert_eq!(projected.cfg_materialization.type_nodes_scanned, 47);
        assert_eq!(projected.cfg_materialization.fact_selections, 11);
        assert_eq!(projected.cfg_materialization.declarations_selected, 13);
        assert_eq!(
            projected.cfg_materialization.anonymous_nominals_selected,
            17
        );
        assert_eq!(projected.cfg_materialization.callables_selected, 19);
        assert_eq!(projected.cfg_materialization.nominal_metadata_selected, 21);
        assert_eq!(projected.cfg_materialization.modules_selected, 23);
        assert_eq!(projected.cfg_materialization.builtin_nominals_selected, 29);
        assert_eq!(projected.cfg_materialization.required_types_selected, 31);
        assert_eq!(projected.cfg_prerequisites.stable_types_scanned, 37);
        assert_eq!(projected.cfg_prerequisites.layout_requests, 41);
        assert_eq!(projected.cfg_prerequisites.type_fact_requests, 43);
        assert_eq!(projected.cfg_prerequisites.drop_glue_requests, 47);
        let runtime = projected.query_runtime;
        assert_eq!(runtime.claims, 41);
        assert_eq!(runtime.reuses, 43);
        assert_eq!(runtime.joins, 47);
        assert_eq!(runtime.declined_joins, 2);
        assert_eq!(runtime.body_completions, 53);
        assert_eq!(runtime.red_publications, 59);
        assert_eq!(runtime.green_publications, 61);
        assert_eq!(runtime.cancellations, 67);
        assert_eq!(runtime.cycles, 71);
        assert_eq!(runtime.validation.traversals, 3);
        assert_eq!(runtime.validation.dependency_observations, 11);
        assert_eq!(runtime.validation.memo_hits, 7);
        assert_eq!(runtime.validation.memo_misses, 2);
        assert_eq!(runtime.validation.duplicate_terminal_lease_observations, 5);
        assert_eq!(runtime.retention_enforcements, 13);
        assert_eq!(runtime.retention_scan_entries, 17);
        assert_eq!(runtime.display_identities.memo_node_materializations, 19);
        assert_eq!(runtime.display_identities.memo_node_bytes, 113);
        assert_eq!(
            runtime.display_identities.structured_wait_materializations,
            23
        );
        assert_eq!(runtime.display_identities.structured_wait_bytes, 211);
        assert_eq!(
            runtime.display_identities.abort_fallback_materializations,
            2
        );
        assert_eq!(runtime.display_identities.abort_fallback_bytes, 17);
    }
    use tracing_subscriber::layer::SubscriberExt as _;

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
    fn discovery_parse_and_pipeline_share_the_cli_compile_root() {
        let dir = TestDir::new("timed-discovery-root");
        let main = dir.write(
            "main.rue",
            "const helper = @import(\"helper.rue\");\nfn main() -> i32 { helper.value() }\n",
        );
        dir.write("helper.rue", "pub fn value() -> i32 { 0 }\n");
        let root_source = main.to_string_lossy().into_owned();
        let data = timing::TimingData::new();
        let subscriber =
            tracing_subscriber::registry().with(timing::TimingLayer::new(data.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let compile_span = tracing::info_span!("compile", target = "test");
            let mut host = {
                let _compile = compile_span.enter();
                FilesystemCompilerHost::open(HostOpenRequest {
                    root_source: &root_source,
                    source_manifest_path: None,
                    std_root: None,
                })
                .unwrap()
            };
            assert_eq!(host.source_snapshot().len(), 2);
            assert_eq!(host.accepted_reads().len(), 2);
            assert_eq!(host.root_path(), main);
            assert_eq!(
                host.discovery_context().project_root(),
                dir.path.to_string_lossy().as_ref()
            );
            {
                let _compile = compile_span.enter();
                host.executable_in_compile_scope(&CompileOptions::default())
                    .unwrap();
            }
            drop(compile_span);
        });

        let edges = data.parent_edges();
        // Discovery parsing now sits under the driver's own source-loading
        // phases (RUE-786) rather than directly under the root, so assert the
        // whole chain: the parse must still reach `compile`, and each link
        // names the phase that owns that share of discovery time.
        for expected in [
            ("compile", "source_loading"),
            ("source_loading", "import_discovery_round"),
            ("import_discovery_round", "import_plan"),
            ("import_plan", "import_stage"),
            ("import_stage", "import_parse_staging"),
            ("import_parse_staging", "parse_program"),
            ("parse_program", "parse_file"),
        ] {
            assert!(
                edges.contains(&(expected.0.to_owned(), expected.1.to_owned())),
                "discovery parse escaped the compile root at {expected:?}: {edges:?}"
            );
        }
        assert!(
            edges.contains(&("compile".to_owned(), "compile_pipeline".to_owned())),
            "post-discovery pipeline escaped the compile root: {edges:?}"
        );
        let timing = data.to_benchmark_timing_with_metrics("test", "test", None, None, None);
        for pass in &timing.passes {
            if pass.name == "compile" {
                assert_eq!(pass.invocations, 1);
                assert_eq!(pass.root_invocations, 1);
                assert_eq!(pass.leaf_invocations, 0);
            } else {
                assert_eq!(pass.root_invocations, 0, "{}", pass.name);
            }
        }
    }

    #[test]
    fn watch_cycle_preserves_last_good_output_across_error_and_fix() {
        let dir = TestDir::new("watch-error-fix");
        let main = dir.write("main.rue", "fn main() -> i32 { 1 }\n");
        let destination = dir.path.join("program");
        let mut host = FilesystemCompilerHost::open(HostOpenRequest {
            root_source: main.to_str().unwrap(),
            source_manifest_path: None,
            std_root: None,
        })
        .unwrap();
        let options = CompileOptions::default();

        let compile_cycle = |host: &mut FilesystemCompilerHost| {
            let publication = output::preflight_destination(
                &destination,
                host.source_snapshot().files().map(|source| source.path),
            )
            .unwrap();
            compile::execute_cancellable(compile::CancellableCompileRequest {
                host,
                options: options.clone(),
                destination: publication,
                cancellation: rue_compiler::unstable::CompilationCancellation::new(),
            })
        };

        let compile::CompileCycleOutcome::Linked(linked) = compile_cycle(&mut host) else {
            panic!("initial watch cycle must compile")
        };
        (*linked).publish().result.unwrap();
        let first = fs::read(&destination).unwrap();

        fs::write(&main, "fn main() -> i32 { missing }\n").unwrap();
        host.reobserve().unwrap();
        assert!(matches!(
            compile_cycle(&mut host),
            compile::CompileCycleOutcome::Errors(_)
        ));
        assert_eq!(fs::read(&destination).unwrap(), first);

        fs::write(&main, "fn main() -> i32 { 2 }\n").unwrap();
        host.reobserve().unwrap();
        let compile::CompileCycleOutcome::Linked(linked) = compile_cycle(&mut host) else {
            panic!("fixed watch cycle must compile")
        };
        (*linked).publish().result.unwrap();
        assert_ne!(fs::read(&destination).unwrap(), first);
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Helper to extract Options from ParseResult, panicking if not Options.
    fn unwrap_options(result: ParseResult) -> Options {
        match result {
            ParseResult::Options(opts) => *opts,
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

    // ========== Basic parsing tests ==========

    #[test]
    fn parse_source_file_only() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert_eq!(opts.source_path, "source.rue");
        assert_eq!(opts.output_path, "a.out");
    }

    #[test]
    fn parse_source_and_output() {
        let opts = unwrap_options(parse_args_from(&["source.rue", "output"]));
        assert_eq!(opts.source_path, "source.rue");
        assert_eq!(opts.output_path, "output");
    }

    #[test]
    fn parse_source_manifest() {
        let opts = unwrap_options(parse_args_from(&[
            "--source-manifest",
            "sources.manifest",
            "source.rue",
        ]));
        assert_eq!(opts.source_path, "source.rue");
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

    // ========== Positional source acceptance tests ==========
    //
    // The flat-mode multi-positional input surface was removed (ADR-0046 /
    // RUE-767): every driver form accepts exactly one root source and refuses
    // additional positional .rue arguments.

    #[test]
    fn parse_extra_source_with_output_flag_refused() {
        // -o names the output, so a second positional is an extra source and
        // is refused (RUE-767); before, both were accepted as flat inputs.
        assert!(is_error(&parse_args_from(&[
            "a.rue", "b.rue", "-o", "output"
        ])));
    }

    #[test]
    fn parse_extra_source_with_output_long_flag_refused() {
        assert!(is_error(&parse_args_from(&[
            "a.rue", "b.rue", "--output", "out",
        ])));
    }

    #[test]
    fn parse_multi_file_without_output_flag_error() {
        // Three positional args without -o should error (RUE-767).
        assert!(is_error(&parse_args_from(&["a.rue", "b.rue", "c.rue"])));
    }

    #[test]
    fn extra_positional_sources_diagnostic_exact_wording() {
        // Pin the exact migration diagnostic (RUE-767). The CLI/UI harnesses can
        // only substring-match, so this golden lives here.
        assert_eq!(
            extra_positional_sources_diagnostic("main.rue", "helper.rue"),
            "Error: unexpected extra source file 'helper.rue': the compiler builds one root module and its @import graph\n\
             Compile the root source only; reach helper modules with @import, or list build inputs with --source-manifest:\n\
             \x20      rue main.rue -o <output>"
        );
    }

    #[test]
    fn parse_defers_extensionless_output_identity_check() {
        // RUE-351 remains enforced after closed discovery, when canonical
        // import diagnostics have already received precedence.
        assert!(matches!(
            parse_args_from(&["prog", "-o", "prog"]),
            ParseResult::Options(_)
        ));
    }

    #[test]
    fn parse_defers_different_spelling_output_identity_check() {
        // The post-discovery guard still compares resolved path spellings.
        assert!(matches!(
            parse_args_from(&["./prog", "-o", "prog"]),
            ParseResult::Options(_)
        ));
    }

    #[test]
    fn parse_distinct_output_and_input_ok() {
        // A genuinely different output path must still be accepted.
        let opts = unwrap_options(parse_args_from(&["prog.rue", "-o", "prog"]));
        assert_eq!(opts.source_path, "prog.rue");
        assert_eq!(opts.output_path, "prog");
    }

    #[test]
    fn parse_multi_file_with_options_refused() {
        // Options interleaved with three positional sources still refuse the
        // extras (RUE-767).
        assert!(is_error(&parse_args_from(&[
            "-O2",
            "main.rue",
            "utils.rue",
            "lib.rue",
            "-o",
            "program",
        ])));
    }

    #[test]
    fn parse_extra_source_before_output_flag_refused() {
        // The extra positional is refused regardless of where -o appears.
        assert!(is_error(&parse_args_from(&[
            "-o", "output", "a.rue", "b.rue",
        ])));
    }

    #[test]
    fn parse_single_file_with_output_flag() {
        // Even single file can use -o explicitly
        let opts = unwrap_options(parse_args_from(&["source.rue", "-o", "myprogram"]));
        assert_eq!(opts.source_path, "source.rue");
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
    fn semantic_emits_share_the_canonical_frontend_route_but_rir_is_pre_semantic() {
        // AIR and later stages require semantic body analysis and share the
        // session-query frontend.
        for stage in [
            EmitStage::Air,
            EmitStage::Cfg,
            EmitStage::Lowering,
            EmitStage::Mir,
            EmitStage::Liveness,
            EmitStage::RegAlloc,
            EmitStage::Asm,
            EmitStage::StackFrame,
        ] {
            assert!(emit_requires_semantic(&[stage]));
            assert_eq!(
                emit_frontend_route(&[stage]),
                EmitFrontendRoute::SessionQuery
            );
        }
        // RIR is a pre-semantic presentation: it lowers but never analyzes bodies,
        // so it routes away from the semantic frontend and requires no semantics.
        assert!(!emit_requires_semantic(&[EmitStage::Rir]));
        assert_eq!(
            emit_frontend_route(&[EmitStage::Rir]),
            EmitFrontendRoute::RirOnly
        );
        // A mixed emit that includes any semantic stage still requires semantics
        // (and so acquisition), regardless of the pre-semantic stages alongside it.
        assert!(emit_requires_semantic(&[EmitStage::Ast, EmitStage::Air]));
        assert_eq!(
            emit_frontend_route(&[EmitStage::Ast, EmitStage::Air]),
            EmitFrontendRoute::SessionQuery
        );
        assert!(emit_requires_semantic(&[EmitStage::Rir, EmitStage::Air]));
        assert_eq!(
            emit_frontend_route(&[EmitStage::Rir, EmitStage::Air]),
            EmitFrontendRoute::SessionQuery
        );
        // Pure pre-semantic emits require no semantics.
        assert!(!emit_requires_semantic(&[EmitStage::Ast]));
        assert_eq!(
            emit_frontend_route(&[EmitStage::Ast]),
            EmitFrontendRoute::AstOnlySyntax
        );
        assert!(!emit_requires_semantic(&[EmitStage::Tokens]));
        assert_eq!(
            emit_frontend_route(&[EmitStage::Tokens]),
            EmitFrontendRoute::None
        );
    }

    #[test]
    fn session_emit_frontend_projects_rooted_query_work() {
        let root = FileId::new(9);
        let helper = FileId::new(2);
        let snapshot = test_snapshot(
            root,
            &[
                (
                    root,
                    "/checkout/main.rue",
                    "main.rue",
                    "fn main() -> i32 { 0 }",
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

        assert_eq!(work.parsed.lexer_invocations, 2);
        assert_eq!(work.parsed.parser_invocations, 2);
        assert_eq!(work.lowered.parser_invocations, 0);
        assert_eq!(work.lowered.ast_payload_clones, 0);
        assert_eq!(work.semantic.binding.bind_invocations, 0);
        assert_eq!(work.semantic.manifest.build_invocations, 0);
        assert_eq!(work.semantic.cfg.cfg_builds_attempted, 1);
        assert_eq!(work.semantic.cfg.cfg_builds_succeeded, 1);
        assert_eq!(work.semantic.cfg.cfg_builds_failed, 0);
        let session_work = &frontend.session_work;
        assert_eq!(session_work.updates(), 1);
        assert_eq!(work.semantic.body.analyses_computed, 1);

        let mut presentation_session = CompilerSession::new();
        let presentation = update_for_presentation(&mut presentation_session, &snapshot);
        let ast_work = presentation.unstable_metrics();
        presentation.into_result().unwrap();
        let syntax = presentation_session.published().unwrap();
        assert_eq!(
            snapshot
                .files()
                .map(|source| {
                    let module = syntax
                        .modules()
                        .find(|module| module.file_id() == source.file_id)
                        .unwrap();
                    (module.path().to_owned(), module.item_count())
                })
                .collect::<Vec<_>>(),
            [
                ("/checkout/main.rue".to_owned(), 1),
                ("/checkout/helper.rue".to_owned(), 1)
            ]
        );
        assert_eq!(ast_work.parser_invocations, 2);
        let presentation_work = presentation_session.unstable_metrics();
        assert_eq!(presentation_work.merge().executions, 0);
        assert_eq!(presentation_work.rir().executions, 0);
        assert_eq!(
            presentation_work.semantic_reachability(),
            rue_compiler::unstable::SemanticReachabilityMetrics::default()
        );
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
        let mut session = CompilerSession::new();
        update_for_presentation(&mut session, &snapshot)
            .into_result()
            .unwrap();
        assert_eq!(
            session
                .published()
                .unwrap()
                .modules()
                .next()
                .unwrap()
                .item_count(),
            2
        );
        assert_eq!(session.unstable_metrics().merge().executions, 0);
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
        // executable mode defers the guard until import discovery has closed.
        let opts = unwrap_options(parse_args_from(&["--emit", "ast", "x.rue", "-o", "x.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Ast]);
        assert!(matches!(
            parse_args_from(&["x.rue", "-o", "x.rue"]),
            ParseResult::Options(_)
        ));
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

    // ========== --error-format json surface tests (RUE-436) ==========

    /// A diagnostic rendering fixture over one in-memory file.
    fn json_output(source: &str) -> DiagnosticOutput<'_> {
        DiagnosticOutput::new(
            ErrorFormat::Json,
            vec![(FileId::DEFAULT, SourceInfo::new(source, "main.rue"))],
        )
    }

    /// Every JSON line the CLI writes is an ARRAY of diagnostics, including the
    /// single-error driver paths (output publication, watch-cycle publication)
    /// that used to emit a bare object. A consumer parses one shape.
    #[test]
    fn json_single_error_is_wrapped_in_an_array() {
        let source = "fn main() -> i32 { 0 }\n";
        let error = CompileError::without_span(rue_error::ErrorKind::InternalError(
            "publication exploded".to_string(),
        ));
        let rendered = json_output(source).render_error(&error);

        let batch: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        let batch = batch.as_array().expect("a JSON array, not a bare object");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["severity"], "error");
    }

    /// A graceful ICE (`ice_error!` -> `ErrorKind::InternalError`) is an
    /// ordinary `CompileError`, so it travels the SAME structured path as any
    /// user-facing diagnostic and carries the E9000 internal-error code.
    #[test]
    fn json_graceful_ice_routes_through_the_structured_surface() {
        let source = "fn main() -> i32 { 0 }\n";
        let errors = CompileErrors::from(vec![CompileError::without_span(
            rue_error::ErrorKind::InternalError("did not resolve type".to_string()),
        )]);
        let rendered = json_output(source).render_errors(&errors);

        let batch: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        let batch = batch.as_array().expect("a JSON array");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["code"], ErrorCode::INTERNAL_ERROR.to_string());
        assert_eq!(batch[0]["severity"], "error");
    }

    /// The ICE panic hook renders text until the driver publishes a parsed
    /// `--error-format`, so a panic *inside* argument parsing still gets the
    /// human banner rather than JSON nobody asked for.
    #[test]
    fn ice_error_format_defaults_to_text() {
        // The published format is process-global; assert the default only
        // through a fresh read of the initial value's meaning.
        assert_eq!(
            if ICE_FORMAT_IS_JSON.load(Ordering::Relaxed) {
                ErrorFormat::Json
            } else {
                ErrorFormat::Text
            },
            ice_error_format()
        );
        assert_eq!(ErrorFormat::default(), ErrorFormat::Text);
    }

    /// A compiler panic under `--error-format json` becomes an ordinary
    /// structured diagnostic: E9000, no spans (a panic has no location in the
    /// *user's* program), the panic payload in the message, and the panic site
    /// and compiler version preserved in notes.
    ///
    /// This surface has no end-to-end test on either side of the fence, which
    /// is why the rendering is pinned this precisely. A CLI case cannot host
    /// it — `rue_test_runner::ice_message` fails ANY case whose compiler
    /// panicked, by design, so a crash never satisfies a `compile_fail`. Nor
    /// can a unit test raise a real panic and read the hook's output: the
    /// crate is built with `panic = "abort"` (`toolchains/rust/defs.bzl`), so
    /// `catch_unwind` cannot catch and the panicking process dies. The hook
    /// itself is therefore a three-line projection onto this function.
    #[test]
    fn ice_panic_is_published_as_a_json_diagnostic() {
        let rendered = ice_diagnostic(
            "cfg edge 7 exploded",
            Some("crates/rue-cfg/src/build.rs:118:9".to_string()),
        )
        .to_json();
        let diagnostic: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

        assert_eq!(diagnostic["severity"], "error");
        assert_eq!(diagnostic["code"], ErrorCode::INTERNAL_ERROR.to_string());
        assert_eq!(
            diagnostic["spans"].as_array().expect("spans array").len(),
            0
        );
        let message = diagnostic["message"].as_str().expect("message string");
        assert!(
            message.contains("internal compiler error"),
            "the ICE marker the harness detector greps for is missing: {message}"
        );
        assert!(
            message.contains("cfg edge 7 exploded"),
            "the panic payload was dropped: {message}"
        );

        let notes: Vec<&str> = diagnostic["notes"]
            .as_array()
            .expect("notes array")
            .iter()
            .map(|note| note.as_str().expect("note string"))
            .collect();
        assert!(
            notes.iter().any(|note| note.contains(VERSION)),
            "the compiler version note was dropped: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|note| *note == "panic at crates/rue-cfg/src/build.rs:118:9"),
            "the panic site note was dropped: {notes:?}"
        );

        let helps: Vec<&str> = diagnostic["helps"]
            .as_array()
            .expect("helps array")
            .iter()
            .map(|help| help.as_str().expect("help string"))
            .collect();
        assert!(
            helps.iter().any(|help| help.contains("issues")),
            "the report-this-bug help was dropped: {helps:?}"
        );
    }

    /// A panic with no recorded location (possible for a panic raised outside
    /// Rust's `#[track_caller]` machinery) still renders a valid diagnostic —
    /// it simply omits the site note rather than emitting a hole.
    #[test]
    fn ice_diagnostic_without_a_panic_site_is_still_valid() {
        let rendered = ice_diagnostic("no location", None).to_json();
        let diagnostic: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        let notes: Vec<&str> = diagnostic["notes"]
            .as_array()
            .expect("notes array")
            .iter()
            .map(|note| note.as_str().expect("note string"))
            .collect();
        assert!(
            !notes.iter().any(|note| note.starts_with("panic at ")),
            "an absent panic site must be omitted, not rendered empty: {notes:?}"
        );
    }

    /// `panic!` hands the hook either a `&'static str` (a literal) or a
    /// `String` (a formatted message); anything else is opaque. All three
    /// produce a usable message rather than an empty one.
    #[test]
    fn panic_payloads_of_every_shape_produce_a_message() {
        let literal: Box<dyn std::any::Any + Send> = Box::new("cfg edge exploded");
        assert_eq!(panic_payload_message(literal.as_ref()), "cfg edge exploded");

        let formatted: Box<dyn std::any::Any + Send> = Box::new("cfg edge 7 exploded".to_string());
        assert_eq!(
            panic_payload_message(formatted.as_ref()),
            "cfg edge 7 exploded"
        );

        let opaque: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(
            panic_payload_message(opaque.as_ref()),
            "panic with a non-string payload"
        );
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
        // --emit produces no executable, so every positional is a source. Only
        // the single root source is accepted; a second positional is refused
        // (RUE-767).
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "x86_64-linux",
            "--linker",
            "clang",
            "-O2",
            "--emit",
            "air",
            "source.rue",
        ]));
        assert_eq!(opts.source_path, "source.rue");
        assert_eq!(opts.target, Target::X86_64Linux);
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit_stages, vec![EmitStage::Air]);
    }

    #[test]
    fn parse_emit_with_extra_source_refused() {
        // The dropped second positional above is refused on its own (RUE-767).
        assert!(is_error(&parse_args_from(&[
            "--emit",
            "air",
            "source.rue",
            "other.rue",
        ])));
    }

    #[test]
    fn parse_options_after_source() {
        // Options can appear after the source file
        let opts = unwrap_options(parse_args_from(&["source.rue", "-O1"]));
        assert_eq!(opts.source_path, "source.rue");
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
        assert_eq!(opts.source_path, "source.rue");
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

    // ========== --watch tests ==========

    #[test]
    fn parse_watch() {
        let opts = unwrap_options(parse_args_from(&["--watch", "source.rue"]));
        assert!(opts.watch);
        assert!(validate_watch_modes(&opts).is_ok());
    }

    #[test]
    fn watch_rejects_presentation_and_benchmark_modes() {
        for args in [
            &["--watch", "--emit", "air", "source.rue"][..],
            &["--watch", "--benchmark-json", "source.rue"][..],
            &["--watch", "--time-passes", "source.rue"][..],
        ] {
            let opts = unwrap_options(parse_args_from(args));
            assert!(validate_watch_modes(&opts).is_err(), "{args:?}");
        }
    }

    #[test]
    fn watch_is_disabled_by_default() {
        let opts = unwrap_options(parse_args_from(&["source.rue"]));
        assert!(!opts.watch);
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
