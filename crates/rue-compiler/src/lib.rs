//! Rue compiler driver.
//!
//! This crate orchestrates the compilation pipeline:
//! Source -> Lexer -> Parser -> AstGen -> Sema -> CodeGen -> ELF
//!
//! It re-exports types from the component crates for convenience.

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
pub use rue_cfg::{Cfg, CfgBuilder, CfgOutput};
pub use rue_codegen::{RelocationKind, X86Mir, aarch64::Aarch64Mir};
pub use rue_error::{
    CompileError, CompileResult, CompileWarning, Diagnostic, ErrorKind, PreviewFeature,
    PreviewFeatures, WarningKind,
};
pub use rue_intern::{Interner, Symbol};
pub use rue_lexer::{Lexer, Token, TokenKind};
pub use rue_linker::{Archive, CodeRelocation, Linker, ObjectBuilder, ObjectFile, RelocationType};
pub use rue_parser::{Ast, Expr, Function, Parser};
pub use rue_rir::{AstGen, Rir, RirPrinter};
pub use rue_span::Span;
pub use rue_target::{Arch, Target};

/// Which linker to use for final linking.
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

/// Options for compilation.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// The target architecture and OS.
    pub target: Target,
    /// Which linker to use.
    pub linker: LinkerMode,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host(),
            linker: LinkerMode::Internal,
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
pub fn compile_frontend(source: &str) -> CompileResult<CompileState> {
    // Lexing
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize()?;

    // Parsing
    let mut parser = Parser::new(tokens);
    let ast = parser.parse()?;

    compile_frontend_from_ast(ast)
}

/// Compile from an already-parsed AST through all remaining frontend phases.
///
/// This runs: AST to RIR → semantic analysis → CFG construction.
/// Use this when you already have a parsed AST (e.g., for `--emit` modes that
/// need both AST output and later stage output without double-parsing).
pub fn compile_frontend_from_ast(ast: Ast) -> CompileResult<CompileState> {
    // AST to RIR (untyped IR)
    let mut interner = Interner::new();
    let astgen = AstGen::new(&ast, &mut interner);
    let rir = astgen.generate();

    // Semantic analysis (RIR to AIR)
    let sema = Sema::new(&rir, &mut interner);
    let sema_output = sema.analyze_all()?;

    // Build CFGs from AIR (one per function), collecting warnings
    let mut functions = Vec::with_capacity(sema_output.functions.len());
    let mut warnings = sema_output.warnings;

    for func in sema_output.functions {
        let cfg_output = CfgBuilder::build(
            &func.air,
            func.num_locals,
            func.num_param_slots,
            &func.name,
            &sema_output.struct_defs,
            &sema_output.array_types,
        );
        warnings.extend(cfg_output.warnings);
        functions.push(FunctionWithCfg {
            analyzed: func,
            cfg: cfg_output.cfg,
        });
    }

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
pub fn compile(source: &str) -> CompileResult<CompileOutput> {
    compile_with_options(source, &CompileOptions::default())
}

/// Compile source code to an ELF binary with the given options.
///
/// This allows specifying the target architecture and other compilation options.
pub fn compile_with_options(
    source: &str,
    options: &CompileOptions,
) -> CompileResult<CompileOutput> {
    let state = compile_frontend(source)?;

    // Check for main function
    let _main_fn = state
        .functions
        .iter()
        .find(|f| f.analyzed.name == "main")
        .ok_or_else(|| CompileError::without_span(ErrorKind::NoMainFunction))?;

    // Dispatch to the appropriate backend based on target architecture
    match options.target.arch() {
        Arch::X86_64 => compile_x86_64(&state, options),
        Arch::Aarch64 => compile_aarch64(&state, options),
    }
}

/// Compile for x86-64 target.
fn compile_x86_64(state: &CompileState, options: &CompileOptions) -> CompileResult<CompileOutput> {
    // Generate machine code for all functions
    let mut object_files: Vec<Vec<u8>> = Vec::new();

    for func in &state.functions {
        let machine_code = rue_codegen::x86_64::generate(
            &func.cfg,
            &state.struct_defs,
            &state.array_types,
            &state.strings,
        )?;

        // Build object file for this function
        let mut obj_builder = ObjectBuilder::new(options.target, &func.analyzed.name)
            .code(machine_code.code)
            .strings(machine_code.strings);

        // Add relocations from codegen (convert RelocationKind to RelocationType).
        for reloc in machine_code.relocations {
            let rel_type = match reloc.kind {
                RelocationKind::X86Pc32 => RelocationType::Pc32,
                RelocationKind::X86Plt32 => RelocationType::Plt32,
                // These shouldn't appear for x86_64, but handle gracefully
                _ => RelocationType::Pc32,
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
) -> CompileResult<CompileOutput> {
    // For macOS targets, the internal linker doesn't support Mach-O,
    // so we delegate to the system linker (clang).
    if options.target.is_macho() {
        return link_system(state, options, object_files, "clang");
    }

    let mut linker = Linker::new(options.target);

    // Add all object files to the linker
    for obj_bytes in object_files {
        let obj = ObjectFile::parse(obj_bytes).map_err(link_error)?;
        linker.add_object(obj).map_err(link_error)?;
    }

    // Mark _start as required so it gets pulled from the archive.
    // The entry point must be marked before adding the archive because
    // archive linking only includes objects that define needed symbols.
    linker.require_symbol("_start");

    // Add the runtime library
    let runtime = Archive::parse(RUNTIME_BYTES).map_err(link_error)?;
    linker.add_archive(runtime).map_err(link_error)?;

    // Link to executable
    // Use _start from the runtime as the entry point (it will call main)
    let elf = linker.link("_start").map_err(link_error)?;

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
) -> CompileResult<CompileOutput> {
    // Set up temporary directory with object files and runtime
    let mut temp_dir = TempLinkDir::new()?;
    temp_dir.write_object_files(object_files)?;
    temp_dir.write_runtime(RUNTIME_BYTES)?;

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
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker '{}': {}",
            linker_cmd, e
        )))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // temp_dir is dropped here, cleaning up automatically
        return Err(CompileError::without_span(ErrorKind::LinkError(format!(
            "linker '{}' failed: {}",
            linker_cmd, stderr
        ))));
    }

    // Read the resulting executable
    let elf = temp_dir.read_output()?;

    // temp_dir is dropped here, cleaning up automatically
    Ok(CompileOutput {
        elf,
        warnings: state.warnings.clone(),
    })
}

