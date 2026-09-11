//! The local compiler service's identity, endpoint, and lifecycle
//! (ADR-0085 §2, §5).
//!
//! A service is owned by one OS user, one scope directory, an optional
//! isolation name, one exact compiler build, and one protocol version. Those
//! five inputs digest into the service identity; the endpoint directory is
//! named by it, so two compiler builds or two worktrees can never meet on one
//! socket, and the handshake compares the same identity again so a stale or
//! foreign endpoint is refused rather than trusted by its pathname.
//!
//! Liveness is a held file lock, never a recorded PID: a live service holds
//! `service.lock` for its whole life, so "can the lock be taken" is the one
//! question every control path asks before it believes a socket or record.
//! Startup is serialized by `startup.lock`, and readiness is published only
//! when the socket answers the handshake.

pub mod client;
pub mod protocol;
mod service;

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::running_image::{RunningImageIdentity, running_image_identity};
pub use client::{ConnectError, Connection, Submission, SubmitError};
pub use protocol::{
    BuildKind, BuildRequest, BuildResult, CompileFailureRecord, CrashRecord,
    DAEMON_PROTOCOL_VERSION, DestinationRecord, DiagnosticFormat, IdentityRecord, InputRecord,
    InventoryEntryRecord, MAX_RESPONSE_BYTES, OutputStream, RequestMeasurement, RequestSummary,
    ResourcePolicy, ResourcePressure, ServiceInfo, StatusReport, StreamWrite, TestImageRecord,
    UnimportedFileRecord, UnimportedRecord,
};
pub use service::{BuildExecutor, BuildOutput, MAX_CONNECTIONS, MAX_QUEUED_REQUESTS, ServeExit};

/// How long an idle service lives by default before retiring itself. A
/// calibrated policy belongs to the qualification phase (ADR-0085 §7); this is
/// the conservative bound an opt-in service uses until then.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How long `start` waits for a launched service to answer its handshake.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Environment variable naming the endpoint root. It exists so a test harness
/// or an operator can keep a set of services apart from the user's ordinary
/// runtime directory; the directory must still be private to the user.
pub const ENDPOINT_ROOT_ENV: &str = "RUE_DAEMON_ROOT";

/// The longest socket path both maintained platforms accept (`sun_path` is
/// 104 bytes on macOS and 108 on Linux, including the terminator).
const MAX_SOCKET_PATH_BYTES: usize = 103;

/// Why a service operation could not proceed. Every variant names what the
/// caller can change; none is a program diagnostic.
#[derive(Debug)]
pub enum DaemonError {
    /// The running compiler image has no verifiable identity, so no service
    /// can be scoped to it.
    IdentityUnavailable(String),
    /// A scope, isolation name, or endpoint root the caller supplied is not
    /// usable.
    InvalidConfiguration(String),
    /// The endpoint root or directory is not private to the current user.
    InsecureEndpoint(PathBuf, String),
    /// A filesystem or socket operation failed.
    Io { context: String, error: io::Error },
    /// A service answered on this endpoint but is not the one this identity
    /// names, or spoke the protocol incorrectly.
    Protocol(String),
    /// A launched service did not become ready within the startup budget.
    StartupTimeout { waited: Duration, detail: String },
}

impl DaemonError {
    fn io(context: impl Into<String>, error: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            error,
        }
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityUnavailable(reason) => {
                write!(formatter, "the compiler daemon is unavailable: {reason}")
            }
            Self::InvalidConfiguration(reason) => formatter.write_str(reason),
            Self::InsecureEndpoint(path, reason) => write!(
                formatter,
                "refusing daemon endpoint {}: {reason}",
                path.display()
            ),
            Self::Io { context, error } => write!(formatter, "{context}: {error}"),
            Self::Protocol(reason) => write!(formatter, "daemon protocol error: {reason}"),
            Self::StartupTimeout { waited, detail } => write!(
                formatter,
                "the compiler daemon did not become ready within {} ms{detail}",
                waited.as_millis()
            ),
        }
    }
}

