//! The client half of the service protocol: connect, prove identity, issue
//! control requests, and submit and await builds.

use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use super::protocol::{
    BuildObservations, BuildReply, BuildRequest, BuildResult, Hello, HelloReply,
    MAX_CONTROL_FRAME_BYTES, MAX_RESULT_FRAME_BYTES, Request, RequestBody, Response, ResponseBody,
    ServiceInfo, StatusReport, read_chunks, read_frame, write_frame,
};
/// Why a connection could not be established or used.
#[derive(Debug)]
pub enum ConnectError {
    /// There is no socket at the endpoint.
    NoSocket,
    /// A socket exists but nothing accepts on it (a dead service left it).
    Refused,
    /// The socket is not owned by the current user.
    Insecure(String),
    /// The service answered the handshake by refusing this client.
    Rejected(String),
    /// The peer did not follow the protocol.
    Protocol(String),
    /// A watch input changed while this request was compiling.
    WatchSuperseded,
    Io(io::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSocket => formatter.write_str("no service socket exists"),
            Self::Refused => formatter.write_str("the service socket refused the connection"),
            Self::Insecure(reason) => write!(formatter, "insecure service socket: {reason}"),
            Self::Rejected(reason) => {
                write!(formatter, "the service refused this client: {reason}")
            }
            Self::Protocol(reason) => write!(formatter, "protocol error: {reason}"),
            Self::WatchSuperseded => formatter.write_str("the watched source changed"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<io::Error> for ConnectError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<super::protocol::FrameError> for ConnectError {
    fn from(error: super::protocol::FrameError) -> Self {
        match error {
            super::protocol::FrameError::Io(error)
                if error
                    .get_ref()
                    .is_some_and(|source| source.is::<WatchSupersededIo>()) =>
            {
                Self::WatchSuperseded
            }
            super::protocol::FrameError::Io(error) => Self::Io(error),
            other => Self::Protocol(other.to_string()),
        }
    }
}

#[derive(Debug)]
struct WatchSupersededIo;

impl std::fmt::Display for WatchSupersededIo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the watched source changed")
    }
}

impl std::error::Error for WatchSupersededIo {}

/// An established, identity-checked connection to a service.
pub struct Connection {
    stream: UnixStream,
    service: ServiceInfo,
    next_id: u64,
    observations: BuildObservations,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("service", &self.service)
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

/// Connect to `socket`, send `hello`, and require a welcome. The socket must
/// be owned by the current user before anything is written to it; `timeout`
/// bounds every read during the handshake.
pub fn connect(
    socket: &Path,
    hello: &Hello,
    timeout: Duration,
) -> Result<Connection, ConnectError> {
    let metadata = match std::fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ConnectError::NoSocket);
        }
        Err(error) => return Err(ConnectError::Io(error)),
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid {
        return Err(ConnectError::Insecure(format!(
            "{} is owned by uid {}, not the current user {uid}",
            socket.display(),
            metadata.uid()
        )));
    }
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ConnectError::NoSocket);
        }
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            return Err(ConnectError::Refused);
        }
        Err(error) => return Err(ConnectError::Io(error)),
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut stream = stream;
    write_frame(&mut stream, hello)?;
    let reply: HelloReply = read_frame(&mut stream, MAX_CONTROL_FRAME_BYTES)?
        .ok_or_else(|| ConnectError::Protocol("the service closed without answering".into()))?;
    match reply {
        HelloReply::Welcome { service } => Ok(Connection {
            stream,
            service,
            next_id: 1,
            observations: BuildObservations::default(),
        }),
        HelloReply::Rejected { reason } => Err(ConnectError::Rejected(reason)),
    }
}

impl Connection {
    pub fn service(&self) -> &ServiceInfo {
        &self.service
    }

