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

use std::io::{self, Read, Write};
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
    MAX_CONTROL_FRAME_BYTES, MAX_RESPONSE_BYTES, Request, RequestBody, RequestSummary,
    ResourcePolicy, ResourcePressure, Response, ResponseBody, ServiceInfo, StatusReport,
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
/// A connected peer must continue issuing control requests. This prevents a
/// client that completed the handshake and then goes silent from pinning a
/// handler forever.
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Socket writes are bounded so stop can join every handler and completed
/// responses cannot become owners of the service lifetime.
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const REFUSAL_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const WATCH_READ_TIMEOUT: Duration = Duration::from_millis(100);
/// How often the accept loop wakes to check for stop and idleness.
const ACCEPT_POLL: Duration = Duration::from_millis(25);
/// How many admitted requests may wait for the compiler owner. One compiles;
/// this many more are accepted; the next is rejected before any work so its
/// client may compile directly (ADR-0085 §6).
pub const MAX_QUEUED_REQUESTS: u32 = 8;
/// Maximum accepted sockets, including status/control clients and builds.
pub const MAX_CONNECTIONS: u32 = 32;
/// One additional slot is reserved for status/stop control when build peers
/// fill the ordinary connection budget.
const MAX_ACCEPTED_CONNECTIONS: u32 = MAX_CONNECTIONS + 1;

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

    /// Retained query charge for status and qualification accounting.
    fn retained_charge_bytes(&self) -> u64 {
        0
    }

    /// Retained dependency pins for status and qualification accounting.
    fn dependency_pins(&self) -> u64 {
        0
    }

    /// Configured retained-query byte budget, for the status policy surface.
    fn retained_byte_budget(&self) -> u64 {
        0
    }

    /// Configured dependency-pin budget, for the status policy surface.
    fn dependency_pin_budget(&self) -> u64 {
        0
    }

    /// Current source input bytes retained by the executor.
    fn source_bytes(&self) -> u64 {
        0
    }

    /// Current source file count retained by the executor.
    fn source_files(&self) -> u32 {
        0
    }

    /// Evict an idle host after its completed request has been accounted for.
    fn enforce_retention_budget(&mut self) {}
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
    reply: mpsc::Sender<LeasedOutput>,
}

struct LeasedOutput {
    encoded_result: Vec<u8>,
    bytes: Vec<u8>,
    ready: bool,
    _lease: Option<ResponseLease>,
}

/// Owns the aggregate response charge from completion handoff through the
/// final socket write. Dropping a disconnected or rejected response releases
/// it immediately, including the serialized result copy.
struct ResponseLease {
    shared: Arc<Shared>,
    amount: u64,
}