impl std::error::Error for DaemonError {}

/// The process/resource grouping a service belongs to (ADR-0085 §2). It is
/// only that: it never changes the compiler's project root, import
/// containment, manifest, or source identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonScope {
    directory: PathBuf,
    isolation: Option<String>,
}

impl DaemonScope {
    /// Scope a service to `directory`, which must exist; it is canonicalized
    /// so every spelling of one directory selects one service. `isolation`
    /// separates otherwise identical services and is limited to a short
    /// portable name.
    pub fn new(directory: &Path, isolation: Option<&str>) -> Result<Self, DaemonError> {
        let directory = fs::canonicalize(directory).map_err(|error| {
            DaemonError::InvalidConfiguration(format!(
                "daemon scope directory {} is not usable: {error}",
                directory.display()
            ))
        })?;
        if !directory.is_dir() {
            return Err(DaemonError::InvalidConfiguration(format!(
                "daemon scope {} is not a directory",
                directory.display()
            )));
        }
        let isolation = match isolation {
            None => None,
            Some(name) => {
                let valid = !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
                if !valid {
                    return Err(DaemonError::InvalidConfiguration(format!(
                        "daemon isolation name {name:?} must be 1-64 characters of \
                         letters, digits, `.`, `_`, or `-`"
                    )));
                }
                Some(name.to_owned())
            }
        };
        Ok(Self {
            directory,
            isolation,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn isolation(&self) -> Option<&str> {
        self.isolation.as_deref()
    }
}

impl std::fmt::Display for DaemonScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.directory.display())?;
        if let Some(isolation) = &self.isolation {
            write!(formatter, " (isolation {isolation})")?;
        }
        Ok(())
    }
}

/// The identity a client and a service must share: the current user, the
/// scope, the protocol version, and the verified running compiler image.
#[derive(Clone, Debug)]
pub struct ServiceIdentity {
    scope: DaemonScope,
    image: RunningImageIdentity,
    digest: [u8; 32],
}

impl ServiceIdentity {
    /// The identity of a service for `scope` run by this process's image and
    /// user. Fails when the running image cannot be identified; there is no
    /// pathname or version fallback (RUE-2153).
    pub fn current(scope: DaemonScope) -> Result<Self, DaemonError> {
        // A process's image cannot change underneath it, and identifying it
        // hashes the whole executable, so it is captured once per process.
        static IMAGE: std::sync::OnceLock<Result<RunningImageIdentity, String>> =
            std::sync::OnceLock::new();
        let image = IMAGE
            .get_or_init(|| running_image_identity().map_err(|error| error.reason().to_owned()))
            .clone()
            .map_err(DaemonError::IdentityUnavailable)?;
        Ok(Self::with_image(scope, image))
    }

    fn with_image(scope: DaemonScope, image: RunningImageIdentity) -> Self {
        let digest = service_digest(current_uid(), &scope, &image);
        Self {
            scope,
            image,
            digest,
        }
    }

    pub fn scope(&self) -> &DaemonScope {
        &self.scope
    }

    /// The endpoint directory name: enough of the digest to be unique across
    /// a user's services while keeping socket paths short.
    pub fn endpoint_name(&self) -> String {
        hex(&self.digest[..8])
    }

    /// The comparable record both ends exchange in the handshake.
    pub fn record(&self) -> IdentityRecord {
        IdentityRecord {
            scheme_version: self.image.scheme_version(),
            architecture: format!("{:?}", self.image.architecture()),
            scheme: format!("{:?}", self.image.scheme()),
            image_hex: hex(self.image.as_bytes()),
            service_hex: hex(&self.digest),
        }
    }

    fn hello(&self) -> protocol::Hello {
        protocol::Hello {
            protocol_version: DAEMON_PROTOCOL_VERSION,
            identity: self.record(),
        }
    }
}

