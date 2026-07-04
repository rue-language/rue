use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{EnvFilter, fmt};

mod timing;

use rue_compiler::{
    CompileOptions, FileId, Lexer, LinkerMode, MultiFileFormatter, OptLevel, ParsedProgram,
    PreviewFeature, PreviewFeatures, SourceFile, SourceInfo, TokenKind,
    compile_multi_file_with_options, configure_thread_pool, generate_emitted_asm,
    generate_liveness_info, generate_lowering_info, generate_mir, generate_regalloc_info,
    generate_stack_frame_info, merge_symbols, parse_all_files,
};
use rue_rir::RirPrinter;
use rue_target::Target;

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
            _ => Err(ParseEmitStageError(s.to_string())),
        }
    }
}

impl EmitStage {
    fn all_names() -> &'static str {
        "tokens, ast, rir, air, cfg, lowering, mir, liveness, regalloc, asm, stackframe"
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

struct Options {
    /// Source files to compile. In single-file mode, contains one path.
    /// In multi-file mode, contains multiple paths.
    source_paths: Vec<String>,
    output_path: String,
    emit_stages: Vec<EmitStage>,
    target: Target,
    linker: LinkerMode,
    opt_level: OptLevel,
    preview_features: PreviewFeatures,
    log_level: LogLevel,
    log_format: LogFormat,
    time_passes: bool,
    benchmark_json: bool,
    /// Number of parallel jobs (0 = auto-detect, use all cores).
    jobs: usize,
}

/// Version string for the rue compiler (single-sourced from rue-error so the
/// `--version` banner and ICE reports can never drift apart).
const VERSION: &str = rue_compiler::VERSION;

fn print_version() {
    println!("rue {}", VERSION);
}

fn print_usage() {
    eprintln!("Usage: rue [options] <source.rue> [output]");
    eprintln!("       rue [options] <source1.rue> <source2.rue> ... -o <output>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <path>  Set output path (required for multiple source files)");
    eprintln!("  --target <target>    Set compilation target (default: host)");
    eprintln!(
        "                       Valid targets: {}",
        Target::all_names()
    );
    eprintln!("  --linker <linker>    Set linker to use (default: internal)");
    eprintln!("                       Use 'internal' for built-in linker, or a command");
    eprintln!("                       like 'clang', 'gcc', or 'ld' for system linker");
    eprintln!("  -O<level>            Set optimization level (default: -O0)");
    eprintln!("                       Levels: {}", OptLevel::all_names());
    eprintln!("  -j, --jobs <N>       Set number of parallel jobs (default: 0 = auto)");
    eprintln!("                       Use -j1 for single-threaded compilation");
    eprintln!("  --emit <stage>       Emit intermediate representation and exit");
    eprintln!("                       Can be specified multiple times for multiple outputs");
    eprintln!("                       Stages: {}", EmitStage::all_names());
    eprintln!("  --preview <feature>  Enable a preview feature (can be repeated)");
    eprintln!(
        "                       Features: {}",
        PreviewFeature::all_names()
    );
    eprintln!("  --log-level <level>  Set logging level (default: off)");
    eprintln!("                       Levels: {}", LogLevel::all_names());
    eprintln!("                       Can also use RUST_LOG environment variable");
    eprintln!("  --log-format <fmt>   Set logging format (default: text)");
    eprintln!("                       Formats: {}", LogFormat::all_names());
    eprintln!("  --time-passes        Show timing for each compilation pass");
    eprintln!("  --benchmark-json     Output timing as JSON (for benchmarking)");
    eprintln!("  --version            Show version information");
    eprintln!("  --help               Show this help message");
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
    let mut time_passes = false;
    let mut benchmark_json = false;
    let mut jobs: Option<usize> = None;
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
            "--jobs" | "-j" => {
                let Some(jobs_str) = args_iter.next() else {
                    eprintln!("Error: --jobs requires a value");
                    return ParseResult::Error;
                };
                match jobs_str.parse::<usize>() {
                    Ok(j) => jobs = Some(j),
                    Err(_) => {
                        eprintln!("Error: --jobs value must be a non-negative integer");
                        return ParseResult::Error;
                    }
                }
            }
            "-o" | "--output" => {
                let Some(out_str) = args_iter.next() else {
                    eprintln!("Error: -o requires an output path");
                    return ParseResult::Error;
                };
                output_path = Some(out_str.to_string());
            }
            "--time-passes" => {
                time_passes = true;
            }
            "--benchmark-json" => {
                benchmark_json = true;
            }
            "--help" | "-h" => {
                print_usage();
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
                match jobs_str.parse::<usize>() {
                    Ok(j) => jobs = Some(j),
                    Err(_) => {
                        eprintln!("Error: --jobs value must be a non-negative integer");
                        return ParseResult::Error;
                    }
                }
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
        // every positional arg is a source file. Without this, the legacy
        // two-positional mode claimed the second FILE as the output path and
        // `--emit air a.rue b.rue` was impossible (RUE-130).
        (positional, "a.out".to_string())
    } else if positional.len() == 1 {
        // Single source file, no -o: default output to a.out
        (positional, "a.out".to_string())
    } else if positional.len() == 2 {
        // Two positional args, no -o: backwards compatible mode
        // First is source, second is output — but NEVER treat a .rue file as
        // the output. `rue a.rue b.rue` used to silently overwrite the b.rue
        // SOURCE FILE with the compiled binary (RUE-130); the user almost
        // certainly meant to compile both.
        if positional[1].ends_with(".rue") {
            eprintln!(
                "Error: refusing to use '{}' as the output path: it looks like a source file",
                positional[1]
            );
            eprintln!("To compile multiple source files, specify the output with -o:");
            eprintln!("       rue {} {} -o <output>", positional[0], positional[1]);
            return ParseResult::Error;
        }
        let mut pos = positional;
        let out = pos.pop().unwrap();
        (pos, out)
    } else {
        // Multiple source files without -o: error
        eprintln!("Error: multiple source files require -o to specify output path");
        eprintln!("Usage: rue a.rue b.rue -o output");
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
        // silently overwrote the source (RUE-351).
        let output_key = clobber_key(&final_output_path);
        if source_paths.iter().any(|s| clobber_key(s) == output_key) {
            eprintln!(
                "Error: output path '{}' is also an input source file; refusing to overwrite it",
                final_output_path
            );
            return ParseResult::Error;
        }
    }

    if log_format.is_some() && log_level.is_none() {
        eprintln!(
            "Warning: --log-format has no effect without --log-level (logging is off by default)"
        );
    }

    ParseResult::Options(Options {
        source_paths,
        output_path: final_output_path,
        emit_stages,
        target: target.unwrap_or_else(Target::host),
        linker: linker.unwrap_or_default(),
        opt_level: opt_level.unwrap_or_default(),
        preview_features,
        log_level: log_level.unwrap_or_default(),
        log_format: log_format.unwrap_or_default(),
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
            let subscriber = tracing_subscriber::registry()
                .with(filter.unwrap())
                .with(timing_layer)
                .with(
                    fmt::layer()
                        .with_target(true)
                        .with_span_events(FmtSpan::CLOSE)
                        .with_writer(std::io::stderr),
                );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // Timing + JSON logging
        (true, true, LogFormat::Json) => {
            let timing_layer = timing::TimingLayer::new(timing_data.clone().unwrap());
            let subscriber = tracing_subscriber::registry()
                .with(filter.unwrap())
                .with(timing_layer)
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_span_events(FmtSpan::CLOSE)
                        .with_writer(std::io::stderr),
                );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // Text logging only (no timing)
        (false, true, LogFormat::Text) => {
            let subscriber = tracing_subscriber::registry().with(filter.unwrap()).with(
                fmt::layer()
                    .with_target(true)
                    .with_span_events(FmtSpan::CLOSE)
                    .with_writer(std::io::stderr),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("failed to set tracing subscriber");
        }

        // JSON logging only (no timing)
        (false, true, LogFormat::Json) => {
            let subscriber = tracing_subscriber::registry().with(filter.unwrap()).with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_span_events(FmtSpan::CLOSE)
                    .with_writer(std::io::stderr),
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

/// Discover `@import("...")` references in the given sources and load the
/// referenced module files from disk, transitively, appending them to
/// `sources` as if the user had listed them on the command line.
///
/// Sema resolves import paths only against loaded files (see
/// `resolve_import_path` in rue-air), so this is the step that makes
/// `@import` work without hand-listing every module (RUE-14).
///
/// Imports are found by scanning the token stream (so comments and string
/// contents are handled correctly) for the `@import ( "<path>" )` shape.
/// Resolution mirrors sema's `ModulePath` order, probing the filesystem:
///
/// - `"std"`: `$RUE_STD_PATH/_std.rue`, then `std/_std.rue` relative to the
///   importing file, then relative to the first (root) source file
/// - `"foo.rue"`: exact path relative to the importing file, then the root
/// - `"foo"` / `"a/b"`: `{path}.rue` then the directory-module facade
///   `{path}/_{basename}.rue` (the facade lives INSIDE the directory, like
///   `std/_std.rue` — ratified in RUE-137), relative to the importing file,
///   then the root
///
/// Unresolvable imports are left for sema to report (E0704 with candidate
/// context); this function only loads what it can find.
fn discover_and_load_imports(sources: &mut Vec<(String, String)>) {
    use std::collections::HashSet;
    use std::path::PathBuf;

    let root_dir = Path::new(&sources[0].0)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default();

    // Canonical paths of everything already loaded (for dedupe and cycles).
    let mut loaded: HashSet<PathBuf> = sources
        .iter()
        .filter_map(|(p, _)| fs::canonicalize(p).ok())
        .collect();

    let mut i = 0;
    while i < sources.len() {
        let (importer_path, content) = (sources[i].0.clone(), sources[i].1.clone());
        i += 1;

        let lexer = Lexer::new(&content);
        let Ok((tokens, interner)) = lexer.tokenize() else {
            // Lex errors will be reported properly during compilation.
            continue;
        };

        for w in tokens.windows(4) {
            let (
                TokenKind::AtImport(_),
                TokenKind::LParen,
                TokenKind::String(s),
                TokenKind::RParen,
            ) = (&w[0].kind, &w[1].kind, &w[2].kind, &w[3].kind)
            else {
                continue;
            };
            let import_str = interner.resolve(s);

            let importer_dir = Path::new(&importer_path)
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default();

            // Candidate GROUPS, nearest base directory first. Within a
            // group, EVERY existing candidate is loaded — if both `foo.rue`
            // and `foo/_foo.rue` exist, loading both lets sema report the
            // dual-entity ambiguity (E0708) instead of the driver silently
            // picking one. Later groups are only probed when the nearer
            // group had nothing.
            let mut groups: Vec<Vec<PathBuf>> = Vec::new();
            // "std" is the general directory-module rule (std/_std.rue) plus
            // one extra candidate group: an installed stdlib via $RUE_STD_PATH.
            if import_str == "std"
                && let Ok(std_path) = env::var("RUE_STD_PATH")
            {
                groups.push(vec![Path::new(&std_path).join("_std.rue")]);
            }
            if import_str.ends_with(".rue") {
                groups.push(vec![importer_dir.join(import_str)]);
                groups.push(vec![root_dir.join(import_str)]);
            } else {
                // Directory-module facade: foo/_foo.rue (inside the
                // directory, mirroring std/_std.rue — RUE-137).
                let rel = Path::new(import_str);
                let basename = rel.file_name().unwrap_or_default().to_string_lossy();
                let facade = rel.join(format!("_{basename}.rue"));
                groups.push(vec![
                    importer_dir.join(format!("{import_str}.rue")),
                    importer_dir.join(&facade),
                ]);
                groups.push(vec![
                    root_dir.join(format!("{import_str}.rue")),
                    root_dir.join(&facade),
                ]);
            }

            'groups: for group in groups {
                let mut group_hit = false;
                for candidate in group {
                    let Ok(canonical) = fs::canonicalize(&candidate) else {
                        continue;
                    };
                    if !canonical.is_file() {
                        continue;
                    }
                    if loaded.contains(&canonical) {
                        group_hit = true; // already loaded (or a cycle)
                        continue;
                    }
                    let Ok(module_content) = fs::read_to_string(&candidate) else {
                        continue;
                    };
                    loaded.insert(canonical);
                    sources.push((candidate.to_string_lossy().into_owned(), module_content));
                    group_hit = true;
                }
                if group_hit {
                    break 'groups;
                }
            }
        }
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

    // Read all source files into memory
    let mut sources: Vec<(String, String)> = options
        .source_paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("Error reading {}: {}", path, e);
                std::process::exit(1);
            });
            (path.clone(), content)
        })
        .collect();

    // Discover and load @import-ed modules from disk, transitively. Sema
    // resolves imports only against already-loaded files, so without this
    // step `const utils = @import("utils")` fails with E0704 unless the
    // user hand-lists every module on the command line (RUE-14).
    discover_and_load_imports(&mut sources);
    let sources = sources;

    // Build SourceFile structs for multi-file compilation
    let source_files: Vec<SourceFile<'_>> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            SourceFile::new(path.as_str(), content.as_str(), FileId::new((i + 1) as u32))
        })
        .collect();

