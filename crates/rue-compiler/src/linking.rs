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

/// A temporary directory for linking that automatically cleans up on drop.
///
/// The `TempDir` is the ownership token for the workspace: it is created
/// atomically with a random name and only that directory is removed on drop.
struct TempLinkDir {
    /// Owner-only directory created by this invocation.
    directory: tempfile::TempDir,
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
    /// The random name is deliberately longer than `tempfile`'s default, and
    /// creation uses `mkdir` rather than accepting an existing path.
    fn new() -> CompileResult<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("rue-link-").rand_bytes(16);
        #[cfg(unix)]
        builder.permissions(std::fs::Permissions::from_mode(0o700));
        Self::create_in(&builder, &std::env::temp_dir())
    }

    fn create_in(builder: &tempfile::Builder<'_, '_>, parent: &Path) -> CompileResult<Self> {
        let directory = builder
            .tempdir_in(parent)
            .map_err(|e| io_link_error("failed to create temp directory", e))?;

        let runtime_path = directory.path().join("librue_runtime.a");
        let output_path = directory.path().join("output");

        Ok(Self {
            directory,
            obj_paths: Vec::new(),
            runtime_path,
            output_path,
        })
    }

    fn create_leaf(path: &Path, context: &str) -> CompileResult<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        options.open(path).map_err(|e| io_link_error(context, e))
    }

    /// Write object files to the temporary directory.
    ///
    /// Each object file is written to a file named `obj{N}.o` where N is
    /// the index. The paths are stored in `self.obj_paths`.
    fn write_object_files(&mut self, object_files: &[Vec<u8>]) -> CompileResult<()> {
        for (i, obj_bytes) in object_files.iter().enumerate() {
            let obj_path = self.directory.path().join(format!("obj{}.o", i));
            let mut file = Self::create_leaf(&obj_path, "failed to create temp object file")?;
            file.write_all(obj_bytes)
                .map_err(|e| io_link_error("failed to write temp object file", e))?;
            self.obj_paths.push(obj_path);
        }
        Ok(())
    }

    /// Write the runtime archive to the temporary directory.
    fn write_runtime(&self, runtime_bytes: &[u8]) -> CompileResult<()> {
        let mut file = Self::create_leaf(&self.runtime_path, "failed to create runtime archive")?;
        file.write_all(runtime_bytes)
            .map_err(|e| io_link_error("failed to write runtime archive", e))
    }

    /// Reserve the output leaf so a pre-existing file of any kind is rejected.
    fn create_output(&self) -> CompileResult<()> {
        Self::create_leaf(&self.output_path, "failed to create linker output")?;
        Ok(())
    }

    /// Read the linked executable from the output path.
    fn read_output(&self) -> CompileResult<Vec<u8>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(&self.output_path)
            .map_err(|e| io_link_error("failed to open linked executable", e))?;
        if !file
            .metadata()
            .map_err(|e| io_link_error("failed to inspect linked executable", e))?
            .is_file()
        {
            return Err(CompileError::without_span(ErrorKind::LinkError(
                "linked executable is not a regular file".to_string(),
            )));
        }
        let mut output = Vec::new();
        file.read_to_end(&mut output)
            .map_err(|e| io_link_error("failed to read linked executable", e))?;
        Ok(output)
    }
}

/// The three target-specific rue-runtime staticlibs embedded at compile time.
static RUNTIME_X86_64_LINUX: &[u8] = include_bytes!("librue_runtime-x86_64-unknown-linux-gnu.a");
static RUNTIME_AARCH64_LINUX: &[u8] = include_bytes!("librue_runtime-aarch64-unknown-linux-gnu.a");
static RUNTIME_AARCH64_MACOS: &[u8] = include_bytes!("librue_runtime-aarch64-apple-darwin.a");
static RUNTIME_X86_64_LINUX_VALIDATION: std::sync::OnceLock<Result<(), String>> =
    std::sync::OnceLock::new();
static RUNTIME_AARCH64_LINUX_VALIDATION: std::sync::OnceLock<Result<(), String>> =
    std::sync::OnceLock::new();
static RUNTIME_AARCH64_MACOS_VALIDATION: std::sync::OnceLock<Result<(), String>> =
    std::sync::OnceLock::new();

/// Return the embedded rue-runtime archive matching `target`.
pub(crate) fn runtime_for_target(target: Target) -> &'static [u8] {
    match target {
        Target::X86_64Linux => RUNTIME_X86_64_LINUX,
        Target::Aarch64Linux => RUNTIME_AARCH64_LINUX,
        Target::Aarch64Macos => RUNTIME_AARCH64_MACOS,
    }
}

/// Validate the embedded runtime selected for `target`.
#[cfg(test)]
pub(crate) fn validate_runtime(target: Target) -> Result<(), String> {
    validate_runtime_archive(runtime_for_target(target), target).map(|_| ())
}