/// Digest the service identity inputs. Each field is length-prefixed so no
/// two input tuples share bytes, and the domain tag pins the layout.
fn service_digest(uid: u32, scope: &DaemonScope, image: &RunningImageIdentity) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    };
    field(b"rue-daemon-service-identity");
    field(&DAEMON_PROTOCOL_VERSION.to_be_bytes());
    field(&uid.to_be_bytes());
    field(scope.directory.as_os_str().as_encoded_bytes());
    field(scope.isolation.as_deref().unwrap_or("").as_bytes());
    field(&image.scheme_version().to_be_bytes());
    field(format!("{:?}", image.architecture()).as_bytes());
    field(format!("{:?}", image.scheme()).as_bytes());
    field(image.as_bytes());
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The user-private directory every service endpoint lives under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointRoot(PathBuf);

impl EndpointRoot {
    /// Resolve the root: [`ENDPOINT_ROOT_ENV`] when set, otherwise `rue/`
    /// under `XDG_RUNTIME_DIR`, otherwise `rue-daemon-<uid>` under the system
    /// temporary directory. Nothing is created here; see [`Self::ensure`].
    pub fn resolve() -> Result<Self, DaemonError> {
        if let Some(explicit) = std::env::var_os(ENDPOINT_ROOT_ENV) {
            if explicit.is_empty() {
                return Err(DaemonError::InvalidConfiguration(format!(
                    "{ENDPOINT_ROOT_ENV} is set but empty"
                )));
            }
            return Ok(Self(PathBuf::from(explicit)));
        }
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty())
        {
            return Ok(Self(PathBuf::from(runtime).join("rue")));
        }
        Ok(Self(
            std::env::temp_dir().join(format!("rue-daemon-{}", current_uid())),
        ))
    }

    /// Use an explicit root, for callers that carry one rather than reading
    /// the environment (a launched service, a test).
    pub fn explicit(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Create the root if needed and verify it is private to this user.
    fn ensure(&self) -> Result<(), DaemonError> {
        ensure_private_dir(&self.0)
    }
}

/// Create `path` as a 0700 directory owned by this user, or verify an
/// existing one is. A directory another user owns or can traverse is refused:
/// the socket inside it would be reachable by them.
fn ensure_private_dir(path: &Path) -> Result<(), DaemonError> {
    match fs::metadata(path) {
        Ok(metadata) => verify_private_dir(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent).map_err(|error| {
                    DaemonError::io(format!("creating {}", parent.display()), error)
                })?;
            }
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                // Lost a race with another client creating it: fine, verify it.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(DaemonError::io(
                        format!("creating {}", path.display()),
                        error,
                    ));
                }
            }
            let metadata = fs::metadata(path).map_err(|error| {
                DaemonError::io(format!("inspecting {}", path.display()), error)
            })?;
            verify_private_dir(path, &metadata)
        }
        Err(error) => Err(DaemonError::io(
            format!("inspecting {}", path.display()),
            error,
        )),
    }
}

