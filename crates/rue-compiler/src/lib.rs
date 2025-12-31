//! Rue compiler driver.
//!
//! This crate orchestrates the compilation pipeline:
//! Source -> Lexer -> Parser -> AstGen -> Sema -> CodeGen -> ELF
//!
//! It re-exports types from the component crates for convenience.
//!
//! # Diagnostic Formatting
//!
//! The [`DiagnosticFormatter`] provides a clean API for formatting errors and warnings:
//!
//! ```ignore
//! use rue_compiler::{DiagnosticFormatter, SourceInfo};
//!
//! let source_info = SourceInfo::new(&source, "example.rue");
//! let formatter = DiagnosticFormatter::new(&source_info);
//!
//! // Format an error
//! let error_output = formatter.format_error(&error);
//! eprintln!("{}", error_output);
//! ```
//!
//! # Tracing
//!
//! This crate is instrumented with `tracing` spans for performance analysis.
//! Use `--log-level info` or `--time-passes` to see timing information.

mod diagnostic;
mod drop_glue;

use rayon::prelude::*;
use tracing::{info, info_span};

pub use diagnostic::{DiagnosticFormatter, SourceInfo};

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// Error Helper Functions
// ============================================================================

/// Convert a displayable error into a `LinkError` without a source span.
///
/// This helper simplifies the common pattern of wrapping various error types
/// (e.g., from I/O operations, parsing, or linking) into `CompileError`.
///
/// # Example
/// ```ignore
/// linker.add_object(obj).map_err(link_error)?;
/// ```
fn link_error<E: std::fmt::Display>(err: E) -> CompileError {
    CompileError::without_span(ErrorKind::LinkError(err.to_string()))
}

/// Convert an I/O result into a `CompileResult` with a contextual message.
///
/// This helper wraps `std::io::Error` with a descriptive message explaining
/// what operation failed.
///
/// # Example
/// ```ignore
/// std::fs::create_dir_all(&path).map_err(|e| io_link_error("failed to create temp directory", e))?;
/// ```
fn io_link_error(context: &str, err: std::io::Error) -> CompileError {
    CompileError::without_span(ErrorKind::LinkError(format!("{}: {}", context, err)))
}

/// Counter for generating unique temp directory names.
static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary directory for linking that automatically cleans up on drop.
///
/// This struct manages the creation of a unique temporary directory for the
/// linking process and automatically removes it when dropped (whether via
/// normal completion or early error return).
struct TempLinkDir {
    /// Path to the temporary directory.
    path: PathBuf,
    /// Paths to the object files written to the directory.
    obj_paths: Vec<PathBuf>,
    /// Path to the runtime archive in the directory.
    runtime_path: PathBuf,
    /// Path where the linked executable will be written.
    output_path: PathBuf,
}

impl TempLinkDir {
    /// Create a new temporary directory for linking.
    ///
    /// Creates a unique directory in the system temp directory with the
    /// format `rue-<pid>-<counter>` to ensure uniqueness even in parallel
    /// test execution.
    fn new() -> CompileResult<Self> {
        let unique_id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("rue-{}-{}", std::process::id(), unique_id));
        std::fs::create_dir_all(&path)
            .map_err(|e| io_link_error("failed to create temp directory", e))?;

        let runtime_path = path.join("librue_runtime.a");
        let output_path = path.join("output");

        Ok(Self {
            path,
            obj_paths: Vec::new(),
            runtime_path,
            output_path,
        })
    }

    /// Write object files to the temporary directory.
    ///
    /// Each object file is written to a file named `obj{N}.o` where N is
    /// the index. The paths are stored in `self.obj_paths`.
    fn write_object_files(&mut self, object_files: &[Vec<u8>]) -> CompileResult<()> {
        for (i, obj_bytes) in object_files.iter().enumerate() {
            let obj_path = self.path.join(format!("obj{}.o", i));
            let mut file = std::fs::File::create(&obj_path)
                .map_err(|e| io_link_error("failed to create temp object file", e))?;
            file.write_all(obj_bytes)
                .map_err(|e| io_link_error("failed to write temp object file", e))?;
            self.obj_paths.push(obj_path);
        }
        Ok(())
    }

    /// Write the runtime archive to the temporary directory.
    fn write_runtime(&self, runtime_bytes: &[u8]) -> CompileResult<()> {
        std::fs::write(&self.runtime_path, runtime_bytes)
            .map_err(|e| io_link_error("failed to write runtime archive", e))
    }

    /// Read the linked executable from the output path.
    fn read_output(&self) -> CompileResult<Vec<u8>> {
        std::fs::read(&self.output_path)
            .map_err(|e| io_link_error("failed to read linked executable", e))
    }
}

impl Drop for TempLinkDir {
    fn drop(&mut self) {
        // Best-effort cleanup; ignore errors
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The rue-runtime staticlib archive bytes, embedded at compile time.
/// This is linked into every Rue executable.
static RUNTIME_BYTES: &[u8] = include_bytes!("librue_runtime.a");

/// Validate that the embedded runtime archive is well-formed.
///
/// This is called by tests to ensure the runtime is valid at build time.
/// Returns an error message if validation fails.
pub fn validate_runtime() -> Result<(), String> {
    Archive::parse(RUNTIME_BYTES)
        .map(|_| ())
        .map_err(|e| format!("embedded rue-runtime archive is invalid: {}", e))
}

// Re-export commonly used types
pub use rue_air::{Air, AnalyzedFunction, ArrayTypeDef, Sema, SemaOutput, StructDef, Type};
pub use rue_cfg::{Cfg, CfgBuilder, CfgOutput, OptLevel};
pub use rue_codegen::{
    LoweringDebugInfo, RegAllocDebugInfo, RelocationKind, StackFrameInfo, X86Mir,
    aarch64::Aarch64Mir, generate_stack_frame_info,
};
pub use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, Diagnostic, ErrorKind,
    MultiErrorResult, PreviewFeature, PreviewFeatures, WarningKind,
};
pub use rue_intern::{Interner, Symbol};
pub use rue_lexer::{Lexer, Token, TokenKind};
pub use rue_linker::{Archive, CodeRelocation, Linker, ObjectBuilder, ObjectFile, RelocationType};
pub use rue_parser::{Ast, Expr, Function, Parser};
pub use rue_rir::{AstGen, Rir, RirPrinter};
pub use rue_span::Span;
pub use rue_target::{Arch, Target};

/// Which linker to use for the final linking phase.
///
/// The Rue compiler can either use its built-in ELF linker or delegate to
/// an external system linker like `clang`, `gcc`, or `ld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkerMode {
    /// Use the internal linker (default).
    Internal,
    /// Use an external system linker (e.g., "clang", "ld", "gcc").
    System(String),
}

impl Default for LinkerMode {
    fn default() -> Self {
        LinkerMode::Internal
    }
}

/// Configuration options for compilation.
///
/// Controls target architecture, linker selection, optimization level, and feature flags.
///
/// # Example
///
/// ```ignore
/// let options = CompileOptions {
///     target: Target::host(),
///     linker: LinkerMode::Internal,
///     opt_level: OptLevel::O1,
///     preview_features: PreviewFeatures::new(),
/// };
/// let output = compile_with_options(source, &options)?;
/// ```
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// The target architecture and OS.
    pub target: Target,
    /// Which linker to use.
    pub linker: LinkerMode,
    /// Optimization level.
    pub opt_level: OptLevel,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host(),
            linker: LinkerMode::Internal,
            opt_level: OptLevel::default(),
            preview_features: PreviewFeatures::new(),
        }
    }
}

