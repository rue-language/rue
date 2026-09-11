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

/// The largest result frame a client accepts. A result carries the rendered
/// diagnostics of one request, which a pathological program can make large;
/// linked bytes never travel inside it.
pub const MAX_RESULT_FRAME_BYTES: usize = 64 << 20;

/// The largest raw chunk of linked bytes either side sends or accepts. The
/// result frame announces the total, so a reader knows how many chunks to
/// expect and never holds more than one unread chunk beyond the bytes it has
/// already accepted.
pub const MAX_CHUNK_BYTES: usize = 4 << 20;

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

/// Write one raw byte chunk as a frame: the same length prefix, an opaque body.
pub fn write_bytes_frame(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a frame cannot exceed 4 GiB"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

/// Read one raw byte chunk, or `None` when the peer closed the connection
/// cleanly between frames. A chunk longer than `limit` is refused before any
/// of its body is read.
pub fn read_bytes_frame(
    stream: &mut impl Read,
    limit: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
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
    Ok(Some(body))
}

/// Write `bytes` as a sequence of bounded chunks. The receiver knows the
/// total from the result frame that preceded them.
pub fn write_chunks(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
        write_bytes_frame(stream, chunk)?;
    }
    Ok(())
}

/// Read exactly `total` bytes sent as bounded chunks.
pub fn read_chunks(stream: &mut impl Read, total: u64) -> Result<Vec<u8>, FrameError> {
    let total = usize::try_from(total).map_err(|_| FrameError::Oversized {
        announced: usize::MAX,
        limit: usize::MAX,
    })?;
    let mut bytes = Vec::with_capacity(total.min(MAX_CHUNK_BYTES));
    while bytes.len() < total {
        let remaining = total - bytes.len();
        let chunk = read_bytes_frame(stream, MAX_CHUNK_BYTES.min(remaining))?
            .ok_or(FrameError::Truncated)?;
        if chunk.is_empty() {
            return Err(FrameError::Malformed(
                "an empty chunk inside a transfer".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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
    /// Compile through the service's retained host (ADR-0085 §3, §5): an
    /// executable, a test image, or a test inventory, as `kind` says. The
    /// service answers [`ResponseBody::Accepted`] or
    /// [`ResponseBody::Rejected`] before any work begins; an accepted request
    /// is followed on the same connection by one [`BuildReply`] frame and,
    /// when it carries linked bytes, by those bytes in raw chunks. The
    /// connection then carries nothing else.
    Build(Box<BuildRequest>),
}

/// Which artifact a build request asks for. All three run over the same
/// retained host; the root selection they imply is request data, never a
/// host or service namespace (ADR-0085 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildKind {
    /// An executable published at the request's output path.
    Executable,
    /// A test image staged at the request's output path, with the inventory
    /// and per-test compile-failure attribution the client runner needs
    /// (ADR-0083 §3).
    TestImage,
    /// The test inventory alone: no codegen, no link, no bytes.
    TestListing,
    /// The analysis-only presentation `--emit air`: semantic analysis of the
    /// executable root set, rendered; no codegen, no link, no bytes.
    Analysis,
}

/// How the client wants diagnostics rendered. The service renders once with
/// the canonical formatter under the client's own policy, so the text on the
/// client's stderr is byte for byte what a direct compile would have written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFormat {
    Text,
    Json,
}

/// An executable build captured at the client boundary (ADR-0085 §3): every
/// path is carried as the command line spelled it together with the
/// directory it was spelled in, so the service resolves it exactly as the
/// client's own process would have, without ever changing its own cwd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRequest {
    /// Named `artifact` rather than `kind` because the request travels
    /// flattened into the internally tagged [`RequestBody`], whose tag is
    /// `kind`.
    pub artifact: BuildKind,
    /// The client's invocation directory.
    pub working_directory: String,
    /// The root source as written.
    pub root_source: String,
    /// The output path as written.
    pub output_path: String,
    /// `--source-manifest` as written.
    pub source_manifest_path: Option<String>,
    /// `--test-candidates` as written, already validated by the client. Every
    /// kind acquires it under the host's read policy, as the direct path does
    /// even for an ordinary build (ADR-0083 §1); a test image also reports
    /// against it.
    pub test_candidates_path: Option<String>,
    /// `RUE_STD_PATH` as captured; `None` when unset, `Some("")` when empty,
    /// each keeping its direct-mode meaning.
    pub std_root: Option<String>,
    /// Compiler workers; `0` selects the automatic policy.
    pub workers: usize,
    /// The target name as `Target` prints it.
    pub target: String,
    /// The optimization level as `-O<n>` spells it, without the `-O`.
    pub opt_level: String,
    /// Preview feature names, as `--preview` accepts them.
    pub preview_features: Vec<String>,
    /// `--link-archive` paths anchored at the working directory.
    pub link_archives: Vec<String>,
    pub error_format: DiagnosticFormat,
    /// Whether the client's stderr wants color.
    pub color: bool,
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
    Status {
        report: Box<StatusReport>,
    },
    Stopping,
    Error {
        message: String,
    },
    /// The build was admitted. `ticket` names it service-wide, which is how a
    /// client attributes a crash record to its own request; `queued_ahead`
    /// is how many admitted requests precede it.
    Accepted {
        ticket: u64,
        queued_ahead: u32,
    },
    /// The build was refused before any work began, so the client may run it
    /// directly (ADR-0085 §6).
    Rejected {
        reason: String,
    },
}

/// The one result frame an accepted build produces.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildReply {
    pub ticket: u64,
    pub result: BuildResult,
}

