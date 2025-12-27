use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rue_compiler::{
    CompileOptions, DiagnosticFormatter, Lexer, LinkerMode, OptLevel, Parser, PreviewFeature,
    PreviewFeatures, SourceInfo, compile_frontend_from_ast_with_options, compile_with_options,
    generate_allocated_mir, generate_emitted_asm, generate_mir,
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
        match compile_frontend_from_ast_with_options(
            ast.clone().unwrap(),
            options.opt_level,
            &options.preview_features,
        ) {
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
                        let asm = match generate_emitted_asm(
                            &func.cfg,
                            &state.struct_defs,
                            &state.array_types,
                            &state.strings,
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
        let opts = unwrap_options(parse_args_from(&[
            "--preview",
            "mutable_strings",
            "source.rue",
        ]));
        assert!(
            opts.preview_features
                .contains(&PreviewFeature::MutableStrings)
        );
    }

    #[test]
    fn parse_preview_multiple_features() {
        let opts = unwrap_options(parse_args_from(&[
            "--preview",
            "mutable_strings",
            "--preview",
            "test_infra",
            "source.rue",
        ]));
        assert!(
            opts.preview_features
                .contains(&PreviewFeature::MutableStrings)
        );
        assert!(opts.preview_features.contains(&PreviewFeature::TestInfra));
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

    // ========== EmitStage FromStr tests ==========

    #[test]
    fn emit_stage_from_str_all_valid() {
        assert_eq!("tokens".parse::<EmitStage>().unwrap(), EmitStage::Tokens);
        assert_eq!("ast".parse::<EmitStage>().unwrap(), EmitStage::Ast);
        assert_eq!("rir".parse::<EmitStage>().unwrap(), EmitStage::Rir);
        assert_eq!("air".parse::<EmitStage>().unwrap(), EmitStage::Air);
        assert_eq!("cfg".parse::<EmitStage>().unwrap(), EmitStage::Cfg);
        assert_eq!("mir".parse::<EmitStage>().unwrap(), EmitStage::Mir);
        assert_eq!("asm".parse::<EmitStage>().unwrap(), EmitStage::Asm);
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
            "tokens, ast, rir, air, cfg, mir, asm"
        );
    }
}
