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

type CancellableLinkResult<T> = Result<T, crate::session::PipelineRequestControl>;

#[cfg(test)]
thread_local! {
    static LINK_CANCELLATION_TRIPWIRE: std::cell::RefCell<Option<(rue_query::CancellationToken, usize)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_link_cancellation_tripwire(
    cancellation: Option<(rue_query::CancellationToken, usize)>,
) {
    LINK_CANCELLATION_TRIPWIRE.with(|slot| *slot.borrow_mut() = cancellation);
}

fn check_cancellation(cancellation: &rue_query::CancellationToken) -> CancellableLinkResult<()> {
    #[cfg(test)]
    LINK_CANCELLATION_TRIPWIRE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some((token, remaining)) = slot.as_mut() else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            token.cancel();
            *slot = None;
        }
    });
    if cancellation.is_canceled() {
        Err(crate::session::PipelineRequestControl::Abort(
            rue_query::QueryAbort::Canceled,
        ))
    } else {
        Ok(())
    }
}

fn compile_control(errors: impl Into<CompileErrors>) -> crate::session::PipelineRequestControl {
    crate::session::PipelineRequestControl::Compile(errors.into())
}

fn map_linker_control(err: rue_linker::LinkError) -> crate::session::PipelineRequestControl {
    if matches!(err, rue_linker::LinkError::Canceled) {
        crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
    } else {
        compile_control(link_error(err))
    }
}

fn uncancellable<T>(result: CancellableLinkResult<T>, context: &str) -> MultiErrorResult<T> {
    result.map_err(|control| match control {
        crate::session::PipelineRequestControl::Compile(errors) => errors,
        crate::session::PipelineRequestControl::Abort(abort) => {
            crate::session::pipeline_abort_errors(context, abort)
        }
        crate::session::PipelineRequestControl::Parked(park) => {
            crate::session::unresolved_toolchain_park_errors(&park)
        }
    })
}

fn clone_warnings_with_cancellation(
    warnings: &[CompileWarning],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<Vec<CompileWarning>> {
    let mut cloned = Vec::with_capacity(warnings.len());
    for warning in warnings {
        check_cancellation(cancellation)?;
        cloned.push(warning.clone());
    }
    Ok(cloned)
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

    fn create_capture_leaf(path: &Path, context: &str) -> CompileResult<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        options.open(path).map_err(|e| io_link_error(context, e))
    }

    /// Write object files to the temporary directory.
    ///
    /// Each object file is written to a file named `obj{N}.o` where N is
    /// the index. The paths are stored in `self.obj_paths`.
    #[cfg_attr(not(test), allow(dead_code))]
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

    fn write_object_files_with_cancellation(
        &mut self,
        object_files: &[Vec<u8>],
        cancellation: &rue_query::CancellationToken,
    ) -> CancellableLinkResult<()> {
        for (i, obj_bytes) in object_files.iter().enumerate() {
            check_cancellation(cancellation)?;
            let obj_path = self.directory.path().join(format!("obj{}.o", i));
            let mut file = Self::create_leaf(&obj_path, "failed to create temp object file")
                .map_err(compile_control)?;
            write_all_with_cancellation(
                &mut file,
                obj_bytes,
                cancellation,
                "failed to write temp object file",
            )?;
            self.obj_paths.push(obj_path);
        }
        Ok(())
    }

    /// Write the runtime archive to the temporary directory.
    #[cfg_attr(not(test), allow(dead_code))]
    fn write_runtime(&self, runtime_bytes: &[u8]) -> CompileResult<()> {
        let mut file = Self::create_leaf(&self.runtime_path, "failed to create runtime archive")?;
        file.write_all(runtime_bytes)
            .map_err(|e| io_link_error("failed to write runtime archive", e))
    }

    fn write_runtime_with_cancellation(
        &self,
        runtime_bytes: &[u8],
        cancellation: &rue_query::CancellationToken,
    ) -> CancellableLinkResult<()> {
        check_cancellation(cancellation)?;
        let mut file = Self::create_leaf(&self.runtime_path, "failed to create runtime archive")
            .map_err(compile_control)?;
        write_all_with_cancellation(
            &mut file,
            runtime_bytes,
            cancellation,
            "failed to write runtime archive",
        )
    }

    /// Reserve the output leaf so a pre-existing file of any kind is rejected.
    fn create_output(&self) -> CompileResult<()> {
        Self::create_leaf(&self.output_path, "failed to create linker output")?;
        Ok(())
    }

    /// Read the linked executable from the output path.
    #[cfg_attr(not(test), allow(dead_code))]
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

    fn read_output_with_cancellation(
        &self,
        cancellation: &rue_query::CancellationToken,
    ) -> CancellableLinkResult<Vec<u8>> {
        check_cancellation(cancellation)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(&self.output_path)
            .map_err(|e| compile_control(io_link_error("failed to open linked executable", e)))?;
        if !file
            .metadata()
            .map_err(|e| compile_control(io_link_error("failed to inspect linked executable", e)))?
            .is_file()
        {
            return Err(compile_control(CompileError::without_span(
                ErrorKind::LinkError("linked executable is not a regular file".to_string()),
            )));
        }
        let mut output = Vec::new();
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            check_cancellation(cancellation)?;
            let read = file.read(&mut chunk).map_err(|e| {
                compile_control(io_link_error("failed to read linked executable", e))
            })?;
            if read == 0 {
                break;
            }
            output.extend_from_slice(&chunk[..read]);
        }
        Ok(output)
    }
}

fn write_all_with_cancellation(
    file: &mut std::fs::File,
    bytes: &[u8],
    cancellation: &rue_query::CancellationToken,
    context: &str,
) -> CancellableLinkResult<()> {
    for chunk in bytes.chunks(64 * 1024) {
        check_cancellation(cancellation)?;
        file.write_all(chunk)
            .map_err(|e| compile_control(io_link_error(context, e)))?;
    }
    Ok(())
}

fn read_capture_with_cancellation(
    file: &mut std::fs::File,
    cancellation: &rue_query::CancellationToken,
    context: &str,
) -> CancellableLinkResult<Vec<u8>> {
    check_cancellation(cancellation)?;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|error| compile_control(io_link_error(context, error)))?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        check_cancellation(cancellation)?;
        let read = file
            .read(&mut chunk)
            .map_err(|error| compile_control(io_link_error(context, error)))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

struct LinkerJob {
    child: std::process::Child,
    reaped: bool,
}

impl LinkerJob {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        // Rue's supported compiler hosts are Unix. A fresh process group makes
        // the driver and every normally spawned ld/lld descendant one owned
        // job that can be terminated before TempLinkDir is dropped. Keep the
        // direct-child fallback narrowly cfg'd for other hosts where std does
        // not expose a portable process-tree/job primitive.
        #[cfg(unix)]
        command.process_group(0);
        Ok(Self {
            child: command.spawn()?,
            reaped: false,
        })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn terminate_and_reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let mut termination_error = None;
        #[cfg(unix)]
        {
            let process_group = -(self.child.id() as libc::pid_t);
            if unsafe { libc::kill(process_group, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    termination_error = Some(error);
                    let _ = self.child.kill();
                }
            }
        }
        #[cfg(not(unix))]
        if let Err(error) = self.child.kill() {
            if error.kind() != std::io::ErrorKind::InvalidInput {
                termination_error = Some(error);
            }
        }

        let status = self.child.wait();
        if status.is_ok() {
            self.reaped = true;
        }
        let status = status?;
        if let Some(error) = termination_error {
            return Err(error);
        }
        Ok(status)
    }
}

impl Drop for LinkerJob {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.terminate_and_reap();
        }
    }
}

