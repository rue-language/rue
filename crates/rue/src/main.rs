use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use annotate_snippets::{Level, Renderer, Snippet};
use rue_compiler::{
    CompileError, CompileOptions, CompileWarning, LinkerMode, compile_frontend,
    compile_with_options, generate_mir,
};
use rue_rir::RirPrinter;
use rue_target::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DumpMode {
    None,
    Rir,
    Air,
    Mir,
}

struct Options {
    source_path: String,
    output_path: String,
    dump_mode: DumpMode,
    target: Target,
    linker: LinkerMode,
}

fn print_usage() {
    eprintln!("Usage: rue [options] <source.rue> [output]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --target <target>  Set compilation target (default: host)");
    eprintln!("                     Valid targets: x86-64-linux, aarch64-linux");
    eprintln!("  --linker <linker>  Set linker to use (default: internal)");
    eprintln!("                     Use 'internal' for built-in linker, or a command");
    eprintln!("                     like 'clang', 'gcc', or 'ld' for system linker");
    eprintln!("  --dump-rir         Dump RIR (untyped intermediate representation)");
    eprintln!("  --dump-air         Dump AIR (typed intermediate representation)");
    eprintln!("  --dump-mir         Dump MIR (machine intermediate representation)");
    eprintln!("  --help             Show this help message");
}

fn parse_args() -> Option<Options> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_usage();
        return None;
    }

    let mut dump_mode = DumpMode::None;
    let mut target: Option<Target> = None;
    let mut linker: Option<LinkerMode> = None;
    let mut positional = Vec::new();
    let mut args_iter = args.iter().peekable();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--dump-rir" => dump_mode = DumpMode::Rir,
            "--dump-air" => dump_mode = DumpMode::Air,
            "--dump-mir" => dump_mode = DumpMode::Mir,
            "--target" => {
                let Some(target_str) = args_iter.next() else {
                    eprintln!("Error: --target requires a value");
                    eprintln!("Valid targets: x86-64-linux, aarch64-linux");
                    return None;
                };
                match target_str.parse::<Target>() {
                    Ok(t) => target = Some(t),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        return None;
                    }
                }
            }
            "--linker" => {
                let Some(linker_str) = args_iter.next() else {
                    eprintln!("Error: --linker requires a value");
                    eprintln!("Use 'internal' or a system linker command like 'clang'");
                    return None;
                };
                linker = Some(if linker_str == "internal" {
                    LinkerMode::Internal
                } else {
                    LinkerMode::System(linker_str.clone())
                });
            }
            "--help" | "-h" => {
                print_usage();
                return None;
            }
            _ if arg.starts_with('-') => {
                eprintln!("Unknown option: {}", arg);
                print_usage();
                return None;
            }
            _ => positional.push(arg.clone()),
        }
    }

    if positional.is_empty() {
        eprintln!("Error: No source file specified");
        print_usage();
        return None;
    }

    let source_path = positional[0].clone();
    let output_path = positional
        .get(1)
        .cloned()
        .unwrap_or_else(|| "a.out".to_string());

    Some(Options {
        source_path,
        output_path,
        dump_mode,
        target: target.unwrap_or_else(Target::host),
        linker: linker.unwrap_or_default(),
    })
}

fn main() {
    let options = match parse_args() {
        Some(opts) => opts,
        None => std::process::exit(1),
    };

    // Read source
    let source = fs::read_to_string(&options.source_path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {}", options.source_path, e);
        std::process::exit(1);
    });

    // Handle dump modes
    if options.dump_mode != DumpMode::None {
        match compile_frontend(&source) {
            Ok(state) => match options.dump_mode {
                DumpMode::Rir => {
                    let printer = RirPrinter::new(&state.rir, &state.interner);
                    println!("{}", printer);
                }
                DumpMode::Air => {
                    for func in &state.functions {
                        println!("function {}:", func.analyzed.name);
                        println!("{}", func.analyzed.air);
                    }
                }
                DumpMode::Mir => {
                    for func in &state.functions {
                        let mir = generate_mir(&func.cfg, &state.struct_defs);
                        println!("function {}:", func.analyzed.name);
                        println!("{}", mir);
                    }
                }
                DumpMode::None => unreachable!(),
            },
            Err(e) => {
                print_error(&e, &source, &options.source_path);
                std::process::exit(1);
            }
        }
        return;
    }

    // Normal compilation
    let compile_options = CompileOptions {
        target: options.target,
        linker: options.linker.clone(),
    };
    match compile_with_options(&source, &compile_options) {
        Ok(output) => {
            // Print warnings first
            for warning in &output.warnings {
                print_warning(warning, &source, &options.source_path);
            }

            // Write output
            if let Err(e) = fs::write(&options.output_path, &output.elf) {
                eprintln!("Error writing {}: {}", options.output_path, e);
                std::process::exit(1);
            }

            // Make executable
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

            let linker_str = match &options.linker {
                LinkerMode::Internal => "internal".to_string(),
                LinkerMode::System(cmd) => cmd.clone(),
            };
            println!(
                "Compiled {} -> {} (target: {}, linker: {})",
                options.source_path, options.output_path, options.target, linker_str
            );
        }
        Err(e) => {
            print_error(&e, &source, &options.source_path);
            std::process::exit(1);
        }
    }
}

fn print_error(error: &CompileError, source: &str, source_path: &str) {
    let message = error.to_string();
    let renderer = Renderer::plain();

    // For errors without a span, just print the message
    let Some(span) = error.span() else {
        let report = Level::Error.title(&message);
        eprintln!("{}", renderer.render(report));
        return;
    };

    let report = Level::Error.title(&message).snippet(
        Snippet::source(source)
            .origin(source_path)
            .fold(true)
            .annotation(Level::Error.span(span.start as usize..span.end as usize)),
    );

    eprintln!("{}", renderer.render(report));
}

fn print_warning(warning: &CompileWarning, source: &str, source_path: &str) {
    let message = warning.to_string();
    let renderer = Renderer::plain();

    // For warnings without a span, just print the message
    let Some(span) = warning.span() else {
        let report = Level::Warning.title(&message);
        eprintln!("{}", renderer.render(report));
        return;
    };

    let report = Level::Warning.title(&message).snippet(
        Snippet::source(source)
            .origin(source_path)
            .fold(true)
            .annotation(Level::Warning.span(span.start as usize..span.end as usize)),
    );

    eprintln!("{}", renderer.render(report));
}
