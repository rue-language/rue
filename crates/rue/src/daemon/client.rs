//! The client half of the service protocol: connect, prove identity, and issue
//! control requests.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::protocol::{
    Hello, HelloReply, MAX_CONTROL_FRAME_BYTES, Request, RequestBody, Response, ResponseBody,
    ServiceInfo, StatusReport, read_frame, write_frame,
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
            super::protocol::FrameError::Io(error) => Self::Io(error),
            other => Self::Protocol(other.to_string()),
        }
    }
}

/// An established, identity-checked connection to a service.
pub struct Connection {
    stream: UnixStream,
    service: ServiceInfo,
    next_id: u64,
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
}

fn unexpected(expected: &str, actual: &ResponseBody) -> ConnectError {
    match actual {
        ResponseBody::Error { message } => ConnectError::Protocol(message.clone()),
        other => ConnectError::Protocol(format!("expected {expected}, got {other:?}")),
    }
}