/// Compile for AArch64 target.
fn compile_aarch64(state: &CompileState, options: &CompileOptions) -> CompileResult<CompileOutput> {
    // Generate machine code for all functions using the aarch64 backend
    let mut object_files: Vec<Vec<u8>> = Vec::new();

    for func in &state.functions {
        let machine_code = rue_codegen::aarch64::generate(
            &func.cfg,
            &state.struct_defs,
            &state.array_types,
            &state.strings,
        )?;

        let mut obj_builder = ObjectBuilder::new(options.target, &func.analyzed.name)
            .code(machine_code.code)
            .strings(machine_code.strings);

        // Add relocations from codegen (convert RelocationKind to RelocationType).
        for reloc in machine_code.relocations {
            let rel_type = match reloc.kind {
                RelocationKind::Aarch64AdrpPage21 => RelocationType::AdrpPage21,
                RelocationKind::Aarch64AddLo12 => RelocationType::AddLo12,
                RelocationKind::Aarch64Call26 => RelocationType::Call26,
                // These shouldn't appear for AArch64, but handle gracefully
                _ => RelocationType::Call26,
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

/// Generate MIR from CFG for the given target (for debugging/inspection).
///
/// This returns the MIR before register allocation, with virtual registers.
pub fn generate_mir(
    cfg: &Cfg,
    struct_defs: &[StructDef],
    array_types: &[ArrayTypeDef],
    strings: &[String],
    target: Target,
) -> Mir {
    match target.arch() {
        Arch::X86_64 => {
            let mir =
                rue_codegen::x86_64::CfgLower::new(cfg, struct_defs, array_types, strings).lower();
            Mir::X86_64(mir)
        }
        Arch::Aarch64 => {
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, struct_defs, array_types, strings).lower();
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
    target: Target,
) -> CompileResult<Mir> {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();
    let existing_slots = num_locals + num_params;

    match target.arch() {
        Arch::X86_64 => {
            // Lower CFG to X86Mir with virtual registers
            let mir =
                rue_codegen::x86_64::CfgLower::new(cfg, struct_defs, array_types, strings).lower();

            // Allocate physical registers
            let (mir, _num_spills, _used_callee_saved) =
                rue_codegen::x86_64::RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

            Ok(Mir::X86_64(mir))
        }
        Arch::Aarch64 => {
            // Lower CFG to Aarch64Mir with virtual registers
            let mir =
                rue_codegen::aarch64::CfgLower::new(cfg, struct_defs, array_types, strings).lower();

            // Allocate physical registers
            let (mir, _num_spills, _used_callee_saved) =
                rue_codegen::aarch64::RegAlloc::new(mir, existing_slots).allocate_with_spills()?;

            Ok(Mir::Aarch64(mir))
        }
    }
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
}
