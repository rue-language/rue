//! The service loop: accept connections, verify the peer, answer control
//! requests, and retire on stop or idleness.
//!
//! Control messages must stay serviceable while compiler work runs
//! (ADR-0085 §6), so every connection is handled on its own thread and the
//! accept loop never blocks on one. The compiler owner and its admission queue
//! arrive with request execution; the loop here is shaped for them: one shared
//! state, connection threads that only submit and wait, and a stop flag every
//! thread observes.

use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::protocol::{
    DAEMON_PROTOCOL_VERSION, Hello, HelloReply, MAX_CONTROL_FRAME_BYTES, Request, RequestBody,
    Response, ResponseBody, ServiceInfo, StatusReport, read_frame, write_frame,
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

struct Shared {
    info: ServiceInfo,
    started: Instant,
    idle_timeout: Duration,
    stop: AtomicBool,
    connections: AtomicU32,
    last_activity: Mutex<Instant>,
}

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
        StatusReport {
            service: self.info.clone(),
            uptime_ms: self.started.elapsed().as_millis() as u64,
            idle_timeout_ms: self.idle_timeout.as_millis() as u64,
            connections: self.connections.load(Ordering::Acquire),
            active_request: None,
            queued_requests: 0,
            retained_hosts: 0,
        }
    }
}

/// Serve `listener` until stopped or idle. The listener is already bound and
/// secured; the caller owns the endpoint lock for the whole call.
pub(super) fn run(listener: UnixListener, info: ServiceInfo, idle_timeout: Duration) -> ServeExit {
    // A client that vanishes mid-write must be a failed connection, never a
    // dead service.
    // SAFETY: changing this process's SIGPIPE disposition has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    if listener.set_nonblocking(true).is_err() {
        return ServeExit::Idle;
    }
    let shared = Arc::new(Shared {
        info,
        started: Instant::now(),
        idle_timeout,
        stop: AtomicBool::new(false),
        connections: AtomicU32::new(0),
        last_activity: Mutex::new(Instant::now()),
    });
    let mut handlers: Vec<thread::JoinHandle<()>> = Vec::new();
    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
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
                    return ServeExit::Idle;
                }
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
    // Let the stopping client's acknowledgement and any other in-flight
    // control reply finish before the endpoint is torn down.
    for handle in handlers {
        let _ = handle.join();
    }
    ServeExit::Stopped
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
    // Requests may legitimately wait: a later compile request holds the
    // connection for its whole run.
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