fn verify_private_dir(path: &Path, metadata: &fs::Metadata) -> Result<(), DaemonError> {
    if !metadata.is_dir() {
        return Err(DaemonError::InsecureEndpoint(
            path.to_path_buf(),
            "it is not a directory".into(),
        ));
    }
    if metadata.uid() != current_uid() {
        return Err(DaemonError::InsecureEndpoint(
            path.to_path_buf(),
            format!(
                "it is owned by uid {}, not the current user {}",
                metadata.uid(),
                current_uid()
            ),
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(DaemonError::InsecureEndpoint(
            path.to_path_buf(),
            format!(
                "its mode {:o} lets other users reach it; it must be 0700",
                metadata.permissions().mode() & 0o777
            ),
        ));
    }
    Ok(())
}

/// The files of one service endpoint.
#[derive(Clone, Debug)]
pub struct Endpoint {
    directory: PathBuf,
    socket: PathBuf,
    service_lock: PathBuf,
    startup_lock: PathBuf,
    record: PathBuf,
}

impl Endpoint {
    fn locate(root: &EndpointRoot, identity: &ServiceIdentity) -> Result<Self, DaemonError> {
        let directory = root.path().join(identity.endpoint_name());
        let socket = directory.join("socket");
        if socket.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            return Err(DaemonError::InvalidConfiguration(format!(
                "daemon socket path {} is {} bytes; Unix sockets allow {MAX_SOCKET_PATH_BYTES}. \
                 Set {ENDPOINT_ROOT_ENV} to a shorter private directory",
                socket.display(),
                socket.as_os_str().len()
            )));
        }
        Ok(Self {
            service_lock: directory.join("service.lock"),
            startup_lock: directory.join("startup.lock"),
            record: directory.join("service.json"),
            socket,
            directory,
        })
    }

    /// Locate and create the endpoint, verifying the root and the directory
    /// are private to this user.
    fn prepare(root: &EndpointRoot, identity: &ServiceIdentity) -> Result<Self, DaemonError> {
        root.ensure()?;
        let endpoint = Self::locate(root, identity)?;
        ensure_private_dir(&endpoint.directory)?;
        Ok(endpoint)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Remove the socket and record a dead service left behind. Only valid
    /// while the caller holds proof nobody is alive (the service lock, or the
    /// startup lock plus a free service lock).
    /// The record a service left when a compiler panic ended it, if any. A
    /// crash record outlives the service it describes so the client whose
    /// request it ended can still read it; the next crash overwrites it.
    pub fn crash_record(&self) -> Option<CrashRecord> {
        let bytes = fs::read(self.directory.join(service::CRASH_RECORD_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn clear_stale(&self) -> Result<(), DaemonError> {
        for path in [&self.socket, &self.record] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(DaemonError::io(
                        format!("removing {}", path.display()),
                        error,
                    ));
                }
            }
        }
        Ok(())
    }
}

/// An advisory exclusive lock on a file, released when dropped.
struct FileLock {
    _file: File,
}

impl FileLock {
    fn open(path: &Path) -> Result<File, DaemonError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)
            .map_err(|error| DaemonError::io(format!("opening {}", path.display()), error))
    }

    /// Take the lock now, or report `None` when someone else holds it.
    fn try_exclusive(path: &Path) -> Result<Option<Self>, DaemonError> {
        let file = Self::open(path)?;
        // SAFETY: flock on an owned, open descriptor.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        Err(DaemonError::io(
            format!("locking {}", path.display()),
            error,
        ))
    }

    /// Take the lock, polling until `deadline`.
    fn exclusive_by(path: &Path, deadline: Instant) -> Result<Option<Self>, DaemonError> {
        loop {
            if let Some(lock) = Self::try_exclusive(path)? {
                return Ok(Some(lock));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            sleep(POLL_INTERVAL);
        }
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// What probing an endpoint found.
enum Probe {
    /// A compatible service answered the handshake.
    Live(Box<client::Connection>),
    /// No service holds the endpoint. Any socket or record there is stale.
    Absent,
    /// A service holds the endpoint's lock but does not answer yet.
    Starting,
}

fn probe(endpoint: &Endpoint, identity: &ServiceIdentity) -> Result<Probe, DaemonError> {
    match client::connect(&endpoint.socket, &identity.hello(), HANDSHAKE_TIMEOUT) {
        Ok(connection) => Ok(Probe::Live(Box::new(connection))),
        Err(client::ConnectError::NoSocket | client::ConnectError::Refused) => {
            match FileLock::try_exclusive(&endpoint.service_lock)? {
                // Holding the lock ourselves proves nobody is alive; drop it
                // right away so a starting service can take it.
                Some(_free) => Ok(Probe::Absent),
                None => Ok(Probe::Starting),
            }
        }
        Err(client::ConnectError::Rejected(reason)) => Err(DaemonError::Protocol(format!(
            "the service at {} refused this compiler: {reason}",
            endpoint.socket.display()
        ))),
        Err(client::ConnectError::Insecure(reason)) => Err(DaemonError::InsecureEndpoint(
            endpoint.socket.clone(),
            reason,
        )),
        Err(client::ConnectError::Protocol(reason)) => Err(DaemonError::Protocol(reason)),
        Err(client::ConnectError::Io(error)) => Err(DaemonError::io(
            format!("connecting to {}", endpoint.socket.display()),
            error,
        )),
    }
}

/// Options for [`start`].
#[derive(Clone, Debug)]
pub struct StartOptions {
    pub idle_timeout: Duration,
    pub startup_timeout: Duration,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        }
    }
}