impl Drop for ResponseLease {
    fn drop(&mut self) {
        self.shared
            .response_bytes
            .fetch_sub(self.amount, Ordering::AcqRel);
    }
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
    active_cancellation: Mutex<Option<CompilationCancellation>>,
    retained_hosts: AtomicU32,
    response_bytes: AtomicU64,
    peak_response_bytes: AtomicU64,
    source_bytes: AtomicU64,
    source_files: AtomicU32,
    retained_charge_bytes: AtomicU64,
    dependency_pins: AtomicU64,
    peak_retained_charge_bytes: AtomicU64,
    peak_dependency_pins: AtomicU64,
    policy: ResourcePolicy,
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
            resource_policy: self.policy.clone(),
            resource_pressure: ResourcePressure {
                connections: self.connections.load(Ordering::Acquire),
                queued_requests: self.queued.load(Ordering::Acquire),
                retained_hosts: self.retained_hosts.load(Ordering::Acquire),
                response_bytes: self.response_bytes.load(Ordering::Acquire),
                peak_response_bytes: self.peak_response_bytes.load(Ordering::Acquire),
                source_bytes: self.source_bytes.load(Ordering::Acquire),
                source_files: self.source_files.load(Ordering::Acquire),
                retained_charge_bytes: self.retained_charge_bytes.load(Ordering::Acquire),
                dependency_pins: self.dependency_pins.load(Ordering::Acquire),
                peak_retained_charge_bytes: self.peak_retained_charge_bytes.load(Ordering::Acquire),
                peak_dependency_pins: self.peak_dependency_pins.load(Ordering::Acquire),
            },
        }
    }

    fn set_active(
        &self,
        active: Option<ActiveRequest>,
        cancellation: Option<CompilationCancellation>,
    ) {
        ACTIVE_TICKET.store(
            active.as_ref().map_or(0, |active| active.ticket),
            Ordering::Release,
        );
        *self
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = active;
        *self
            .active_cancellation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = cancellation;
    }

    fn cancel_active(&self) {
        let cancellation = self
            .active_cancellation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
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
    let policy = ResourcePolicy {
        max_connections: MAX_ACCEPTED_CONNECTIONS,
        max_queued_requests: MAX_QUEUED_REQUESTS,
        max_retained_hosts: 1,
        max_retained_charge_bytes: executor.retained_byte_budget(),
        max_dependency_pins: executor.dependency_pin_budget(),
        max_response_bytes: MAX_RESPONSE_BYTES as u64,
        control_read_timeout_ms: CONTROL_READ_TIMEOUT.as_millis() as u64,
        response_write_timeout_ms: RESPONSE_WRITE_TIMEOUT.as_millis() as u64,
    };
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
        active_cancellation: Mutex::new(None),
        retained_hosts: AtomicU32::new(executor.retained_hosts()),
        response_bytes: AtomicU64::new(0),
        peak_response_bytes: AtomicU64::new(0),
        source_bytes: AtomicU64::new(executor.source_bytes()),
        source_files: AtomicU32::new(executor.source_files()),
        retained_charge_bytes: AtomicU64::new(executor.retained_charge_bytes()),
        dependency_pins: AtomicU64::new(executor.dependency_pins()),
        peak_retained_charge_bytes: AtomicU64::new(executor.retained_charge_bytes()),
        peak_dependency_pins: AtomicU64::new(executor.dependency_pins()),
        policy,
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
                let Some(guard) = ConnectionGuard::new(&shared) else {
                    // The cap is acquired before spawning, so an accept burst
                    // cannot create an unbounded number of blocked handlers.
                    let mut stream = stream;
                    let _ = write_frame_with_timeout_value(
                        &mut stream,
                        &HelloReply::Rejected {
                            reason: format!(
                                "the compiler service already has {MAX_ACCEPTED_CONNECTIONS} connections"
                            ),
                        },
                        REFUSAL_WRITE_TIMEOUT,
                    );
                    continue;
                };
                let shared = Arc::clone(&shared);
                handlers.push(thread::spawn(move || {
                    handle_connection(stream, &shared, guard)
                }));
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
    shared.cancel_active();
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
    shared: &Arc<Shared>,
) {
    for job in queue {
        // A completed response remains charged until its handler finishes
        // writing or drops it. Hold the owner at this boundary so queued
        // requests cannot create an unbounded backlog of completed outputs
        // behind a slow reader.
        while shared.response_bytes.load(Ordering::Acquire) != 0 {
            thread::sleep(Duration::from_millis(5));
        }
        shared.queued.fetch_sub(1, Ordering::AcqRel);
        if shared.stop.load(Ordering::Acquire) {
            let _ = job.reply.send(leased_output(
                job.ticket,
                BuildOutput::failed("the compiler service is stopping"),
                shared,
            ));
            continue;
        }
        if job.cancellation.is_canceled() {
            // The client left before its turn: cancel queued work without
            // starting it (ADR-0085 §6).
            let _ = job.reply.send(leased_output(
                job.ticket,
                BuildOutput {
                    result: BuildResult::Canceled,
                    bytes: Vec::new(),
                },
                shared,
            ));
            continue;
        }
        shared.set_active(
            Some(ActiveRequest {
                ticket: job.ticket,
                root_source: job.request.root_source.clone(),
                working_directory: job.request.working_directory.clone(),
                started: Instant::now(),
            }),
            Some(job.cancellation.clone()),
        );
        let output = executor.build(&job.request, &job.cancellation);
        shared.set_active(None, None);
        shared
            .source_bytes
            .store(executor.source_bytes(), Ordering::Release);
        shared
            .source_files
            .store(executor.source_files(), Ordering::Release);
        shared
            .retained_hosts
            .store(executor.retained_hosts(), Ordering::Release);
        let retained_charge_bytes = executor.retained_charge_bytes();
        let dependency_pins = executor.dependency_pins();
        shared
            .retained_charge_bytes
            .store(retained_charge_bytes, Ordering::Release);
        shared
            .dependency_pins
            .store(dependency_pins, Ordering::Release);
        update_peak(&shared.peak_retained_charge_bytes, retained_charge_bytes);
        update_peak(&shared.peak_dependency_pins, dependency_pins);
        executor.enforce_retention_budget();
        shared
            .retained_hosts
            .store(executor.retained_hosts(), Ordering::Release);
        shared
            .retained_charge_bytes
            .store(executor.retained_charge_bytes(), Ordering::Release);
        shared
            .dependency_pins
            .store(executor.dependency_pins(), Ordering::Release);
        shared
            .source_bytes
            .store(executor.source_bytes(), Ordering::Release);
        shared
            .source_files
            .store(executor.source_files(), Ordering::Release);
        shared.touch();
        let _ = job.reply.send(leased_output(job.ticket, output, shared));
    }
}

