use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use annotate_snippets::{Level, Renderer, Snippet};
use rue_compiler::{
    CompileError, CompileOptions, CompileWarning, Lexer, LinkerMode, Parser,
    compile_frontend_from_ast, compile_with_options, generate_allocated_mir, generate_mir,
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
    /// Emit MIR (machine intermediate representation).
    Mir,
    /// Emit assembly text.
    Asm,
}

impl EmitStage {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "tokens" => Some(EmitStage::Tokens),
            "ast" => Some(EmitStage::Ast),
            "rir" => Some(EmitStage::Rir),
            "air" => Some(EmitStage::Air),
            "cfg" => Some(EmitStage::Cfg),
            "mir" => Some(EmitStage::Mir),
            "asm" => Some(EmitStage::Asm),
            _ => None,
        }
    }

    fn all_names() -> &'static str {
        "tokens, ast, rir, air, cfg, mir, asm"
    }
}

struct Options {
    source_path: String,
    output_path: String,
    emit_stages: Vec<EmitStage>,
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
    eprintln!("  --emit <stage>     Emit intermediate representation and exit");
    eprintln!("                     Can be specified multiple times for multiple outputs");
    eprintln!("                     Stages: tokens, ast, rir, air, cfg, mir, asm");
    eprintln!("  --help             Show this help message");
}