/// What a launcher needs to bring a service up for an endpoint.
#[derive(Clone, Debug)]
pub struct LaunchPlan {
    pub scope: DaemonScope,
    pub root: EndpointRoot,
    pub idle_timeout: Duration,
}

/// A service being brought up, so `start` can tell an early death from a slow
/// start.
pub trait LaunchedService {
    /// `Some(reason)` once the service has ended without becoming ready.
    fn ended(&mut self) -> Option<String>;
}

/// How `start` brings a service up. The command-line driver launches a
/// detached process running `rue daemon serve`; tests may run the service on
/// a thread of their own process.
pub trait ServiceLauncher {
    fn launch(&self, plan: &LaunchPlan) -> Result<Box<dyn LaunchedService>, DaemonError>;
}

/// The result of [`start`].
#[derive(Debug)]
pub struct StartOutcome {
    pub service: ServiceInfo,
    /// Whether this call launched the service, as opposed to finding one.
    pub launched: bool,
}

/// Use or start the service for `scope` (ADR-0085 §5): serialize with the
/// startup lock, believe only a service that answers the handshake, recover a
/// stale endpoint whose lock nobody holds, launch, and wait a bounded time for
/// readiness.
pub fn start(
    scope: DaemonScope,
    root: &EndpointRoot,
    options: &StartOptions,
    launcher: &dyn ServiceLauncher,
) -> Result<StartOutcome, DaemonError> {
    let started = start_connection(scope, root, options, launcher)?;
    Ok(StartOutcome {
        service: started.connection.service().clone(),
        launched: started.launched,
    })
}

/// A live, identity-checked connection to the service `start_connection`
/// used or launched, ready to carry one build.
pub struct StartedConnection {
    pub connection: Connection,
    pub launched: bool,
    /// The endpoint the service holds, where a crash record would be found.
    pub endpoint: Endpoint,
}

/// [`start`], keeping the handshake's connection for the caller's request
/// instead of closing it: the client that compiles through the service
/// submits on the very connection that proved the service is the right one.
pub fn start_connection(
    scope: DaemonScope,
    root: &EndpointRoot,
    options: &StartOptions,
    launcher: &dyn ServiceLauncher,
) -> Result<StartedConnection, DaemonError> {
    let identity = ServiceIdentity::current(scope)?;
    let endpoint = Endpoint::prepare(root, &identity)?;
    let began = Instant::now();
    let deadline = began + options.startup_timeout;
    let Some(_startup) = FileLock::exclusive_by(&endpoint.startup_lock, deadline)? else {
        return Err(DaemonError::StartupTimeout {
            waited: began.elapsed(),
            detail: ": another start is still holding the startup lock".into(),
        });
    };
    let mut launched: Option<Box<dyn LaunchedService>> = None;
    loop {
        match probe(&endpoint, &identity)? {
            Probe::Live(connection) => {
                return Ok(StartedConnection {
                    connection: *connection,
                    launched: launched.is_some(),
                    endpoint,
                });
            }
            Probe::Absent => match launched.as_mut() {
                Some(service) => {
                    if let Some(reason) = service.ended() {
                        return Err(DaemonError::StartupTimeout {
                            waited: began.elapsed(),
                            detail: format!(": the launched service ended first ({reason})"),
                        });
                    }
                }
                None => {
                    endpoint.clear_stale()?;
                    launched = Some(launcher.launch(&LaunchPlan {
                        scope: identity.scope().clone(),
                        root: root.clone(),
                        idle_timeout: options.idle_timeout,
                    })?);
                }
            },
            Probe::Starting => {}
        }
        if Instant::now() >= deadline {
            let detail = match launched.as_mut().and_then(|service| service.ended()) {
                Some(reason) => format!(": the launched service ended ({reason})"),
                None if launched.is_some() => ": it holds the endpoint but never answered".into(),
                None => ": another service holds the endpoint but never answered".into(),
            };
            return Err(DaemonError::StartupTimeout {
                waited: began.elapsed(),
                detail,
            });
        }
        sleep(POLL_INTERVAL);
    }
}

