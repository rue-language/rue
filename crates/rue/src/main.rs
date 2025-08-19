use bpaf::{Parser, construct};
use rue_compiler::{
    CompileOptions, RueDatabase, SourceFile, compile_file_to_assembly_with_options,
    compile_file_with_diagnostics, emit_mir_with_diagnostics,
    logging::{LogConfig, LogFormat, init_tracing, verbosity_to_filter},
};
use rue_diagnostic::{DiagnosticFormatter, SourceManager};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{error, info, trace, warn};

#[derive(Debug, Clone, PartialEq)]
enum EmitMode {
    Executable,
    Assembly { stdout_only: bool },
    Mir { stdout_only: bool },
    CompileOnly,
}

#[derive(Debug, Clone)]
struct Args {
    output: Option<PathBuf>,
    input: Option<PathBuf>,
    emit_mode: EmitMode,
    optimize: u8,
    verbose: u8,
    quiet: bool,
    log_format: Option<String>,
    log_filter: Option<String>,
}

fn parser() -> impl Parser<Args> {
    let output = bpaf::short('o')
        .long("output")
        .help("Output filename (use '-' for stdout with --emit-mir)")
        .argument::<PathBuf>("OUTPUT")
        .optional();

    let emit_asm = bpaf::short('S')
        .long("emit-asm")
        .help("Emit assembly instead of executable")
        .switch();

    let emit_mir = bpaf::long("emit-mir")
        .help("Emit MIR representation")
        .switch();

    let compile_only = bpaf::long("compile-only")
        .help("Check compilation without generating output")
        .switch();

    // Determine emit mode based on flags (mutually exclusive)
    let emit_mode =
        bpaf::construct!(emit_asm, emit_mir, compile_only).map(|(asm, mir, compile)| {
            match (asm, mir, compile) {
                (true, false, false) => EmitMode::Assembly { stdout_only: false },
                (false, true, false) => EmitMode::Mir { stdout_only: false },
                (false, false, true) => EmitMode::CompileOnly,
                (false, false, false) => EmitMode::Executable,
                _ => {
                    eprintln!(
                        "Error: --emit-asm, --emit-mir, and --compile-only are mutually exclusive"
                    );
                    std::process::exit(1);
                }
            }
        });

    let optimize = bpaf::short('O')
        .help("Optimization level (0-3): 0=none, 1=basic, 2=aggressive, 3=experimental")
        .argument::<u8>("LEVEL")
        .fallback(0)
        .guard(|&x| x <= 3, "Optimization level must be 0-3");

    let verbose = bpaf::short('v')
        .long("verbose")
        .help("Increase verbosity (can be repeated: -v, -vv, -vvv)")
        .req_flag(())
        .many()
        .map(|v| v.len() as u8);

    let log_format = bpaf::long("log-format")
        .help("Log output format: pretty, json, compact, tree")
        .argument::<String>("FORMAT")
        .optional();

    let log_filter = bpaf::long("log-filter")
        .help("Custom log filter (overrides -v)")
        .argument::<String>("FILTER")
        .optional();

    let quiet = bpaf::short('q')
        .long("quiet")
        .help("Suppress info logs (only show warnings and errors)")
        .switch();

    let input = bpaf::positional::<PathBuf>("INPUT")
        .help("Input Rue source file (use '-' for stdin)")
        .optional();

    construct!(Args {
        output,
        emit_mode,
        optimize,
        verbose,
        quiet,
        log_format,
        log_filter,
        input,
    })
}

fn main() {
    let opts = parser()
        .to_options()
        .descr("The Rue programming language compiler")
        .run();

    std::process::exit(run(opts).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        1
    }));
}