fn parse_args() -> Option<Options> {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print_usage();
        return None;
    }

    let mut emit_stages = Vec::new();
    let mut target: Option<Target> = None;
    let mut linker: Option<LinkerMode> = None;
    let mut positional = Vec::new();
    let mut args_iter = args.iter().peekable();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--emit" => {
                let Some(stage_str) = args_iter.next() else {
                    eprintln!("Error: --emit requires a value");
                    eprintln!("Valid stages: {}", EmitStage::all_names());
                    return None;
                };
                match EmitStage::from_str(stage_str) {
                    Some(stage) => emit_stages.push(stage),
                    None => {
                        eprintln!("Error: unknown emit stage '{}'", stage_str);
                        eprintln!("Valid stages: {}", EmitStage::all_names());
                        return None;
                    }
                }
            }
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
        emit_stages,
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

    // Handle emit modes
    if !options.emit_stages.is_empty() {
        if let Err(()) = handle_emit(&source, &options) {
            std::process::exit(1);
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

/// Handle emit stages - print requested IRs and exit.
///
/// This uses a single-pass approach: each compilation stage is run at most once,
/// and the results are reused for later stages.
fn handle_emit(source: &str, options: &Options) -> Result<(), ()> {
    // Determine the highest stage we need to compute
    let max_stage = options
        .emit_stages
        .iter()
        .map(|s| match s {
            EmitStage::Tokens => 0,
            EmitStage::Ast => 1,
            EmitStage::Rir | EmitStage::Air | EmitStage::Cfg | EmitStage::Mir | EmitStage::Asm => 2,
        })
        .max()
        .unwrap_or(0);

    // Stage 0: Tokenize (needed for tokens output or any later stage)
    let tokens = if max_stage >= 0 {
        let mut lexer = Lexer::new(source);
        match lexer.tokenize() {
            Ok(tokens) => Some(tokens),
            Err(e) => {
                print_error(&e, source, &options.source_path);
                return Err(());
            }
        }
    } else {
        None
    };

    // Stage 1: Parse (needed for AST output or any later stage)
    // Only clone tokens if we're also emitting them; otherwise move them into the parser
    let needs_tokens = options.emit_stages.contains(&EmitStage::Tokens);
    let (tokens, ast) = if max_stage >= 1 {
        let (kept_tokens, parser_tokens) = if needs_tokens {
            let t = tokens.unwrap();
            (Some(t.clone()), t)
        } else {
            (None, tokens.unwrap())
        };
        let parser = Parser::new(parser_tokens);
        match parser.parse() {
            Ok(ast) => (kept_tokens, Some(ast)),
            Err(e) => {
                print_error(&e, source, &options.source_path);
                return Err(());
            }
        }
    } else {
        (tokens, None)
    };

    // Stage 2: Full frontend (RIR, AIR, CFG) - reuses the already-parsed AST
    let frontend_state = if max_stage >= 2 {
        match compile_frontend_from_ast(ast.clone().unwrap()) {
            Ok(state) => Some(state),
            Err(e) => {
                print_error(&e, source, &options.source_path);
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
            EmitStage::Mir => {
                println!("=== MIR ===");
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        let mir = generate_mir(&func.cfg, &state.struct_defs, &state.array_types);
                        println!("function {}:", func.analyzed.name);
                        println!("{}", mir);
                    }
                }
                println!();
            }
            EmitStage::Asm => {
                println!("=== Assembly (x86-64) ===");
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        println!(".globl {}", func.analyzed.name);
                        println!("{}:", func.analyzed.name);
                        let mir = generate_allocated_mir(
                            &func.cfg,
                            &state.struct_defs,
                            &state.array_types,
                        );
                        print_assembly(&mir);
                        println!();
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}

/// Print x86-64 assembly from MIR.
///
/// This prints the MIR instructions in assembly-like format.
/// When called with allocated MIR (post-regalloc), physical registers
/// like rax, rbx, r12 are shown.
fn print_assembly(mir: &rue_compiler::X86Mir) {
    use rue_codegen::x86_64::X86Inst;

    // Print instructions
    for inst in mir.instructions() {
        match inst {
            X86Inst::Label { id } => println!("{}:", id),
            _ => println!("    {}", inst),
        }
    }
}

fn print_error(error: &CompileError, source: &str, source_path: &str) {
    let message = error.to_string();
    let renderer = Renderer::plain();
    let diagnostic = error.diagnostic();

    // For errors without a span, just print the message with any footers
    let Some(span) = error.span() else {
        let mut report = Level::Error.title(&message);
        // Add notes and helps as footers
        for note in &diagnostic.notes {
            report = report.footer(Level::Note.title(note.0.as_str()));
        }
        for help in &diagnostic.helps {
            report = report.footer(Level::Help.title(help.0.as_str()));
        }
        eprintln!("{}", renderer.render(report));
        return;
    };

    // Build snippet with primary annotation
    let mut snippet = Snippet::source(source)
        .origin(source_path)
        .fold(true)
        .annotation(Level::Error.span(span.start as usize..span.end as usize));

    // Add secondary labels as Info annotations
    for label in &diagnostic.labels {
        snippet = snippet.annotation(
            Level::Info
                .span(label.span.start as usize..label.span.end as usize)
                .label(&label.message),
        );
    }

    let mut report = Level::Error.title(&message).snippet(snippet);

    // Add notes and helps as footers
    for note in &diagnostic.notes {
        report = report.footer(Level::Note.title(note.0.as_str()));
    }
    for help in &diagnostic.helps {
        report = report.footer(Level::Help.title(help.0.as_str()));
    }

    eprintln!("{}", renderer.render(report));
}

fn print_warning(warning: &CompileWarning, source: &str, source_path: &str) {
    let message = warning.to_string();
    let renderer = Renderer::plain();
    let diagnostic = warning.diagnostic();

    // For warnings without a span, just print the message with any footers
    let Some(span) = warning.span() else {
        let mut report = Level::Warning.title(&message);
        // Add notes and helps as footers
        for note in &diagnostic.notes {
            report = report.footer(Level::Note.title(note.0.as_str()));
        }
        for help in &diagnostic.helps {
            report = report.footer(Level::Help.title(help.0.as_str()));
        }
        eprintln!("{}", renderer.render(report));
        return;
    };

    // Build snippet with primary annotation
    let mut snippet = Snippet::source(source)
        .origin(source_path)
        .fold(true)
        .annotation(Level::Warning.span(span.start as usize..span.end as usize));

    // Add secondary labels as Info annotations
    for label in &diagnostic.labels {
        snippet = snippet.annotation(
            Level::Info
                .span(label.span.start as usize..label.span.end as usize)
                .label(&label.message),
        );
    }

    let mut report = Level::Warning.title(&message).snippet(snippet);

    // Add notes and helps as footers
    for note in &diagnostic.notes {
        report = report.footer(Level::Note.title(note.0.as_str()));
    }
    for help in &diagnostic.helps {
        report = report.footer(Level::Help.title(help.0.as_str()));
    }

    eprintln!("{}", renderer.render(report));
}