/// Report the service for `scope`, or `None` when none answers. Never
/// starts one and creates nothing (ADR-0085 §2).
pub fn status(
    scope: DaemonScope,
    root: &EndpointRoot,
) -> Result<Option<StatusReport>, DaemonError> {
    let identity = ServiceIdentity::current(scope)?;
    let endpoint = Endpoint::locate(root, &identity)?;
    if !endpoint.directory.exists() {
        return Ok(None);
    }
    match probe(&endpoint, &identity)? {
        Probe::Live(mut connection) => connection
            .status()
            .map(Some)
            .map_err(|error| DaemonError::Protocol(error.to_string())),
        Probe::Absent | Probe::Starting => Ok(None),
    }
}

/// The result of [`stop`].
#[derive(Debug, PartialEq, Eq)]
pub enum StopOutcome {
    NotRunning,
    Stopped { pid: u32 },
}

/// Stop the service for `scope` and wait for it to release its endpoint.
/// Never starts one; a service that is still starting is waited for and then
/// stopped, so `stop` after a racing `start` leaves nothing behind.
pub fn stop(
    scope: DaemonScope,
    root: &EndpointRoot,
    timeout: Duration,
) -> Result<StopOutcome, DaemonError> {
    let identity = ServiceIdentity::current(scope)?;
    let endpoint = Endpoint::locate(root, &identity)?;
    if !endpoint.directory.exists() {
        return Ok(StopOutcome::NotRunning);
    }
    let deadline = Instant::now() + timeout;
    let mut connection = loop {
        match probe(&endpoint, &identity)? {
            Probe::Live(connection) => break connection,
            Probe::Absent => return Ok(StopOutcome::NotRunning),
            Probe::Starting => {
                if Instant::now() >= deadline {
                    return Err(DaemonError::StartupTimeout {
                        waited: timeout,
                        detail: ": a starting service never answered, so it could not be stopped"
                            .into(),
                    });
                }
                sleep(POLL_INTERVAL);
            }
        }
    };
    let pid = connection.service().pid;
    connection
        .stop()
        .map_err(|error| DaemonError::Protocol(error.to_string()))?;
    drop(connection);
    // The service releases its lock as it exits; that is the completion
    // signal, not the acknowledgement above.
    match FileLock::exclusive_by(&endpoint.service_lock, deadline)? {
        Some(_free) => Ok(StopOutcome::Stopped { pid }),
        None => Err(DaemonError::StartupTimeout {
            waited: timeout,
            detail: format!(
                ": service pid {pid} acknowledged the stop but still holds its endpoint"
            ),
        }),
    }
}

