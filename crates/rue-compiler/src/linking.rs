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
///
/// NOTE: there is exactly one embedded archive and it is built for the
/// *host* this compiler binary was built on. Linking an executable for any
/// other target is therefore refused (see [`runtime_for_target`] and
/// RUE-36 / ADR-0034) until per-target runtime archives are embedded.
static RUNTIME_BYTES: &[u8] = include_bytes!("librue_runtime.a");

/// Return the embedded rue-runtime archive for `target`, or a clear error
/// if this compiler build doesn't carry a runtime for that target.
///
/// The build system embeds only the host-configuration staticlib, so any
/// cross-target link would silently pull host machine code into the foreign
/// binary (the original RUE-36 failure mode: an "AArch64" ELF whose entry
/// point was x86-64 code). Refusing here turns that into an honest,
/// actionable error while leaving cross-target code generation (`--emit
/// asm`, `--emit mir`, ...) fully usable. ADR-0034 describes the full fix
/// (per-target runtime archives selected at link time).
pub(crate) fn runtime_for_target(target: Target) -> CompileResult<&'static [u8]> {
    runtime_for_target_with_host(target, Target::host(), Target::host_description())
}

pub(crate) fn runtime_for_target_with_host(
    target: Target,
    host: Option<Target>,
    host_description: &str,
) -> CompileResult<&'static [u8]> {
    match host {
        Some(host) if target == host => Ok(RUNTIME_BYTES),
        Some(host) => Err(CompileError::without_span(ErrorKind::LinkError(format!(
            "cannot link an executable for {target}: this rue compiler was built for {host} \
             and only embeds the {host} runtime library, so the result would not run on \
             {target} (RUE-36). Cross-target code generation still works: use \
             `--emit asm` to inspect {target} assembly.",
        )))),
        None => Err(CompileError::without_span(ErrorKind::LinkError(format!(
            "cannot link an executable for {target}: this rue compiler was built on {} \
             and does not have a supported host runtime to embed (RUE-36). Cross-target \
             code generation still works: use `--emit asm` to inspect {target} assembly.",
            host_description
        )))),
    }
}

/// Validate that the embedded runtime archive is well-formed.
///
/// This is called by tests to ensure the runtime is valid at build time.
/// Returns an error message if validation fails.
#[cfg(test)]
pub(crate) fn validate_runtime() -> Result<(), String> {
    parse_runtime_archive(RUNTIME_BYTES).map(|_| ())
}

pub(crate) fn parse_runtime_archive(runtime_bytes: &[u8]) -> Result<Archive, String> {
    let archive = Archive::parse(runtime_bytes)
        .map_err(|e| format!("embedded rue-runtime archive is invalid: {}", e))?;

    if archive.is_empty() {
        return Err("embedded rue-runtime archive contains no object files".to_string());
    }

    Ok(archive)
}

pub(crate) fn link_internal_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "internal").entered();

    // Refuse cross-target links up front: only the host runtime is embedded
    // (RUE-36), and linking without a matching runtime is impossible.
    let runtime_bytes = runtime_for_target(options.target).map_err(CompileErrors::from)?;

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

    // Determine the entry point symbol based on target.
    // ELF: _start (runtime's entry point that calls main)
    // Mach-O: __main (runtime's entry point that calls _main)
    let entry_point = if options.target.is_macho() {
        "__main"
    } else {
        "_start"
    };

    // Mark the entry point as required so it gets pulled from the archive.
    // The entry point must be marked before adding the archive because
    // archive linking only includes objects that define needed symbols.
    linker.require_symbol(entry_point);

    // Add the runtime library
    let runtime = parse_runtime_archive(runtime_bytes)
        .map_err(link_error)
        .map_err(CompileErrors::from)?;
    linker
        .add_archive(runtime)
        .map_err(link_error)
        .map_err(CompileErrors::from)?;

    // Link to executable
    let executable = linker
        .link(entry_point)
        .map_err(link_error)
        .map_err(CompileErrors::from)?;
    info!(
        object_count = object_files.len(),
        output_bytes = executable.len(),
        "linking complete"
    );

    Ok(CompileOutput {
        elf: executable,
        warnings: warnings.to_vec(),
        source_stats: SourceStats::default(),
        work: PipelineWork::default(),
    })
}

/// Link using an external system linker.
pub(crate) fn link_system_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "system", command = linker_cmd).entered();

    // Refuse cross-target links up front: only the host runtime is embedded
    // (RUE-36); a system linker would happily mix architectures (or fail
    // with an opaque message), so catch it here with a clear error.
    let runtime_bytes = runtime_for_target(options.target).map_err(CompileErrors::from)?;

    // Set up temporary directory with object files and runtime
    let mut temp_dir = TempLinkDir::new().map_err(CompileErrors::from)?;
    temp_dir
        .write_object_files(object_files)
        .map_err(CompileErrors::from)?;
    temp_dir
        .write_runtime(runtime_bytes)
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
        warnings: warnings.to_vec(),
        source_stats: SourceStats::default(),
        work: PipelineWork::default(),
    })
}
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{info, info_span};

use crate::*;