/// A function with its typed IR (AIR) and control flow graph (CFG).
///
/// This combines the output of semantic analysis with CFG construction.
pub struct FunctionWithCfg {
    /// The analyzed function from semantic analysis.
    pub analyzed: AnalyzedFunction,
    /// The control flow graph built from the AIR.
    pub cfg: Cfg,
}

/// Intermediate compilation state after frontend processing.
///
/// This allows inspection of the IR at each stage, useful for
/// debugging and the `--emit` CLI flags.
pub struct CompileState {
    /// The abstract syntax tree.
    pub ast: Ast,
    /// String interner used during compilation.
    pub interner: Interner,
    /// The untyped IR (RIR).
    pub rir: Rir,
    /// Analyzed functions with typed IR and control flow graphs.
    pub functions: Vec<FunctionWithCfg>,
    /// Struct definitions.
    pub struct_defs: Vec<StructDef>,
    /// Array type definitions (element type and length).
    pub array_types: Vec<ArrayTypeDef>,
    /// String literals indexed by their string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during compilation.
    pub warnings: Vec<CompileWarning>,
}

/// Output from successful compilation.
///
/// Contains the compiled executable binary and any warnings generated
/// during compilation. The binary format depends on the target platform
/// (ELF for Linux, Mach-O for macOS).
pub struct CompileOutput {
    /// The compiled ELF binary.
    pub elf: Vec<u8>,
    /// Warnings generated during compilation.
    pub warnings: Vec<CompileWarning>,
}

/// Compile source code through all frontend phases (up to but not including codegen).
///
/// This runs: lexing → parsing → AST to RIR → semantic analysis → CFG construction.
/// Returns the compile state which can be inspected for debugging.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
///
/// Uses default optimization level (O0) and no preview features. For custom options,
/// use [`compile_frontend_with_options`].
pub fn compile_frontend(source: &str) -> MultiErrorResult<CompileState> {
    compile_frontend_with_options(source, OptLevel::default(), &PreviewFeatures::new())
}

/// Compile source code through all frontend phases with optimization.
///
/// This runs: lexing → parsing → AST to RIR → semantic analysis → CFG construction → optimization.
/// Returns the compile state which can be inspected for debugging.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile_frontend_with_options(
    source: &str,
    opt_level: OptLevel,
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<CompileState> {
    let _span = info_span!("frontend", source_bytes = source.len()).entered();

    // Lexing - errors here are fatal (can't continue without tokens)
    let (tokens, interner) = {
        let _span = info_span!("lexer").entered();
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from)?;
        info!(token_count = tokens.len(), "lexing complete");
        (tokens, interner)
    };

    // Parsing - errors here are fatal (can't continue without AST)
    let (ast, interner) = {
        let _span = info_span!("parser").entered();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().map_err(CompileErrors::from)?;
        info!(item_count = ast.items.len(), "parsing complete");
        (ast, interner)
    };

    compile_frontend_from_ast_with_options(ast, interner, opt_level, preview_features)
}

/// Compile from an already-parsed AST through all remaining frontend phases.
///
/// This runs: AST to RIR → semantic analysis → CFG construction.
/// Use this when you already have a parsed AST (e.g., for `--emit` modes that
/// need both AST output and later stage output without double-parsing).
///
/// Uses default optimization level (O0) and no preview features. For custom options,
/// use [`compile_frontend_from_ast_with_options`].
pub fn compile_frontend_from_ast(ast: Ast, interner: Interner) -> MultiErrorResult<CompileState> {
    compile_frontend_from_ast_with_options(
        ast,
        interner,
        OptLevel::default(),
        &PreviewFeatures::new(),
    )
}

/// Compile from an already-parsed AST through all remaining frontend phases with optimization.
///
/// This runs: AST to RIR → semantic analysis → CFG construction → optimization.
/// Use this when you already have a parsed AST (e.g., for `--emit` modes that
/// need both AST output and later stage output without double-parsing).
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile_frontend_from_ast_with_options(
    ast: Ast,
    interner: Interner,
    opt_level: OptLevel,
    preview_features: &PreviewFeatures,
) -> MultiErrorResult<CompileState> {
    // AST to RIR (untyped IR)
    let (rir, interner) = {
        let _span = info_span!("astgen").entered();
        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();
        info!(instruction_count = rir.len(), "AST generation complete");
        (rir, interner)
    };

    // Semantic analysis (RIR to AIR) - this now collects multiple errors
    let sema_output = {
        let _span = info_span!("sema").entered();
        let sema = Sema::new(&rir, &interner, preview_features.clone());
        let output = sema.analyze_all()?;
        info!(
            function_count = output.functions.len(),
            struct_count = output.struct_defs.len(),
            "semantic analysis complete"
        );
        output
    };

    // Synthesize drop glue functions for structs that need them
    let drop_glue_functions =
        drop_glue::synthesize_drop_glue(&sema_output.struct_defs, &sema_output.array_types);

    // Combine user functions with synthesized drop glue functions
    let all_functions: Vec<_> = sema_output
        .functions
        .into_iter()
        .chain(drop_glue_functions)
        .collect();

    // Build CFGs from AIR (one per function) in parallel, collecting warnings
    let (functions, warnings) = {
        let _span = info_span!("cfg_construction").entered();

        // Build CFGs in parallel - each function is independent
        let results: Vec<(FunctionWithCfg, Vec<CompileWarning>)> = all_functions
            .into_par_iter()
            .map(|func| {
                let cfg_output = CfgBuilder::build(
                    &func.air,
                    func.num_locals,
                    func.num_param_slots,
                    &func.name,
                    &sema_output.struct_defs,
                    &sema_output.array_types,
                    func.param_modes.clone(),
                    &interner,
                );

                // Apply optimizations to the CFG
                let mut cfg = cfg_output.cfg;
                rue_cfg::opt::optimize(&mut cfg, opt_level);

                (
                    FunctionWithCfg {
                        analyzed: func,
                        cfg,
                    },
                    cfg_output.warnings,
                )
            })
            .collect();

        // Unzip the results and collect all warnings
        let mut functions = Vec::with_capacity(results.len());
        let mut warnings = sema_output.warnings;
        for (func, func_warnings) in results {
            functions.push(func);
            warnings.extend(func_warnings);
        }

        info!(
            function_count = functions.len(),
            "CFG construction complete"
        );
        (functions, warnings)
    };

    Ok(CompileState {
        ast,
        interner,
        rir,
        functions,
        struct_defs: sema_output.struct_defs,
        array_types: sema_output.array_types,
        strings: sema_output.strings,
        warnings,
    })
}

/// Compile source code to an ELF binary.
///
/// This is the main entry point for compilation.
/// Returns the ELF binary along with any warnings generated during compilation.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile(source: &str) -> MultiErrorResult<CompileOutput> {
    compile_with_options(source, &CompileOptions::default())
}