fn run(opts: Args) -> anyhow::Result<i32> {
    // Check environment variables for color support
    let use_color = should_use_color();

    // Handle unknown log format before tracing initialization
    let mut unknown_log_format = None;
    let log_format = match opts.log_format.as_deref() {
        Some("json") => LogFormat::Json,
        Some("compact") => LogFormat::Compact,
        Some("tree") => LogFormat::Tree,
        Some("pretty") | None => LogFormat::Pretty,
        Some(fmt) => {
            unknown_log_format = Some(fmt.to_string());
            LogFormat::Pretty
        }
    };

    // Initialize logging
    let log_config = LogConfig {
        format: log_format,
        filter: opts.log_filter.or_else(|| {
            if opts.quiet {
                Some("warn".to_string())
            } else if opts.verbose > 0 {
                Some(verbosity_to_filter(opts.verbose).to_string())
            } else {
                // Check RUST_LOG environment variable
                std::env::var("RUST_LOG").ok()
            }
        }),
        with_source_location: opts.verbose >= 3,
        with_thread_ids: opts.verbose >= 3,
        with_timestamps: matches!(opts.log_format.as_deref(), Some("json")),
    };

    if let Err(e) = init_tracing(log_config) {
        // this is using eprintln because we didn't set up logging, so we can't
        // use warn!
        eprintln!("Failed to initialize logging: {e}");
        // Continue without logging
    }

    // Now that tracing is initialized, warn about unknown log format
    if let Some(fmt) = unknown_log_format {
        warn!("Unknown log format: {fmt}, using 'pretty'");
    }

    // Handle input (file or stdin)
    let (input_path, source) = if let Some(path) = opts.input {
        if path.as_os_str() == "-" {
            // Read from stdin
            use std::io::Read;
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| anyhow::anyhow!("Error reading from stdin: {e}"))?;
            (PathBuf::from("<stdin>"), buffer)
        } else {
            // Canonicalize input path for stable keys
            let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
            let source = fs::read_to_string(&canonical_path).map_err(|e| {
                anyhow::anyhow!("Error reading file '{}': {e}", canonical_path.display())
            })?;
            (canonical_path, source)
        }
    } else {
        return Err(anyhow::anyhow!(
            "No input file specified (use '-' for stdin)"
        ));
    };

    // Determine emit mode based on output flag for MIR or Assembly
    let emit_mode = match (&opts.emit_mode, &opts.output) {
        (EmitMode::Mir { .. }, Some(path)) if path.as_os_str() == "-" => {
            EmitMode::Mir { stdout_only: true }
        }
        (EmitMode::Assembly { .. }, Some(path)) if path.as_os_str() == "-" => {
            EmitMode::Assembly { stdout_only: true }
        }
        _ => opts.emit_mode,
    };

    let output_path = match &emit_mode {
        EmitMode::Assembly { stdout_only: false } => Some(
            opts.output
                .unwrap_or_else(|| input_path.with_extension("s")),
        ),
        EmitMode::Assembly { stdout_only: true } => None,
        EmitMode::Mir { stdout_only: false } => Some(
            opts.output
                .unwrap_or_else(|| input_path.with_extension("mir")),
        ),
        EmitMode::Mir { stdout_only: true } => None,
        EmitMode::CompileOnly => None,
        EmitMode::Executable => {
            // For stdin input, use "a.out" as default
            let default_name = if input_path.as_os_str() == "<stdin>" {
                PathBuf::from(if cfg!(windows) { "a.exe" } else { "a.out" })
            } else if cfg!(windows) {
                input_path.with_extension("exe")
            } else {
                input_path.with_extension("")
            };
            Some(opts.output.unwrap_or(default_name))
        }
    };

    // Set up Salsa database
    info!("Starting compilation of {}", input_path.display());
    let db = RueDatabase::default();
    let file = SourceFile::new(&db, input_path.to_string_lossy().to_string(), source);
    let optimize_enabled = opts.optimize > 0;
    let options = CompileOptions::new(&db, optimize_enabled);

    // Compile based on mode
    match emit_mode {
        EmitMode::CompileOnly => {
            // Just check compilation without generating output
            match compile_file_with_diagnostics(&db, file, options) {
                Ok(_) => {
                    // UNIX programs should have no output on success
                    // Do not output anything for --compile-only mode
                }
                Err(diagnostics) => {
                    print_diagnostics(&diagnostics, &file, &db, use_color);
                    return Ok(1);
                }
            }
        }
        EmitMode::Mir { stdout_only } => {
            // Emit MIR representation
            match emit_mir_with_diagnostics(&db, file, options) {
                Ok(mir_output) => {
                    if stdout_only {
                        println!("{}", mir_output);
                        info!("Successfully emitted MIR to stdout");
                    } else {
                        let output_path = output_path.unwrap(); // Safe because we set it above
                        if let Err(e) = write_file_atomic(&output_path, mir_output.as_bytes()) {
                            return Err(anyhow::anyhow!(
                                "Error writing output file '{}': {e}",
                                output_path.display()
                            ));
                        }
                        info!("Successfully emitted MIR to '{}'", output_path.display());
                    }
                }
                Err(diagnostics) => {
                    print_diagnostics(&diagnostics, &file, &db, use_color);
                    return Ok(1);
                }
            }
        }
        EmitMode::Assembly { stdout_only } => {
            // Generate assembly
            match compile_file_to_assembly_with_options(&db, file, options) {
                Ok(assembly) => {
                    if stdout_only {
                        print!("{}", assembly);
                        info!("Successfully generated assembly to stdout");
                    } else {
                        let output_path = output_path.unwrap(); // Safe because we set it above
                        if let Err(e) = write_file_atomic(&output_path, assembly.as_bytes()) {
                            return Err(anyhow::anyhow!(
                                "Error writing output file '{}': {e}",
                                output_path.display()
                            ));
                        }
                        info!(
                            "Successfully generated assembly to '{}'",
                            output_path.display()
                        );
                    }
                }
                Err(error) => {
                    error!("Compilation failed: {error}");
                    return Ok(1);
                }
            }
        }
        EmitMode::Executable => {
            // Generate executable using diagnostic-enabled compilation
            let output_path = output_path.unwrap(); // Safe because we set it above
            match compile_file_with_diagnostics(&db, file, options) {
                Ok(executable) => {
                    if let Err(e) = write_executable_atomic(&output_path, &executable) {
                        return Err(anyhow::anyhow!(
                            "Error writing output file '{}': {e}",
                            output_path.display()
                        ));
                    }

                    info!("Successfully compiled to '{}'", output_path.display());
                }
                Err(diagnostics) => {
                    print_diagnostics(&diagnostics, &file, &db, use_color);
                    return Ok(1);
                }
            }
        }
    }

    Ok(0)
}

