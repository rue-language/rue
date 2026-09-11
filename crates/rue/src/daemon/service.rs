//! The service loop: accept connections, verify the peer, answer control
//! requests, run build requests through one compiler owner, and retire on
//! stop or idleness.
//!
//! Control messages must stay serviceable while compiler work runs
//! (ADR-0085 §6), so every connection is handled on its own thread and the
//! accept loop never blocks on one. Compiler work is another matter: exactly
//! one request compiles at a time, on the owner thread that holds the
//! retained host, and admitted requests wait in a bounded FIFO queue. A
//! connection thread only submits, watches its peer for disconnection, and
//! relays the owner's answer.

use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use rue_compiler::unstable::CompilationCancellation;

use super::protocol::{
    BuildReply, BuildRequest, BuildResult, CrashRecord, DAEMON_PROTOCOL_VERSION, Hello, HelloReply,
    MAX_CONTROL_FRAME_BYTES, Request, RequestBody, RequestSummary, Response, ResponseBody,
    ServiceInfo, StatusReport, read_frame, write_chunks, write_frame,
};

/// Why the service loop returned.
#[derive(Debug, PartialEq, Eq)]
pub enum ServeExit {
    /// A client asked it to stop.
    Stopped,
    /// No connection arrived within the idle timeout.
    Idle,
    /// Another service already held the endpoint; nothing was served.
    AlreadyRunning,
}

/// How long a connection has to complete its handshake.
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the accept loop wakes to check for stop and idleness.
const ACCEPT_POLL: Duration = Duration::from_millis(25);
/// How many admitted requests may wait for the compiler owner. One compiles;
/// this many more are accepted; the next is rejected before any work so its
/// client may compile directly (ADR-0085 §6).
pub const MAX_QUEUED_REQUESTS: u32 = 8;

/// The name of the file a service writes when a compiler panic ends it.
pub(super) const CRASH_RECORD_FILE: &str = "crash.json";

/// What executes admitted build requests: the retained-host compile path the
/// command-line driver supplies. The service owns the queue, the connection,
/// and the cancellation; the executor owns the compiler.
pub trait BuildExecutor: Send {
    /// Run one request to its owned result. The executor observes
    /// `cancellation` through the compiler's own checkpoints and reports
    /// [`BuildResult::Canceled`] when it fires.
    fn build(
        &mut self,
        request: &BuildRequest,
        cancellation: &CompilationCancellation,
    ) -> BuildOutput;

    /// How many compiler hosts the executor retains right now.
    fn retained_hosts(&self) -> u32;
}

/// An executor's answer: the result frame and the bytes that follow it when
/// the result is [`BuildResult::Ready`].
pub struct BuildOutput {
    pub result: BuildResult,
    pub bytes: Vec<u8>,
}

impl BuildOutput {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            result: BuildResult::Failed {
                message: message.into(),
                internal: false,
            },
            bytes: Vec::new(),
        }
    }
}

/// One admitted request on its way to the compiler owner.
struct Job {
    ticket: u64,
    request: BuildRequest,
    cancellation: CompilationCancellation,
    reply: mpsc::Sender<BuildOutput>,
}

struct ActiveRequest {
    ticket: u64,
    root_source: String,
    working_directory: String,
    started: Instant,
}

struct Shared {
    info: ServiceInfo,
    started: Instant,
    idle_timeout: Duration,
    stop: AtomicBool,
    connections: AtomicU32,
    last_activity: Mutex<Instant>,
    /// Where admitted jobs go; `None` once the service is stopping, so a late
    /// request is rejected rather than accepted and abandoned.
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    next_ticket: AtomicU64,
    /// Admitted requests the owner has not started yet.
    queued: AtomicU32,
    active: Mutex<Option<ActiveRequest>>,
    retained_hosts: AtomicU32,
}

/// The ticket of the request the compiler owner is executing, `0` when none,
/// read by the panic recorder so a crash names the request it ended.
static ACTIVE_TICKET: AtomicU64 = AtomicU64::new(0);