pub(crate) fn parse_runtime_archive(runtime_bytes: &[u8]) -> Result<Archive, String> {
    let archive = Archive::parse_strict_objects(runtime_bytes)
        .map_err(|e| format!("embedded rue-runtime archive is invalid: {}", e))?;

    if archive.is_empty() {
        return Err("embedded rue-runtime archive contains no object files".to_string());
    }

    Ok(archive)
}

/// Read and parse a user-supplied static archive passed with `--link-archive`
/// (ADR-0064 C FFI). Its object members satisfy undefined `extern "C"` symbols.
fn read_user_archive(path: &Path) -> Result<Archive, ErrorKind> {
    let bytes = std::fs::read(path).map_err(|e| {
        ErrorKind::LinkError(format!(
            "failed to read link archive `{}`: {e}",
            path.display()
        ))
    })?;
    Archive::parse_strict_objects(&bytes).map_err(|e| {
        ErrorKind::LinkError(format!(
            "failed to parse link archive `{}`: {e}",
            path.display()
        ))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeSymbolKind {
    Function,
    Data,
    Other,
}

#[derive(Debug, Clone)]
struct RuntimeDefinedSymbol {
    name: String,
    kind: RuntimeSymbolKind,
    size: u64,
    section_allocated: bool,
    section_writable: bool,
    section_executable: bool,
    bytes_from_symbol: u64,
    first_byte: Option<u8>,
}

#[derive(Debug, Default)]
struct RuntimeArchiveInventory {
    object_targets: Vec<rue_runtime_abi::RuntimeTarget>,
    symbols: Vec<RuntimeDefinedSymbol>,
}

fn runtime_target(target: Target) -> rue_runtime_abi::RuntimeTarget {
    match target {
        Target::X86_64Linux => rue_runtime_abi::RuntimeTarget::X86_64Linux,
        Target::Aarch64Linux => rue_runtime_abi::RuntimeTarget::Aarch64Linux,
        Target::Aarch64Macos => rue_runtime_abi::RuntimeTarget::Aarch64Macos,
    }
}

fn parsed_object_target(
    object: &ObjectFile,
    object_index: usize,
) -> Result<rue_runtime_abi::RuntimeTarget, String> {
    use rue_linker::{ElfMachine, ObjectFormat};
    use rue_runtime_abi::RuntimeTarget;

    match (object.format, object.machine) {
        (ObjectFormat::Elf, ElfMachine::X86_64) => Ok(RuntimeTarget::X86_64Linux),
        (ObjectFormat::Elf, ElfMachine::Aarch64) => Ok(RuntimeTarget::Aarch64Linux),
        (ObjectFormat::MachO, ElfMachine::Aarch64) => Ok(RuntimeTarget::Aarch64Macos),
        (ObjectFormat::MachO, ElfMachine::X86_64) => Err(format!(
            "embedded rue-runtime archive object {object_index} has unsupported Mach-O/x86-64 target"
        )),
    }
}

fn runtime_archive_inventory(archive: &Archive) -> Result<RuntimeArchiveInventory, String> {
    use rue_linker::{ObjectFormat, SectionFlags, SymbolBinding, SymbolType};

    let mut inventory = RuntimeArchiveInventory::default();
    for (object_index, object) in archive.objects.iter().enumerate() {
        let object_target = parsed_object_target(object, object_index)?;
        inventory.object_targets.push(object_target);

        for symbol in &object.symbols {
            if symbol.section_index.is_none()
                || matches!(symbol.binding, SymbolBinding::Local)
                || symbol.name.is_empty()
            {
                continue;
            }

            let section_index = symbol.section_index.expect("checked above");
            let section = object.sections.get(section_index).ok_or_else(|| {
                format!(
                    "embedded rue-runtime archive symbol `{}` has invalid section index {}",
                    symbol.name, section_index
                )
            })?;
            let bytes_from_symbol = u64::try_from(section.data.len())
                .unwrap_or(u64::MAX)
                .saturating_sub(symbol.value);
            let format_size = match object.format {
                ObjectFormat::Elf => symbol.size,
                ObjectFormat::MachO => {
                    let next_symbol = object
                        .symbols
                        .iter()
                        .filter(|candidate| {
                            candidate.section_index == symbol.section_index
                                && candidate.value > symbol.value
                        })
                        .map(|candidate| candidate.value)
                        .min()
                        .unwrap_or(section.size);
                    next_symbol.saturating_sub(symbol.value)
                }
            };
            inventory.symbols.push(RuntimeDefinedSymbol {
                name: symbol.name.clone(),
                kind: match symbol.sym_type {
                    SymbolType::Func => RuntimeSymbolKind::Function,
                    SymbolType::Object => RuntimeSymbolKind::Data,
                    SymbolType::None | SymbolType::Section | SymbolType::File => {
                        RuntimeSymbolKind::Other
                    }
                },
                // Mach-O's nlist entries do not carry symbol sizes. Derive the
                // symbol's extent from the next symbol in the same section (or
                // the section end) so retained ABI data can still be checked
                // exactly.
                size: format_size,
                section_allocated: section.flags.contains(SectionFlags::ALLOC),
                section_writable: section.flags.contains(SectionFlags::WRITE),
                section_executable: section.flags.contains(SectionFlags::EXEC),
                bytes_from_symbol,
                first_byte: usize::try_from(symbol.value)
                    .ok()
                    .and_then(|offset| section.data.get(offset))
                    .copied(),
            });
        }
    }
    inventory
        .symbols
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(inventory)
}

fn validate_runtime_inventory(
    inventory: &RuntimeArchiveInventory,
    target: rue_runtime_abi::RuntimeTarget,
) -> Result<(), String> {
    use rue_runtime_abi::{
        RUNTIME_ABI_VERSION_SYMBOL, ReservedExportId, ReservedExportKind, RuntimeHelperId,
        classify_export,
    };

    let mut errors = Vec::new();
    for (object_index, object_target) in inventory.object_targets.iter().copied().enumerate() {
        if object_target != target {
            errors.push(format!(
                "object {object_index} targets {object_target:?}, expected {target:?}"
            ));
        }
    }

    let definitions = |name: &str| {
        inventory
            .symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .collect::<Vec<_>>()
    };

    for id in RuntimeHelperId::ALL {
        let helper = id.helper();
        let found = definitions(helper.symbol);
        if helper.availability.contains(target) {
            match found.len() {
                0 => errors.push(format!("missing runtime helper `{}`", helper.symbol)),
                1 => {
                    if found[0].kind != RuntimeSymbolKind::Function {
                        errors.push(format!(
                            "runtime helper `{}` is not callable code",
                            helper.symbol
                        ));
                    }
                }
                count => errors.push(format!(
                    "runtime helper `{}` is defined {count} times",
                    helper.symbol
                )),
            }
        } else if !found.is_empty() {
            errors.push(format!(
                "runtime helper `{}` is not available for {target:?}",
                helper.symbol
            ));
        }
    }

    for id in ReservedExportId::ALL {
        let export = id.export();
        let found = definitions(export.symbol);
        if export.availability.contains(target) {
            match found.len() {
                0 => errors.push(format!(
                    "missing reserved runtime export `{}`",
                    export.symbol
                )),
                1 => match export.kind {
                    ReservedExportKind::Function(_) => {
                        if found[0].kind != RuntimeSymbolKind::Function {
                            errors.push(format!(
                                "reserved runtime export `{}` is not callable code",
                                export.symbol
                            ));
                        }
                    }
                    ReservedExportKind::ReadOnlyData { size } => {
                        let marker = found[0];
                        if marker.kind != RuntimeSymbolKind::Data {
                            errors.push(format!(
                                "runtime ABI marker `{}` is not an object symbol",
                                export.symbol
                            ));
                        }
                        if marker.size != u64::from(size) {
                            errors.push(format!(
                                "runtime ABI marker `{}` has size {}, expected {} byte",
                                export.symbol, marker.size, size
                            ));
                        }
                        if !marker.section_allocated
                            || marker.section_writable
                            || marker.section_executable
                        {
                            errors.push(format!(
                                "runtime ABI marker `{}` is not retained read-only data",
                                export.symbol
                            ));
                        }
                        if marker.bytes_from_symbol < u64::from(size) {
                            errors.push(format!(
                                "runtime ABI marker `{}` has no accessible marker byte",
                                export.symbol
                            ));
                        }
                        if marker.first_byte != Some(0) {
                            errors.push(format!(
                                "runtime ABI marker `{}` does not contain the required zero byte",
                                export.symbol
                            ));
                        }
                    }
                },
                count => errors.push(format!(
                    "reserved runtime export `{}` is defined {count} times",
                    export.symbol
                )),
            }
        } else if !found.is_empty() {
            errors.push(format!(
                "reserved runtime export `{}` is not available for {target:?}",
                export.symbol
            ));
        }
    }

    for symbol in &inventory.symbols {
        let abi_owned_name = symbol.name.starts_with("__rue_");
        if abi_owned_name && classify_export(&symbol.name).is_none() {
            let message = if symbol.name.starts_with("__rue_runtime_abi_v") {
                format!(
                    "stale runtime ABI marker `{}`; expected `{RUNTIME_ABI_VERSION_SYMBOL}`",
                    symbol.name
                )
            } else {
                format!("unknown runtime ABI export `{}`", symbol.name)
            };
            errors.push(message);
        }
    }

    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "embedded rue-runtime archive does not match the typed ABI manifest:\n  - {}",
            errors.join("\n  - ")
        ))
    }
}

