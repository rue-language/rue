use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rue_compiler::{
    CompileOptions, DiagnosticFormatter, Lexer, LinkerMode, OptLevel, Parser, PreviewFeature,
    PreviewFeatures, SourceInfo, compile_frontend_from_ast_with_options, compile_with_options,
    generate_allocated_mir, generate_mir,
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
            "mir" => Ok(EmitStage::Mir),
            "asm" => Ok(EmitStage::Asm),
            _ => Err(ParseEmitStageError(s.to_string())),
        }
    }
}

impl EmitStage {
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
    opt_level: OptLevel,
    preview_features: PreviewFeatures,
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
    eprintln!("  --version            Show version information");
    eprintln!("  --help               Show this help message");
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
    let mut opt_level: Option<OptLevel> = None;
    let mut preview_features = PreviewFeatures::new();
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
                match stage_str.parse::<EmitStage>() {
                    Ok(stage) => emit_stages.push(stage),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid stages: {}", EmitStage::all_names());
                        return None;
                    }
                }
            }
            "--target" => {
                let Some(target_str) = args_iter.next() else {
                    eprintln!("Error: --target requires a value");
                    eprintln!("Valid targets: {}", Target::all_names());
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
            "--preview" => {
                let Some(feature_str) = args_iter.next() else {
                    eprintln!("Error: --preview requires a feature name");
                    eprintln!("Available features: {}", PreviewFeature::all_names());
                    return None;
                };
                match feature_str.parse::<PreviewFeature>() {
                    Ok(feature) => {
                        preview_features.insert(feature);
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Available features: {}", PreviewFeature::all_names());
                        return None;
                    }
                }
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                print_version();
                std::process::exit(0);
            }
            _ if arg.starts_with("-O") => {
                // Parse -O0, -O1, -O2, -O3
                let level_str = &arg[2..];
                match level_str.parse::<OptLevel>() {
                    Ok(level) => opt_level = Some(level),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        eprintln!("Valid levels: {}", OptLevel::all_names());
                        return None;
                    }
                }
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
        opt_level: opt_level.unwrap_or_default(),
        preview_features,
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

    // Create source info for diagnostic formatting
    let source_info = SourceInfo::new(&source, &options.source_path);
    let formatter = DiagnosticFormatter::new(&source_info);

    // Handle emit modes
    if !options.emit_stages.is_empty() {
        if let Err(()) = handle_emit(&source, &options, &formatter) {
            std::process::exit(1);
        }
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
            eprintln!("{}", formatter.format_error(&e));
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
                eprintln!("{}", formatter.format_error(&e));
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
                eprintln!("{}", formatter.format_error(&e));
                return Err(());
            }
        }
    } else {
        (tokens, None)
    };

    // Stage 2: Full frontend (RIR, AIR, CFG) - reuses the already-parsed AST
    // Applies optimization based on the -O level
    let frontend_state = if max_stage >= 2 {
        match compile_frontend_from_ast_with_options(ast.clone().unwrap(), options.opt_level) {
            Ok(state) => Some(state),
            Err(e) => {
                eprintln!("{}", formatter.format_error(&e));
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
                println!("=== MIR ({}) ===", options.target);
                if let Some(ref state) = frontend_state {
                    for func in &state.functions {
                        let mir = generate_mir(
                            &func.cfg,
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
                            options.target,
                        );
                        println!("function {}:", func.analyzed.name);
                        println!("{}", mir);
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
                        let mir = match generate_allocated_mir(
                            &func.cfg,
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
                            options.target,
                        ) {
                            Ok(mir) => mir,
                            Err(e) => {
                                eprintln!("{}", formatter.format_error(&e));
                                return Err(());
                            }
                        };
                        print!("{}", mir.format_assembly());
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}