/// How an accepted build ended. `stderr` is everything the direct compile
/// would have written to its diagnostic stream up to the point where the
/// client takes over: source-load failures, program diagnostics, a refused
/// destination, or the warnings of a successful compile.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildResult {
    /// The program was rejected or the destination refused; nothing to publish.
    Rejected { stderr: String },
    /// The executable or test image is linked. `bytes` raw chunk bytes
    /// follow this frame; the client publishes them at `destination` after
    /// revalidating `inputs`, exactly as a watch cycle does. A test image
    /// also carries what its runner needs.
    Ready {
        stderr: String,
        target: String,
        destination: DestinationRecord,
        inputs: Vec<InputRecord>,
        bytes: u64,
        test_image: Option<Box<TestImageRecord>>,
    },
    /// The test inventory. `entries` is `None` when the listing itself was
    /// refused; `stderr` then carries why, rendered.
    Listing {
        stderr: String,
        entries: Option<Vec<InventoryEntryRecord>>,
    },
    /// A presentation: everything the direct `--emit` would have written, in
    /// the order it would have written it, to whichever stream. `ok` is
    /// whether the direct invocation would have succeeded.
    Presentation { ok: bool, writes: Vec<StreamWrite> },
    /// The request's cancellation was observed; nothing was produced.
    Canceled,
    /// The service could not run the request. `internal` marks a compiler
    /// defect (an internal compiler error) as opposed to an infrastructure
    /// failure.
    Failed { message: String, internal: bool },
}

/// The publication destination as the service's preflight validated it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationRecord {
    pub path: String,
    pub display_path: String,
    pub source_paths: Vec<String>,
}

/// One accepted filesystem observation, in the shape the watch publication
/// guard revalidates before the rename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub requested_path: String,
    pub canonical_path: String,
    /// `None` records an expected absence.
    pub fingerprint: Option<u64>,
    pub symlink_boundary: Option<String>,
    /// `(volume, file)` identities along the expected symlink route.
    pub symlink_route: Vec<(u64, u64)>,
}

/// Which standard stream a write belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// One write the direct path would have made, verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamWrite {
    pub stream: OutputStream,
    pub text: String,
}

/// One test of an inventory, as the compiler's inventory entry spells it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryEntryRecord {
    pub id: String,
    pub module: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub ordinal: u32,
    /// `(issue, platform)`: `@known_bug` markers, `platform` `None` when
    /// unscoped.
    pub expected_failures: Vec<(String, Option<String>)>,
}

/// What a test image carries beside its bytes (ADR-0083 §3), projected once
/// by the canonical renderer so the runner never renders a diagnostic itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestImageRecord {
    /// Whether the closure spans more than one user module.
    pub multi_module_closure: bool,
    pub entries: Vec<InventoryEntryRecord>,
    pub compile_failures: Vec<CompileFailureRecord>,
    pub unimported: UnimportedRecord,
}

