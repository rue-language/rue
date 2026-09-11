//! The wire protocol between a Rue compiler client and its local service.
//!
//! Messages are length-prefixed JSON frames on a Unix-domain socket: a 4-byte
//! big-endian length followed by that many bytes of one JSON object. The
//! protocol is deliberately narrow (ADR-0085 §5): every message is an explicit
//! data-transfer object, never a serialized compiler struct, query key, or
//! arena index. Both ends run the same compiler build — the handshake proves
//! it — so the protocol version below changes only when the message shapes do.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The version of the message shapes in this module. It participates in the
/// service identity, so a client and a service that disagree on it can never
/// meet on one socket, and the handshake checks it again anyway.
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;

/// The largest control frame either side accepts. Control messages are a few
/// hundred bytes; a frame claiming more than this is a malformed or hostile
/// peer, and the connection is dropped instead of allocating for it.
pub const MAX_CONTROL_FRAME_BYTES: usize = 1 << 20;

/// Why a frame could not be read.
#[derive(Debug)]
pub enum FrameError {
    /// The peer closed the connection in the middle of a frame.
    Truncated,
    /// The peer announced a frame larger than the bound the reader allows.
    Oversized { announced: usize, limit: usize },
    /// The frame's bytes were not the expected JSON object.
    Malformed(String),
    /// The socket itself failed.
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("the connection closed inside a frame"),
            Self::Oversized { announced, limit } => write!(
                formatter,
                "the peer announced a {announced}-byte frame; the limit is {limit} bytes"
            ),
            Self::Malformed(message) => write!(formatter, "malformed frame: {message}"),
            Self::Io(error) => write!(formatter, "socket error: {error}"),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Write one message as a frame.
pub fn write_frame(stream: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a frame cannot exceed 4 GiB"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

/// Read one frame, or `None` when the peer closed the connection cleanly
/// between frames. A frame longer than `limit` is refused before any of its
/// body is read.
pub fn read_frame<T: DeserializeOwned>(
    stream: &mut impl Read,
    limit: usize,
) -> Result<Option<T>, FrameError> {
    let mut header = [0u8; 4];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(FrameError::Io(error)),
    }
    let announced = u32::from_be_bytes(header) as usize;
    if announced > limit {
        return Err(FrameError::Oversized { announced, limit });
    }
    let mut body = vec![0u8; announced];
    stream.read_exact(&mut body).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(error)
        }
    })?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| FrameError::Malformed(error.to_string()))
}

/// The running compiler image and service scope a peer claims, in a form
/// both ends can compare byte for byte.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub scheme_version: u16,
    pub architecture: String,
    pub scheme: String,
    /// The image identity bytes, lowercase hex.
    pub image_hex: String,
    /// The service digest over user, scope, isolation, protocol, and image,
    /// lowercase hex. The endpoint directory is named by its prefix.
    pub service_hex: String,
}

/// The first message a client sends.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub identity: IdentityRecord,
}

/// What a service knows about itself, reported in the handshake and in
/// status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub pid: u32,
    pub protocol_version: u32,
    pub identity: IdentityRecord,
    pub scope_directory: String,
    pub isolation: Option<String>,
    pub endpoint_directory: String,
    pub started_at_unix_ms: u64,
}

/// The service's answer to [`Hello`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HelloReply {
    Welcome { service: ServiceInfo },
    Rejected { reason: String },
}

/// One request on an established connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub body: RequestBody,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestBody {
    Ping,
    Status,
    /// End the service. It answers [`ResponseBody::Stopping`] and then exits;
    /// the caller observes completion by the socket disappearing.
    Stop,
}

/// One response, carrying the id of the request it answers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub body: ResponseBody,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseBody {
    Pong,
    Status { report: Box<StatusReport> },
    Stopping,
    Error { message: String },
}

/// What `rue daemon status` reports (ADR-0085 §2): identity, PID, scope,
/// active request, queue, retained hosts, and the resource policy in force.
/// The request and host fields are structural placeholders until the service
/// executes compiler requests; a status consumer sees the same shape then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub service: ServiceInfo,
    pub uptime_ms: u64,
    pub idle_timeout_ms: u64,
    /// Connections open right now, this one included.
    pub connections: u32,
    pub active_request: Option<String>,
    pub queued_requests: u32,
    pub retained_hosts: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> IdentityRecord {
        IdentityRecord {
            scheme_version: 1,
            architecture: "x86_64".into(),
            scheme: "test".into(),
            image_hex: "ab".into(),
            service_hex: "cd".into(),
        }
    }

    #[test]
    fn frames_round_trip_and_stop_cleanly_at_eof() {
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Request {
                id: 7,
                body: RequestBody::Status,
            },
        )
        .unwrap();
        write_frame(
            &mut wire,
            &Response {
                id: 7,
                body: ResponseBody::Pong,
            },
        )
        .unwrap();
        let mut reader = wire.as_slice();
        let request: Request = read_frame(&mut reader, MAX_CONTROL_FRAME_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(request.id, 7);
        assert!(matches!(request.body, RequestBody::Status));
        let response: Response = read_frame(&mut reader, MAX_CONTROL_FRAME_BYTES)
            .unwrap()
            .unwrap();
        assert!(matches!(response.body, ResponseBody::Pong));
        let end: Option<Request> = read_frame(&mut reader, MAX_CONTROL_FRAME_BYTES).unwrap();
        assert!(end.is_none(), "a clean EOF between frames is not an error");
    }

    #[test]
    fn oversized_and_truncated_frames_are_refused_before_allocation() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(u32::MAX).to_be_bytes());
        let mut reader = wire.as_slice();
        let error = read_frame::<Request>(&mut reader, 16).unwrap_err();
        assert!(
            matches!(error, FrameError::Oversized { limit: 16, .. }),
            "{error}"
        );

        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Hello {
                protocol_version: 1,
                identity: identity(),
            },
        )
        .unwrap();
        wire.truncate(wire.len() - 3);
        let mut reader = wire.as_slice();
        let error = read_frame::<Hello>(&mut reader, MAX_CONTROL_FRAME_BYTES).unwrap_err();
        assert!(matches!(error, FrameError::Truncated), "{error}");
    }

    #[test]
    fn malformed_frames_name_the_problem() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&3_u32.to_be_bytes());
        wire.extend_from_slice(b"nop");
        let mut reader = wire.as_slice();
        let error = read_frame::<Hello>(&mut reader, MAX_CONTROL_FRAME_BYTES).unwrap_err();
        assert!(matches!(error, FrameError::Malformed(_)), "{error}");
    }

    #[test]
    fn hello_reply_and_status_serialize_with_tagged_kinds() {
        let reply = HelloReply::Rejected {
            reason: "identity mismatch".into(),
        };
        let json = serde_json::to_string(&reply).unwrap();
        assert!(json.contains("\"kind\":\"rejected\""), "{json}");
        let body = ResponseBody::Status {
            report: Box::new(StatusReport {
                service: ServiceInfo {
                    pid: 1,
                    protocol_version: DAEMON_PROTOCOL_VERSION,
                    identity: identity(),
                    scope_directory: "/p".into(),
                    isolation: None,
                    endpoint_directory: "/e".into(),
                    started_at_unix_ms: 0,
                },
                uptime_ms: 5,
                idle_timeout_ms: 10,
                connections: 1,
                active_request: None,
                queued_requests: 0,
                retained_hosts: 0,
            }),
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"kind\":\"status\""), "{json}");
        let back: ResponseBody = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ResponseBody::Status { .. }));
    }
}