fn update_peak(peak: &AtomicU64, observed: u64) {
    let mut current = peak.load(Ordering::Acquire);
    while observed > current {
        match peak.compare_exchange(current, observed, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
}

fn leased_output(ticket: u64, mut output: BuildOutput, shared: &Arc<Shared>) -> LeasedOutput {
    let ready = match &output.result {
        BuildResult::Ready {
            bytes: announced, ..
        } => *announced == output.bytes.len() as u64,
        _ => false,
    };
    if matches!(output.result, BuildResult::Ready { .. }) && !ready {
        output = BuildOutput::failed("the compiler service produced an invalid response length");
    }
    let ready = matches!(output.result, BuildResult::Ready { .. }) && ready;
    let encoded = encode_build_reply(ticket, output.result);
    let amount = encoded
        .as_ref()
        .ok()
        .and_then(|body| (body.len() as u64).checked_add(output.bytes.len() as u64));
    let Some(amount) = amount else {
        output = BuildOutput::failed(
            "the compiler service response could not fit in its bounded result frame",
        );
        let encoded_result = serde_json::to_vec(&BuildReply {
            ticket,
            result: output.result,
        })
        .unwrap_or_default();
        return LeasedOutput {
            encoded_result,
            bytes: Vec::new(),
            ready: false,
            _lease: None,
        };
    };
    if !reserve_response(
        &shared.response_bytes,
        &shared.peak_response_bytes,
        shared.policy.max_response_bytes,
        amount,
    ) {
        output = BuildOutput::failed(
            "the compiler service has too many completed response bytes in flight",
        );
        let encoded_result = serde_json::to_vec(&BuildReply {
            ticket,
            result: output.result,
        })
        .unwrap_or_default();
        return LeasedOutput {
            encoded_result,
            bytes: Vec::new(),
            ready: false,
            _lease: None,
        };
    }
    let encoded_result = encoded.unwrap_or_else(|_| {
        serde_json::to_vec(&BuildReply {
            ticket,
            result: BuildResult::Failed {
                message: "the compiler service could not serialize its response".into(),
                internal: true,
            },
        })
        .unwrap_or_default()
    });
    LeasedOutput {
        encoded_result,
        bytes: output.bytes,
        ready,
        _lease: Some(ResponseLease {
            shared: Arc::clone(shared),
            amount,
        }),
    }
}

/// Serialize a build result into a capped buffer. `serde_json::to_vec` would
/// allocate an unbounded second copy of a diagnostic/listing payload before
/// the aggregate response limit could reject it.
fn encode_build_reply(ticket: u64, result: BuildResult) -> Result<Vec<u8>, io::Error> {
    let mut buffer = CappedBuffer {
        bytes: Vec::new(),
        limit: MAX_RESPONSE_BYTES,
    };
    serde_json::to_writer(&mut buffer, &BuildReply { ticket, result }).map_err(io::Error::other)?;
    Ok(buffer.bytes)
}

struct CappedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for CappedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(bytes.len()) else {
            return Err(io::Error::other("response serialization size overflow"));
        };
        if next > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response serialization exceeds daemon resource limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
struct ConnectionGuard(Arc<Shared>);

impl ConnectionGuard {
    fn new(shared: &Arc<Shared>) -> Option<Self> {
        let mut current = shared.connections.load(Ordering::Acquire);
        loop {
            if current >= MAX_ACCEPTED_CONNECTIONS {
                return None;
            }
            match shared.connections.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(Arc::clone(shared))),
                Err(next) => current = next,
            }
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
        self.0.touch();
    }
}

fn handle_connection(stream: UnixStream, shared: &Shared, _guard: ConnectionGuard) {
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
    let hello: Hello =
        match read_frame_deadline(&mut stream, MAX_CONTROL_FRAME_BYTES, HANDSHAKE_READ_TIMEOUT) {
            Ok(Some(hello)) => hello,
            Ok(None) | Err(_) => return,
        };
    if let Some(reason) = rejection(&hello, &shared.info) {
        let _ = write_frame_with_timeout(&mut stream, &HelloReply::Rejected { reason });
        return;
    }
    if write_frame_with_timeout(
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
    loop {
        let request: Request =
            match read_frame_deadline(&mut stream, MAX_CONTROL_FRAME_BYTES, CONTROL_READ_TIMEOUT) {
                Ok(Some(request)) => request,
                Ok(None) | Err(_) => return,
            };
        // A stop request on another connection ends continuously-chatty
        // control peers as well, allowing the service to join every handler.
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
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
        let written = write_frame_with_timeout(
            &mut stream,
            &Response {
                id: request.id,
                body,
            },
        );
        if stop_after {
            shared.stop.store(true, Ordering::Release);
            shared.cancel_active();
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
        let _ = write_frame_with_timeout(
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
    if shared.connections.load(Ordering::Acquire) > MAX_CONNECTIONS {
        refuse(
            &mut stream,
            "the compiler service reserves its last connection for control requests".into(),
        );
        return;
    }
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
    if write_frame_with_timeout(
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
    // thing the watcher reads is the end of the connection. A short timeout
    // lets completion join this helper instead of leaking a thread per build.
    let done = Arc::new(AtomicBool::new(false));
    if let Ok(mut reader) = stream.try_clone() {
        let cancellation = cancellation.clone();
        let done_watcher = Arc::clone(&done);
        let watcher = thread::spawn(move || {
            loop {
                match read_watcher_frame(&mut reader) {
                    WatcherRead::Frame => {
                        if done_watcher.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    WatcherRead::Idle => {
                        if done_watcher.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    WatcherRead::Partial | WatcherRead::Closed | WatcherRead::Error => break,
                }
            }
            if !done_watcher.load(Ordering::Acquire) {
                cancellation.cancel();
            }
        });
        let leased = match answer.recv() {
            Ok(output) => output,
            Err(_) => LeasedOutput {
                encoded_result: serde_json::to_vec(&BuildReply {
                    ticket,
                    result: BuildResult::Failed {
                        message: "the compiler service ended before answering".into(),
                        internal: true,
                    },
                })
                .unwrap_or_default(),
                bytes: Vec::new(),
                ready: false,
                _lease: None,
            },
        };
        done.store(true, Ordering::Release);
        let _ = watcher.join();
        relay_build_output(&mut stream, leased);
        return;
    }
    let leased = match answer.recv() {
        Ok(output) => output,
        Err(_) => LeasedOutput {
            encoded_result: serde_json::to_vec(&BuildReply {
                ticket,
                result: BuildResult::Failed {
                    message: "the compiler service ended before answering".into(),
                    internal: true,
                },
            })
            .unwrap_or_default(),
            bytes: Vec::new(),
            ready: false,
            _lease: None,
        },
    };
    relay_build_output(&mut stream, leased);
}

enum WatcherRead {
    Frame,
    Idle,
    Partial,
    Closed,
    Error,
}

/// Read one watcher frame while distinguishing an idle peer from a peer
/// slowly dripping a partial frame. The latter must cancel the request, while
/// an ordinary client waiting for its build answer must remain connected.
fn read_watcher_frame(stream: &mut UnixStream) -> WatcherRead {
    let _ = stream.set_nonblocking(true);
    let deadline = Instant::now() + WATCH_READ_TIMEOUT;
    let mut header = [0_u8; 4];
    let mut offset = 0;
    while offset < header.len() {
        if Instant::now() >= deadline {
            let _ = stream.set_nonblocking(false);
            return if offset == 0 {
                WatcherRead::Idle
            } else {
                WatcherRead::Partial
            };
        }
        match stream.read(&mut header[offset..]) {
            Ok(0) if offset == 0 => {
                let _ = stream.set_nonblocking(false);
                return WatcherRead::Closed;
            }
            Ok(0) => {
                let _ = stream.set_nonblocking(false);
                return WatcherRead::Partial;
            }
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = stream.set_nonblocking(false);
                    return if offset == 0 {
                        WatcherRead::Idle
                    } else {
                        WatcherRead::Partial
                    };
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = stream.set_nonblocking(false);
                return WatcherRead::Error;
            }
        }
    }
    let announced = u32::from_be_bytes(header) as usize;
    if announced > MAX_CONTROL_FRAME_BYTES {
        let _ = stream.set_nonblocking(false);
        return WatcherRead::Error;
    }
    let mut body = vec![0_u8; announced];
    offset = 0;
    while offset < body.len() {
        if Instant::now() >= deadline {
            let _ = stream.set_nonblocking(false);
            return WatcherRead::Partial;
        }
        match stream.read(&mut body[offset..]) {
            Ok(0) => {
                let _ = stream.set_nonblocking(false);
                return WatcherRead::Partial;
            }
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = stream.set_nonblocking(false);
                    return WatcherRead::Partial;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = stream.set_nonblocking(false);
                return WatcherRead::Error;
            }
        }
    }
    let _ = stream.set_nonblocking(false);
    if serde_json::from_slice::<Request>(&body).is_ok() {
        WatcherRead::Frame
    } else {
        WatcherRead::Error
    }
}

/// Read one control frame against an overall deadline. `set_read_timeout`
/// alone bounds each successful byte read and lets a slow-drip peer pin a
/// handler forever, so this path owns the partial-frame buffer and deadline.
fn read_frame_deadline<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
    limit: usize,
    timeout: Duration,
) -> Result<Option<T>, super::protocol::FrameError> {
    stream
        .set_nonblocking(true)
        .map_err(super::protocol::FrameError::Io)?;
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let mut header = [0u8; 4];
        if !read_part(stream, &mut header, deadline)? {
            return Ok(None);
        }
        let announced = u32::from_be_bytes(header) as usize;
        if announced > limit {
            return Err(super::protocol::FrameError::Oversized { announced, limit });
        }
        let mut body = vec![0u8; announced];
        read_part(stream, &mut body, deadline)?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|error| super::protocol::FrameError::Malformed(error.to_string()))
    })();
    let restore = stream.set_nonblocking(false);
    if let Err(error) = restore {
        return Err(super::protocol::FrameError::Io(error));
    }
    result
}

/// Return `false` only for EOF before any byte of a frame, and otherwise fill
/// the complete slice while periodically checking the absolute deadline.
fn read_part(
    stream: &mut UnixStream,
    target: &mut [u8],
    deadline: Instant,
) -> Result<bool, super::protocol::FrameError> {
    let mut offset = 0;
    while offset < target.len() {
        if Instant::now() >= deadline {
            return Err(super::protocol::FrameError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "control frame deadline exceeded",
            )));
        }
        match stream.read(&mut target[offset..]) {
            Ok(0) if offset == 0 => return Ok(false),
            Ok(0) => return Err(super::protocol::FrameError::Truncated),
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(super::protocol::FrameError::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "control frame deadline exceeded",
                    )));
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(super::protocol::FrameError::Io(error)),
        }
    }
    Ok(true)
}