impl Shared {
    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .elapsed()
    }

    fn report(&self) -> StatusReport {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map(|active| RequestSummary {
                ticket: active.ticket,
                root_source: active.root_source.clone(),
                working_directory: active.working_directory.clone(),
                elapsed_ms: active.started.elapsed().as_millis() as u64,
            });
        StatusReport {
            service: self.info.clone(),
            uptime_ms: self.started.elapsed().as_millis() as u64,
            idle_timeout_ms: self.idle_timeout.as_millis() as u64,
            connections: self.connections.load(Ordering::Acquire),
            active_request: active,
            queued_requests: self.queued.load(Ordering::Acquire),
            retained_hosts: self.retained_hosts.load(Ordering::Acquire),
        }
    }

    fn set_active(&self, active: Option<ActiveRequest>) {
        ACTIVE_TICKET.store(
            active.as_ref().map_or(0, |active| active.ticket),
            Ordering::Release,
        );
        *self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = active;
    }
}

/// Serve `listener` until stopped or idle. The listener is already bound and
/// secured; the caller owns the endpoint lock for the whole call.
pub(super) fn run(
    listener: UnixListener,
    info: ServiceInfo,
    idle_timeout: Duration,
    executor: Box<dyn BuildExecutor>,
) -> ServeExit {
    // A client that vanishes mid-write must be a failed connection, never a
    // dead service.
    // SAFETY: changing this process's SIGPIPE disposition has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    if listener.set_nonblocking(true).is_err() {
        return ServeExit::Idle;
    }
    let (jobs, queue) = mpsc::channel::<Job>();
    let shared = Arc::new(Shared {
        info,
        started: Instant::now(),
        idle_timeout,
        stop: AtomicBool::new(false),
        connections: AtomicU32::new(0),
        last_activity: Mutex::new(Instant::now()),
        jobs: Mutex::new(Some(jobs)),
        next_ticket: AtomicU64::new(0),
        queued: AtomicU32::new(0),
        active: Mutex::new(None),
        retained_hosts: AtomicU32::new(executor.retained_hosts()),
    });
    let owner = {
        let shared = Arc::clone(&shared);
        thread::spawn(move || compiler_owner(queue, executor, &shared))
    };
    let mut handlers: Vec<thread::JoinHandle<()>> = Vec::new();
    let exit = loop {
        if shared.stop.load(Ordering::Acquire) {
            break ServeExit::Stopped;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                shared.touch();
                let shared = Arc::clone(&shared);
                handlers.push(thread::spawn(move || handle_connection(stream, &shared)));
                handlers.retain(|handle| !handle.is_finished());
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if shared.connections.load(Ordering::Acquire) == 0
                    && shared.idle_for() >= shared.idle_timeout
                {
                    break ServeExit::Idle;
                }
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    };
    // No further admissions; the owner drains what was admitted (answering
    // it as a stopping service) and ends when the last sender is gone.
    shared.stop.store(true, Ordering::Release);
    drop(
        shared
            .jobs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take(),
    );
    // Let the stopping client's acknowledgement, every in-flight control
    // reply, and the active request's answer finish before the endpoint is
    // torn down.
    for handle in handlers {
        let _ = handle.join();
    }
    let _ = owner.join();
    exit
}

/// The compiler owner: one request at a time, in admission order.
fn compiler_owner(
    queue: mpsc::Receiver<Job>,
    mut executor: Box<dyn BuildExecutor>,
    shared: &Shared,
) {
    for job in queue {
        shared.queued.fetch_sub(1, Ordering::AcqRel);
        if shared.stop.load(Ordering::Acquire) {
            let _ = job
                .reply
                .send(BuildOutput::failed("the compiler service is stopping"));
            continue;
        }
        if job.cancellation.is_canceled() {
            // The client left before its turn: cancel queued work without
            // starting it (ADR-0085 §6).
            let _ = job.reply.send(BuildOutput {
                result: BuildResult::Canceled,
                bytes: Vec::new(),
            });
            continue;
        }
        shared.set_active(Some(ActiveRequest {
            ticket: job.ticket,
            root_source: job.request.root_source.clone(),
            working_directory: job.request.working_directory.clone(),
            started: Instant::now(),
        }));
        let output = executor.build(&job.request, &job.cancellation);
        shared.set_active(None);
        shared
            .retained_hosts
            .store(executor.retained_hosts(), Ordering::Release);
        shared.touch();
        let _ = job.reply.send(output);
    }
}