fn validate_runtime_archive(runtime_bytes: &[u8], target: Target) -> Result<Archive, String> {
    let archive = parse_runtime_archive(runtime_bytes)?;
    let validate = || {
        let inventory = runtime_archive_inventory(&archive)?;
        validate_runtime_inventory(&inventory, runtime_target(target))
    };
    let embedded_runtime = runtime_for_target(target);
    let embedded_validation = match target {
        Target::X86_64Linux => &RUNTIME_X86_64_LINUX_VALIDATION,
        Target::Aarch64Linux => &RUNTIME_AARCH64_LINUX_VALIDATION,
        Target::Aarch64Macos => &RUNTIME_AARCH64_MACOS_VALIDATION,
    };
    if std::ptr::eq(runtime_bytes, embedded_runtime) {
        embedded_validation.get_or_init(validate).clone()?;
    } else {
        validate()?;
    }
    Ok(archive)
}

pub(crate) fn link_internal_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "internal", phase = "linking").entered();

    let runtime_bytes = runtime_for_target(options.target);

    let mut linker = Linker::new(options.target);

    // Add all object files to the linker
    {
        let _span = info_span!("link_parse_objects", object_count = object_files.len()).entered();
        for obj_bytes in object_files {
            let obj = ObjectFile::parse(obj_bytes)
                .map_err(link_error)
                .map_err(CompileErrors::from)?;
            linker
                .add_object(obj)
                .map_err(link_error)
                .map_err(CompileErrors::from)?;
        }
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

    // Add user-supplied static archives (`--link-archive`, ADR-0064 C FFI)
    // before the runtime so a foreign member that itself depends on a runtime
    // symbol can still be satisfied. Each archive resolves the undefined
    // `extern "C"` symbols referenced by the compiled objects.
    {
        let _span = info_span!("link_archive_resolve").entered();
        for archive_path in &options.link_archives {
            let archive = read_user_archive(archive_path)
                .map_err(CompileError::without_span)
                .map_err(CompileErrors::from)?;
            linker
                .add_archive(archive)
                .map_err(link_error)
                .map_err(CompileErrors::from)?;
        }

        // Add the runtime library
        let runtime = validate_runtime_archive(runtime_bytes, options.target)
            .map_err(link_error)
            .map_err(CompileErrors::from)?;
        linker
            .add_archive(runtime)
            .map_err(link_error)
            .map_err(CompileErrors::from)?;
    }

    // Link to executable. An undefined symbol at this point is an unresolved
    // `extern "C"` import (or a runtime gap); name the symbol and the archives
    // that were searched (ADR-0064 C FFI).
    let executable = linker
        .link(entry_point)
        .map_err(|err| match &err {
            rue_linker::LinkError::UndefinedSymbol(symbol) => {
                let mut searched = vec!["the bundled rue-runtime archive".to_string()];
                for archive_path in &options.link_archives {
                    searched.push(format!("`{}`", archive_path.display()));
                }
                CompileError::without_span(ErrorKind::LinkError(format!(
                    "undefined symbol `{symbol}`: no supplied archive defines it \
                     (searched {})",
                    searched.join(", ")
                )))
            }
            _ => link_error(err),
        })
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
        query_runtime: crate::unstable::QueryRuntimeMetrics::default(),
        semantic_reachability: crate::unstable::SemanticReachabilityMetrics::default(),
        provider_observations: crate::unstable::ProviderObservationMetrics::default(),
        publication: crate::unstable::PublicationMetrics::default(),
    })
}

