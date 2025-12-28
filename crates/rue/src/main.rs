use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;

use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

mod timing;

use rue_compiler::{
    CompileOptions, DiagnosticFormatter, Lexer, LinkerMode, OptLevel, Parser, PreviewFeature,
    PreviewFeatures, SourceInfo, compile_frontend_from_ast_with_options, compile_with_options,
    generate_allocated_mir, generate_emitted_asm, generate_liveness_info, generate_lowering_info,
    generate_mir, generate_regalloc_info, generate_stack_frame_info,
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
    source_path: String,
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
}

/// Version string for the rue compiler.
const VERSION: &str = "0.1.0";

fn print_version() {
    println!("rue {}", VERSION);
}

fn print_usage() {
    eprintln!("Usage: rue [options] <source.rue> [output]");
    eprintln!();
    eprintln!("Options:");
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
    eprintln!("  --emit <stage>       Emit intermediate representation and exit");
    eprintln!("                       Can be specified multiple times for multiple outputs");
    eprintln!("                       Stages: tokens, ast, rir, air, cfg, mir, asm");
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

    let source_path = positional[0].clone();
    let output_path = positional
        .get(1)
        .cloned()
        .unwrap_or_else(|| "a.out".to_string());

    ParseResult::Options(Options {
        source_path,
        output_path,
        emit_stages,
        target: target.unwrap_or_else(Target::host),
        linker: linker.unwrap_or_default(),
        opt_level: opt_level.unwrap_or_default(),
        preview_features,
        log_level: log_level.unwrap_or_default(),
        log_format: log_format.unwrap_or_default(),
        time_passes,
        benchmark_json,
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
) {
    if let Some(timing) = timing_data {
        if benchmark_json {
            // JSON output goes to stdout for easy capture
            // Include metadata for historical analysis
            println!("{}", timing.to_json(&target.to_string(), VERSION));
        } else if time_passes {
            // Human-readable output goes to stderr
            eprintln!("{}", timing.report());
        }
    }
}

fn main() {
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

    // Read source
    let source = fs::read_to_string(&options.source_path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", options.source_path, e);
        std::process::exit(1);
    });

    // Create source info for diagnostic formatting
    let source_info = SourceInfo::new(&source, &options.source_path);
    let formatter = DiagnosticFormatter::new(&source_info);

    // Handle emit modes
    if !options.emit_stages.is_empty() {
        if let Err(()) = handle_emit(&source, &options, &formatter) {
            std::process::exit(1);
        }
        print_timing_output(
            &timing_data,
            options.time_passes,
            options.benchmark_json,
            &options.target,
        );
        return;
    }

    // Normal compilation
    let compile_options = CompileOptions {
        target: options.target,
        linker: options.linker.clone(),
        opt_level: options.opt_level,
        preview_features: options.preview_features.clone(),
    };
    match compile_with_options(&source, &compile_options) {
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
            );
        }
        Err(errors) => {
            eprintln!("{}", formatter.format_errors(&errors));
            std::process::exit(1);
        }
    }
}

/// Handle emit stages - print requested IRs and exit.
///
/// This uses a single-pass approach: each compilation stage is run at most once,
/// and the results are reused for later stages.
fn handle_emit(source: &str, options: &Options, formatter: &DiagnosticFormatter) -> Result<(), ()> {
    // Determine the highest stage we need to compute
    let max_stage = options
        .emit_stages
        .iter()
        .map(|s| match s {
            EmitStage::Tokens => 0,
            EmitStage::Ast => 1,
            EmitStage::Rir
            | EmitStage::Air
            | EmitStage::Cfg
            | EmitStage::Lowering
            | EmitStage::Mir
            | EmitStage::Liveness
            | EmitStage::RegAlloc
            | EmitStage::Asm
            | EmitStage::StackFrame => 2,
        })
        .max()
        .unwrap_or(0);

    // Stage 0: Tokenize (needed for tokens output or any later stage)
    let (tokens, interner) = if max_stage >= 0 {
        let lexer = Lexer::new(source);
        match lexer.tokenize() {
            Ok((tokens, interner)) => (Some(tokens), Some(interner)),
            Err(e) => {
                eprintln!("{}", formatter.format_error(&e));
                return Err(());
            }
        }
    } else {
        (None, None)
    };

    // Stage 1: Parse (needed for AST output or any later stage)
    // Only clone tokens if we're also emitting them; otherwise move them into the parser
    let needs_tokens = options.emit_stages.contains(&EmitStage::Tokens);
    let (tokens, ast, interner) = if max_stage >= 1 {
        let (kept_tokens, parser_tokens) = if needs_tokens {
            let t = tokens.unwrap();
            (Some(t.clone()), t)
        } else {
            (None, tokens.unwrap())
        };
        let parser = Parser::new(parser_tokens, interner.unwrap());
        match parser.parse() {
            Ok((ast, interner)) => (kept_tokens, Some(ast), Some(interner)),
            Err(e) => {
                eprintln!("{}", formatter.format_error(&e));
                return Err(());
            }
        }
    } else {
        (tokens, None, interner)
    };

    // Stage 2: Full frontend (RIR, AIR, CFG) - reuses the already-parsed AST
    // Applies optimization based on the -O level
    let frontend_state = if max_stage >= 2 {
        match compile_frontend_from_ast_with_options(
            ast.clone().unwrap(),
            interner.unwrap(),
            options.opt_level,
            &options.preview_features,
        ) {
            Ok(state) => Some(state),
            Err(errors) => {
                eprintln!("{}", formatter.format_errors(&errors));
                return Err(());
            }
        }
    } else {
        None
    };

    // Now emit in order
    for stage in &options.emit_stages {
        match stage {
            EmitStage::Tokens => {
                println!("=== Tokens ===");
                if let Some(ref tokens) = tokens {
                    for token in tokens {
                        println!("{}", token);
                    }
                }
                println!();
            }
            EmitStage::Ast => {
                println!("=== AST ===");
                // Prefer the AST from frontend_state if available (same AST, avoids clone)
                if let Some(ref state) = frontend_state {
                    print!("{}", state.ast);
                } else if let Some(ref ast) = ast {
                    print!("{}", ast);
                }
                println!();
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
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
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
                        let mir = generate_mir(
                            &func.cfg,
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
                            &state.interner,
                            options.target,
                        );
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
                        let liveness_info = generate_liveness_info(
                            &func.cfg,
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
                            &state.interner,
                            options.target,
                        );
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
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
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
                            &state.struct_defs,
                            &state.array_types,
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
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
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
    fn parse_no_args_returns_error() {
        assert!(is_error(&parse_args_from(&[])));
    }

    // ========== --emit tests ==========

    #[test]
    fn parse_emit_tokens() {
        let opts = unwrap_options(parse_args_from(&["--emit", "tokens", "source.rue"]));
        assert_eq!(opts.emit_stages, vec![EmitStage::Tokens]);
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
        let opts = unwrap_options(parse_args_from(&[
            "--target",
            "x86_64-linux",
            "--linker",
            "clang",
            "-O2",
            "--emit",
            "air",
            "source.rue",
            "output",
        ]));
        assert_eq!(opts.source_path, "source.rue");
        assert_eq!(opts.output_path, "output");
        assert_eq!(opts.target, Target::X86_64Linux);
        assert_eq!(opts.linker, LinkerMode::System("clang".to_string()));
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit_stages, vec![EmitStage::Air]);
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