/// Install a panic hook that records the panic, and the request it ended,
/// beside the endpoint before the process aborts (ADR-0085 §6). The compiler
/// is built to abort on panic, so the record is the only account the request's
/// client can get of what happened.
pub(super) fn install_panic_recorder(directory: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let ticket = ACTIVE_TICKET.load(Ordering::Acquire);
        let record = CrashRecord {
            pid: std::process::id(),
            ticket: (ticket != 0).then_some(ticket),
            message: panic_message(info.payload()),
            location: info.location().map(|location| location.to_string()),
        };
        write_crash_record(&directory, &record);
        previous(info);
    }));
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic with a non-string payload".to_string()
    }
}

fn write_crash_record(directory: &Path, record: &CrashRecord) {
    let Ok(body) = serde_json::to_vec_pretty(record) else {
        return;
    };
    let pending = directory.join(format!("{CRASH_RECORD_FILE}.pending"));
    if std::fs::write(&pending, body).is_ok() {
        let _ = std::fs::rename(&pending, directory.join(CRASH_RECORD_FILE));
    }
}

/// A connection counted for the whole time its thread runs.
struct ConnectionGuard<'a>(&'a Shared);

impl<'a> ConnectionGuard<'a> {
    fn new(shared: &'a Shared) -> Self {
        shared.connections.fetch_add(1, Ordering::AcqRel);
        Self(shared)
    }
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
        self.0.touch();
    }
}

fn handle_connection(stream: UnixStream, shared: &Shared) {
    let _guard = ConnectionGuard::new(shared);
    let mut stream = stream;
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    // The socket file is 0600 in a 0700 directory, so only this user can
    // reach it; the kernel's peer identity is checked anyway so a
    // misconfigured directory can never widen who the service talks to.
    match peer_uid(&stream) {
        // SAFETY: geteuid has no preconditions and cannot fail.
        Ok(uid) if uid == unsafe { libc::geteuid() } => {}
        _ => return,
    }
    if stream
        .set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT))
        .is_err()
    {
        return;
    }
    let hello: Hello = match read_frame(&mut stream, MAX_CONTROL_FRAME_BYTES) {
        Ok(Some(hello)) => hello,
        Ok(None) | Err(_) => return,
    };
    if let Some(reason) = rejection(&hello, &shared.info) {
        let _ = write_frame(&mut stream, &HelloReply::Rejected { reason });
        return;
    }
    if write_frame(
        &mut stream,
        &HelloReply::Welcome {
            service: shared.info.clone(),
        },
    )
    .is_err()
    {
        return;
    }
    // Requests may legitimately wait: a build request holds the connection
    // for its whole run.
    if stream.set_read_timeout(None).is_err() {
        return;
    }
    loop {
        let request: Request = match read_frame(&mut stream, MAX_CONTROL_FRAME_BYTES) {
            Ok(Some(request)) => request,
            Ok(None) | Err(_) => return,
        };
        shared.touch();
        let (body, stop_after) = match request.body {
            RequestBody::Ping => (ResponseBody::Pong, false),
            RequestBody::Status => (
                ResponseBody::Status {
                    report: Box::new(shared.report()),
                },
                false,
            ),
            RequestBody::Stop => (ResponseBody::Stopping, true),
            RequestBody::Build(build) => {
                // A build is the last thing a connection carries.
                serve_build(stream, request.id, *build, shared);
                return;
            }
        };
        let written = write_frame(
            &mut stream,
            &Response {
                id: request.id,
                body,
            },
        );
        if stop_after {
            shared.stop.store(true, Ordering::Release);
            return;
        }
        if written.is_err() {
            return;
        }
    }
}