/// Link using an external system linker.
pub(crate) fn link_system_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!(
        "linker",
        mode = "system",
        command = linker_cmd,
        phase = "linking"
    )
    .entered();

    let runtime_bytes = runtime_for_target(options.target);
    // The system linker consumes the archive bytes directly, so validate the
    // embedded target and typed ABI before writing them to disk.
    validate_runtime_archive(runtime_bytes, options.target)
        .map_err(link_error)
        .map_err(CompileErrors::from)?;

    // Set up temporary directory with object files and runtime
    let mut temp_dir = TempLinkDir::new().map_err(CompileErrors::from)?;
    temp_dir
        .write_object_files(object_files)
        .map_err(CompileErrors::from)?;
    temp_dir
        .write_runtime(runtime_bytes)
        .map_err(CompileErrors::from)?;
    temp_dir.create_output().map_err(CompileErrors::from)?;

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

    // Add user-supplied static archives (`--link-archive`, ADR-0064 C FFI)
    // before the runtime so the runtime can satisfy any dependency they pull.
    for archive_path in &options.link_archives {
        cmd.arg(archive_path);
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
        query_runtime: crate::unstable::QueryRuntimeMetrics::default(),
        semantic_reachability: crate::unstable::SemanticReachabilityMetrics::default(),
        provider_observations: crate::unstable::ProviderObservationMetrics::default(),
        publication: crate::unstable::PublicationMetrics::default(),
    })
}
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use tracing::{info, info_span};

use crate::*;