/// Transfer one completed answer under the same response-byte lease for the
/// result frame and all linked chunks. A linked payload may be a single soft
/// pressure overflow, but bytes are never truncated into something that could
/// be mistaken for a valid executable.
fn relay_build_output(stream: &mut UnixStream, leased: LeasedOutput) {
    let LeasedOutput {
        encoded_result,
        bytes,
        ready,
        _lease: lease,
    } = leased;
    let _ = stream.set_nonblocking(true);
    let deadline = Instant::now() + RESPONSE_WRITE_TIMEOUT;
    let result = write_encoded_frame_deadline(stream, &encoded_result, deadline);
    if result.is_ok() && ready {
        let _ = write_chunks_deadline(stream, &bytes, deadline);
    }
    let _ = stream.set_nonblocking(false);
    // Release the aggregate charge only after both owned transfer buffers are
    // gone, so a waiting owner cannot observe zero while this handler still
    // retains a completed response.
    drop(encoded_result);
    drop(bytes);
    drop(lease);
}

fn reserve_response(
    response_bytes: &AtomicU64,
    peak_response_bytes: &AtomicU64,
    limit: u64,
    amount: u64,
) -> bool {
    let mut current = response_bytes.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(amount) else {
            return false;
        };
        // A single completed answer may be larger than the soft aggregate
        // policy. Keep its exact charge and let the owner wait on its lease;
        // replacing a valid executable with a failure would lose the answer
        // merely because it was large. Concurrent answers still remain
        // bounded by the normal aggregate limit.
        if next > limit && current != 0 {
            return false;
        }
        match response_bytes.compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                update_peak(peak_response_bytes, next);
                return true;
            }
            Err(observed) => current = observed,
        }
    }
}