    /// Issue one request and wait for its response.
    pub fn request(&mut self, body: RequestBody) -> Result<ResponseBody, ConnectError> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(&mut self.stream, &Request { id, body })?;
        let response: Response = read_frame(&mut self.stream, MAX_CONTROL_FRAME_BYTES)?
            .ok_or_else(|| ConnectError::Protocol("the service closed mid-request".into()))?;
        if response.id != id {
            return Err(ConnectError::Protocol(format!(
                "the service answered request {} while {id} was pending",
                response.id
            )));
        }
        Ok(response.body)
    }

    pub fn ping(&mut self) -> Result<(), ConnectError> {
        match self.request(RequestBody::Ping)? {
            ResponseBody::Pong => Ok(()),
            other => Err(unexpected("pong", &other)),
        }
    }

    pub fn status(&mut self) -> Result<StatusReport, ConnectError> {
        match self.request(RequestBody::Status)? {
            ResponseBody::Status { report } => Ok(*report),
            other => Err(unexpected("a status report", &other)),
        }
    }

    /// Ask the service to stop. It acknowledges before exiting; completion
    /// is observed by its endpoint lock coming free.
    pub fn stop(&mut self) -> Result<(), ConnectError> {
        match self.request(RequestBody::Stop)? {
            ResponseBody::Stopping => Ok(()),
            other => Err(unexpected("a stop acknowledgement", &other)),
        }
    }

    /// Submit a build. The answer says whether the service admitted it; an
    /// admitted build is then awaited with [`Self::await_build`] on this same
    /// connection, which carries nothing else afterwards.
    pub fn submit_build(&mut self, request: BuildRequest) -> Result<Submission, SubmitError> {
        let id = self.next_id;
        self.next_id += 1;
        // Until the frame is written the service knows nothing of the
        // request, so a failure here proves no work began. Once it is
        // written, a failure is ambiguous: the service may have admitted and
        // started the request (ADR-0085 §6).
        write_frame(
            &mut self.stream,
            &Request {
                id,
                body: RequestBody::Build(Box::new(request)),
            },
        )
        .map_err(|error| SubmitError::BeforeSubmission(ConnectError::Io(error)))?;
        let response: Response = read_frame(&mut self.stream, MAX_CONTROL_FRAME_BYTES)
            .map_err(|error| SubmitError::Ambiguous(error.into()))?
            .ok_or_else(|| {
                SubmitError::Ambiguous(ConnectError::Protocol(
                    "the service closed without answering the submission".into(),
                ))
            })?;
        if response.id != id {
            return Err(SubmitError::Ambiguous(ConnectError::Protocol(format!(
                "the service answered request {} while {id} was pending",
                response.id
            ))));
        }
        match response.body {
            ResponseBody::Accepted {
                ticket,
                queued_ahead,
            } => Ok(Submission::Accepted {
                ticket,
                queued_ahead,
            }),
            ResponseBody::Rejected { reason } => Ok(Submission::Rejected { reason }),
            other => Err(SubmitError::Ambiguous(unexpected(
                "an admission answer",
                &other,
            ))),
        }
    }

    /// Wait for an admitted build's result and, when it is ready, its linked
    /// bytes. There is no read timeout: the request may legitimately queue
    /// and compile for as long as the program takes, and the process ending
    /// is what cancels it.
    pub fn await_build(&mut self, ticket: u64) -> Result<(BuildResult, Vec<u8>), ConnectError> {
        let (result, bytes, _, _) = self.await_build_with_measurement(ticket, false)?;
        Ok((result, bytes))
    }

    /// Like [`Self::await_build`], retaining the canonical measurement that
    /// belongs to this ticket. Status is only a best-effort display and must
    /// not be used for attribution when requests contend.
    pub fn await_build_with_measurement(
        &mut self,
        ticket: u64,
        measure_transfer: bool,
    ) -> Result<
        (
            BuildResult,
            Vec<u8>,
            Option<super::protocol::RequestMeasurement>,
            Option<u64>,
        ),
        ConnectError,
    > {
        self.stream.set_read_timeout(None)?;
        // Waiting for the first response byte includes queueing and compiler
        // work. The transfer interval starts only once that byte arrives and
        // includes frame decoding and the linked-image chunks that follow.
        let mut first_byte = [0_u8; 1];
        let mut transfer_started = None;
        let prefix_len = if measure_transfer {
            self.stream.read_exact(&mut first_byte)?;
            transfer_started = Some(Instant::now());
            1
        } else {
            0
        };
        let (reply, bytes) = read_build_reply(
            &mut first_byte[..prefix_len].chain(&mut self.stream),
            ticket,
        )?;
        self.observations = reply.observations.clone();
        Ok((
            reply.result,
            bytes,
            reply.measurement,
            transfer_started.map(|started| started.elapsed().as_nanos() as u64),
        ))
    }

    /// Await an admitted build while the caller's shared watch monitor owns
    /// cancellation. Dropping this connection after `true` cancels only this
    /// service request through its peer watcher.
    pub fn await_build_cancellable<F: Fn() -> bool>(
        &mut self,
        ticket: u64,
        canceled: F,
    ) -> Result<(BuildResult, Vec<u8>), ConnectError> {
        self.stream.set_nonblocking(true)?;
        let mut reader = CancellableReader {
            stream: &mut self.stream,
            canceled: &canceled,
        };
        let (reply, bytes) = read_build_reply(&mut reader, ticket)?;
        self.observations = reply.observations.clone();
        self.stream.set_nonblocking(false)?;
        Ok((reply.result, bytes))
    }

    /// Observations from the most recently completed build request.
    pub fn observations(&self) -> &BuildObservations {
        &self.observations
    }
}

/// One decoder for ordinary, measured, and cancelable build clients. Readers
/// supply their timing or cancellation policy around the same frame/chunk path.
fn read_build_reply(
    reader: &mut impl Read,
    ticket: u64,
) -> Result<(BuildReply, Vec<u8>), ConnectError> {
    let reply: BuildReply = read_frame(reader, MAX_RESULT_FRAME_BYTES)?
        .ok_or_else(|| ConnectError::Protocol("the service closed mid-request".into()))?;
    if reply.ticket != ticket {
        return Err(ConnectError::Protocol(format!(
            "the service answered ticket {} while {ticket} was pending",
            reply.ticket
        )));
    }
    let bytes = match &reply.result {
        BuildResult::Ready { bytes, .. } => read_chunks(reader, *bytes)?,
        _ => Vec::new(),
    };
    Ok((reply, bytes))
}