    // Create multi-file formatter for diagnostics that may span multiple files
    let source_infos: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, (path, content))| {
            (
                FileId::new((i + 1) as u32),
                SourceInfo::new(content.as_str(), path.as_str()),
            )
        })
        .collect();
    let formatter = MultiFileFormatter::new(source_infos);

    // Compute source metrics if benchmark JSON is requested
    let source_metrics = if options.benchmark_json {
        // Sum metrics across ALL files — measuring only the first file made
        // multi-file benchmark numbers meaningless (RUE-130).
        let mut bytes = 0usize;
        let mut lines = 0usize;
        let mut tokens = 0usize;
        for (_, content) in &sources {
            bytes += content.len();
            lines += content.lines().count();
            let lexer = Lexer::new(content);
            if let Ok((toks, _interner)) = lexer.tokenize() {
                tokens += toks.len();
            }
            // If lexing fails, we'll get the error during compilation anyway
        }
        Some(timing::SourceMetrics {
            bytes,
            lines,
            tokens,
        })
    } else {
        None
    };

    // --emit and --benchmark-json both own stdout, so combining them would
    // interleave IR text with the benchmark JSON and corrupt it — reject early.
    if options.benchmark_json && !options.emit_stages.is_empty() {
        eprintln!("Error: --emit cannot be combined with --benchmark-json (both write to stdout)");
        std::process::exit(1);
    }

    // Handle emit modes with multi-file support
    if !options.emit_stages.is_empty() {
        if let Err(()) = handle_emit_multi_file(&source_files, &options, &formatter) {
            std::process::exit(1);
        }
        print_timing_output(
            &timing_data,
            options.time_passes,
            options.benchmark_json,
            &options.target,
            source_metrics,
        );
        return;
    }

    // Normal compilation - uses multi-file compilation for all source files
    let compile_options = CompileOptions {
        target: options.target,
        linker: options.linker.clone(),
        opt_level: options.opt_level,
        preview_features: options.preview_features.clone(),
        jobs: options.jobs,
    };
    match compile_multi_file_with_options(&source_files, &compile_options) {
        Ok(output) => {
            // Print warnings using the diagnostic formatter
            if !output.warnings.is_empty() {
                eprintln!("{}", formatter.format_warnings(&output.warnings));
            }

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
                    let result = Command::new("codesign")
                        .args(["-f", "-s", "-", &options.output_path])
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
                source_metrics,
            );
        }
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    }
}

