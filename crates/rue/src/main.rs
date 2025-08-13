use bpaf::{Parser, construct};
use rue_compiler::{
    CompileOptions, RueDatabase, SourceFile, compile_file_to_assembly_with_options,
    compile_file_with_diagnostics,
    logging::{LogConfig, LogFormat, init_tracing, verbosity_to_filter},
};
use rue_diagnostic::{DiagnosticFormatter, SourceManager};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
struct Args {
    output: Option<PathBuf>,
    input: PathBuf,
    emit_asm: bool,
    optimize: bool,
    verbose: u8,
    log_format: Option<String>,
    log_filter: Option<String>,
}

fn parser() -> impl Parser<Args> {
    let output = bpaf::short('o')
        .long("output")
        .help("Output binary filename")
        .argument::<PathBuf>("OUTPUT")
        .optional();

    let emit_asm = bpaf::short('S')
        .long("emit-asm")
        .help("Emit assembly instead of executable")
        .switch();

    let optimize = bpaf::short('O').help("Enable MIR optimizations").switch();

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

    let input = bpaf::positional::<PathBuf>("INPUT").help("Input Rue source file");

    construct!(Args {
        output,
        emit_asm,
        optimize,
        verbose,
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

    // Initialize logging
    let log_config = LogConfig {
        format: match opts.log_format.as_deref() {
            Some("json") => LogFormat::Json,
            Some("compact") => LogFormat::Compact,
            Some("tree") => LogFormat::Tree,
            Some("pretty") | None => LogFormat::Pretty,
            Some(fmt) => {
                warn!("Unknown log format: {fmt}, using 'pretty'");
                LogFormat::Pretty
            }
        },
        filter: opts.log_filter.or_else(|| {
            if opts.verbose > 0 {
                Some(verbosity_to_filter(opts.verbose).to_string())
            } else {
                // Check RUST_LOG environment variable
                std::env::var("RUST_LOG").ok()
            }
        }),
        with_source_location: opts.verbose >= 3,
        with_thread_ids: false,
        with_timestamps: matches!(opts.log_format.as_deref(), Some("json")),
    };

    if let Err(e) = init_tracing(log_config) {
        // this is using eprintln because we didn't set up logging, so we can't
        // use warn!
        eprintln!("Failed to initialize logging: {e}");
        // Continue without logging
    }

    let input_path = opts.input;
    let output_path = if opts.emit_asm {
        opts.output
            .unwrap_or_else(|| input_path.with_extension("s"))
    } else {
        opts.output.unwrap_or_else(|| input_path.with_extension(""))
    };

    // Read source file
    let source = match fs::read_to_string(&input_path) {
        Ok(content) => content,
        Err(e) => {
            error!("Error reading file '{}': {}", input_path.display(), e);
            std::process::exit(1);
        }
    };

    // Set up Salsa database
    info!("Starting compilation of {}", input_path.display());
    let db = RueDatabase::default();
    let file = SourceFile::new(&db, input_path.to_string_lossy().to_string(), source);
    let options = CompileOptions::new(&db, opts.optimize);

    // Compile
    if opts.emit_asm {
        // Generate assembly
        match compile_file_to_assembly_with_options(&db, file, options) {
            Ok(assembly) => {
                if let Err(e) = fs::write(&output_path, &*assembly) {
                    error!(
                        "Error writing output file '{}': {}",
                        output_path.display(),
                        e
                    );
                    std::process::exit(1);
                }

                info!(
                    "Successfully generated assembly to '{}'",
                    output_path.display()
                );
            }
            Err(error) => {
                error!("Compilation failed: {error}");
                std::process::exit(1);
            }
        }
    } else {
        // Generate executable using diagnostic-enabled compilation
        match compile_file_with_diagnostics(&db, file, options) {
            Ok(executable) => {
                if let Err(e) = fs::write(&output_path, &*executable) {
                    error!(
                        "Error writing output file '{}': {}",
                        output_path.display(),
                        e
                    );
                    std::process::exit(1);
                }

                // Make executable on Unix systems
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&output_path)
                        .expect("Failed to read file metadata")
                        .permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&output_path, perms)
                        .expect("Failed to set executable permissions");
                }

                info!("Successfully compiled to '{}'", output_path.display());
            }
            Err(diagnostics) => {
                // Format and display diagnostics
                let formatter = if atty::is(atty::Stream::Stderr) {
                    DiagnosticFormatter::terminal()
                } else {
                    DiagnosticFormatter::plain()
                };

                let mut source_manager = SourceManager::new();
                let source_text = file.text(&db);
                let source_path = file.path(&db);
                source_manager.add_source(source_path, source_text);

                error!("Compilation failed:\n");
                for diagnostic in &diagnostics {
                    if let Ok(formatted) = formatter.format_diagnostic(diagnostic, &source_manager)
                    {
                        error!("{formatted}\n");
                    }
                }

                std::process::exit(1);
            }
        }
    }
}