fn wait_for_linker(
    job: &mut LinkerJob,
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<std::process::ExitStatus> {
    loop {
        // An already-observed exit wins the child-lifecycle race. Overall
        // compilation still checks cancellation while constructing and before
        // publishing the final bytes.
        match job.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if cancellation.is_canceled() => {
                job.terminate_and_reap().map_err(|e| {
                    compile_control(io_link_error(
                        "failed to terminate and reap canceled linker job",
                        e,
                    ))
                })?;
                return Err(crate::session::PipelineRequestControl::Abort(
                    rue_query::QueryAbort::Canceled,
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => {
                if let Err(cleanup_error) = job.terminate_and_reap() {
                    return Err(compile_control(CompileError::without_span(
                        ErrorKind::LinkError(format!(
                            "failed to wait for linker: {err}; also failed to terminate and reap linker job: {cleanup_error}"
                        )),
                    )));
                }
                return Err(compile_control(io_link_error(
                    "failed to wait for linker",
                    err,
                )));
            }
        }
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
static EMBEDDED_RUNTIME_INDEXES: EmbeddedRuntimeIndexCaches = EmbeddedRuntimeIndexCaches::new();
/// Times the embedded runtime archive has actually been decoded. Parsing it
/// materializes every member — headers, symbol tables, relocations, and a
/// `Vec<u8>` per section — so this count is the real cost being avoided
/// (RUE-1845).
static RUNTIME_ARCHIVE_PARSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    /// Exact count of index parses reached through the production helper on
    /// this test thread. Keeping the guard thread-local makes assertions
    /// independent of Rust's parallel test scheduling.
    static RUNTIME_ARCHIVE_INDEX_PARSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn record_runtime_archive_index_parse() {
    #[cfg(test)]
    RUNTIME_ARCHIVE_INDEX_PARSES.with(|count| count.set(count.get() + 1));
}

struct EmbeddedRuntimeIndexCache {
    index: std::sync::OnceLock<rue_linker::ArchiveIndex<'static>>,
    initialization_active: std::sync::atomic::AtomicBool,
    waiters: std::sync::atomic::AtomicUsize,
}

struct EmbeddedRuntimeIndexInitializationLease<'a> {
    cache: &'a EmbeddedRuntimeIndexCache,
}

impl EmbeddedRuntimeIndexCache {
    const fn new() -> Self {
        Self {
            index: std::sync::OnceLock::new(),
            initialization_active: std::sync::atomic::AtomicBool::new(false),
            waiters: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn get_or_try_index(
        &self,
        cancellation: &rue_query::CancellationToken,
        initialize: impl FnOnce() -> CancellableLinkResult<rue_linker::ArchiveIndex<'static>>,
    ) -> CancellableLinkResult<&rue_linker::ArchiveIndex<'static>> {
        check_cancellation(cancellation)?;
        if let Some(index) = self.index.get() {
            return Ok(index);
        }

        // OnceLock cannot discard a canceled fallible initialization. Elect one
        // initializer explicitly and let other requests wait in short bounded
        // intervals so their own cancellation remains observable. A lease resets
        // the election on every exit, including cancellation and unwinding.
        loop {
            check_cancellation(cancellation)?;
            if let Some(index) = self.index.get() {
                return Ok(index);
            }
            if self
                .initialization_active
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::Acquire,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
            self.waiters
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(1));
            self.waiters
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        let initialization = EmbeddedRuntimeIndexInitializationLease { cache: self };
        self.initialize_after_election(initialization, cancellation, initialize)
    }

    fn initialize_after_election(
        &self,
        initialization: EmbeddedRuntimeIndexInitializationLease<'_>,
        cancellation: &rue_query::CancellationToken,
        initialize: impl FnOnce() -> CancellableLinkResult<rue_linker::ArchiveIndex<'static>>,
    ) -> CancellableLinkResult<&rue_linker::ArchiveIndex<'static>> {
        // A contender can observe an empty OnceLock before the previous
        // initializer publishes, then win the election only after that
        // initializer releases it. Recheck under this election before doing
        // any work or attempting the one-time publication.
        check_cancellation(cancellation)?;
        if let Some(index) = self.index.get() {
            return Ok(index);
        }

        let index = initialize()?;
        check_cancellation(cancellation)?;
        self.index
            .set(index)
            .expect("embedded runtime index has one elected initializer");
        drop(initialization);
        Ok(self
            .index
            .get()
            .expect("embedded runtime index was just initialized"))
    }

    fn get_or_index(
        &self,
        runtime_bytes: &'static [u8],
        cancellation: &rue_query::CancellationToken,
    ) -> CancellableLinkResult<&rue_linker::ArchiveIndex<'static>> {
        self.get_or_try_index(cancellation, || {
            parse_runtime_index_with_cancellation(runtime_bytes, cancellation)
        })
    }

    #[cfg(test)]
    fn waiter_count(&self) -> usize {
        self.waiters.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for EmbeddedRuntimeIndexInitializationLease<'_> {
    fn drop(&mut self) {
        self.cache
            .initialization_active
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

struct EmbeddedRuntimeIndexCaches {
    x86_64_linux: EmbeddedRuntimeIndexCache,
    aarch64_linux: EmbeddedRuntimeIndexCache,
    aarch64_macos: EmbeddedRuntimeIndexCache,
}

impl EmbeddedRuntimeIndexCaches {
    const fn new() -> Self {
        Self {
            x86_64_linux: EmbeddedRuntimeIndexCache::new(),
            aarch64_linux: EmbeddedRuntimeIndexCache::new(),
            aarch64_macos: EmbeddedRuntimeIndexCache::new(),
        }
    }

    fn for_target(&self, target: Target) -> &EmbeddedRuntimeIndexCache {
        match target {
            Target::X86_64Linux => &self.x86_64_linux,
            Target::Aarch64Linux => &self.aarch64_linux,
            Target::Aarch64Macos => &self.aarch64_macos,
        }
    }
}

enum ValidatedRuntimeIndex<'cache, 'bytes> {
    Embedded(&'cache rue_linker::ArchiveIndex<'static>),
    Supplied(rue_linker::ArchiveIndex<'bytes>),
}

impl ValidatedRuntimeIndex<'_, '_> {
    fn as_index(&self) -> &rue_linker::ArchiveIndex<'_> {
        match self {
            Self::Embedded(index) => index,
            Self::Supplied(index) => index,
        }
    }
}

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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_runtime_archive(runtime_bytes: &[u8]) -> Result<Archive, String> {
    let archive = Archive::parse_strict_objects(runtime_bytes)
        .map_err(|e| format!("embedded rue-runtime archive is invalid: {}", e))?;

    if archive.is_empty() {
        return Err("embedded rue-runtime archive contains no object files".to_string());
    }

    Ok(archive)
}

fn read_user_archive_with_cancellation(
    path: &Path,
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<Archive> {
    check_cancellation(cancellation)?;
    let mut file = std::fs::File::open(path).map_err(|e| {
        compile_control(CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to read link archive `{}`: {e}",
            path.display()
        ))))
    })?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        check_cancellation(cancellation)?;
        let read = file.read(&mut chunk).map_err(|e| {
            compile_control(CompileError::without_span(ErrorKind::LinkError(format!(
                "failed to read link archive `{}`: {e}",
                path.display()
            ))))
        })?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Archive::parse_strict_objects_with_cancellation(&bytes, || cancellation.is_canceled()).map_err(
        |error| {
            if matches!(error, rue_linker::ArchiveError::Canceled) {
                crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
            } else {
                compile_control(CompileError::without_span(ErrorKind::LinkError(format!(
                    "failed to parse link archive `{}`: {error}",
                    path.display()
                ))))
            }
        },
    )
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

#[derive(Debug)]
enum RuntimeArchiveWorkError {
    Canceled,
    Invalid(String),
}

#[cfg(test)]
thread_local! {
    static RUNTIME_ARCHIVE_CANCELLATION_TRIPWIRE: std::cell::RefCell<Option<(rue_query::CancellationToken, usize)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_runtime_archive_cancellation_tripwire(
    cancellation: Option<(rue_query::CancellationToken, usize)>,
) {
    RUNTIME_ARCHIVE_CANCELLATION_TRIPWIRE.with(|slot| *slot.borrow_mut() = cancellation);
}

fn check_runtime_archive_work(
    cancellation: &rue_query::CancellationToken,
) -> Result<(), RuntimeArchiveWorkError> {
    #[cfg(test)]
    RUNTIME_ARCHIVE_CANCELLATION_TRIPWIRE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some((token, remaining)) = slot.as_mut() else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            token.cancel();
            *slot = None;
        }
    });
    if cancellation.is_canceled() {
        Err(RuntimeArchiveWorkError::Canceled)
    } else {
        Ok(())
    }
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
    match runtime_archive_inventory_with_cancellation(archive, &rue_query::CancellationToken::new())
    {
        Ok(inventory) => Ok(inventory),
        Err(RuntimeArchiveWorkError::Invalid(error)) => Err(error),
        Err(RuntimeArchiveWorkError::Canceled) => unreachable!("fresh token cannot be canceled"),
    }
}

fn runtime_archive_inventory_with_cancellation(
    archive: &Archive,
    cancellation: &rue_query::CancellationToken,
) -> Result<RuntimeArchiveInventory, RuntimeArchiveWorkError> {
    check_runtime_archive_work(cancellation)?;
    use rue_linker::{ObjectFormat, SectionFlags, SymbolBinding, SymbolType};

    let mut inventory = RuntimeArchiveInventory::default();
    for (object_index, object) in archive.objects.iter().enumerate() {
        check_runtime_archive_work(cancellation)?;
        let object_target =
            parsed_object_target(object, object_index).map_err(RuntimeArchiveWorkError::Invalid)?;
        inventory.object_targets.push(object_target);

        for symbol in &object.symbols {
            check_runtime_archive_work(cancellation)?;
            if symbol.section_index.is_none()
                || matches!(symbol.binding, SymbolBinding::Local)
                || symbol.name.is_empty()
            {
                continue;
            }

            let section_index = symbol.section_index.expect("checked above");
            let section = object.sections.get(section_index).ok_or_else(|| {
                RuntimeArchiveWorkError::Invalid(format!(
                    "embedded rue-runtime archive symbol `{}` has invalid section index {}",
                    symbol.name, section_index
                ))
            })?;
            let bytes_from_symbol = u64::try_from(section.data.len())
                .unwrap_or(u64::MAX)
                .saturating_sub(symbol.value);
            let format_size = match object.format {
                ObjectFormat::Elf => symbol.size,
                ObjectFormat::MachO => {
                    let mut next_symbol = section.size;
                    for candidate in &object.symbols {
                        check_runtime_archive_work(cancellation)?;
                        if candidate.section_index == symbol.section_index
                            && candidate.value > symbol.value
                        {
                            next_symbol = next_symbol.min(candidate.value);
                        }
                    }
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
    check_runtime_archive_work(cancellation)?;
    Ok(inventory)
}

fn validate_runtime_inventory(
    inventory: &RuntimeArchiveInventory,
    target: rue_runtime_abi::RuntimeTarget,
) -> Result<(), String> {
    match validate_runtime_inventory_with_cancellation(
        inventory,
        target,
        &rue_query::CancellationToken::new(),
    ) {
        Ok(()) => Ok(()),
        Err(RuntimeArchiveWorkError::Invalid(error)) => Err(error),
        Err(RuntimeArchiveWorkError::Canceled) => unreachable!("fresh token cannot be canceled"),
    }
}

fn runtime_definitions_with_cancellation<'a>(
    inventory: &'a RuntimeArchiveInventory,
    name: &str,
    cancellation: &rue_query::CancellationToken,
) -> Result<Vec<&'a RuntimeDefinedSymbol>, RuntimeArchiveWorkError> {
    let mut definitions = Vec::new();
    for symbol in &inventory.symbols {
        check_runtime_archive_work(cancellation)?;
        if symbol.name == name {
            definitions.push(symbol);
        }
    }
    Ok(definitions)
}

fn validate_runtime_inventory_with_cancellation(
    inventory: &RuntimeArchiveInventory,
    target: rue_runtime_abi::RuntimeTarget,
    cancellation: &rue_query::CancellationToken,
) -> Result<(), RuntimeArchiveWorkError> {
    check_runtime_archive_work(cancellation)?;
    use rue_runtime_abi::{
        RUNTIME_ABI_VERSION_SYMBOL, ReservedExportId, ReservedExportKind, RuntimeHelperId,
        classify_export,
    };

    let mut errors = std::collections::BTreeSet::new();
    for (object_index, object_target) in inventory.object_targets.iter().copied().enumerate() {
        check_runtime_archive_work(cancellation)?;
        if object_target != target {
            errors.insert(format!(
                "object {object_index} targets {object_target:?}, expected {target:?}"
            ));
        }
    }

    for id in RuntimeHelperId::ALL {
        check_runtime_archive_work(cancellation)?;
        let helper = id.helper();
        let found = runtime_definitions_with_cancellation(inventory, helper.symbol, cancellation)?;
        if helper.availability.contains(target) {
            match found.len() {
                0 => {
                    errors.insert(format!("missing runtime helper `{}`", helper.symbol));
                }
                1 => {
                    if found[0].kind != RuntimeSymbolKind::Function {
                        errors.insert(format!(
                            "runtime helper `{}` is not callable code",
                            helper.symbol
                        ));
                    }
                }
                count => {
                    errors.insert(format!(
                        "runtime helper `{}` is defined {count} times",
                        helper.symbol
                    ));
                }
            }
        } else if !found.is_empty() {
            errors.insert(format!(
                "runtime helper `{}` is not available for {target:?}",
                helper.symbol
            ));
        }
    }

    for id in ReservedExportId::ALL {
        check_runtime_archive_work(cancellation)?;
        let export = id.export();
        let found = runtime_definitions_with_cancellation(inventory, export.symbol, cancellation)?;
        if export.availability.contains(target) {
            match found.len() {
                0 => {
                    errors.insert(format!(
                        "missing reserved runtime export `{}`",
                        export.symbol
                    ));
                }
                1 => match export.kind {
                    ReservedExportKind::Function(_) => {
                        if found[0].kind != RuntimeSymbolKind::Function {
                            errors.insert(format!(
                                "reserved runtime export `{}` is not callable code",
                                export.symbol
                            ));
                        }
                    }
                    ReservedExportKind::ReadOnlyData { size } => {
                        let marker = found[0];
                        if marker.kind != RuntimeSymbolKind::Data {
                            errors.insert(format!(
                                "runtime ABI marker `{}` is not an object symbol",
                                export.symbol
                            ));
                        }
                        if marker.size != u64::from(size) {
                            errors.insert(format!(
                                "runtime ABI marker `{}` has size {}, expected {} byte",
                                export.symbol, marker.size, size
                            ));
                        }
                        if !marker.section_allocated
                            || marker.section_writable
                            || marker.section_executable
                        {
                            errors.insert(format!(
                                "runtime ABI marker `{}` is not retained read-only data",
                                export.symbol
                            ));
                        }
                        if marker.bytes_from_symbol < u64::from(size) {
                            errors.insert(format!(
                                "runtime ABI marker `{}` has no accessible marker byte",
                                export.symbol
                            ));
                        }
                        if marker.first_byte != Some(0) {
                            errors.insert(format!(
                                "runtime ABI marker `{}` does not contain the required zero byte",
                                export.symbol
                            ));
                        }
                    }
                },
                count => {
                    errors.insert(format!(
                        "reserved runtime export `{}` is defined {count} times",
                        export.symbol
                    ));
                }
            }
        } else if !found.is_empty() {
            errors.insert(format!(
                "reserved runtime export `{}` is not available for {target:?}",
                export.symbol
            ));
        }
    }

    for symbol in &inventory.symbols {
        check_runtime_archive_work(cancellation)?;
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
            errors.insert(message);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(RuntimeArchiveWorkError::Invalid(format!(
            "embedded rue-runtime archive does not match the typed ABI manifest:\n  - {}",
            errors.into_iter().collect::<Vec<_>>().join("\n  - ")
        )))
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn validate_runtime_archive(runtime_bytes: &[u8], target: Target) -> Result<Archive, String> {
    let archive = parse_runtime_archive(runtime_bytes)?;
    validate_parsed_runtime_archive(archive, runtime_bytes, target)
}

/// The per-target validation memo.
fn embedded_runtime_validation(target: Target) -> &'static std::sync::OnceLock<Result<(), String>> {
    match target {
        Target::X86_64Linux => &RUNTIME_X86_64_LINUX_VALIDATION,
        Target::Aarch64Linux => &RUNTIME_AARCH64_LINUX_VALIDATION,
        Target::Aarch64Macos => &RUNTIME_AARCH64_MACOS_VALIDATION,
    }
}

/// Validate the embedded runtime archive without materializing it.
///
/// The system-linker path writes the archive bytes to disk verbatim and never
/// needs the parsed form. Once the per-target verdict is memoized there is
/// nothing left to compute, so decoding several megabytes of archive only to
/// drop it is pure overhead on every link after the first — including every
/// warm rebuild in a retained `--watch` session, where the embedded bytes
/// cannot change within the process (RUE-1845).
fn validate_runtime_archive_only_with_cancellation(
    runtime_bytes: &[u8],
    target: Target,
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<()> {
    check_cancellation(cancellation)?;
    // The same physical-identity test the memo itself is keyed on: a
    // caller-supplied archive that merely compares equal is still validated.
    if std::ptr::eq(runtime_bytes, runtime_for_target(target))
        && let Some(validation) = embedded_runtime_validation(target).get()
    {
        return validation
            .clone()
            .map_err(|error| compile_control(link_error(error)));
    }
    validate_runtime_archive_with_cancellation(runtime_bytes, target, cancellation).map(|_| ())
}

/// Index the runtime archive for linking, decoding only what selection reads.
///
/// Two costs used to be paid on every link, both proportional to the whole
/// archive rather than to what the link takes from it (RUE-1845). The embedded
/// x86-64 runtime is 297 members and 4.2 MB, of which a typical link extracts
/// one 45 KB member.
///
/// **The parse.** Selection asks only which members define which symbols, so
/// members are indexed rather than decoded, and only the selected ones are
/// parsed in full. `ArchiveIndex` documents what that narrows: a member whose
/// section or relocation contents are malformed now fails when it is linked
/// rather than when the archive is read, and a member that is never linked
/// cannot affect the output.
///
/// **The ABI check.** `validate_runtime_inventory` is a conformance check over
/// the whole archive — every required runtime helper present exactly once,
/// reserved export IDs, the ABI version symbol — and it reads section flags and
/// bytes, so it needs every member decoded. For the *embedded* archive it is
/// also redundant: `pipeline_tests::test_embedded_runtimes_are_valid` runs
/// exactly this validation over all three embedded runtimes, and the archive is
/// `include_bytes!` data, so the bytes that test checks are the bytes every
/// compile links. Re-deriving the verdict per process moved a build-time
/// guarantee into every user's compile. A caller-supplied archive is not
/// covered by that test and still takes the full parse and the full check.
///
/// **Retained links.** The embedded bytes have process lifetime and cannot
/// change, so their parsed index is retained once per target. Physical identity
/// is the authority: equal caller-owned bytes still take both fresh validation
/// and fresh indexing on every call (RUE-1881).
#[cfg(test)]
fn validated_runtime_index_with_cancellation<'a>(
    runtime_bytes: &'a [u8],
    target: Target,
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<ValidatedRuntimeIndex<'static, 'a>> {
    validated_runtime_index_in_caches_with_cancellation(
        runtime_bytes,
        target,
        cancellation,
        &EMBEDDED_RUNTIME_INDEXES,
    )
}

fn validated_runtime_index_in_caches_with_cancellation<'cache, 'bytes>(
    runtime_bytes: &'bytes [u8],
    target: Target,
    cancellation: &rue_query::CancellationToken,
    embedded_indexes: &'cache EmbeddedRuntimeIndexCaches,
) -> CancellableLinkResult<ValidatedRuntimeIndex<'cache, 'bytes>> {
    check_cancellation(cancellation)?;
    let embedded_runtime = runtime_for_target(target);
    if std::ptr::eq(runtime_bytes, embedded_runtime) {
        return embedded_indexes
            .for_target(target)
            .get_or_index(embedded_runtime, cancellation)
            .map(ValidatedRuntimeIndex::Embedded);
    }

    validate_runtime_archive_with_cancellation(runtime_bytes, target, cancellation)?;
    check_cancellation(cancellation)?;
    parse_runtime_index_with_cancellation(runtime_bytes, cancellation)
        .map(ValidatedRuntimeIndex::Supplied)
}

fn parse_runtime_index_with_cancellation<'a>(
    runtime_bytes: &'a [u8],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<rue_linker::ArchiveIndex<'a>> {
    record_runtime_archive_index_parse();
    let index =
        rue_linker::ArchiveIndex::parse_strict_objects_with_cancellation(runtime_bytes, || {
            cancellation.is_canceled()
        })
        .map_err(|error| {
            if matches!(error, rue_linker::ArchiveError::Canceled) {
                crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
            } else {
                compile_control(link_error(format!(
                    "embedded rue-runtime archive is invalid: {error}"
                )))
            }
        })?;
    if index.is_empty() {
        return Err(compile_control(link_error(
            "embedded rue-runtime archive contains no object files",
        )));
    }
    Ok(index)
}

fn validate_runtime_archive_with_cancellation(
    runtime_bytes: &[u8],
    target: Target,
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<Archive> {
    check_cancellation(cancellation)?;
    RUNTIME_ARCHIVE_PARSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let archive = Archive::parse_strict_objects_with_cancellation(runtime_bytes, || {
        cancellation.is_canceled()
    })
    .map_err(|error| {
        if matches!(error, rue_linker::ArchiveError::Canceled) {
            crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
        } else {
            compile_control(link_error(format!(
                "embedded rue-runtime archive is invalid: {error}"
            )))
        }
    })?;
    if archive.is_empty() {
        return Err(compile_control(link_error(
            "embedded rue-runtime archive contains no object files",
        )));
    }
    check_cancellation(cancellation)?;
    let embedded_runtime = runtime_for_target(target);
    let embedded_validation = embedded_runtime_validation(target);
    if std::ptr::eq(runtime_bytes, embedded_runtime) {
        if let Some(validation) = embedded_validation.get() {
            validation
                .clone()
                .map_err(|error| compile_control(link_error(error)))?;
        } else {
            let validation = (|| {
                let inventory =
                    runtime_archive_inventory_with_cancellation(&archive, cancellation)?;
                validate_runtime_inventory_with_cancellation(
                    &inventory,
                    runtime_target(target),
                    cancellation,
                )
            })();
            let validation = match validation {
                Ok(()) => Ok(()),
                Err(RuntimeArchiveWorkError::Invalid(error)) => Err(error),
                Err(RuntimeArchiveWorkError::Canceled) => {
                    return Err(crate::session::PipelineRequestControl::Abort(
                        rue_query::QueryAbort::Canceled,
                    ));
                }
            };
            embedded_validation
                .get_or_init(|| validation)
                .clone()
                .map_err(|error| compile_control(link_error(error)))?;
        }
    } else {
        let inventory = runtime_archive_inventory_with_cancellation(&archive, cancellation)
            .map_err(map_runtime_archive_work_control)?;
        validate_runtime_inventory_with_cancellation(
            &inventory,
            runtime_target(target),
            cancellation,
        )
        .map_err(map_runtime_archive_work_control)?;
    }
    check_cancellation(cancellation)?;
    Ok(archive)
}

fn map_runtime_archive_work_control(
    error: RuntimeArchiveWorkError,
) -> crate::session::PipelineRequestControl {
    match error {
        RuntimeArchiveWorkError::Canceled => {
            crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
        }
        RuntimeArchiveWorkError::Invalid(error) => compile_control(link_error(error)),
    }
}

fn validate_parsed_runtime_archive(
    archive: Archive,
    runtime_bytes: &[u8],
    target: Target,
) -> Result<Archive, String> {
    let validate = || {
        let inventory = runtime_archive_inventory(&archive)?;
        validate_runtime_inventory(&inventory, runtime_target(target))
    };
    let embedded_runtime = runtime_for_target(target);
    let embedded_validation = embedded_runtime_validation(target);
    if std::ptr::eq(runtime_bytes, embedded_runtime) {
        embedded_validation.get_or_init(validate).clone()?;
    } else {
        validate()?;
    }
    Ok(archive)
}

#[allow(dead_code)] // retained for byte-container linker callers and focused tests
pub(crate) fn link_internal_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    let _span = info_span!("linker", mode = "internal", phase = "linking").entered();

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

    finish_internal_link(linker, options, object_files.len(), warnings)
}

fn finish_internal_link(
    linker: Linker,
    options: &CompileOptions,
    object_count: usize,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    uncancellable(
        finish_internal_link_with_cancellation(
            linker,
            options,
            object_count,
            warnings,
            &rue_query::CancellationToken::new(),
        ),
        "internal link",
    )
}

fn finish_internal_link_with_cancellation(
    mut linker: Linker,
    options: &CompileOptions,
    object_count: usize,
    warnings: &[CompileWarning],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<CompileOutput> {
    check_cancellation(cancellation)?;
    let runtime_bytes = runtime_for_target(options.target);
    let entry_point = if options.target.is_macho() {
        "__main"
    } else {
        "_start"
    };
    linker.require_symbol(entry_point);
    {
        let _span = info_span!("link_archive_resolve").entered();
        for archive_path in &options.link_archives {
            check_cancellation(cancellation)?;
            let archive = read_user_archive_with_cancellation(archive_path, cancellation)?;
            linker
                .add_archive_with_cancellation(archive, &mut || cancellation.is_canceled())
                .map_err(map_linker_control)?;
        }
        check_cancellation(cancellation)?;
        add_runtime_archive_to_linker_with_cancellation(
            &mut linker,
            runtime_bytes,
            options.target,
            cancellation,
            &EMBEDDED_RUNTIME_INDEXES,
        )?;
    }
    let executable = linker
        .link_with_cancellation(entry_point, || cancellation.is_canceled())
        .map_err(|err| match &err {
            rue_linker::LinkError::Canceled => {
                crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
            }
            rue_linker::LinkError::UndefinedSymbol(symbol) => {
                let mut searched = vec!["the bundled rue-runtime archive".to_string()];
                for archive_path in &options.link_archives {
                    searched.push(format!("`{}`", archive_path.display()));
                }
                compile_control(CompileError::without_span(ErrorKind::LinkError(format!(
                    "undefined symbol `{symbol}`: no supplied archive defines it \
                     (searched {})",
                    searched.join(", ")
                ))))
            }
            _ => compile_control(link_error(err)),
        })?;
    check_cancellation(cancellation)?;
    info!(
        object_count,
        output_bytes = executable.len(),
        "linking complete"
    );
    Ok(CompileOutput {
        elf: executable,
        warnings: clone_warnings_with_cancellation(warnings, cancellation)?,
        source_stats: SourceStats::default(),
        work: PipelineWork::default(),
        query_runtime: crate::unstable::QueryRuntimeMetrics::default(),
        semantic_reachability: crate::unstable::SemanticReachabilityMetrics::default(),
        provider_observations: crate::unstable::ProviderObservationMetrics::default(),
        publication: crate::unstable::PublicationMetrics::default(),
    })
}

fn add_runtime_archive_to_linker_with_cancellation(
    linker: &mut Linker,
    runtime_bytes: &[u8],
    target: Target,
    cancellation: &rue_query::CancellationToken,
    embedded_indexes: &EmbeddedRuntimeIndexCaches,
) -> CancellableLinkResult<()> {
    let runtime = validated_runtime_index_in_caches_with_cancellation(
        runtime_bytes,
        target,
        cancellation,
        embedded_indexes,
    )?;
    linker
        .add_archive_index_with_cancellation(runtime.as_index(), &mut || cancellation.is_canceled())
        .map_err(map_linker_control)
}

/// Link retained compiler units directly. Export thunks remain serialized
/// because they are synthesized outside the retained CodegenUnit query.
pub(crate) fn link_internal_structured_with_warnings_and_cancellation(
    options: &CompileOptions,
    objects: &[crate::object_query::CollectedObjectProjection],
    export_thunk_objects: &[Vec<u8>],
    warnings: &[CompileWarning],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<CompileOutput> {
    link_internal_structured_admission_with_cancellation(
        options,
        objects.len(),
        export_thunk_objects,
        warnings,
        cancellation,
        |linker| {
            for collected in objects {
                check_cancellation(cancellation)?;
                admit_structured_unit(linker, &collected.unit, options.target, cancellation)?;
            }
            Ok(())
        },
    )
}

pub(crate) fn link_internal_structured_units_with_warnings_and_cancellation(
    options: &CompileOptions,
    units: &[crate::codegen_query::CollectedCodegenUnit],
    export_thunk_objects: &[Vec<u8>],
    warnings: &[CompileWarning],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<CompileOutput> {
    link_internal_structured_admission_with_cancellation(
        options,
        units.len(),
        export_thunk_objects,
        warnings,
        cancellation,
        |linker| {
            for collected in units {
                check_cancellation(cancellation)?;
                admit_structured_unit(linker, &collected.unit, options.target, cancellation)?;
            }
            Ok(())
        },
    )
}

fn link_internal_structured_admission_with_cancellation(
    options: &CompileOptions,
    object_count: usize,
    export_thunk_objects: &[Vec<u8>],
    warnings: &[CompileWarning],
    cancellation: &rue_query::CancellationToken,
    mut admit: impl FnMut(&mut Linker) -> CancellableLinkResult<()>,
) -> CancellableLinkResult<CompileOutput> {
    check_cancellation(cancellation)?;
    let _span = info_span!("linker", mode = "internal", phase = "linking").entered();
    let mut linker = Linker::new(options.target);
    {
        let _span = info_span!(
            "link_structured_admission",
            object_count = object_count,
            export_thunk_count = export_thunk_objects.len()
        )
        .entered();
        admit(&mut linker)?;
        add_export_thunks(&mut linker, export_thunk_objects, cancellation)?;
    }
    finish_internal_link_with_cancellation(
        linker,
        options,
        object_count + export_thunk_objects.len(),
        warnings,
        cancellation,
    )
}

fn add_export_thunks(
    linker: &mut Linker,
    export_thunk_objects: &[Vec<u8>],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<()> {
    // C-ABI entry thunks are not retained CodegenUnits; preserve their
    // established byte-container path and diagnostics.
    for bytes in export_thunk_objects {
        check_cancellation(cancellation)?;
        let object = ObjectFile::parse(bytes)
            .map_err(link_error)
            .map_err(compile_control)?;
        linker
            .add_object_with_cancellation(object, &mut || cancellation.is_canceled())
            .map_err(map_linker_control)?;
    }
    Ok(())
}

fn admit_structured_unit(
    linker: &mut Linker,
    unit: &crate::codegen_query::CodegenUnit,
    target: Target,
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<()> {
    let object = crate::backend::project_backend_structured_object_with_cancellation(
        unit,
        target,
        cancellation,
    )?;
    linker
        .add_structured_object_with_cancellation(object, &mut || cancellation.is_canceled())
        .map_err(map_linker_control)
}

/// Link using an external system linker.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn link_system_with_warnings(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
    warnings: &[CompileWarning],
) -> MultiErrorResult<CompileOutput> {
    uncancellable(
        link_system_with_warnings_and_cancellation(
            options,
            object_files,
            linker_cmd,
            warnings,
            &rue_query::CancellationToken::new(),
        ),
        "system link",
    )
}

pub(crate) fn link_system_with_warnings_and_cancellation(
    options: &CompileOptions,
    object_files: &[Vec<u8>],
    linker_cmd: &str,
    warnings: &[CompileWarning],
    cancellation: &rue_query::CancellationToken,
) -> CancellableLinkResult<CompileOutput> {
    check_cancellation(cancellation)?;
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
    validate_runtime_archive_only_with_cancellation(runtime_bytes, options.target, cancellation)?;
    check_cancellation(cancellation)?;

    // Set up temporary directory with object files and runtime
    let mut temp_dir = TempLinkDir::new().map_err(compile_control)?;
    temp_dir.write_object_files_with_cancellation(object_files, cancellation)?;
    temp_dir.write_runtime_with_cancellation(runtime_bytes, cancellation)?;
    temp_dir.create_output().map_err(compile_control)?;
    check_cancellation(cancellation)?;

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

    // Redirect output into owner-only workspace leaves. This preserves
    // `Command::output` diagnostics without risking a full pipe blocking the
    // child while the parent polls its lifecycle.
    let stdout_path = temp_dir.directory.path().join("linker.stdout");
    let stderr_path = temp_dir.directory.path().join("linker.stderr");
    let stdout = TempLinkDir::create_leaf(&stdout_path, "failed to create linker stdout")
        .map_err(compile_control)?;
    let mut stderr =
        TempLinkDir::create_capture_leaf(&stderr_path, "failed to create linker stderr")
            .map_err(compile_control)?;
    let stderr_for_child = stderr.try_clone().map_err(|error| {
        compile_control(io_link_error(
            "failed to duplicate linker stderr capture",
            error,
        ))
    })?;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(stdout));
    cmd.stderr(Stdio::from(stderr_for_child));

    let mut job = LinkerJob::spawn(&mut cmd).map_err(|e| {
        compile_control(CompileError::without_span(ErrorKind::LinkError(format!(
            "failed to execute linker '{}': {}",
            linker_cmd, e
        ))))
    })?;

    let status = wait_for_linker(&mut job, cancellation)?;

    check_cancellation(cancellation)?;
    if !status.success() {
        // Read the capture inode we created, not the pathname the arbitrary
        // linker could have unlinked and replaced while it owned the workspace.
        let stderr_bytes = read_capture_with_cancellation(
            &mut stderr,
            cancellation,
            "failed to read linker stderr",
        )?;
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        // temp_dir is dropped here, cleaning up automatically
        return Err(compile_control(CompileError::without_span(
            ErrorKind::LinkError(format!("linker '{}' failed: {}", linker_cmd, stderr)),
        )));
    }

    // Read the resulting executable
    let elf = temp_dir.read_output_with_cancellation(cancellation)?;
    check_cancellation(cancellation)?;
    info!(
        object_count = object_files.len(),
        output_bytes = elf.len(),
        "linking complete"
    );

    // temp_dir is dropped here, cleaning up automatically
    Ok(CompileOutput {
        elf,
        warnings: clone_warnings_with_cancellation(warnings, cancellation)?,
        source_stats: SourceStats::default(),
        work: PipelineWork::default(),
        query_runtime: crate::unstable::QueryRuntimeMetrics::default(),
        semantic_reachability: crate::unstable::SemanticReachabilityMetrics::default(),
        provider_observations: crate::unstable::ProviderObservationMetrics::default(),
        publication: crate::unstable::PublicationMetrics::default(),
    })
}
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use tracing::{info, info_span};

use crate::*;

#[cfg(all(test, unix))]
mod temp_link_dir_tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn write_mock_linker(directory: &tempfile::TempDir, body: &str) -> PathBuf {
        let path = directory.path().join("mock-linker");
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        let mut permissions = path.metadata().unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn mock_output_argument() -> &'static str {
        "out=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = '-o' ]; then out=$arg; break; fi\n  prev=$arg\ndone\n"
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_system_link_cancellation_reaps_child_and_cleans_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let pid_path = directory.path().join("pid");
        let descendant_pid_path = directory.path().join("descendant-pid");
        let workspace_path = directory.path().join("workspace");
        let body = format!(
            "{}printf '%s\\n' \"$$\" > '{}'\nsleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nprintf '%s\\n' \"$(dirname \"$out\")\" > '{}'\n: > '{}'\nwait",
            mock_output_argument(),
            pid_path.display(),
            descendant_pid_path.display(),
            workspace_path.display(),
            ready.display(),
        );
        let linker = write_mock_linker(&directory, &body);
        let cancellation = rue_query::CancellationToken::new();
        let canceler = cancellation.clone();
        let ready_for_thread = ready.clone();
        let cancel_thread = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !ready_for_thread.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "mock linker never started"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            canceler.cancel();
        });
        let mut options = CompileOptions::default();
        options.target = Target::host().unwrap();
        let started = std::time::Instant::now();
        let result = link_system_with_warnings_and_cancellation(
            &options,
            &[],
            linker.to_str().unwrap(),
            &[],
            &cancellation,
        );
        cancel_thread.join().unwrap();

        assert!(matches!(
            result,
            Err(crate::session::PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled
            ))
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        let workspace = std::fs::read_to_string(&workspace_path).unwrap();
        assert!(!Path::new(workspace.trim()).exists());
        for pid_path in [&pid_path, &descendant_pid_path] {
            let pid: libc::pid_t = std::fs::read_to_string(pid_path)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "linker process {pid} survived process-group termination"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_system_link_preserves_ordinary_success_and_failure() {
        let directory = tempfile::tempdir().unwrap();
        let success = write_mock_linker(
            &directory,
            &format!("{}printf 'linked' > \"$out\"", mock_output_argument()),
        );
        let mut options = CompileOptions::default();
        options.target = Target::host().unwrap();
        let output =
            link_system_with_warnings(&options, &[], success.to_str().unwrap(), &[]).unwrap();
        assert_eq!(output.elf, b"linked");

        let failure_dir = tempfile::tempdir().unwrap();
        let failure = write_mock_linker(&failure_dir, "printf 'ordinary failure\\n' >&2\nexit 23");
        let error =
            link_system_with_warnings(&options, &[], failure.to_str().unwrap(), &[]).unwrap_err();
        assert!(error.to_string().contains("ordinary failure"));
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_system_link_reads_only_the_owned_stderr_capture() {
        for replacement in ["symlink", "fifo"] {
            let directory = tempfile::tempdir().unwrap();
            let secret = directory.path().join("secret");
            std::fs::write(&secret, "pathname replacement must not be read").unwrap();
            let replacement_command = if replacement == "symlink" {
                format!("ln -s '{}' \"$workspace/linker.stderr\"", secret.display())
            } else {
                "mkfifo \"$workspace/linker.stderr\"".to_owned()
            };
            let linker = write_mock_linker(
                &directory,
                &format!(
                    "{}workspace=$(dirname \"$out\")\nprintf 'retained diagnostic\\n' >&2\nrm \"$workspace/linker.stderr\"\n{}\nexit 23",
                    mock_output_argument(),
                    replacement_command,
                ),
            );
            let mut options = CompileOptions::default();
            options.target = Target::host().unwrap();
            let started = std::time::Instant::now();
            let error = link_system_with_warnings(&options, &[], linker.to_str().unwrap(), &[])
                .unwrap_err();
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{replacement} replacement blocked stderr capture"
            );
            let diagnostic = error.to_string();
            assert!(diagnostic.contains("retained diagnostic"));
            assert!(!diagnostic.contains("pathname replacement must not be read"));
        }
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_observed_system_link_exit_wins_child_lifecycle_race() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let mut job = LinkerJob::spawn(&mut command).unwrap();
        while job.try_wait().unwrap().is_none() {
            std::thread::yield_now();
        }
        let cancellation = rue_query::CancellationToken::new();
        cancellation.cancel();

        let status = wait_for_linker(&mut job, &cancellation).unwrap();
        assert!(status.success());
        assert!(matches!(
            check_cancellation(&cancellation),
            Err(crate::session::PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled
            ))
        ));
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_workspace_is_owner_only() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_precreated_workspace_is_rejected_and_not_cleaned_up() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_symlinked_object_leaf_is_rejected() {
        let (_outside, target) = symlink_target();
        let mut workspace = TempLinkDir::new().unwrap();
        symlink(&target, workspace.directory.path().join("obj0.o")).unwrap();

        assert!(workspace.write_object_files(&[b"object".to_vec()]).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_symlinked_runtime_leaf_is_rejected() {
        let (_outside, target) = symlink_target();
        let workspace = TempLinkDir::new().unwrap();
        symlink(&target, &workspace.runtime_path).unwrap();

        assert!(workspace.write_runtime(b"runtime").is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_symlinked_output_leaf_is_rejected_on_creation_and_read() {
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

    #[test]
    fn large_runtime_inventory_cancellation_maps_to_query_abort() {
        let target = RuntimeTarget::X86_64Linux;
        let mut inventory = valid_inventory(target, 1);
        inventory
            .symbols
            .extend((0..4_096).map(|index| function(&format!("foreign_{index}"))));
        let cancellation = rue_query::CancellationToken::new();
        set_runtime_archive_cancellation_tripwire(Some((cancellation.clone(), 128)));

        let result =
            validate_runtime_inventory_with_cancellation(&inventory, target, &cancellation);

        let error = result.unwrap_err();
        assert!(matches!(error, RuntimeArchiveWorkError::Canceled));
        assert!(matches!(
            map_runtime_archive_work_control(error),
            crate::session::PipelineRequestControl::Abort(rue_query::QueryAbort::Canceled)
        ));
        set_runtime_archive_cancellation_tripwire(None);
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
    fn indexing_an_archive_yields_the_symbols_a_full_parse_yields() {
        // RUE-1845: archive member selection reads only symbols, so members are
        // indexed rather than decoded and only the selected ones are parsed in
        // full. That is only sound if indexing produces the *same* symbols —
        // selection is first-eligible in member order, so a divergence in
        // either the member list or a member's symbols silently extracts a
        // different member.
        for &target in Target::all() {
            let bytes = runtime_for_target(target);
            let full = Archive::parse_strict_objects(bytes)
                .unwrap_or_else(|error| panic!("{target} full parse: {error}"));
            let index = rue_linker::ArchiveIndex::parse_strict_objects(bytes)
                .unwrap_or_else(|error| panic!("{target} index parse: {error}"));

            assert_eq!(
                full.objects.len(),
                index.len(),
                "{target}: indexing found a different number of members"
            );
            for (position, (object, member)) in full.objects.iter().zip(index.members()).enumerate()
            {
                assert_eq!(
                    object.machine, member.parsed.machine,
                    "{target} member {position}: machine differs"
                );
                assert_eq!(
                    object.format, member.parsed.format,
                    "{target} member {position}: format differs"
                );
                assert_eq!(
                    object.symbols.len(),
                    member.parsed.symbols.len(),
                    "{target} member {position}: symbol count differs"
                );
                for (a, b) in object.symbols.iter().zip(&member.parsed.symbols) {
                    assert_eq!(a.name, b.name, "{target} member {position}: name");
                    assert_eq!(
                        a.section_index, b.section_index,
                        "{target} member {position}: section index for `{}`",
                        a.name
                    );
                    assert_eq!(
                        a.value, b.value,
                        "{target} member {position}: value for `{}`",
                        a.name
                    );
                    assert_eq!(
                        a.size, b.size,
                        "{target} member {position}: size for `{}`",
                        a.name
                    );
                    assert_eq!(
                        a.binding, b.binding,
                        "{target} member {position}: binding for `{}`",
                        a.name
                    );
                    assert_eq!(
                        a.sym_type, b.sym_type,
                        "{target} member {position}: type for `{}`",
                        a.name
                    );
                }
            }
        }
    }

    #[test]
    fn indexing_the_embedded_runtime_does_not_parse_every_member() {
        // The point of the index (RUE-1845): a fresh compile stops decoding the
        // whole archive. `RUNTIME_ARCHIVE_PARSES` counts the whole-archive
        // decode, so the embedded path must not reach it — its ABI conformance
        // is established by `test_embedded_runtimes_are_valid` at build time,
        // over the same `include_bytes!` bytes every compile links.
        let target = Target::X86_64Linux;
        let before = RUNTIME_ARCHIVE_PARSES.load(std::sync::atomic::Ordering::Relaxed);
        let index = validated_runtime_index_with_cancellation(
            runtime_for_target(target),
            target,
            &rue_query::CancellationToken::default(),
        )
        .expect("the embedded runtime indexes");
        let after = RUNTIME_ARCHIVE_PARSES.load(std::sync::atomic::Ordering::Relaxed);

        assert!(
            !index.as_index().is_empty(),
            "the embedded runtime has members"
        );
        assert_eq!(
            before, after,
            "indexing the embedded runtime fell back to a whole-archive parse"
        );

        // A caller-supplied archive is not covered by that build-time test, so
        // it still takes the full parse and the full ABI check.
        let supplied = runtime_for_target(target).to_vec();
        let before = RUNTIME_ARCHIVE_PARSES.load(std::sync::atomic::Ordering::Relaxed);
        validated_runtime_index_with_cancellation(
            &supplied,
            target,
            &rue_query::CancellationToken::default(),
        )
        .expect("a copy of the embedded runtime is still a valid runtime");
        let after = RUNTIME_ARCHIVE_PARSES.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            before + 1,
            after,
            "a caller-supplied archive must still be fully validated"
        );
    }

    #[test]
    fn internal_link_dispatch_caches_only_each_embedded_runtime_index() {
        let cancellation = rue_query::CancellationToken::default();
        let embedded_indexes = EmbeddedRuntimeIndexCaches::new();

        for &target in Target::all() {
            let bytes = runtime_for_target(target);
            RUNTIME_ARCHIVE_INDEX_PARSES.with(|count| count.set(0));
            for _ in 0..2 {
                add_runtime_archive_to_linker_with_cancellation(
                    &mut Linker::new(target),
                    bytes,
                    target,
                    &cancellation,
                    &embedded_indexes,
                )
                .expect("the production internal-link dispatch admits the embedded runtime");
            }
            RUNTIME_ARCHIVE_INDEX_PARSES.with(|count| {
                assert_eq!(
                    count.get(),
                    1,
                    "two {target} embedded links must index once"
                )
            });

            // Equal contents do not confer embedded identity. Pass two
            // physically distinct copies through the same production dispatch
            // boundary used above; neither may consult the embedded cache.
            let supplied_first = bytes.to_vec();
            let supplied_second = bytes.to_vec();
            assert!(!std::ptr::eq(supplied_first.as_slice(), bytes));
            assert!(!std::ptr::eq(supplied_second.as_slice(), bytes));
            assert!(!std::ptr::eq(
                supplied_first.as_slice(),
                supplied_second.as_slice()
            ));
            RUNTIME_ARCHIVE_INDEX_PARSES.with(|count| count.set(0));
            for supplied in [&supplied_first, &supplied_second] {
                add_runtime_archive_to_linker_with_cancellation(
                    &mut Linker::new(target),
                    supplied,
                    target,
                    &cancellation,
                    &embedded_indexes,
                )
                .expect("the production internal-link dispatch admits a supplied runtime");
            }
            RUNTIME_ARCHIVE_INDEX_PARSES.with(|count| {
                assert_eq!(
                    count.get(),
                    2,
                    "each physically distinct {target} caller archive must be indexed"
                )
            });
        }
    }

    #[test]
    fn canceled_embedded_index_waiter_does_not_wait_for_the_initializer() {
        let target = Target::X86_64Linux;
        let bytes = runtime_for_target(target);
        let cache = std::sync::Arc::new(EmbeddedRuntimeIndexCache::new());
        let (initializer_entered_tx, initializer_entered_rx) = std::sync::mpsc::channel();
        let (release_initializer_tx, release_initializer_rx) = std::sync::mpsc::channel();

        let initializer_cache = std::sync::Arc::clone(&cache);
        let initializer = std::thread::spawn(move || {
            let cancellation = rue_query::CancellationToken::new();
            initializer_cache
                .get_or_try_index(&cancellation, || {
                    initializer_entered_tx.send(()).unwrap();
                    release_initializer_rx.recv().unwrap();
                    parse_runtime_index_with_cancellation(bytes, &cancellation)
                })
                .map(|_| ())
        });
        initializer_entered_rx.recv().unwrap();

        let waiter_cache = std::sync::Arc::clone(&cache);
        let waiter_cancellation = rue_query::CancellationToken::new();
        let cancel_waiter = waiter_cancellation.clone();
        let (waiter_result_tx, waiter_result_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = waiter_cache.get_or_try_index(&waiter_cancellation, || {
                panic!("a waiter must not become the initializer while one is blocked")
            });
            waiter_result_tx
                .send(matches!(
                    result,
                    Err(crate::session::PipelineRequestControl::Abort(
                        rue_query::QueryAbort::Canceled
                    ))
                ))
                .unwrap();
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while cache.waiter_count() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the second request never began waiting"
            );
            std::thread::yield_now();
        }
        cancel_waiter.cancel();
        assert!(
            waiter_result_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("the canceled waiter remained blocked behind the initializer"),
            "the waiter did not report cancellation"
        );
        waiter.join().unwrap();

        // The cancellation result above arrived while this initializer was
        // deliberately blocked. Release it only after proving that ordering.
        release_initializer_tx.send(()).unwrap();
        initializer.join().unwrap().unwrap();
    }

    #[test]
    fn canceled_embedded_index_initializer_permits_a_live_retry() {
        let target = Target::X86_64Linux;
        let bytes = runtime_for_target(target);
        let cache = std::sync::Arc::new(EmbeddedRuntimeIndexCache::new());
        let cancellation = rue_query::CancellationToken::new();
        let cancel_initializer = cancellation.clone();
        let (initializer_entered_tx, initializer_entered_rx) = std::sync::mpsc::channel();
        let (release_initializer_tx, release_initializer_rx) = std::sync::mpsc::channel();

        let initializer_cache = std::sync::Arc::clone(&cache);
        let initializer = std::thread::spawn(move || {
            initializer_cache
                .get_or_try_index(&cancellation, || {
                    initializer_entered_tx.send(()).unwrap();
                    release_initializer_rx.recv().unwrap();
                    parse_runtime_index_with_cancellation(bytes, &cancellation)
                })
                .map(|_| ())
        });
        initializer_entered_rx.recv().unwrap();
        cancel_initializer.cancel();
        release_initializer_tx.send(()).unwrap();
        assert!(matches!(
            initializer.join().unwrap(),
            Err(crate::session::PipelineRequestControl::Abort(
                rue_query::QueryAbort::Canceled
            ))
        ));
        assert!(
            cache.index.get().is_none(),
            "a canceled initializer must not publish its result"
        );

        let retry = cache
            .get_or_index(bytes, &rue_query::CancellationToken::new())
            .expect("a live request can retry after canceled initialization");
        assert!(!retry.is_empty());
    }

    #[test]
    fn elected_waiter_rechecks_an_index_published_before_election() {
        let target = Target::X86_64Linux;
        let bytes = runtime_for_target(target);
        let cancellation = rue_query::CancellationToken::new();
        let cache = EmbeddedRuntimeIndexCache::new();
        let published = parse_runtime_index_with_cancellation(bytes, &cancellation).unwrap();
        cache.index.set(published).unwrap();

        // Reproduce the state after a contender observed the OnceLock empty,
        // the prior initializer published and released, and the contender then
        // won the election using that stale observation.
        cache
            .initialization_active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let election = EmbeddedRuntimeIndexInitializationLease { cache: &cache };
        let expected = cache.index.get().unwrap();
        let actual = cache
            .initialize_after_election(election, &cancellation, || {
                panic!("an elected waiter must recheck the published index")
            })
            .unwrap();

        assert!(std::ptr::eq(actual, expected));
        assert!(
            !cache
                .initialization_active
                .load(std::sync::atomic::Ordering::Relaxed),
            "returning the published index must release the stale election"
        );
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
            "stale runtime ABI marker `__rue_runtime_abi_v0`; expected `__rue_runtime_abi_v5`"
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_strict_archive_validation_rejects_malformed_native_object_member() {
        let malformed = archive_with_member("broken.o", b"\x7fELFnot-an-object");
        let err = validate_runtime_archive(&malformed, Target::host().unwrap()).unwrap_err();
        assert!(
            err.contains("failed to parse object member `broken.o/`"),
            "{err}"
        );
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_strict_archive_validation_rejects_unsupported_macho_cpu() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_strict_archive_validation_skips_true_metadata_and_bitcode_members() {
        let archive = archive_with_member("metadata", b"rust metadata");
        let archive = append_archive_member(&archive, "module.bc", b"BC\xc0\xdebitcode");
        let archive = append_archive_member(&archive, "valid.o", &host_object("fixture"));
        let parsed = Archive::parse_strict_objects(&archive).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_archive_bytes_reject_missing_and_misspelled_helper() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_archive_bytes_reject_duplicate_helper() {
        let duplicate = host_object(RuntimeHelperId::Alloc.symbol());
        let bytes = append_archive_member(host_runtime(), "duplicate.o", &duplicate);
        let err = validation_error(&bytes);
        assert!(
            err.contains("runtime helper `__rue_alloc` is defined 2 times"),
            "{err}"
        );
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_archive_bytes_reject_stale_marker() {
        let bytes = replace_export_name(
            host_runtime(),
            RUNTIME_ABI_VERSION_SYMBOL,
            "__rue_runtime_abi_v0",
        );
        let err = validation_error(&bytes);
        assert!(
            err.contains(
                "stale runtime ABI marker `__rue_runtime_abi_v0`; expected `__rue_runtime_abi_v5`"
            ),
            "{err}"
        );
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_archive_bytes_reject_missing_and_duplicate_current_marker() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_archive_bytes_reject_wrong_target_object() {
        let object = ObjectBuilder::new(foreign_target(), "foreign")
            .code(vec![0; 4])
            .build();
        let bytes = append_archive_member(host_runtime(), "foreign.o", &object);
        let err = validation_error(&bytes);
        assert!(err.contains("expected"), "{err}");
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_archive_bytes_reject_non_applicable_reserved_export() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_x86_64_linux_sigreturn_export_is_callable_code() {
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
    #[ignore = "platform_native_ host coverage; run by rue-compiler-platform-native-test"]
    fn platform_native_real_macho_marker_mutations_reject_nonzero_value_and_nonunit_extent() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// RUE-1845: the system-linker path writes the embedded archive bytes to
    /// disk verbatim, so once the per-target verdict is memoized it must not
    /// decode the archive again. The decode materializes every member, so this
    /// is the cost the validation-only entry point exists to avoid.
    #[test]
    fn validating_the_embedded_runtime_stops_reparsing_it() {
        let target = Target::X86_64Linux;
        let bytes = runtime_for_target(target);
        let cancellation = rue_query::CancellationToken::new();

        // Prime the verdict. Whether this particular call is the one that
        // parses depends on what else ran first in the process, so only the
        // steady state below is asserted.
        validate_runtime_archive_only_with_cancellation(bytes, target, &cancellation).unwrap();
        assert!(
            embedded_runtime_validation(target).get().is_some(),
            "the verdict should be memoized after one validation"
        );

        let before = RUNTIME_ARCHIVE_PARSES.load(std::sync::atomic::Ordering::Relaxed);
        for _ in 0..4 {
            validate_runtime_archive_only_with_cancellation(bytes, target, &cancellation).unwrap();
        }
        assert_eq!(
            RUNTIME_ARCHIVE_PARSES.load(std::sync::atomic::Ordering::Relaxed),
            before,
            "validating the embedded archive re-parsed it after the verdict was memoized"
        );
    }
}
