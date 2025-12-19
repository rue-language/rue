//! Rue compiler driver.
//!
//! This crate orchestrates the compilation pipeline:
//! Source → Lexer → Parser → AstGen → Sema → CodeGen → ELF
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
pub use rue_air::{Air, AnalyzedFunction, Sema, StructDef, Type};
pub use rue_codegen::{CodeGen, X86Mir};
pub use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, WarningKind};
pub use rue_intern::{Interner, Symbol};
pub use rue_lexer::{Lexer, Token, TokenKind};
pub use rue_linker::{Archive, CodeRelocation, Linker, ObjectBuilder, ObjectFile, RelocationType};
pub use rue_parser::{Ast, Expr, Function, Parser};
pub use rue_rir::{AstGen, Rir, RirPrinter};
pub use rue_span::Span;
pub use rue_target::{Arch, Target};

/// Intermediate compilation state, allowing inspection at each stage.
pub struct CompileState {
    pub interner: Interner,
    pub rir: Rir,
    pub functions: Vec<AnalyzedFunction>,
    pub struct_defs: Vec<StructDef>,
    /// Warnings generated during compilation.
    pub warnings: Vec<CompileWarning>,
}

/// Compile source code through all phases up to (but not including) codegen.
///
/// Returns the compile state which can be inspected for debugging.
pub fn compile_to_air(source: &str) -> CompileResult<CompileState> {
    // Phase 1: Lexing
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize()?;

    // Phase 2: Parsing
    let mut parser = Parser::new(tokens);
    let ast = parser.parse()?;

    // Phase 3: AST to RIR (untyped IR)
    let mut interner = Interner::new();
    let astgen = AstGen::new(&ast, &mut interner);
    let rir = astgen.generate();

    // Phase 4: Semantic analysis (RIR to AIR)
    let mut sema = Sema::new(&rir, &interner);
    let functions = sema.analyze_all()?;
    let struct_defs = sema.struct_defs().to_vec();
    let warnings = sema.take_warnings();

    Ok(CompileState {
        interner,
        rir,
        functions,
        struct_defs,
        warnings,
    })
}

/// Output from successful compilation.
pub struct CompileOutput {
    /// The compiled ELF binary.
    pub elf: Vec<u8>,
    /// Warnings generated during compilation.
    pub warnings: Vec<CompileWarning>,
}

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
    let state = compile_to_air(source)?;

    // Check for main function
    let _main_fn = state
        .functions
        .iter()
        .find(|f| f.name == "main")
        .ok_or_else(|| CompileError::without_span(ErrorKind::NoMainFunction))?;

    // Dispatch to the appropriate backend based on target architecture
    match options.target.arch() {
        Arch::X86_64 => compile_x86_64(&state, options),
        Arch::Aarch64 => compile_aarch64(&state, options),
    }
}

/// Compile for x86-64 target.
fn compile_x86_64(state: &CompileState, options: &CompileOptions) -> CompileResult<CompileOutput> {
    // Phase 5: Code generation (AIR to machine code) for ALL functions
    // Build object files for all functions
    let mut object_files: Vec<Vec<u8>> = Vec::new();

    for func in &state.functions {
        let codegen = CodeGen::new(
            &func.air,
            &state.struct_defs,
            func.num_locals,
            func.num_param_slots,
            &func.name,
        );
        let machine_code = codegen.generate();

        // Build object file for this function
        let mut obj_builder =
            ObjectBuilder::new(options.target, &func.name).code(machine_code.code);

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

    // Phase 6 & 7: Link to executable
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
    // We support common linkers: clang, gcc, ld
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
        // Clean up temp directory before returning error
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
        let machine_code = rue_codegen::aarch64::generate(
            &func.air,
            &state.struct_defs,
            func.num_locals,
            func.num_param_slots,
            &func.name,
        );

        // Build object file for this function
        // Use the appropriate relocation type for ARM64
        let mut obj_builder =
            ObjectBuilder::new(options.target, &func.name).code(machine_code.code);

        for reloc in machine_code.relocations {
            obj_builder = obj_builder.relocation(CodeRelocation {
                offset: reloc.offset,
                symbol: reloc.symbol,
                rel_type: RelocationType::Call26, // ARM64 branch instruction
                addend: reloc.addend,
            });
        }

        object_files.push(obj_builder.build());
    }

    // For macOS/ARM64, we always use the system linker since we don't have
    // a Mach-O linker implementation
    link_system_macos(state, options, &object_files)
}

/// Link using the macOS system linker (clang).
fn link_system_macos(
    state: &CompileState,
    _options: &CompileOptions,
    object_files: &[Vec<u8>],
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

    // Use clang as the linker on macOS
    let mut cmd = Command::new("clang");

    // macOS-specific flags for static linking without libc
    cmd.arg("-nostdlib");
    cmd.arg("-arch").arg("arm64");
    cmd.arg("-e").arg("__main"); // Entry point (macOS uses underscore prefix)
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
        // Clean up temp directory before returning error
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(CompileError::without_span(ErrorKind::LinkError(format!(
            "linker failed: {}",
            stderr
        ))));
    }

    // Read the resulting executable
    let executable = std::fs::read(&output_path).map_err(|e| {
        CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to read linked executable: {}",
            e
        )))
    })?;

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&temp_dir);

    Ok(CompileOutput {
        elf: executable, // It's actually Mach-O, but we reuse the field name
        warnings: state.warnings.clone(),
    })
}

/// Generate X86Mir from AIR (for debugging/inspection).
pub fn generate_mir(
    air: &Air,
    struct_defs: &[StructDef],
    num_locals: u32,
    num_params: u32,
    fn_name: &str,
) -> X86Mir {
    rue_codegen::x86_64::Lower::new(air, struct_defs, num_locals, num_params, fn_name).lower()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_runtime_is_valid() {
        // Validate that the embedded runtime archive parses correctly.
        // This catches issues with the embedded archive at test time.
        validate_runtime().expect("embedded runtime should be valid");
    }

    #[test]
    fn test_compile_simple() {
        let output = compile("fn main() -> i32 { 42 }").unwrap();
        // Should produce a valid executable (ELF on Linux, Mach-O on macOS)
        let magic = &output.elf[0..4];
        let is_elf = magic == &[0x7F, b'E', b'L', b'F'];
        let is_macho = magic == &0xFEEDFACF_u32.to_le_bytes(); // Mach-O 64-bit
        assert!(is_elf || is_macho, "should produce valid ELF or Mach-O binary");
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
}