/// Compile source code to an ELF binary with the given options.
///
/// This allows specifying the target architecture, optimization level, and other compilation options.
///
/// This function collects errors from multiple functions instead of stopping at the
/// first error, allowing users to see all issues at once.
pub fn compile_with_options(
    source: &str,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!(
        "compile",
        target = %options.target,
        source_bytes = source.len()
    )
    .entered();

    let state =
        compile_frontend_with_options(source, options.opt_level, &options.preview_features)?;

    // Check for main function
    let _main_fn = state
        .functions
        .iter()
        .find(|f| f.analyzed.name == "main")
        .ok_or_else(|| {
            CompileErrors::from(CompileError::without_span(ErrorKind::NoMainFunction))
        })?;

    // Dispatch to the appropriate backend based on target architecture
    match options.target.arch() {
        Arch::X86_64 => compile_x86_64(&state, options),
        Arch::Aarch64 => compile_aarch64(&state, options),
    }
}

/// Compile for x86-64 target.
fn compile_x86_64(
    state: &CompileState,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    // Generate machine code for all functions
    let object_files = {
        let _span = info_span!("codegen", arch = "x86_64").entered();
        let mut object_files: Vec<Vec<u8>> = Vec::new();
        let mut total_code_bytes = 0usize;

        for func in &state.functions {
            let machine_code = rue_codegen::x86_64::generate(
                &func.cfg,
                &state.struct_defs,
                &state.array_types,
                &state.strings,
                &state.interner,
            )
            .map_err(CompileErrors::from)?;
            total_code_bytes += machine_code.code.len();

            // Build object file for this function
            let mut obj_builder = ObjectBuilder::new(options.target, &func.analyzed.name)
                .code(machine_code.code)
                .strings(machine_code.strings);

            // Add relocations from codegen (convert RelocationKind to RelocationType).
            for reloc in machine_code.relocations {
                let rel_type = match reloc.kind {
                    RelocationKind::X86Pc32 => RelocationType::Pc32,
                    RelocationKind::X86Plt32 => RelocationType::Plt32,
                    // AArch64 relocations should never appear in x86-64 codegen
                    RelocationKind::Aarch64AdrpPage21
                    | RelocationKind::Aarch64AddLo12
                    | RelocationKind::Aarch64Call26 => {
                        unreachable!("x86-64 codegen emitted AArch64 relocation {:?}", reloc.kind)
                    }
                };

                obj_builder = obj_builder.relocation(CodeRelocation {
                    offset: reloc.offset,
                    symbol: reloc.symbol,
                    rel_type,
                    addend: reloc.addend,
                });
            }

            object_files.push(obj_builder.build());
        }
        info!(
            function_count = state.functions.len(),
            code_bytes = total_code_bytes,
            "codegen complete"
        );
        object_files
    };

    // Link to executable
    match &options.linker {
        LinkerMode::Internal => link_internal(state, options, &object_files),
        LinkerMode::System(linker_cmd) => link_system(state, options, &object_files, linker_cmd),
    }
}

/// Link using the internal linker.
///
/// For ELF targets (Linux), uses the built-in ELF linker.
/// For Mach-O targets (macOS), delegates to the system linker (clang) since
/// the internal linker only supports ELF.
fn link_internal(
    state: &CompileState,
    options: &CompileOptions,
    object_files: &[Vec<u8>],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "internal").entered();

    // For macOS targets, the internal linker doesn't support Mach-O,
    // so we delegate to the system linker (clang).
    if options.target.is_macho() {
        return link_system(state, options, object_files, "clang");
    }

    // HACK: Use system linker on Linux until internal ELF linker bug is fixed.
    // See: String methods crash on Linux (both x86-64 and ARM64) but work on macOS.
    // TODO: Remove this once the internal linker is fixed.
    if options.target.is_elf() {
        return link_system(state, options, object_files, "clang");
    }

    let mut linker = Linker::new(options.target);

    // Add all object files to the linker
    for obj_bytes in object_files {
        let obj = ObjectFile::parse(obj_bytes)
            .map_err(link_error)
            .map_err(CompileErrors::from)?;
        linker
            .add_object(obj)
            .map_err(link_error)
            .map_err(CompileErrors::from)?;
    }

    // Mark _start as required so it gets pulled from the archive.
    // The entry point must be marked before adding the archive because
    // archive linking only includes objects that define needed symbols.
    linker.require_symbol("_start");

    // Add the runtime library
    let runtime = Archive::parse(RUNTIME_BYTES)
        .map_err(link_error)
        .map_err(CompileErrors::from)?;
    linker
        .add_archive(runtime)
        .map_err(link_error)
        .map_err(CompileErrors::from)?;

    // Link to executable
    // Use _start from the runtime as the entry point (it will call main)
    let elf = linker
        .link("_start")
        .map_err(link_error)
        .map_err(CompileErrors::from)?;
    info!(
        object_count = object_files.len(),
        output_bytes = elf.len(),
        "linking complete"
    );

    Ok(CompileOutput {
        elf,
        warnings: state.warnings.clone(),
    })
}

/// Link using an external system linker.
///
/// Handles target-specific linker flags for both Linux and macOS.
fn link_system(
    state: &CompileState,
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "system", command = linker_cmd).entered();

    // Set up temporary directory with object files and runtime
    let mut temp_dir = TempLinkDir::new().map_err(CompileErrors::from)?;
    temp_dir
        .write_object_files(object_files)
        .map_err(CompileErrors::from)?;
    temp_dir
        .write_runtime(RUNTIME_BYTES)
        .map_err(CompileErrors::from)?;

    // Build the linker command
    let mut cmd = Command::new(linker_cmd);

    // Add target-specific linker flags
    if options.target.is_macho() {
        // macOS-specific flags
        cmd.arg("-nostdlib");
        cmd.arg("-arch").arg("arm64");
        cmd.arg("-e").arg("__main");
    } else {
        // Linux/ELF-specific flags
        cmd.arg("-static");
        cmd.arg("-nostdlib");
    }

    cmd.arg("-o");
    cmd.arg(&temp_dir.output_path);

    // Add object files
    for path in &temp_dir.obj_paths {
        cmd.arg(path);
    }

    // Add the runtime library
    cmd.arg(&temp_dir.runtime_path);

    // macOS requires libSystem for syscalls
    if options.target.is_macho() {
        cmd.arg("-lSystem");
    }

    // Run the linker
    let output = cmd.output().map_err(|e| {
        CompileErrors::from(CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker '{}': {}",
            linker_cmd, e
        ))))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // temp_dir is dropped here, cleaning up automatically
        return Err(CompileErrors::from(CompileError::without_span(
            ErrorKind::LinkError(format!("linker '{}' failed: {}", linker_cmd, stderr)),
        )));
    }

    // Read the resulting executable
    let elf = temp_dir.read_output().map_err(CompileErrors::from)?;
    info!(
        object_count = object_files.len(),
        output_bytes = elf.len(),
        "linking complete"
    );

    // temp_dir is dropped here, cleaning up automatically
    Ok(CompileOutput {
        elf,
        warnings: state.warnings.clone(),
    })
}