/// Handle emit stages for multi-file compilation.
///
/// For early stages (tokens, ast), each file is processed and labeled individually.
/// For later stages (rir, air, cfg, etc.), the merged program is used.
fn handle_emit_multi_file(
    sources: &[SourceFile<'_>],
    options: &Options,
    formatter: &MultiFileFormatter,
) -> Result<(), ()> {
    // Determine which stages we need
    let needs_tokens = options.emit_stages.contains(&EmitStage::Tokens);
    let needs_ast = options.emit_stages.contains(&EmitStage::Ast);
    let needs_later_stages = options.emit_stages.iter().any(|s| {
        matches!(
            s,
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
    });

    // For tokens, we need to lex each file separately (before parsing merges interners)
    // We'll collect per-file tokens if needed
    let per_file_tokens: Option<Vec<(String, Vec<rue_compiler::Token>)>> = if needs_tokens {
        let mut file_tokens = Vec::with_capacity(sources.len());
        for source in sources {
            // Lex with the file's real FileId so a lex error in the Nth file
            // is attributed to that file, not to the first one (RUE-38).
            let lexer = Lexer::with_file_id(source.source, source.file_id);
            match lexer.tokenize() {
                Ok((tokens, _interner)) => {
                    file_tokens.push((source.path.to_string(), tokens));
                }
                Err(e) => {
                    eprintln!("{}", formatter.format_error(&e));
                    return Err(());
                }
            }
        }
        Some(file_tokens)
    } else {
        None
    };

    // Parse all files (needed for AST output or later stages)
    let mut parsed: Option<ParsedProgram> = if needs_ast || needs_later_stages {
        match parse_all_files(sources) {
            Ok(program) => Some(program),
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        }
    } else {
        None
    };

    // For AST output, collect the per-file AST info before merging (which consumes the program)
    let per_file_asts: Option<Vec<(String, rue_compiler::Ast)>> = if needs_ast {
        parsed.as_ref().map(|program| {
            program
                .files
                .iter()
                .map(|f| (f.path.clone(), f.ast.clone()))
                .collect()
        })
    } else {
        None
    };

    // Merge symbols and compile frontend (needed for later stages)
    let frontend_state = if needs_later_stages {
        // Take ownership of the parsed program (already parsed above)
        let program = parsed
            .take()
            .expect("parsed should be Some when needs_later_stages is true");

        let merged = match merge_symbols(program) {
            Ok(m) => m,
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        };

        // Thread file paths through so @import resolution works under --emit
        // exactly as in a normal build (RUE-130).
        let file_paths: std::collections::HashMap<FileId, String> = sources
            .iter()
            .map(|s| (s.file_id, s.path.to_string()))
            .collect();
        let state = match rue_compiler::compile_frontend_from_ast_with_file_paths(
            merged.ast,
            merged.interner,
            options.opt_level,
            &options.preview_features,
            file_paths,
        ) {
            Ok(state) => state,
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        };

        // Warnings used to be silently dropped in all --emit modes (RUE-130).
        if !state.warnings.is_empty() {
            eprintln!("{}", formatter.format_warnings(&state.warnings));
        }

        Some(state)
    } else {
        None
    };

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
                    let printer = RirPrinter::new(&state.rir, &state.interner);
                    println!("{}", printer);
                }
                println!();
            }
            EmitStage::Air => {
                println!("=== AIR ===");
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!("function {}:", func.analyzed.name);
                        println!("{}", func.analyzed.air);
                    }
                }
                println!();
            }
            EmitStage::Cfg => {
                println!("=== CFG ===");
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!("{}", func.cfg);
                    }
                }
                println!();
            }
            EmitStage::Lowering => {
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        let lowering_info = generate_lowering_info(
                            &func.cfg,
                            &state.type_pool,
                            &state.interner,
                            options.target,
                        );
                        print!("{}", lowering_info);
                    }
                }
                println!();
            }
            EmitStage::Mir => {
                println!("=== MIR ({}) ===", options.target);
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        let mir = match generate_mir(
                            &func.cfg,
                            &state.type_pool,
                            &state.interner,
                            options.target,
                        ) {
                            Ok(mir) => mir,
                            Err(e) => {
                                eprintln!("{}", formatter.format_error(&e));
                                return Err(());
                            }
                        };
                        println!("function {}:", func.analyzed.name);
                        println!("{}", mir);
                    }
                }
                println!();
            }
            EmitStage::Liveness => {
                println!("=== Liveness Analysis ({}) ===", options.target);
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!("function {}:", func.analyzed.name);
                        let liveness_info = match generate_liveness_info(
                            &func.cfg,
                            &state.type_pool,
                            &state.interner,
                            options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                eprintln!("{}", formatter.format_error(&e));
                                return Err(());
                            }
                        };
                        println!("{}", liveness_info);
                    }
                }
                println!();
            }
            EmitStage::RegAlloc => {
                println!("=== Register Allocation ({}) ===", options.target);
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!("function {}:", func.analyzed.name);
                        let regalloc_info = match generate_regalloc_info(
                            &func.cfg,
                            &state.type_pool,
                            &state.interner,
                            options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                eprintln!("{}", formatter.format_error(&e));
                                return Err(());
                            }
                        };
                        print!("{}", regalloc_info);
                    }
                }
                println!();
            }
            EmitStage::Asm => {
                println!("=== Assembly ({}) ===", options.target);
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!(".globl {}", func.analyzed.name);
                        println!("{}:", func.analyzed.name);
                        let asm = match generate_emitted_asm(
                            &func.cfg,
                            &state.type_pool,
                            &state.strings,
                            &state.interner,
                            options.target,
                        ) {
                            Ok(asm) => asm,
                            Err(e) => {
                                eprintln!("{}", formatter.format_error(&e));
                                return Err(());
                            }
                        };
                        print!("{}", asm);
                    }
                }
                println!();
            }
            EmitStage::StackFrame => {
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        let frame_info = match generate_stack_frame_info(
                            &func.cfg,
                            &func.analyzed.name,
                            &state.type_pool,
                            &state.interner,
                            options.target,
                        ) {
                            Ok(info) => info,
                            Err(e) => {
                                eprintln!("{}", formatter.format_error(&e));
                                return Err(());
                            }
                        };
                        println!("{}", frame_info);
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // RUE-351: `-o` targeting an extensionless input source is refused. The
        // old guard only recognized inputs by a `.rue` suffix. Paths resolve
        // against the cwd; neither file needs to exist for the keys to match.
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
            "tokens, ast, rir, air, cfg, lowering, mir, liveness, regalloc, asm, stackframe"
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