/// Admit or reject one build, then relay the owner's answer. Between the two,
/// a watcher thread reads the connection so a client that disconnects trips
/// the request's cancellation whether it is queued or compiling.
fn serve_build(mut stream: UnixStream, request_id: u64, request: BuildRequest, shared: &Shared) {
    let sender = shared
        .jobs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let refuse = |stream: &mut UnixStream, reason: String| {
        let _ = write_frame(
            stream,
            &Response {
                id: request_id,
                body: ResponseBody::Rejected { reason },
            },
        );
    };
    let Some(sender) = sender else {
        refuse(&mut stream, "the compiler service is stopping".into());
        return;
    };
    // Reserve a queue place before answering, so two connections cannot both
    // be admitted into the last one.
    let mut queued_before = shared.queued.load(Ordering::Acquire);
    loop {
        if queued_before >= MAX_QUEUED_REQUESTS {
            refuse(
                &mut stream,
                format!("the compiler service already has {MAX_QUEUED_REQUESTS} requests waiting"),
            );
            return;
        }
        match shared.queued.compare_exchange(
            queued_before,
            queued_before + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(current) => queued_before = current,
        }
    }
    let ticket = shared.next_ticket.fetch_add(1, Ordering::AcqRel) + 1;
    let active = u32::from(
        shared
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_some(),
    );
    let cancellation = CompilationCancellation::new();
    let (reply, answer) = mpsc::channel();
    let job = Job {
        ticket,
        request,
        cancellation: cancellation.clone(),
        reply,
    };
    if sender.send(job).is_err() {
        shared.queued.fetch_sub(1, Ordering::AcqRel);
        refuse(&mut stream, "the compiler service is stopping".into());
        return;
    }
    if write_frame(
        &mut stream,
        &Response {
            id: request_id,
            body: ResponseBody::Accepted {
                ticket,
                queued_ahead: queued_before + active,
            },
        },
    )
    .is_err()
    {
        cancellation.cancel();
        return;
    }
    // The client sends nothing more until it has its answer, so the next
    // thing the watcher reads is the end of the connection.
    if let Ok(mut reader) = stream.try_clone() {
        let cancellation = cancellation.clone();
        thread::spawn(move || {
            loop {
                match read_frame::<Request>(&mut reader, MAX_CONTROL_FRAME_BYTES) {
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
            cancellation.cancel();
        });
    }
    let output = match answer.recv() {
        Ok(output) => output,
        Err(_) => BuildOutput::failed("the compiler service ended before answering"),
    };
    let BuildOutput { result, bytes } = output;
    let ready = matches!(result, BuildResult::Ready { .. });
    if write_frame(&mut stream, &BuildReply { ticket, result }).is_err() {
        return;
    }
    if ready {
        let _ = write_chunks(&mut stream, &bytes);
    }
}

/// Why a hello is refused, if it is: the protocol version and the full
/// identity record must both match this service's own.
fn rejection(hello: &Hello, info: &ServiceInfo) -> Option<String> {
    if hello.protocol_version != DAEMON_PROTOCOL_VERSION {
        return Some(format!(
            "protocol version {} differs from the service's {}",
            hello.protocol_version, DAEMON_PROTOCOL_VERSION
        ));
    }
    if hello.identity != info.identity {
        return Some(format!(
            "compiler identity {} differs from the service's {}",
            short(&hello.identity.service_hex),
            short(&info.identity.service_hex)
        ));
    }
    None
}

fn short(hex: &str) -> &str {
    hex.get(..16).unwrap_or(hex)
}

/// The effective uid of the process on the other end of `stream`.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `length` bytes into `credentials`,
    // which is exactly the struct SO_PEERCRED fills.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast::<libc::c_void>(),
            &raw mut length,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: getpeereid writes one uid and one gid into the provided slots.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_uid(_stream: &UnixStream) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer credentials are not available on this platform",
    ))
}