/// Compile for AArch64 target.
fn compile_aarch64(
    state: &CompileState,
    options: &CompileOptions,
) -> MultiErrorResult<CompileOutput> {
    // Generate machine code for all functions using the aarch64 backend
    let object_files = {
        let _span = info_span!("codegen", arch = "aarch64").entered();
        let mut object_files: Vec<Vec<u8>> = Vec::new();
        let mut total_code_bytes = 0usize;

        for func in &state.functions {
            let machine_code = rue_codegen::aarch64::generate(
                &func.cfg,
                &state.struct_defs,
                &state.array_types,
                &state.strings,
                &state.interner,
            )
            .map_err(CompileErrors::from)?;
            total_code_bytes += machine_code.code.len();

            let mut obj_builder = ObjectBuilder::new(options.target, &func.analyzed.name)
                .code(machine_code.code)
                .strings(machine_code.strings);

            // Add relocations from codegen (convert RelocationKind to RelocationType).
            for reloc in machine_code.relocations {
                let rel_type = match reloc.kind {
                    RelocationKind::Aarch64AdrpPage21 => RelocationType::AdrpPage21,
                    RelocationKind::Aarch64AddLo12 => RelocationType::AddLo12,
                    RelocationKind::Aarch64Call26 => RelocationType::Call26,
                    // x86-64 relocations should never appear in AArch64 codegen
                    RelocationKind::X86Pc32 | RelocationKind::X86Plt32 => {
                        unreachable!("AArch64 codegen emitted x86-64 relocation {:?}", reloc.kind)
                    }
                };

                obj_builder = obj_builder.relocation(CodeRelocation {
                    offset: reloc.offset,
                    symbol: reloc.symbol,
                    rel_type,
                    addend: reloc.addend,
                });
            }

            object_files.push(obj_builder.build());
        }
        info!(
            function_count = state.functions.len(),
            code_bytes = total_code_bytes,
            "codegen complete"
        );
        object_files
    };

    // Link to executable (linker selection is handled at the top level, not here)
    match &options.linker {
        LinkerMode::Internal => link_internal(state, options, &object_files),
        LinkerMode::System(linker_cmd) => link_system(state, options, &object_files, linker_cmd),
    }
}

/// Machine IR that can hold either x86-64 or AArch64 MIR.
///
/// This enum allows the `--emit mir` and `--emit asm` commands to work
/// with any target architecture.
pub enum Mir {
    /// x86-64 machine IR.
    X86_64(X86Mir),
    /// AArch64 machine IR.
    Aarch64(Aarch64Mir),
}

impl std::fmt::Display for Mir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mir::X86_64(mir) => write!(f, "{}", mir),
            Mir::Aarch64(mir) => write!(f, "{}", mir),
        }
    }
}

impl Mir {
    /// Format MIR as assembly text.
    ///
    /// This prints the MIR instructions in assembly-like format.
    /// When called with allocated MIR (post-regalloc), physical registers
    /// are shown (rax, rbx, r12 for x86-64; x0, x1, x19 for aarch64).
    pub fn format_assembly(&self) -> String {
        let mut output = String::new();
        match self {
            Mir::X86_64(mir) => {
                use rue_codegen::x86_64::X86Inst;
                for inst in mir.instructions() {
                    match inst {
                        X86Inst::Label { id } => {
                            output.push_str(&format!("{}:\n", id));
                        }
                        X86Inst::CallRel { symbol_id } => {
                            output.push_str(&format!("    call {}\n", mir.get_symbol(*symbol_id)));
                        }
                        _ => {
                            output.push_str(&format!("    {}\n", inst));
                        }
                    }
                }
            }
            Mir::Aarch64(mir) => {
                use rue_codegen::aarch64::Aarch64Inst;
                for inst in mir.instructions() {
                    match inst {
                        Aarch64Inst::Label { id } => {
                            output.push_str(&format!("{}:\n", id));
                        }
                        Aarch64Inst::Bl { symbol_id } => {
                            output.push_str(&format!("    bl {}\n", mir.get_symbol(*symbol_id)));
                        }
                        _ => {
                            output.push_str(&format!("    {}\n", inst));
                        }
                    }
                }
            }
        }
        output
    }
}

/// Generate MIR from CFG for the given target (for debugging/inspection).
///
/// This returns the MIR before register allocation, with virtual registers.
pub fn generate_mir(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    interner: &Interner,
    target: Target,
) -> Mir {
    match target.arch() {
        Arch::X86_64 => {
            let mir = rue_codegen::x86_64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower();
            Mir::X86_64(mir)
        }
        Arch::Aarch64 => {
            let mir = rue_codegen::aarch64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower();
            Mir::Aarch64(mir)
        }
    }
}

/// Generate MIR after register allocation for the given target (for debugging/inspection).
///
/// This returns the MIR after register allocation, with physical registers.
/// This is closer to the final assembly that will be emitted.
pub fn generate_allocated_mir(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    interner: &Interner,
    target: Target,
) -> CompileResult<Mir> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();
    let existing_slots = num_locals + num_params;

    match target.arch() {
        Arch::X86_64 => {
            // Lower CFG to X86Mir with virtual registers
            let mir = rue_codegen::x86_64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower();

            // Allocate physical registers
            let (mir, _num_spills, _used_callee_saved) =
                rue_codegen::x86_64::RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

            Ok(Mir::X86_64(mir))
        }
        Arch::Aarch64 => {
            // Lower CFG to Aarch64Mir with virtual registers
            let mir = rue_codegen::aarch64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower();

            // Allocate physical registers
            let (mir, _num_spills, _used_callee_saved) =
                rue_codegen::aarch64::RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

            Ok(Mir::Aarch64(mir))
        }
    }
}

/// Generate liveness debug information for a CFG.
///
/// This performs liveness analysis on the MIR (before register allocation)
/// and returns detailed per-instruction liveness information.
///
/// Used by `--emit liveness` to visualize which values are live at each program point.
pub fn generate_liveness_info(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    interner: &Interner,
    target: Target,
) -> rue_codegen::LivenessDebugInfo {
    match target.arch() {
        Arch::X86_64 => {
            let mir = rue_codegen::x86_64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower();
            rue_codegen::x86_64::liveness::analyze_debug(&mir)
        }
        Arch::Aarch64 => {
            let mir = rue_codegen::aarch64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower();
            rue_codegen::aarch64::liveness::analyze_debug(&mir)
        }
    }
}

/// Generate lowering debug information for a CFG.
///
/// This performs CFG-to-MIR lowering (instruction selection) and returns
/// detailed information about how each CFG instruction maps to MIR instructions.
///
/// Used by `--emit lowering` to visualize the instruction selection process.
pub fn generate_lowering_info(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    interner: &Interner,
    target: Target,
) -> rue_codegen::LoweringDebugInfo {
    match target.arch() {
        Arch::X86_64 => {
            let (_mir, debug_info) = rue_codegen::x86_64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower_with_debug();
            debug_info
        }
        Arch::Aarch64 => {
            let (_mir, debug_info) = rue_codegen::aarch64::CfgLower::new(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )
            .lower_with_debug();
            debug_info
        }
    }
}