/// The `compile_error` verdict the compiler decided for one test.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileFailureRecord {
    pub ordinal: u32,
    pub xfail_eligible: bool,
    pub payload: String,
    pub message: String,
    /// `(file, line, column)` of the first diagnostic's primary span.
    pub location: Option<(String, u32, u32)>,
    pub diagnostics: Vec<serde_json::Value>,
}

/// The unimported-test-file report (ADR-0083 §1), rendered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnimportedRecord {
    /// No `--test-candidates` was declared.
    NotDeclared,
    /// The declared files outside the closure, with the warnings the direct
    /// path prints for them already rendered (empty when there are none).
    Files {
        stderr: String,
        files: Vec<UnimportedFileRecord>,
    },
    /// The report itself failed; `stderr` carries the diagnostics rendered.
    Failed { stderr: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnimportedFileRecord {
    pub path: String,
    pub tests: u32,
    pub parse_failed: bool,
}

/// A request the service is executing or holding, as status reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSummary {
    pub ticket: u64,
    pub root_source: String,
    pub working_directory: String,
    pub elapsed_ms: u64,
}

/// What a service records when a compiler panic ends it during a request, so
/// the client whose request it was can report the defect (ADR-0085 §6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashRecord {
    pub pid: u32,
    /// The request that was active, if any.
    pub ticket: Option<u64>,
    pub message: String,
    pub location: Option<String>,
}

/// What `rue daemon status` reports (ADR-0085 §2): identity, PID, scope,
/// active request, queue, retained hosts, and the resource policy in force.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub service: ServiceInfo,
    pub uptime_ms: u64,
    pub idle_timeout_ms: u64,
    /// Connections open right now, this one included.
    pub connections: u32,
    pub active_request: Option<RequestSummary>,
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
    fn a_build_request_round_trips_inside_the_tagged_request_body() {
        let request = Request {
            id: 3,
            body: RequestBody::Build(Box::new(BuildRequest {
                artifact: BuildKind::TestImage,
                working_directory: "/w".into(),
                root_source: "main.rue".into(),
                output_path: "/w/.run/image".into(),
                source_manifest_path: None,
                test_candidates_path: Some("tests.txt".into()),
                std_root: Some(String::new()),
                workers: 0,
                target: "x86-64-linux".into(),
                opt_level: "O2".into(),
                preview_features: vec!["test_infra".into()],
                link_archives: Vec::new(),
                error_format: DiagnosticFormat::Json,
                color: true,
            })),
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &request).unwrap();
        let mut reader = wire.as_slice();
        let decoded: Request = read_frame(&mut reader, MAX_CONTROL_FRAME_BYTES)
            .unwrap()
            .unwrap();
        let RequestBody::Build(decoded) = decoded.body else {
            panic!("a build request decodes as a build");
        };
        let RequestBody::Build(original) = request.body else {
            unreachable!()
        };
        assert_eq!(decoded, original);
    }

    #[test]
    fn chunked_bytes_round_trip_under_the_chunk_bound() {
        let bytes: Vec<u8> = (0..(2 * MAX_CHUNK_BYTES + 17)).map(|i| i as u8).collect();
        let mut wire = Vec::new();
        write_chunks(&mut wire, &bytes).unwrap();
        // Three chunks: two full, one of 17 bytes.
        assert_eq!(wire.len(), bytes.len() + 3 * 4);
        let mut reader = wire.as_slice();
        assert_eq!(read_chunks(&mut reader, bytes.len() as u64).unwrap(), bytes);

        let mut wire = Vec::new();
        write_bytes_frame(&mut wire, &vec![1u8; MAX_CHUNK_BYTES + 1]).unwrap();
        let mut reader = wire.as_slice();
        let error = read_chunks(&mut reader, (MAX_CHUNK_BYTES + 1) as u64).unwrap_err();
        assert!(matches!(error, FrameError::Oversized { .. }), "{error}");

        let mut wire = Vec::new();
        write_chunks(&mut wire, &[1, 2, 3]).unwrap();
        let mut reader = wire.as_slice();
        let error = read_chunks(&mut reader, 5).unwrap_err();
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