fn write_frame_with_timeout<T: serde::Serialize>(
    stream: &mut UnixStream,
    message: &T,
) -> io::Result<()> {
    write_frame_with_timeout_value(stream, message, RESPONSE_WRITE_TIMEOUT)
}

fn write_frame_with_timeout_value<T: serde::Serialize>(
    stream: &mut UnixStream,
    message: &T,
    timeout: Duration,
) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    write_encoded_frame_with_timeout(stream, &body, timeout)
}

fn write_encoded_frame_with_timeout(
    stream: &mut UnixStream,
    body: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    let result = write_encoded_frame_deadline(stream, body, deadline);
    let restore = stream.set_nonblocking(false);
    restore?;
    result
}

fn write_encoded_frame_deadline(
    stream: &mut UnixStream,
    body: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "response exceeds daemon resource limit",
        ));
    }
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "response frame is too large"))?;
    write_part_deadline(stream, &length.to_be_bytes(), deadline)?;
    write_part_deadline(stream, body, deadline)?;
    stream.flush()
}

fn write_chunks_deadline(
    stream: &mut UnixStream,
    bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    for chunk in bytes.chunks(super::protocol::MAX_CHUNK_BYTES) {
        let length = u32::try_from(chunk.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "chunk is too large"))?;
        write_part_deadline(stream, &length.to_be_bytes(), deadline)?;
        write_part_deadline(stream, chunk, deadline)?;
    }
    Ok(())
}