/// Generate the actual emitted assembly text for a CFG.
///
/// Unlike `format_assembly()` on Mir which shows MIR instructions,
/// this returns the actual assembly that will be emitted, including
/// prologue/epilogue code that the emitter adds.
///
/// This is useful for debugging and for --emit asm output that accurately
/// reflects what's in the binary.
pub fn generate_emitted_asm(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    interner: &Interner,
    target: Target,
) -> CompileResult<String> {
    match target.arch() {
        Arch::X86_64 => {
            let (_machine_code, asm) = rue_codegen::x86_64::generate_with_asm(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )?;
            Ok(asm)
        }
        Arch::Aarch64 => {
            let (_machine_code, asm) = rue_codegen::aarch64::generate_with_asm(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )?;
            Ok(asm)
        }
    }
}

/// Generate register allocation debug information for a CFG.
///
/// This returns information about the register allocation process,
/// including live ranges, interference edges, and allocation decisions.
/// The output is formatted as a human-readable string.
pub fn generate_regalloc_info(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    interner: &Interner,
    target: Target,
) -> CompileResult<String> {
    match target.arch() {
        Arch::X86_64 => {
            let debug_info = rue_codegen::x86_64::generate_regalloc_info(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )?;
            Ok(debug_info.to_string())
        }
        Arch::Aarch64 => {
            let debug_info = rue_codegen::aarch64::generate_regalloc_info(
                cfg,
                struct_defs,
                array_types,
                strings,
                interner,
            )?;
            Ok(debug_info.to_string())
        }
    }
}

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Output from semantic analysis (compile_to_air).
///
/// This struct provides access to the typed IR (AIR) for each function,
/// useful for testing semantic analysis without generating machine code.
#[derive(Debug)]
pub struct AirOutput {
    /// The abstract syntax tree.
    pub ast: Ast,
    /// String interner used during compilation.
    pub interner: Interner,
    /// The untyped IR (RIR).
    pub rir: Rir,
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// Struct definitions.
    pub struct_defs: Vec<StructDef>,
    /// Array type definitions.
    pub array_types: Vec<ArrayTypeDef>,
    /// String literals.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

/// Compile source code up to AIR (typed IR) without building CFG.
///
/// This is a test helper that runs: lexing → parsing → AST to RIR → semantic analysis.
/// It stops before CFG construction, making it fast for testing type checking
/// and semantic analysis.
///
/// # Example
///
/// ```ignore
/// use rue_compiler::compile_to_air;
///
/// let result = compile_to_air("fn main() -> i32 { 1 + 2 * 3 }");
/// assert!(result.is_ok());
/// ```
pub fn compile_to_air(source: &str) -> MultiErrorResult<AirOutput> {
    // Lexing
    let lexer = Lexer::new(source);
    let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from)?;

    // Parsing
    let parser = Parser::new(tokens, interner);
    let (ast, interner) = parser.parse().map_err(CompileErrors::from)?;

    // AST to RIR (untyped IR)
    let astgen = AstGen::new(&ast, &interner);
    let rir = astgen.generate();

    // Semantic analysis (RIR to AIR)
    let sema = Sema::new(&rir, &interner, PreviewFeatures::new());
    let sema_output = sema.analyze_all()?;

    Ok(AirOutput {
        ast,
        interner,
        rir,
        functions: sema_output.functions,
        struct_defs: sema_output.struct_defs,
        array_types: sema_output.array_types,
        strings: sema_output.strings,
        warnings: sema_output.warnings,
    })
}