/// Run the service for `scope` in this process until it is stopped or idles
/// out. This is the body of `rue daemon serve`; a client never calls it.
pub fn serve(
    scope: DaemonScope,
    root: &EndpointRoot,
    idle_timeout: Duration,
    executor: Box<dyn BuildExecutor>,
) -> Result<ServeExit, DaemonError> {
    let identity = ServiceIdentity::current(scope)?;
    let endpoint = Endpoint::prepare(root, &identity)?;
    let Some(_service_lock) = FileLock::try_exclusive(&endpoint.service_lock)? else {
        return Ok(ServeExit::AlreadyRunning);
    };
    // Holding the service lock proves whatever socket is there is dead, and
    // that any crash record there describes a service that is gone.
    endpoint.clear_stale()?;
    service::install_panic_recorder(endpoint.directory.clone());
    let listener = std::os::unix::net::UnixListener::bind(&endpoint.socket).map_err(|error| {
        DaemonError::io(format!("binding {}", endpoint.socket.display()), error)
    })?;
    fs::set_permissions(&endpoint.socket, fs::Permissions::from_mode(0o600)).map_err(|error| {
        DaemonError::io(format!("securing {}", endpoint.socket.display()), error)
    })?;
    let info = ServiceInfo {
        pid: std::process::id(),
        protocol_version: DAEMON_PROTOCOL_VERSION,
        identity: identity.record(),
        scope_directory: identity.scope().directory().display().to_string(),
        isolation: identity.scope().isolation().map(str::to_owned),
        endpoint_directory: endpoint.directory.display().to_string(),
        started_at_unix_ms: unix_millis(),
    };
    write_record(&endpoint, &info)?;
    let exit = service::run(listener, info, idle_timeout, executor);
    endpoint.clear_stale()?;
    Ok(exit)
}

fn write_record(endpoint: &Endpoint, info: &ServiceInfo) -> Result<(), DaemonError> {
    let pending = endpoint.directory.join("service.json.pending");
    let body = serde_json::to_vec_pretty(info).map_err(io::Error::other);
    let body = body.map_err(|error| DaemonError::io("encoding the service record", error))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&pending)
        .map_err(|error| DaemonError::io(format!("writing {}", pending.display()), error))?;
    io::Write::write_all(&mut file, &body)
        .map_err(|error| DaemonError::io(format!("writing {}", pending.display()), error))?;
    fs::rename(&pending, &endpoint.record).map_err(|error| {
        DaemonError::io(format!("publishing {}", endpoint.record.display()), error)
    })
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Launches `rue daemon serve` as a detached process: its own session, null
/// standard streams, cwd at the endpoint directory rather than in any
/// worktree, and the endpoint root passed explicitly so it never depends on
/// the launching client's environment beyond the executable itself.
pub struct ProcessLauncher {
    executable: PathBuf,
}

impl ProcessLauncher {
    /// Launch the image this process is running. The launched service proves
    /// its own identity in the handshake, so a replacement installed between
    /// this call and its exec is refused rather than trusted by path.
    pub fn current_executable() -> Result<Self, DaemonError> {
        let executable = std::env::current_exe()
            .map_err(|error| DaemonError::io("locating the compiler executable", error))?;
        Ok(Self { executable })
    }
}

struct LaunchedProcess(Child);

impl LaunchedService for LaunchedProcess {
    fn ended(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(format!("exit status {status}")),
            Ok(None) => None,
            Err(error) => Some(format!("cannot observe the service process: {error}")),
        }
    }
}

impl ServiceLauncher for ProcessLauncher {
    fn launch(&self, plan: &LaunchPlan) -> Result<Box<dyn LaunchedService>, DaemonError> {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(&self.executable);
        command
            .arg("daemon")
            .arg("serve")
            .arg("--scope")
            .arg(plan.scope.directory())
            .arg("--endpoint-root")
            .arg(plan.root.path())
            .arg("--idle-timeout-ms")
            .arg(plan.idle_timeout.as_millis().to_string());
        if let Some(isolation) = plan.scope.isolation() {
            command.arg("--isolation").arg(isolation);
        }
        command
            .current_dir(plan.root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and touches no parent state.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|error| DaemonError::io("launching the compiler daemon", error))?;
        Ok(Box::new(LaunchedProcess(child)))
    }
}

#[cfg(test)]
mod tests;