#[cfg(all(test, unix))]
mod temp_link_dir_tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    #[test]
    fn workspace_is_owner_only() {
        let workspace = TempLinkDir::new().unwrap();
        let mode = workspace
            .directory
            .path()
            .metadata()
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(mode & 0o077, 0, "workspace mode was {mode:o}");
    }

    #[test]
    fn precreated_workspace_is_rejected_and_not_cleaned_up() {
        let parent = tempfile::tempdir().unwrap();
        let occupied = parent.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("sentinel"), b"not ours").unwrap();
        let mut builder = tempfile::Builder::new();
        builder.prefix("occupied").rand_bytes(0);

        let result = TempLinkDir::create_in(&builder, parent.path());

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(occupied.join("sentinel")).unwrap(),
            b"not ours"
        );
    }

    fn symlink_target() -> (tempfile::TempDir, PathBuf) {
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("target");
        std::fs::write(&target, b"outside").unwrap();
        (outside, target)
    }

    #[test]
    fn symlinked_object_leaf_is_rejected() {
        let (_outside, target) = symlink_target();
        let mut workspace = TempLinkDir::new().unwrap();
        symlink(&target, workspace.directory.path().join("obj0.o")).unwrap();

        assert!(workspace.write_object_files(&[b"object".to_vec()]).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[test]
    fn symlinked_runtime_leaf_is_rejected() {
        let (_outside, target) = symlink_target();
        let workspace = TempLinkDir::new().unwrap();
        symlink(&target, &workspace.runtime_path).unwrap();

        assert!(workspace.write_runtime(b"runtime").is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[test]
    fn symlinked_output_leaf_is_rejected_on_creation_and_read() {
        let (_outside, target) = symlink_target();
        let workspace = TempLinkDir::new().unwrap();
        symlink(&target, &workspace.output_path).unwrap();

        assert!(workspace.create_output().is_err());
        assert!(workspace.read_output().is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }
}

#[cfg(test)]
mod runtime_archive_validation_tests {
    use super::*;
    use rue_linker::ObjectBuilder;
    use rue_runtime_abi::{
        RUNTIME_ABI_VERSION_SYMBOL, ReservedExportId, ReservedExportKind, RuntimeHelperId,
        RuntimeTarget,
    };

    fn host_runtime() -> &'static [u8] {
        runtime_for_target(Target::host().expect("tests require a supported host"))
    }

    fn function(name: &str) -> RuntimeDefinedSymbol {
        RuntimeDefinedSymbol {
            name: name.to_owned(),
            kind: RuntimeSymbolKind::Function,
            size: 1,
            section_allocated: true,
            section_writable: false,
            section_executable: true,
            bytes_from_symbol: 1,
            first_byte: Some(0),
        }
    }

    fn marker(name: &str, size: u64) -> RuntimeDefinedSymbol {
        RuntimeDefinedSymbol {
            name: name.to_owned(),
            kind: RuntimeSymbolKind::Data,
            size,
            section_allocated: true,
            section_writable: false,
            section_executable: false,
            bytes_from_symbol: 1,
            first_byte: Some(0),
        }
    }

    fn valid_inventory(target: RuntimeTarget, marker_size: u64) -> RuntimeArchiveInventory {
        let mut symbols = RuntimeHelperId::ALL
            .iter()
            .copied()
            .filter(|id| id.helper().availability.contains(target))
            .map(|id| function(id.symbol()))
            .collect::<Vec<_>>();
        symbols.extend(
            ReservedExportId::ALL
                .iter()
                .copied()
                .filter(|id| id.export().availability.contains(target))
                .map(|id| match id.export().kind {
                    ReservedExportKind::Function(_) => function(id.symbol()),
                    ReservedExportKind::ReadOnlyData { .. } => marker(id.symbol(), marker_size),
                }),
        );
        RuntimeArchiveInventory {
            object_targets: vec![target],
            symbols,
        }
    }

    fn error(inventory: &RuntimeArchiveInventory, target: RuntimeTarget) -> String {
        validate_runtime_inventory(inventory, target).expect_err("inventory must be rejected")
    }

    fn archive_with_member(name: &str, member: &[u8]) -> Vec<u8> {
        append_archive_member(b"!<arch>\n", name, member)
    }

    fn append_archive_member(archive: &[u8], name: &str, member: &[u8]) -> Vec<u8> {
        assert!(name.len() <= 15);
        let mut result = archive.to_vec();
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            format!("{name}/"),
            0,
            0,
            0,
            "100644",
            member.len()
        );
        assert_eq!(header.len(), 60);
        result.extend_from_slice(header.as_bytes());
        result.extend_from_slice(member);
        if member.len() % 2 == 1 {
            result.push(b'\n');
        }
        result
    }

    fn host_object(symbol: &str) -> Vec<u8> {
        ObjectBuilder::new(Target::host().unwrap(), symbol)
            .code(vec![0; 4])
            .build()
    }

    fn raw_object_symbol(symbol: &str) -> Vec<u8> {
        let mut raw = Vec::new();
        if Target::host().is_some_and(|target| target.is_macho()) {
            raw.push(b'_');
        }
        raw.extend_from_slice(symbol.as_bytes());
        raw.push(0);
        raw
    }

    fn replace_export_name(archive: &[u8], old: &str, new: &str) -> Vec<u8> {
        assert_eq!(old.len(), new.len());
        let old = raw_object_symbol(old);
        let new = raw_object_symbol(new);
        let mut result = archive.to_vec();
        let mut replacements = 0;
        for offset in 0..=result.len().saturating_sub(old.len()) {
            if result[offset..].starts_with(&old) {
                result[offset..offset + old.len()].copy_from_slice(&new);
                replacements += 1;
            }
        }
        assert!(replacements > 0, "runtime archive did not contain export");
        result
    }

    fn validation_error(bytes: &[u8]) -> String {
        validate_runtime_archive(bytes, Target::host().unwrap())
            .expect_err("mutated archive must be rejected")
    }

    fn foreign_target() -> Target {
        match Target::host().unwrap() {
            Target::X86_64Linux => Target::Aarch64Linux,
            Target::Aarch64Linux | Target::Aarch64Macos => Target::X86_64Linux,
        }
    }

    #[test]
    fn embedded_runtime_selection_rejects_every_wrong_archive() {
        for &target in Target::all() {
            for &archive_target in Target::all() {
                if target == archive_target {
                    continue;
                }
                let error = validate_runtime_archive(runtime_for_target(archive_target), target)
                    .expect_err("runtime validation must reject a foreign archive");
                assert!(
                    error.contains("expected"),
                    "{archive_target} archive accepted for {target}: {error}"
                );
            }
        }
    }

    fn non_applicable_export() -> &'static str {
        match Target::host().unwrap() {
            Target::X86_64Linux | Target::Aarch64Linux => ReservedExportId::MacosMain.symbol(),
            Target::Aarch64Macos => ReservedExportId::LinuxStart.symbol(),
        }
    }

    #[test]
    fn accepts_elf_and_macho_marker_metadata() {
        validate_runtime_inventory(
            &valid_inventory(RuntimeTarget::X86_64Linux, 1),
            RuntimeTarget::X86_64Linux,
        )
        .unwrap();
        validate_runtime_inventory(
            &valid_inventory(RuntimeTarget::Aarch64Macos, 1),
            RuntimeTarget::Aarch64Macos,
        )
        .unwrap();
    }

    #[test]
    fn rejects_missing_and_duplicate_helpers() {
        let mut inventory = valid_inventory(RuntimeTarget::X86_64Linux, 1);
        inventory
            .symbols
            .retain(|symbol| symbol.name != RuntimeHelperId::Alloc.symbol());
        assert!(
            error(&inventory, RuntimeTarget::X86_64Linux)
                .contains("missing runtime helper `__rue_alloc`")
        );

        inventory
            .symbols
            .push(function(RuntimeHelperId::Exit.symbol()));
        inventory
            .symbols
            .push(function(RuntimeHelperId::Exit.symbol()));
        assert!(
            error(&inventory, RuntimeTarget::X86_64Linux)
                .contains("runtime helper `__rue_exit` is defined 3 times")
        );
    }

    #[test]
    fn rejects_missing_stale_and_duplicate_markers() {
        let mut inventory = valid_inventory(RuntimeTarget::Aarch64Linux, 1);
        inventory
            .symbols
            .retain(|symbol| symbol.name != RUNTIME_ABI_VERSION_SYMBOL);
        assert!(
            error(&inventory, RuntimeTarget::Aarch64Linux).contains(&format!(
                "missing reserved runtime export `{RUNTIME_ABI_VERSION_SYMBOL}`"
            ))
        );

        inventory.symbols.push(marker("__rue_runtime_abi_v0", 1));
        let stale = error(&inventory, RuntimeTarget::Aarch64Linux);
        assert!(stale.contains(
            "stale runtime ABI marker `__rue_runtime_abi_v0`; expected `__rue_runtime_abi_v1`"
        ));

        inventory
            .symbols
            .push(marker(RUNTIME_ABI_VERSION_SYMBOL, 1));
        inventory
            .symbols
            .push(marker(RUNTIME_ABI_VERSION_SYMBOL, 1));
        assert!(
            error(&inventory, RuntimeTarget::Aarch64Linux).contains(&format!(
                "reserved runtime export `{RUNTIME_ABI_VERSION_SYMBOL}` is defined 2 times"
            ))
        );
    }

    #[test]
    fn rejects_invalid_marker_data_contract() {
        let mut inventory = valid_inventory(RuntimeTarget::X86_64Linux, 2);
        let marker = inventory
            .symbols
            .iter_mut()
            .find(|symbol| symbol.name == RUNTIME_ABI_VERSION_SYMBOL)
            .unwrap();
        marker.section_writable = true;
        marker.bytes_from_symbol = 0;
        marker.first_byte = Some(1);
        let err = error(&inventory, RuntimeTarget::X86_64Linux);
        assert!(err.contains("has size 2, expected 1 byte"));
        assert!(err.contains("is not retained read-only data"));
        assert!(err.contains("has no accessible marker byte"));
        assert!(err.contains("does not contain the required zero byte"));
    }

    #[test]
    fn rejects_wrong_object_target_and_non_applicable_exports() {
        let mut inventory = valid_inventory(RuntimeTarget::X86_64Linux, 1);
        inventory.object_targets[0] = RuntimeTarget::Aarch64Macos;
        inventory
            .symbols
            .push(function(ReservedExportId::MacosMain.symbol()));
        let err = error(&inventory, RuntimeTarget::X86_64Linux);
        assert!(err.contains("object 0 targets Aarch64Macos, expected X86_64Linux"));
        assert!(err.contains("reserved runtime export `_main` is not available for X86_64Linux"));
    }

    #[test]
    fn rejects_unknown_abi_owned_export_without_matching_mangled_internals() {
        let mut inventory = valid_inventory(RuntimeTarget::Aarch64Macos, 1);
        inventory.symbols.push(function("__rue_removed_helper"));
        inventory
            .symbols
            .push(function("_ZN11rue_runtime26__rue_internal_implementation"));
        let err = error(&inventory, RuntimeTarget::Aarch64Macos);
        assert!(err.contains("unknown runtime ABI export `__rue_removed_helper`"));
        assert!(!err.contains("__rue_internal_implementation"));
    }

    #[test]
    fn diagnostics_are_sorted_and_deterministic() {
        let mut inventory = valid_inventory(RuntimeTarget::X86_64Linux, 1);
        inventory
            .symbols
            .retain(|symbol| symbol.name != RuntimeHelperId::Alloc.symbol());
        inventory.symbols.push(function("__rue_z_removed_helper"));
        inventory.symbols.push(function("__rue_a_removed_helper"));

        let first = error(&inventory, RuntimeTarget::X86_64Linux);
        let second = error(&inventory, RuntimeTarget::X86_64Linux);
        assert_eq!(first, second);
        assert!(
            first.find("__rue_a_removed_helper").unwrap()
                < first.find("__rue_z_removed_helper").unwrap()
        );
    }

    #[test]
    fn strict_archive_validation_rejects_malformed_native_object_member() {
        let malformed = archive_with_member("broken.o", b"\x7fELFnot-an-object");
        let err = validate_runtime_archive(&malformed, Target::host().unwrap()).unwrap_err();
        assert!(
            err.contains("failed to parse object member `broken.o/`"),
            "{err}"
        );
    }

    #[test]
    fn strict_archive_validation_rejects_unsupported_macho_cpu() {
        let mut object = ObjectBuilder::new(Target::Aarch64Macos, "wrong_cpu")
            .code(vec![0; 4])
            .build();
        object[4..8].copy_from_slice(&0x0100_0007u32.to_le_bytes());
        let archive = archive_with_member("x86-macho.o", &object);
        let err = validate_runtime_archive(&archive, Target::host().unwrap()).unwrap_err();
        assert!(
            err.contains("unsupported Mach-O CPU type: 0x1000007"),
            "{err}"
        );
    }

    #[test]
    fn strict_archive_validation_skips_true_metadata_and_bitcode_members() {
        let archive = archive_with_member("metadata", b"rust metadata");
        let archive = append_archive_member(&archive, "module.bc", b"BC\xc0\xdebitcode");
        let archive = append_archive_member(&archive, "valid.o", &host_object("fixture"));
        let parsed = Archive::parse_strict_objects(&archive).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn archive_bytes_reject_missing_and_misspelled_helper() {
        let bytes = replace_export_name(
            host_runtime(),
            RuntimeHelperId::Alloc.symbol(),
            "__rue_alloq",
        );
        let err = validation_error(&bytes);
        assert!(
            err.contains("missing runtime helper `__rue_alloc`"),
            "{err}"
        );
        assert!(
            err.contains("unknown runtime ABI export `__rue_alloq`"),
            "{err}"
        );
    }

    #[test]
    fn archive_bytes_reject_duplicate_helper() {
        let duplicate = host_object(RuntimeHelperId::Alloc.symbol());
        let bytes = append_archive_member(host_runtime(), "duplicate.o", &duplicate);
        let err = validation_error(&bytes);
        assert!(
            err.contains("runtime helper `__rue_alloc` is defined 2 times"),
            "{err}"
        );
    }

    #[test]
    fn archive_bytes_reject_stale_marker() {
        let bytes = replace_export_name(
            host_runtime(),
            RUNTIME_ABI_VERSION_SYMBOL,
            "__rue_runtime_abi_v0",
        );
        let err = validation_error(&bytes);
        assert!(
            err.contains(
                "stale runtime ABI marker `__rue_runtime_abi_v0`; expected `__rue_runtime_abi_v1`"
            ),
            "{err}"
        );
    }

    #[test]
    fn archive_bytes_reject_missing_and_duplicate_current_marker() {
        let missing = replace_export_name(
            host_runtime(),
            RUNTIME_ABI_VERSION_SYMBOL,
            "__rue_runtime_abj_v1",
        );
        let err = validation_error(&missing);
        assert!(
            err.contains(&format!(
                "missing reserved runtime export `{RUNTIME_ABI_VERSION_SYMBOL}`"
            )),
            "{err}"
        );

        let duplicate = host_object(RUNTIME_ABI_VERSION_SYMBOL);
        let bytes = append_archive_member(host_runtime(), "marker.o", &duplicate);
        let err = validation_error(&bytes);
        assert!(
            err.contains(&format!(
                "reserved runtime export `{RUNTIME_ABI_VERSION_SYMBOL}` is defined 2 times"
            )),
            "{err}"
        );
    }

    #[test]
    fn archive_bytes_reject_wrong_target_object() {
        let object = ObjectBuilder::new(foreign_target(), "foreign")
            .code(vec![0; 4])
            .build();
        let bytes = append_archive_member(host_runtime(), "foreign.o", &object);
        let err = validation_error(&bytes);
        assert!(err.contains("expected"), "{err}");
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    fn archive_bytes_reject_non_applicable_reserved_export() {
        let object = host_object(non_applicable_export());
        let bytes = append_archive_member(host_runtime(), "wrong-os.o", &object);
        let err = validation_error(&bytes);
        assert!(
            err.contains(&format!(
                "reserved runtime export `{}` is not available",
                non_applicable_export()
            )),
            "{err}"
        );
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn x86_64_linux_sigreturn_export_is_callable_code() {
        let archive = parse_runtime_archive(host_runtime()).unwrap();
        let inventory = runtime_archive_inventory(&archive).unwrap();
        let symbol = inventory
            .symbols
            .iter()
            .find(|symbol| symbol.name == ReservedExportId::RtSigreturn.symbol())
            .unwrap();
        assert_eq!(symbol.kind, RuntimeSymbolKind::Function);
        assert!(symbol.section_executable);
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn archive_member_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        let mut offset = 8;
        while offset + 60 <= bytes.len() {
            let header = &bytes[offset..offset + 60];
            let name = std::str::from_utf8(&header[..16]).unwrap().trim();
            let size: usize = std::str::from_utf8(&header[48..58])
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            offset += 60;
            let name_len = name
                .strip_prefix("#1/")
                .map(|length| length.trim().parse().unwrap())
                .unwrap_or(0);
            ranges.push((offset + name_len, offset + size));
            offset += size + (size % 2);
        }
        ranges
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn macho_marker_offsets(bytes: &[u8]) -> (usize, usize, u64) {
        const LC_SEGMENT_64: u32 = 0x19;
        const LC_SYMTAB: u32 = 0x2;
        let raw_marker = raw_object_symbol(RUNTIME_ABI_VERSION_SYMBOL);

        for (member_start, member_end) in archive_member_ranges(bytes) {
            let member = &bytes[member_start..member_end];
            if member.len() < 32 || read_u32(member, 0) != 0xfeed_facf {
                continue;
            }
            let mut sections = Vec::new();
            let mut symtab = None;
            let mut command = 32;
            for _ in 0..read_u32(member, 16) {
                let kind = read_u32(member, command);
                let size = read_u32(member, command + 4) as usize;
                if kind == LC_SEGMENT_64 {
                    let count = read_u32(member, command + 64) as usize;
                    let mut section = command + 72;
                    for _ in 0..count {
                        sections.push((
                            read_u64(member, section + 32),
                            read_u64(member, section + 40),
                            read_u32(member, section + 48) as usize,
                        ));
                        section += 80;
                    }
                } else if kind == LC_SYMTAB {
                    symtab = Some((
                        read_u32(member, command + 8) as usize,
                        read_u32(member, command + 12) as usize,
                        read_u32(member, command + 16) as usize,
                    ));
                }
                command += size;
            }
            let Some((symbols, symbol_count, strings)) = symtab else {
                continue;
            };
            for index in 0..symbol_count {
                let symbol = symbols + index * 16;
                let name = strings + read_u32(member, symbol) as usize;
                if !member[name..].starts_with(&raw_marker) {
                    continue;
                }
                let section_index = usize::from(member[symbol + 5]) - 1;
                let (section_address, _, section_offset) = sections[section_index];
                let value = read_u64(member, symbol + 8);
                let data_offset = member_start
                    + section_offset
                    + usize::try_from(value - section_address).unwrap();
                return (data_offset, member_start + symbol + 8, value);
            }
        }
        panic!("embedded Mach-O archive did not contain the ABI marker");
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn real_macho_marker_mutations_reject_nonzero_value_and_nonunit_extent() {
        let (data_offset, value_offset, value) = macho_marker_offsets(host_runtime());

        let mut nonzero = host_runtime().to_vec();
        nonzero[data_offset] = 1;
        let err = validation_error(&nonzero);
        assert!(
            err.contains("does not contain the required zero byte"),
            "{err}"
        );

        let mut oversized = host_runtime().to_vec();
        oversized[value_offset..value_offset + 8].copy_from_slice(&(value - 1).to_le_bytes());
        let err = validation_error(&oversized);
        assert!(err.contains("has size 2, expected 1 byte"), "{err}");
    }
}