/// Compile source code up to CFG (control flow graph).
///
/// This is an alias for `compile_frontend` that provides a more intuitive name
/// for test code. It runs the full frontend pipeline:
/// lexing → parsing → AST to RIR → semantic analysis → CFG construction.
///
/// # Example
///
/// ```ignore
/// use rue_compiler::compile_to_cfg;
///
/// let result = compile_to_cfg("fn main() -> i32 { if true { 1 } else { 2 } }");
/// assert!(result.is_ok());
/// let state = result.unwrap();
/// assert_eq!(state.functions.len(), 1);
/// ```
pub fn compile_to_cfg(source: &str) -> MultiErrorResult<CompileState> {
    compile_frontend(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_runtime_is_valid() {
        validate_runtime().expect("embedded runtime should be valid");
    }

    #[test]
    fn test_compile_simple() {
        let output = compile("fn main() -> i32 { 42 }").unwrap();
        // Should produce a valid executable (ELF on Linux, Mach-O on macOS)
        let magic = &output.elf[0..4];
        let is_elf = magic == &[0x7F, b'E', b'L', b'F'];
        let is_macho = magic == &0xFEEDFACF_u32.to_le_bytes();
        assert!(
            is_elf || is_macho,
            "should produce valid ELF or Mach-O binary"
        );
    }

    #[test]
    fn test_compile_no_main() {
        let result = compile("fn foo() -> i32 { 42 }");
        assert!(result.is_err());
    }

    #[test]
    fn test_unused_variable_warning() {
        let output = compile("fn main() -> i32 { let x = 42; 0 }").unwrap();
        assert_eq!(output.warnings.len(), 1);
        assert!(output.warnings[0].to_string().contains("unused variable"));
        assert!(output.warnings[0].to_string().contains("'x'"));
    }

    #[test]
    fn test_underscore_prefix_no_warning() {
        let output = compile("fn main() -> i32 { let _x = 42; 0 }").unwrap();
        assert_eq!(output.warnings.len(), 0);
    }

    #[test]
    fn test_used_variable_no_warning() {
        let output = compile("fn main() -> i32 { let x = 42; x }").unwrap();
        assert_eq!(output.warnings.len(), 0);
    }

    #[test]
    fn test_compile_frontend_includes_warnings() {
        let state = compile_frontend("fn main() -> i32 { let x = 42; 0 }").unwrap();
        assert_eq!(state.warnings.len(), 1);
        assert!(state.warnings[0].to_string().contains("unused variable"));
    }

    #[test]
    fn test_multiple_errors_collected() {
        // Test that errors from multiple functions are collected together
        // Use examples that both result in type mismatch errors
        let source = r#"
            fn foo() -> i32 { true }
            fn bar() -> i32 { false }
            fn main() -> i32 { 0 }
        "#;
        let result = compile_frontend(source);
        let errors = match result {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        // Should have at least 2 errors (one from foo, one from bar)
        assert!(
            errors.len() >= 2,
            "expected at least 2 errors, got {}",
            errors.len()
        );

        // All errors should be type mismatches (returning bool where i32 expected)
        for error in errors.iter() {
            assert!(
                error.to_string().contains("type mismatch"),
                "expected type mismatch error, got: {}",
                error
            );
        }
    }

    #[test]
    fn test_multiple_errors_display() {
        // Use examples that both result in type mismatch errors
        let source = r#"
            fn foo() -> i32 { true }
            fn bar() -> i32 { false }
            fn main() -> i32 { 0 }
        "#;
        let errors = match compile_frontend(source) {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        // Display should show both errors
        let display = errors.to_string();
        assert!(
            display.contains("type mismatch"),
            "display should contain error message"
        );
        if errors.len() > 1 {
            assert!(
                display.contains("more error"),
                "display should indicate more errors"
            );
        }
    }

    #[test]
    fn test_single_error_still_works() {
        // Single error should still be collected and returned properly
        let source = "fn main() -> i32 { true }";
        let errors = match compile_frontend(source) {
            Ok(_) => panic!("expected error, got success"),
            Err(e) => e,
        };

        assert_eq!(errors.len(), 1);
        assert!(
            errors
                .first()
                .unwrap()
                .to_string()
                .contains("type mismatch")
        );
    }
}

// ============================================================================
// Integration Unit Tests
// ============================================================================
//
// These tests verify the compilation pipeline without execution. They test:
// - Type checking and semantic analysis
// - CFG construction
// - Error message quality
//
// Benefits:
// - Fast: No file I/O, no process spawning, no execution
// - Comprehensive: Tests full parse→sema→codegen pipeline
// - Debuggable: Can inspect intermediate IRs in tests

#[cfg(test)]
mod integration_tests {
    use super::*;

    // ========================================================================
    // Integer Types
    // ========================================================================

    mod integer_types {
        use super::*;

        #[test]
        fn signed_integer_return() {
            assert!(compile_to_air("fn main() -> i8 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> i16 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> i32 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> i64 { 42 }").is_ok());
        }

        #[test]
        fn unsigned_integer_return() {
            assert!(compile_to_air("fn main() -> u8 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> u16 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> u32 { 42 }").is_ok());
            assert!(compile_to_air("fn main() -> u64 { 42 }").is_ok());
        }

        #[test]
        fn integer_type_mismatch() {
            let result = compile_to_air("fn main() -> i32 { let x: i64 = 1; x }");
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("type mismatch") || err.contains("expected"));
        }

        #[test]
        fn integer_literal_type_inference() {
            // Type inferred from return type
            assert!(compile_to_air("fn main() -> i64 { 100 }").is_ok());
            // Type inferred from annotation
            assert!(compile_to_air("fn main() -> i32 { let x: i64 = 100; 0 }").is_ok());
        }
    }

    // ========================================================================
    // Boolean Type
    // ========================================================================

    mod boolean_type {
        use super::*;

        #[test]
        fn boolean_literals() {
            assert!(compile_to_air("fn main() -> bool { true }").is_ok());
            assert!(compile_to_air("fn main() -> bool { false }").is_ok());
        }

        #[test]
        fn boolean_in_condition() {
            assert!(compile_to_cfg("fn main() -> i32 { if true { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn non_boolean_condition_rejected() {
            let result = compile_to_air("fn main() -> i32 { if 1 { 1 } else { 0 } }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Unit Type
    // ========================================================================

    mod unit_type {
        use super::*;

        #[test]
        fn unit_return_type() {
            assert!(compile_to_air("fn main() -> () { () }").is_ok());
        }

        #[test]
        fn unit_in_expression() {
            assert!(compile_to_air("fn main() -> () { let _x = (); () }").is_ok());
        }

        #[test]
        fn implicit_unit_return() {
            assert!(compile_to_air("fn foo() -> () { } fn main() -> i32 { 0 }").is_ok());
        }
    }

    // ========================================================================
    // Arithmetic Operations
    // ========================================================================

    mod arithmetic {
        use super::*;

        #[test]
        fn basic_addition() {
            assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        }

        #[test]
        fn basic_subtraction() {
            assert!(compile_to_air("fn main() -> i32 { 5 - 3 }").is_ok());
        }

        #[test]
        fn basic_multiplication() {
            assert!(compile_to_air("fn main() -> i32 { 3 * 4 }").is_ok());
        }

        #[test]
        fn basic_division() {
            assert!(compile_to_air("fn main() -> i32 { 10 / 2 }").is_ok());
        }

        #[test]
        fn basic_modulo() {
            assert!(compile_to_air("fn main() -> i32 { 10 % 3 }").is_ok());
        }

        #[test]
        fn unary_negation() {
            assert!(compile_to_air("fn main() -> i32 { -42 }").is_ok());
        }

        #[test]
        fn operator_precedence() {
            // Multiplication before addition
            let state = compile_to_cfg("fn main() -> i32 { 1 + 2 * 3 }").unwrap();
            assert_eq!(state.functions.len(), 1);
        }

        #[test]
        fn chained_operations() {
            assert!(compile_to_air("fn main() -> i32 { 1 + 2 + 3 + 4 }").is_ok());
        }

        #[test]
        fn mixed_type_arithmetic_rejected() {
            let result = compile_to_air("fn main() -> i32 { 1 + true }");
            assert!(result.is_err());
        }

        #[test]
        fn unsigned_arithmetic() {
            assert!(compile_to_air("fn main() -> u32 { 10 + 5 }").is_ok());
            assert!(compile_to_air("fn main() -> u32 { 10 - 5 }").is_ok());
            assert!(compile_to_air("fn main() -> u32 { 10 * 5 }").is_ok());
        }
    }

    // ========================================================================
    // Comparison Operations
    // ========================================================================

    mod comparison {
        use super::*;

        #[test]
        fn equality_comparison() {
            assert!(compile_to_air("fn main() -> bool { 1 == 1 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 1 != 2 }").is_ok());
        }

        #[test]
        fn ordering_comparison() {
            assert!(compile_to_air("fn main() -> bool { 1 < 2 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 2 > 1 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 1 <= 2 }").is_ok());
            assert!(compile_to_air("fn main() -> bool { 2 >= 1 }").is_ok());
        }

        #[test]
        fn boolean_equality() {
            assert!(compile_to_air("fn main() -> bool { true == true }").is_ok());
            assert!(compile_to_air("fn main() -> bool { true != false }").is_ok());
        }

        #[test]
        fn comparison_returns_bool() {
            let result = compile_to_air("fn main() -> i32 { 1 < 2 }");
            assert!(result.is_err()); // Type mismatch: bool vs i32
        }

        #[test]
        fn mixed_type_comparison_rejected() {
            let result = compile_to_air("fn main() -> bool { 1 == true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Logical Operations
    // ========================================================================

    mod logical {
        use super::*;

        #[test]
        fn logical_and() {
            assert!(compile_to_cfg("fn main() -> bool { true && false }").is_ok());
        }

        #[test]
        fn logical_or() {
            assert!(compile_to_cfg("fn main() -> bool { true || false }").is_ok());
        }

        #[test]
        fn logical_not() {
            assert!(compile_to_air("fn main() -> bool { !true }").is_ok());
        }

        #[test]
        fn chained_logical() {
            assert!(compile_to_cfg("fn main() -> bool { true && false || true }").is_ok());
        }

        #[test]
        fn logical_with_non_bool_rejected() {
            let result = compile_to_air("fn main() -> bool { 1 && true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Bitwise Operations
    // ========================================================================

    mod bitwise {
        use super::*;

        #[test]
        fn bitwise_and() {
            assert!(compile_to_air("fn main() -> i32 { 5 & 3 }").is_ok());
        }

        #[test]
        fn bitwise_or() {
            assert!(compile_to_air("fn main() -> i32 { 5 | 3 }").is_ok());
        }

        #[test]
        fn bitwise_xor() {
            assert!(compile_to_air("fn main() -> i32 { 5 ^ 3 }").is_ok());
        }

        #[test]
        fn bitwise_not() {
            assert!(compile_to_air("fn main() -> i32 { ~5 }").is_ok());
        }

        #[test]
        fn shift_left() {
            assert!(compile_to_air("fn main() -> i32 { 1 << 4 }").is_ok());
        }

        #[test]
        fn shift_right() {
            assert!(compile_to_air("fn main() -> i32 { 16 >> 2 }").is_ok());
        }

        #[test]
        fn bitwise_on_bool_rejected() {
            let result = compile_to_air("fn main() -> bool { true & false }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - If Expressions
    // ========================================================================

    mod if_expressions {
        use super::*;

        #[test]
        fn basic_if_else() {
            assert!(compile_to_cfg("fn main() -> i32 { if true { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn if_with_condition_expr() {
            assert!(compile_to_cfg("fn main() -> i32 { if 1 < 2 { 1 } else { 0 } }").is_ok());
        }

        #[test]
        fn nested_if() {
            let src = "fn main() -> i32 { if true { if false { 1 } else { 2 } } else { 3 } }";
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn if_branches_must_match_type() {
            let result = compile_to_air("fn main() -> i32 { if true { 1 } else { true } }");
            assert!(result.is_err());
        }

        #[test]
        fn if_result_type_checked() {
            let result = compile_to_air("fn main() -> bool { if true { 1 } else { 0 } }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - Match Expressions
    // ========================================================================

    mod match_expressions {
        use super::*;

        #[test]
        fn match_on_integer() {
            let src = r#"
                fn main() -> i32 {
                    let x = 1;
                    match x {
                        1 => 10,
                        2 => 20,
                        _ => 0,
                    }
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn match_on_boolean() {
            let src = r#"
                fn main() -> i32 {
                    match true {
                        true => 1,
                        false => 0,
                    }
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn match_exhaustiveness_required() {
            // Missing case should error
            let result = compile_to_air(
                r#"
                fn main() -> i32 {
                    match 1 {
                        1 => 10,
                    }
                }
            "#,
            );
            assert!(result.is_err());
        }

        #[test]
        fn match_branches_must_match_type() {
            let result = compile_to_air(
                r#"
                fn main() -> i32 {
                    match true {
                        true => 1,
                        false => true,
                    }
                }
            "#,
            );
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Control Flow - Loops
    // ========================================================================

    mod loops {
        use super::*;

        #[test]
        fn while_loop_basic() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while x < 10 {
                        x = x + 1;
                    }
                    x
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn while_with_break() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while true {
                        x = x + 1;
                        if x == 5 {
                            break;
                        }
                    }
                    x
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn while_with_continue() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    let mut sum = 0;
                    while x < 10 {
                        x = x + 1;
                        if x == 5 {
                            continue;
                        }
                        sum = sum + x;
                    }
                    sum
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn break_outside_loop_rejected() {
            let result = compile_to_air("fn main() -> i32 { break; 0 }");
            assert!(result.is_err());
        }

        #[test]
        fn continue_outside_loop_rejected() {
            let result = compile_to_air("fn main() -> i32 { continue; 0 }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Let Bindings
    // ========================================================================

    mod let_bindings {
        use super::*;

        #[test]
        fn basic_let() {
            assert!(compile_to_air("fn main() -> i32 { let x = 42; x }").is_ok());
        }

        #[test]
        fn let_with_type_annotation() {
            assert!(compile_to_air("fn main() -> i32 { let x: i32 = 42; x }").is_ok());
        }

        #[test]
        fn mutable_let() {
            let src = "fn main() -> i32 { let mut x = 1; x = 2; x }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn immutable_assignment_rejected() {
            let result = compile_to_air("fn main() -> i32 { let x = 1; x = 2; x }");
            assert!(result.is_err());
        }

        #[test]
        fn shadowing_allowed() {
            let src = "fn main() -> i32 { let x = 1; let x = 2; x }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn shadowing_can_change_type() {
            let src = "fn main() -> bool { let x = 1; let x = true; x }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn undefined_variable_rejected() {
            let result = compile_to_air("fn main() -> i32 { x }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Functions
    // ========================================================================

    mod functions {
        use super::*;

        #[test]
        fn function_call() {
            let src = r#"
                fn add(a: i32, b: i32) -> i32 { a + b }
                fn main() -> i32 { add(1, 2) }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn function_forward_reference() {
            let src = r#"
                fn main() -> i32 { foo() }
                fn foo() -> i32 { 42 }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn recursion() {
            let src = r#"
                fn factorial(n: i32) -> i32 {
                    if n <= 1 { 1 } else { n * factorial(n - 1) }
                }
                fn main() -> i32 { factorial(5) }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn mutual_recursion() {
            let src = r#"
                fn is_even(n: i32) -> bool {
                    if n == 0 { true } else { is_odd(n - 1) }
                }
                fn is_odd(n: i32) -> bool {
                    if n == 0 { false } else { is_even(n - 1) }
                }
                fn main() -> i32 { if is_even(4) { 1 } else { 0 } }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn wrong_argument_count_rejected() {
            let src = r#"
                fn add(a: i32, b: i32) -> i32 { a + b }
                fn main() -> i32 { add(1) }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn wrong_argument_type_rejected() {
            let src = r#"
                fn foo(x: i32) -> i32 { x }
                fn main() -> i32 { foo(true) }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn undefined_function_rejected() {
            let result = compile_to_air("fn main() -> i32 { unknown() }");
            assert!(result.is_err());
        }

        #[test]
        fn return_type_mismatch_rejected() {
            let result = compile_to_air("fn main() -> i32 { true }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Structs
    // ========================================================================

    mod structs {
        use super::*;

        #[test]
        fn struct_definition() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 { 0 }
            "#;
            let result = compile_to_air(src).unwrap();
            assert_eq!(result.struct_defs.len(), 1);
        }

        #[test]
        fn struct_literal() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let _p = Point { x: 1, y: 2 };
                    0
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_field_access() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 10, y: 20 };
                    p.x + p.y
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_field_order_independent() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { y: 2, x: 1 };
                    p.x
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_unknown_field_rejected() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 1, z: 2 };
                    0
                }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn struct_equality() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> bool {
                    let a = Point { x: 1, y: 2 };
                    let b = Point { x: 1, y: 2 };
                    a == b
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn struct_move_semantics() {
            // After moving a struct, it should not be usable
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn consume(p: Point) -> i32 { p.x }
                fn main() -> i32 {
                    let p = Point { x: 1, y: 2 };
                    let _a = consume(p);
                    p.x
                }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Enums
    // ========================================================================

    mod enums {
        use super::*;

        #[test]
        fn enum_definition() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 { 0 }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn enum_variant_access() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let _c = Color::Red;
                    0
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn enum_match() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let c = Color::Green;
                    match c {
                        Color::Red => 1,
                        Color::Green => 2,
                        Color::Blue => 3,
                    }
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn enum_comparison_via_match() {
            // Enum equality comparison is done via match, not ==
            // (== is not yet implemented for enums)
            let src = r#"
                enum Color { Red, Green, Blue }
                fn eq(a: Color, b: Color) -> bool {
                    match a {
                        Color::Red => match b { Color::Red => true, _ => false },
                        Color::Green => match b { Color::Green => true, _ => false },
                        Color::Blue => match b { Color::Blue => true, _ => false },
                    }
                }
                fn main() -> i32 { if eq(Color::Red, Color::Red) { 1 } else { 0 } }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn unknown_enum_variant_rejected() {
            let src = r#"
                enum Color { Red, Green, Blue }
                fn main() -> i32 {
                    let _c = Color::Yellow;
                    0
                }
            "#;
            let result = compile_to_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Arrays
    // ========================================================================

    mod arrays {
        use super::*;

        #[test]
        fn array_literal() {
            let src = "fn main() -> i32 { let _arr: [i32; 3] = [1, 2, 3]; 0 }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn array_indexing() {
            let src = "fn main() -> i32 { let arr: [i32; 3] = [1, 2, 3]; arr[1] }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn array_element_assignment() {
            let src = r#"
                fn main() -> i32 {
                    let mut arr: [i32; 3] = [1, 2, 3];
                    arr[0] = 10;
                    arr[0]
                }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn array_wrong_length_rejected() {
            let src = "fn main() -> i32 { let _arr: [i32; 3] = [1, 2]; 0 }";
            let result = compile_to_air(src);
            assert!(result.is_err());
        }

        #[test]
        fn array_mixed_types_rejected() {
            let src = "fn main() -> i32 { let _arr: [i32; 2] = [1, true]; 0 }";
            let result = compile_to_air(src);
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Strings
    // ========================================================================

    mod strings {
        use super::*;

        #[test]
        fn string_literal() {
            let src = r#"fn main() -> i32 { let _s = "hello"; 0 }"#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn string_with_quote_escape() {
            // String escape sequences: \" is supported
            let src = r#"fn main() -> i32 { let _s = "hello\"world"; 0 }"#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn string_with_backslash_escape() {
            // String escape sequences: \\ is supported
            let src = r#"fn main() -> i32 { let _s = "hello\\world"; 0 }"#;
            assert!(compile_to_air(src).is_ok());
        }
    }

    // ========================================================================
    // Block Expressions
    // ========================================================================

    mod blocks {
        use super::*;

        #[test]
        fn block_returns_final_expression() {
            let src = "fn main() -> i32 { { 1; 2; 3 } }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn block_with_let_bindings() {
            let src = "fn main() -> i32 { { let x = 1; let y = 2; x + y } }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn nested_blocks() {
            let src = "fn main() -> i32 { { { { 42 } } } }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn block_scoping() {
            // Variable should not be accessible outside block
            let result = compile_to_air("fn main() -> i32 { { let x = 1; } x }");
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Never Type
    // ========================================================================

    mod never_type {
        use super::*;

        #[test]
        fn return_is_never() {
            let src = "fn main() -> i32 { return 42; }";
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn break_is_never() {
            let src = r#"
                fn main() -> i32 {
                    while true {
                        break;
                    }
                    0
                }
            "#;
            assert!(compile_to_cfg(src).is_ok());
        }

        #[test]
        fn never_in_if_branch() {
            let src = "fn main() -> i32 { if true { 1 } else { return 2; } }";
            assert!(compile_to_cfg(src).is_ok());
        }
    }

    // ========================================================================
    // Type Intrinsics
    // ========================================================================

    mod intrinsics {
        use super::*;

        #[test]
        fn size_of_intrinsic() {
            // @size_of returns i32
            let src = "fn main() -> i32 { @size_of(i32) }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn align_of_intrinsic() {
            // @align_of returns i32
            let src = "fn main() -> i32 { @align_of(i64) }";
            assert!(compile_to_air(src).is_ok());
        }
    }

    // ========================================================================
    // CFG Construction
    // ========================================================================

    mod cfg_construction {
        use super::*;

        #[test]
        fn cfg_has_correct_function_count() {
            let src = r#"
                fn foo() -> i32 { 1 }
                fn bar() -> i32 { 2 }
                fn main() -> i32 { foo() + bar() }
            "#;
            let state = compile_to_cfg(src).unwrap();
            assert_eq!(state.functions.len(), 3);
        }

        #[test]
        fn cfg_branches_for_if() {
            let src = "fn main() -> i32 { if true { 1 } else { 0 } }";
            let state = compile_to_cfg(src).unwrap();
            // CFG should have multiple blocks for branching
            let main_cfg = &state.functions[0].cfg;
            assert!(main_cfg.blocks().len() >= 3); // entry, then, else, merge
        }

        #[test]
        fn cfg_loop_for_while() {
            let src = r#"
                fn main() -> i32 {
                    let mut x = 0;
                    while x < 10 { x = x + 1; }
                    x
                }
            "#;
            let state = compile_to_cfg(src).unwrap();
            let main_cfg = &state.functions[0].cfg;
            assert!(main_cfg.blocks().len() >= 3); // header, body, exit
        }
    }

    // ========================================================================
    // Error Messages
    // ========================================================================

    mod error_messages {
        use super::*;

        #[test]
        fn type_mismatch_error_is_descriptive() {
            let result = compile_to_air("fn main() -> i32 { true }");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("type mismatch") || err.contains("expected"));
            assert!(err.contains("i32") || err.contains("bool"));
        }

        #[test]
        fn undefined_variable_error_is_descriptive() {
            let result = compile_to_air("fn main() -> i32 { unknown_var }");
            let err = result.unwrap_err().to_string();
            assert!(err.contains("undefined") || err.contains("unknown"));
        }

        #[test]
        fn missing_field_error_is_descriptive() {
            let src = r#"
                struct Point { x: i32, y: i32 }
                fn main() -> i32 {
                    let p = Point { x: 1 };
                    0
                }
            "#;
            let result = compile_to_air(src);
            let err = result.unwrap_err().to_string();
            assert!(err.contains("missing") || err.contains("field"));
        }
    }

    // ========================================================================
    // Warnings
    // ========================================================================

    mod warnings {
        use super::*;

        #[test]
        fn unused_variable_warning() {
            let result = compile_to_air("fn main() -> i32 { let x = 42; 0 }").unwrap();
            assert_eq!(result.warnings.len(), 1);
            assert!(result.warnings[0].to_string().contains("unused"));
        }

        #[test]
        fn underscore_prefix_suppresses_warning() {
            let result = compile_to_air("fn main() -> i32 { let _x = 42; 0 }").unwrap();
            assert_eq!(result.warnings.len(), 0);
        }

        #[test]
        fn used_variable_no_warning() {
            let result = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();
            assert_eq!(result.warnings.len(), 0);
        }
    }

    // ========================================================================
    // Edge Cases
    // ========================================================================

    mod edge_cases {
        use super::*;

        #[test]
        fn empty_function_body() {
            assert!(compile_to_air("fn main() -> () { }").is_ok());
        }

        #[test]
        fn deeply_nested_expressions() {
            let src = "fn main() -> i32 { ((((((1 + 2)))))) }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn many_parameters() {
            let src = r#"
                fn many(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
                    a + b + c + d + e + f
                }
                fn main() -> i32 { many(1, 2, 3, 4, 5, 6) }
            "#;
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn long_chain_of_operations() {
            let src = "fn main() -> i32 { 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 }";
            assert!(compile_to_air(src).is_ok());
        }

        #[test]
        fn multiple_functions_same_local_names() {
            let src = r#"
                fn foo() -> i32 { let x = 1; x }
                fn bar() -> i32 { let x = 2; x }
                fn main() -> i32 { foo() + bar() }
            "#;
            assert!(compile_to_air(src).is_ok());
        }
    }
}