/// Atomically write data to a file by writing to a temporary file first, then renaming.
/// Creates parent directories if they don't exist.
fn write_file_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Create a temporary file in the same directory
    let temp_path = if let Some(parent) = path.parent() {
        parent.join(format!(
            ".tmp.{}.{}",
            std::process::id(),
            path.file_name().unwrap().to_string_lossy()
        ))
    } else {
        PathBuf::from(format!(
            ".tmp.{}.{}",
            std::process::id(),
            path.to_string_lossy()
        ))
    };

    // Write to temporary file
    fs::write(&temp_path, data)?;

    // Atomically move temp file to final location
    fs::rename(&temp_path, path)?;

    Ok(())
}

/// Atomically write an executable file with proper permissions.
/// Creates parent directories if they don't exist.
fn write_executable_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_file_atomic(path, data)?;

    // Set executable permissions on Unix systems
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }

    Ok(())
}

/// Check if color output should be used based on environment variables
fn should_use_color() -> bool {
    // Honor NO_COLOR and CLICOLOR_FORCE environment variables
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    if std::env::var("CLICOLOR_FORCE").is_ok() {
        return true;
    }
    // Default to checking if stderr is a terminal
    atty::is(atty::Stream::Stderr)
}

fn print_diagnostics(
    diagnostics: &[rue_diagnostic::Diagnostic],
    file: &SourceFile,
    db: &RueDatabase,
    use_color: bool,
) {
    // Format and display diagnostics
    let formatter = if use_color {
        DiagnosticFormatter::terminal()
    } else {
        DiagnosticFormatter::plain()
    };

    let mut source_manager = SourceManager::new();
    let source_text = file.text(db);
    let source_path = file.path(db);
    source_manager.add_source(source_path, source_text);

    error!("Compilation failed");
    eprintln!(); // Keep blank line for formatting
    for diagnostic in diagnostics {
        if let Ok(formatted) = formatter.format_diagnostic(diagnostic, &source_manager) {
            eprintln!("{formatted}\n");
        }
    }
}
