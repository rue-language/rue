//! Rue compiler driver.
//!
//! This crate orchestrates the compilation pipeline:
//! Source -> Lexer -> Parser -> AstGen -> Sema -> CodeGen -> ELF
//!
//! It re-exports types from the component crates for convenience.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counter for generating unique temp directory names.
static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

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
pub use rue_air::{Air, AnalyzedFunction, Sema, SemaOutput, StructDef, Type};
pub use rue_cfg::{Cfg, CfgBuilder, CfgOutput};
pub use rue_codegen::X86Mir;
pub use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, WarningKind};
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
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            target: Target::host(),
            linker: LinkerMode::Internal,
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
/// debugging and the `--dump-*` CLI flags.
pub struct CompileState {
    /// String interner used during compilation.
    pub interner: Interner,
    /// The untyped IR (RIR).
    pub rir: Rir,
    /// Analyzed functions with typed IR and control flow graphs.
    pub functions: Vec<FunctionWithCfg>,
    /// Struct definitions.
    pub struct_defs: Vec<StructDef>,
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

    // AST to RIR (untyped IR)
    let mut interner = Interner::new();
    let astgen = AstGen::new(&ast, &mut interner);
    let rir = astgen.generate();

    // Semantic analysis (RIR to AIR)
    let sema = Sema::new(&rir, &interner);
    let sema_output = sema.analyze_all()?;

    // Build CFGs from AIR (one per function), collecting warnings
    let mut functions = Vec::with_capacity(sema_output.functions.len());
    let mut warnings = sema_output.warnings;

    for func in sema_output.functions {
        let cfg_output =
            CfgBuilder::build(&func.air, func.num_locals, func.num_param_slots, &func.name);
        warnings.extend(cfg_output.warnings);
        functions.push(FunctionWithCfg {
            analyzed: func,
            cfg: cfg_output.cfg,
        });
    }

    Ok(CompileState {
        interner,
        rir,
        functions,
        struct_defs: sema_output.struct_defs,
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
        let machine_code = rue_codegen::x86_64::generate(&func.cfg, &state.struct_defs);

        // Build object file for this function
        let mut obj_builder =
            ObjectBuilder::new(options.target, &func.analyzed.name).code(machine_code.code);

        // Add relocations from codegen (convert emitted relocations to linker relocations).
        // We use PLT32 for call instructions since this is the standard relocation type
        // for function calls on x86-64. While we're doing static linking without a PLT,
        // PLT32 and PC32 are treated identically by the linker for direct calls.
        // Using PLT32 follows the convention established by GCC/Clang.
        for reloc in machine_code.relocations {
            obj_builder = obj_builder.relocation(CodeRelocation {
                offset: reloc.offset,
                symbol: reloc.symbol,
                rel_type: RelocationType::Plt32,
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
fn link_internal(
    state: &CompileState,
    options: &CompileOptions,
    object_files: &[Vec<u8>],
) -> CompileResult<CompileOutput> {
    let mut linker = Linker::new(options.target);

    // Add all object files to the linker
    for obj_bytes in object_files {
        let obj = ObjectFile::parse(obj_bytes)
            .map_err(|e| CompileError::without_span(ErrorKind::LinkError(e.to_string())))?;

        linker
            .add_object(obj)
            .map_err(|e| CompileError::without_span(ErrorKind::LinkError(e.to_string())))?;
    }

    // Mark _start as required so it gets pulled from the archive.
    // The entry point must be marked before adding the archive because
    // archive linking only includes objects that define needed symbols.
    linker.require_symbol("_start");

    // Add the runtime library
    let runtime = Archive::parse(RUNTIME_BYTES)
        .map_err(|e| CompileError::without_span(ErrorKind::LinkError(e.to_string())))?;
    linker
        .add_archive(runtime)
        .map_err(|e| CompileError::without_span(ErrorKind::LinkError(e.to_string())))?;

    // Link to executable
    // Use _start from the runtime as the entry point (it will call main)
    let elf = linker
        .link("_start")
        .map_err(|e| CompileError::without_span(ErrorKind::LinkError(e.to_string())))?;

    Ok(CompileOutput {
        elf,
        warnings: state.warnings.clone(),
    })
}

/// Link using an external system linker.
fn link_system(
    state: &CompileState,
    _options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
) -> CompileResult<CompileOutput> {
    // Create a temporary directory for object files.
    // Use pid + atomic counter to ensure uniqueness even in parallel test execution.
    let unique_id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!("rue-{}-{}", std::process::id(), unique_id));
    std::fs::create_dir_all(&temp_dir).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to create temp directory: {}",
            e
        )))
    })?;

    // Write object files to temp directory
    let mut obj_paths = Vec::new();
    for (i, obj_bytes) in object_files.iter().enumerate() {
        let path = temp_dir.join(format!("obj{}.o", i));
        let mut file = std::fs::File::create(&path).map_err(|e| {
            CompileError::without_span(ErrorKind::LinkError(format!(
                "failed to create temp object file: {}",
                e
            )))
        })?;
        file.write_all(obj_bytes).map_err(|e| {
            CompileError::without_span(ErrorKind::LinkError(format!(
                "failed to write temp object file: {}",
                e
            )))
        })?;
        obj_paths.push(path);
    }