struct CancellableReader<'a, F> {
    stream: &'a mut UnixStream,
    canceled: &'a F,
}

impl<F: Fn() -> bool> Read for CancellableReader<'_, F> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if (self.canceled)() {
                return Err(io::Error::other(WatchSupersededIo));
            }
            match self.stream.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }
}

/// Why a submission produced no admission answer.
#[derive(Debug)]
pub enum SubmitError {
    /// The request never reached the service; no work began.
    BeforeSubmission(ConnectError),
    /// The request was written but its answer was lost; the service may have
    /// started it.
    Ambiguous(ConnectError),
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeSubmission(error) => write!(formatter, "{error}"),
            Self::Ambiguous(error) => write!(
                formatter,
                "{error} after the request was submitted; whether the service started it is unknown"
            ),
        }
    }
}

/// The service's admission answer to a build.
#[derive(Debug)]
pub enum Submission {
    Accepted {
        ticket: u64,
        queued_ahead: u32,
    },
    /// Refused before any work began.
    Rejected {
        reason: String,
    },
}

fn unexpected(expected: &str, actual: &ResponseBody) -> ConnectError {
    match actual {
        ResponseBody::Error { message } => ConnectError::Protocol(message.clone()),
        other => ConnectError::Protocol(format!("expected {expected}, got {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn send_in_pieces(mut stream: UnixStream, pieces: Vec<Vec<u8>>) {
        for piece in pieces {
            stream.write_all(&piece).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn framed(body: &[u8]) -> Vec<u8> {
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn cancellable_frame_reassembles_partial_header_and_body() {
        let (writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let body = serde_json::to_vec("fragmented response").unwrap();
        let frame = framed(&body);
        let writer = thread::spawn(move || {
            send_in_pieces(
                writer,
                vec![
                    frame[..1].to_vec(),
                    frame[1..3].to_vec(),
                    frame[3..7].to_vec(),
                    frame[7..].to_vec(),
                ],
            );
        });
        let canceled = || false;
        let mut adapter = CancellableReader {
            stream: &mut reader,
            canceled: &canceled,
        };
        let received: String = read_frame(&mut adapter, 1024).unwrap().unwrap();
        writer.join().unwrap();
        assert_eq!(received.as_bytes(), &body[1..body.len() - 1]);
    }

    #[test]
    fn cancellable_chunks_reassemble_across_fragmented_frames() {
        let (writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let writer = thread::spawn(move || {
            let first = framed(b"first ");
            let second = framed(b"chunk");
            send_in_pieces(
                writer,
                vec![
                    first[..2].to_vec(),
                    first[2..].to_vec(),
                    second[..1].to_vec(),
                    second[1..].to_vec(),
                ],
            );
        });
        let canceled = || false;
        let mut adapter = CancellableReader {
            stream: &mut reader,
            canceled: &canceled,
        };
        let received = read_chunks(&mut adapter, 11).unwrap();
        writer.join().unwrap();
        assert_eq!(received, b"first chunk");
    }

    #[test]
    fn cancellable_frame_rejects_oversized_prefix_before_allocation() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.write_all(&4097_u32.to_be_bytes()).unwrap();
        let canceled = || false;
        let mut adapter = CancellableReader {
            stream: &mut reader,
            canceled: &canceled,
        };
        let error = read_frame::<serde_json::Value>(&mut adapter, 4096).unwrap_err();
        assert!(matches!(
            error,
            super::super::protocol::FrameError::Oversized {
                announced: 4097,
                limit: 4096
            }
        ));
    }

    #[test]
    fn cancellable_chunks_reject_a_chunk_larger_than_the_announced_remainder() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.write_all(&11_u32.to_be_bytes()).unwrap();
        writer.write_all(b"eleven bytes").unwrap();
        let canceled = || false;
        let mut adapter = CancellableReader {
            stream: &mut reader,
            canceled: &canceled,
        };
        let error = read_chunks(&mut adapter, 5).unwrap_err();
        assert!(matches!(
            error,
            super::super::protocol::FrameError::Oversized {
                announced: 11,
                limit: 5
            }
        ));
    }

    #[test]
    fn cancellable_frame_aborts_while_waiting_for_the_rest_of_a_frame() {
        let (writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&canceled);
        let writer = thread::spawn(move || {
            writer.try_clone().unwrap().write_all(&[0, 0]).unwrap();
            thread::sleep(Duration::from_millis(40));
            signal.store(true, Ordering::Release);
            drop(writer);
        });
        let canceled_fn = || canceled.load(Ordering::Acquire);
        let mut adapter = CancellableReader {
            stream: &mut reader,
            canceled: &canceled_fn,
        };
        let error = read_frame::<serde_json::Value>(&mut adapter, 4096).unwrap_err();
        writer.join().unwrap();
        assert!(matches!(
            error,
            super::super::protocol::FrameError::Io(error)
                if error.get_ref().is_some_and(|source| source.is::<WatchSupersededIo>())
        ));
    }
}