fn write_part_deadline(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response write deadline exceeded",
            ));
        }
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "socket closed")),
            Ok(written) => offset += written,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "response write deadline exceeded",
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
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

#[cfg(test)]
mod qualification_tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_slow_drip_control_frame_has_one_absolute_deadline() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let sender = thread::spawn(move || {
            writer.write_all(&8_u32.to_be_bytes()).unwrap();
            for byte in b"12345678" {
                let _ = writer.write_all(std::slice::from_ref(byte));
                thread::sleep(Duration::from_millis(60));
            }
        });
        let started = Instant::now();
        let result = read_frame_deadline::<serde_json::Value>(
            &mut reader,
            MAX_CONTROL_FRAME_BYTES,
            Duration::from_millis(100),
        );
        assert!(matches!(
            result,
            Err(super::super::protocol::FrameError::Io(error))
                if error.kind() == io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = sender.join();
    }

    #[test]
    fn a_nonreading_peer_cannot_hold_a_chunk_transfer_past_its_deadline() {
        let (mut writer, _reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let bytes = vec![0_u8; super::super::protocol::MAX_CHUNK_BYTES * 4];
        let started = Instant::now();
        let error = write_chunks_deadline(
            &mut writer,
            &bytes,
            Instant::now() + Duration::from_millis(100),
        )
        .expect_err("the peer does not drain the transfer");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn one_oversized_answer_is_a_single_protected_pressure_overflow() {
        let response_bytes = AtomicU64::new(0);
        let peak = AtomicU64::new(0);
        assert!(reserve_response(&response_bytes, &peak, 10, 11));
        assert_eq!(response_bytes.load(Ordering::Acquire), 11);
        assert!(
            !reserve_response(&response_bytes, &peak, 10, 1),
            "a second answer cannot join an oversized protected response"
        );
        response_bytes.store(0, Ordering::Release);
        assert!(reserve_response(&response_bytes, &peak, 10, 10));
    }
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