    // Write the runtime archive to temp directory
    let runtime_path = temp_dir.join("librue_runtime.a");
    std::fs::write(&runtime_path, RUNTIME_BYTES).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to write runtime archive: {}",
            e
        )))
    })?;

    // Output path for linked executable
    let output_path = temp_dir.join("output");

    // Build the linker command
    let mut cmd = Command::new(linker_cmd);

    // Add common flags for static linking
    cmd.arg("-static");
    cmd.arg("-nostdlib");
    cmd.arg("-o");
    cmd.arg(&output_path);

    // Add object files
    for path in &obj_paths {
        cmd.arg(path);
    }

    // Add the runtime library
    cmd.arg(&runtime_path);

    // Run the linker
    let output = cmd.output().map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker '{}': {}",
            linker_cmd, e
        )))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(CompileError::without_span(ErrorKind::LinkError(format!(
            "linker '{}' failed: {}",
            linker_cmd, stderr
        ))));
    }

    // Read the resulting executable
    let elf = std::fs::read(&output_path).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to read linked executable: {}",
            e
        )))
    })?;

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&temp_dir);

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
        let machine_code = rue_codegen::aarch64::generate(&func.cfg, &state.struct_defs);

        let mut obj_builder =
            ObjectBuilder::new(options.target, &func.analyzed.name).code(machine_code.code);

        for reloc in machine_code.relocations {
            obj_builder = obj_builder.relocation(CodeRelocation {
                offset: reloc.offset,
                symbol: reloc.symbol,
                rel_type: RelocationType::Call26,
                addend: reloc.addend,
            });
        }

        object_files.push(obj_builder.build());
    }

    // For macOS/ARM64, we always use the system linker
    link_system_macos(state, options, &object_files)
}

/// Link using the macOS system linker (clang).
fn link_system_macos(
    state: &CompileState,
    _options: &CompileOptions,
    object_files: &[Vec<u8>],
) -> CompileResult<CompileOutput> {
    // Create a temporary directory for object files.
    let unique_id = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!("rue-{}-{}", std::process::id(), unique_id));
    std::fs::create_dir_all(&temp_dir).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to create temp directory: {}",
            e
        )))
    })?;

    // Write object files to temp directory
    let mut obj_paths = Vec::new();
    for (i, obj_bytes) in object_files.iter().enumerate() {
        let path = temp_dir.join(format!("obj{}.o", i));
        let mut file = std::fs::File::create(&path).map_err(|e| {
            CompileError::without_span(ErrorKind::LinkError(format!(
                "failed to create temp object file: {}",
                e
            )))
        })?;
        file.write_all(obj_bytes).map_err(|e| {
            CompileError::without_span(ErrorKind::LinkError(format!(
                "failed to write temp object file: {}",
                e
            )))
        })?;
        obj_paths.push(path);
    }

    // Write the runtime archive to temp directory
    let runtime_path = temp_dir.join("librue_runtime.a");
    std::fs::write(&runtime_path, RUNTIME_BYTES).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to write runtime archive: {}",
            e
        )))
    })?;

    // Output path for linked executable
    let output_path = temp_dir.join("output");

    // Use clang as the linker on macOS
    let mut cmd = Command::new("clang");

    // macOS-specific flags for static linking without libc
    cmd.arg("-nostdlib");
    cmd.arg("-arch").arg("arm64");
    cmd.arg("-e").arg("__main");
    cmd.arg("-o");
    cmd.arg(&output_path);

    // Add object files
    for path in &obj_paths {
        cmd.arg(path);
    }

    // Add the runtime library
    cmd.arg(&runtime_path);

    // Link with libSystem for syscalls on macOS
    cmd.arg("-lSystem");

    // Run the linker
    let output = cmd.output().map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker 'clang': {}",
            e
        )))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(CompileError::without_span(ErrorKind::LinkError(format!(
            "linker failed: {}",
            stderr
        ))));
    }

    // Read the resulting executable
    let elf = std::fs::read(&output_path).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to read linked executable: {}",
            e
        )))
    })?;

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(CompileOutput {
        elf,
        warnings: state.warnings.clone(),
    })
}

/// Generate X86Mir from CFG (for debugging/inspection).
///
/// This returns the MIR before register allocation, with virtual registers.
pub fn generate_mir(cfg: &Cfg, struct_defs: &[StructDef]) -> X86Mir {
    rue_codegen::x86_64::CfgLower::new(cfg, struct_defs).lower()
}

/// Generate X86Mir after register allocation (for debugging/inspection).
///
/// This returns the MIR after register allocation, with physical registers.
/// This is closer to the final assembly that will be emitted.
pub fn generate_allocated_mir(cfg: &Cfg, struct_defs: &[StructDef]) -> X86Mir {
    let num_locals = cfg.num_locals();
    let num_params = cfg.num_params();

    // Lower CFG to X86Mir with virtual registers
    let mir = rue_codegen::x86_64::CfgLower::new(cfg, struct_defs).lower();

    // Allocate physical registers
    let existing_slots = num_locals + num_params;
    let (mir, _num_spills, _used_callee_saved) =
        rue_codegen::x86_64::RegAlloc::new(mir, existing_slots).allocate_with_spills();

    mir
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
