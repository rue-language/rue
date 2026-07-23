use std::env;
use std::path::{Path, PathBuf};
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
mod source_loader;
mod timing;

use emit::EmitStage;
#[cfg(test)]
use emit::{EmitFrontendRoute, build_emit_frontend, emit_frontend_route, emit_requires_semantic};
#[cfg(test)]
use rue_compiler::unstable::update_for_presentation;
use rue_compiler::unstable::{MultiFileFormatter, MultiFileJsonFormatter, SourceInfo};
use source_loader::{SourceLoadError, SourceLoadRequest};
#[cfg(test)]
use source_loader::{
    SourceManifest, derive_symbol_paths_with_std_root, discover_and_load_imports,
    parse_source_manifest_entry,
};

use rue_compiler::{
    CompileErrors, CompileOptions, CompileWarning, FileId, ImportDiscoveryStatus, LinkerMode,
    OptLevel, PreviewFeature, PreviewFeatures, configure_thread_pool,
};
#[cfg(test)]
use rue_compiler::{CompilerSession, SourceMetadata, SourceSnapshot};
use rue_error::CompileError;
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
    emitted_output: Option<&[u8]>,
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

    // Reject incompatible output modes before any tracing, thread-pool, manifest,
    // or source I/O work. This is a pure options check, so surfacing it first
    // keeps an options error from being masked by — or nondeterministically
    // ordered behind — an unrelated missing-file or manifest failure (RUE-798).
    if let Err(message) = emit::validate_output_modes(&options.emit_stages, options.benchmark_json)
    {
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

    // Configure Rayon's global thread pool ONCE, before dispatching to either
    // the `--emit` path or the normal compile path. `build_global()` panics if
    // called twice, so it lives here rather than inside a per-compilation entry
    // point — the previous placement meant `--emit` ignored `-j`/`--jobs`
    // entirely (RUE-352).
    configure_thread_pool(options.jobs);

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
    let mut import_discovery = {
        let _compile = compile_span.enter();
        match source_loader::load(SourceLoadRequest {
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
        if let Err(error) = source_loader::acquire_reached_toolchain_modules(
            &mut import_discovery,
            &compile_options,
        ) {
            report_source_load_error(error, options.error_format);
        }
    }

    #[cfg(rue_benchmark_allocations)]
    if options.benchmark_json {
        allocation::pause();
    }
    let source_snapshot = import_discovery.source_snapshot.clone();
    tracing::debug!(
        root = %import_discovery.resolution.root_path.display(),
        project_root = import_discovery.resolution.context.project_root(),
        std_root = import_discovery.resolution.context.std_root(),
        read_policy_revision = import_discovery.resolution.context.read_policy_revision(),
        accepted_reads = import_discovery.read_manifest.len(),
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
                source_snapshot: &source_snapshot,
                session: &mut import_discovery.session,
                stages: &options.emit_stages,
                discovery_revision: &import_discovery.revision,
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
        );
        return;
    }

    if import_discovery.revision.status() != ImportDiscoveryStatus::ClosedValid {
        diagnostics.print_errors(import_discovery.revision.diagnostics());
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
            session: &mut import_discovery.session,
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
            let publication = output.publish();
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

            print_timing_output(
                &timing_data,
                options.time_passes,
                options.benchmark_json,
                &options.target,
                options.benchmark_json.then(|| {
                    let source_stats = output.unstable_metrics();
                    timing::SourceMetrics {
                        files: source_stats.files,
                        bytes: source_stats.bytes,
                        lines: source_stats.lines,
                        tokens: source_stats.tokens,
                    }
                }),
                options
                    .benchmark_json
                    .then_some(output.linked_bytes.as_slice()),
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
            let mut discovery = {
                let _compile = compile_span.enter();
                discover_and_load_imports(&root_source, None, None).unwrap()
            };
            assert_eq!(discovery.source_snapshot.len(), 2);
            assert_eq!(discovery.read_manifest.len(), 2);
            assert_eq!(discovery.resolution.root_path, main);
            assert_eq!(
                discovery.resolution.context.project_root(),
                dir.path.to_string_lossy().as_ref()
            );
            {
                let _compile = compile_span.enter();
                rue_compiler::unstable::executable_in_compile_scope(
                    &mut discovery.session,
                    &CompileOptions::default(),
                )
                .unwrap();
            }
            drop(compile_span);
        });

        let edges = data.parent_edges();
        assert!(
            edges.contains(&("compile".to_owned(), "parse_file".to_owned())),
            "discovery parse escaped the compile root: {edges:?}"
        );
        assert!(
            edges.contains(&("compile".to_owned(), "compile_pipeline".to_owned())),
            "post-discovery pipeline escaped the compile root: {edges:?}"
        );
        let timing = data.to_benchmark_timing_with_metrics("test", "test", None, None);
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
        assert_eq!(work.semantic.binding.bind_invocations, 1);
        assert_eq!(work.semantic.manifest.build_invocations, 1);
        assert_eq!(work.semantic.cfg.cfg_builds_attempted, 1);
        assert_eq!(work.semantic.cfg.cfg_builds_succeeded, 1);
        assert_eq!(work.semantic.cfg.cfg_builds_failed, 0);
        let session_work = &frontend.session_work;
        assert_eq!(session_work.updates(), 1);
        assert_eq!(session_work.merge().executions, 1);
        assert_eq!(session_work.rir().executions, 1);
        assert_eq!(session_work.semantic().executions, 1);

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
        assert_eq!(presentation_work.semantic().executions, 0);
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

        let root_source = main_path.to_string_lossy().into_owned();
        let result = discover_and_load_imports(&root_source, None, None);
        assert!(
            result.is_err(),
            "unreadable existing candidate must error, not resolve or vanish"
        );

        // Control: once the candidate is valid text, discovery loads it.
        fs::write(dir.join("helper.rue"), "pub fn h() -> i32 {{ 1 }}\n").unwrap();
        let result = discover_and_load_imports(&root_source, None, None).unwrap();
        assert_eq!(
            result.source_snapshot.len(),
            2,
            "helper must be discovered and loaded"
        );

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
