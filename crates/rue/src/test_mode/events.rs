//! The `rue test` event stream (ADR-0083 §2), schema `1.0`.
//!
//! Events are produced as ordinary Rust values first and serialized second.
//! That ordering is the point: the human renderer consumes the same values in
//! the same process rather than re-parsing NDJSON, so a field the machine
//! surface publishes and a field a person is shown can never come from two
//! different computations. `docs/process/test-events.md` is the schema's
//! normative description.
//!
//! Object keys are serialized in alphabetical order, matching the determinism
//! stance `docs/process/diagnostics.md` takes for `--error-format json`: two
//! runs over the same inputs owe a consumer byte-identical output. That falls
//! out of `serde_json::Map` being a `BTreeMap` here, which is also how the
//! diagnostic formatter gets it.

use serde_json::{Map, Value};

use super::verdict::Verdict;

/// The event schema version, published in the stream's head event.
pub(crate) const SCHEMA_VERSION: &str = "1.0";

/// The `capability_summary` every `test_finished` carries.
///
/// ADR-0083 ships zero hermeticity claims and says so in-band rather than by
/// omitting the field: consumers handle the shape from v1.0, and the deferred
/// capability ADR populates it as an additive change.
const CAPABILITY_UNAVAILABLE: &str = "unavailable";

/// How captured bytes are carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Base64,
}

impl Encoding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8",
            Self::Base64 => "base64",
        }
    }
}

/// One captured stream's published record.
///
/// `bytes_total` is what the process wrote; `data` is the retained prefix, and
/// is present only for a non-pass. A pass carries `digest` instead — a passing
/// test's output is evidence a consumer may want to compare, not read, and
/// inlining it for every green test is exactly the wall of green ADR-0083 §2
/// rejects. A pass can never have overflowed its budget (an overflow is a
/// failure verdict), so its digest always covers the whole stream.
#[derive(Debug, Clone)]
pub(crate) struct Capture {
    pub(crate) encoding: Encoding,
    pub(crate) bytes_total: u64,
    pub(crate) retained: Vec<u8>,
    /// `true` for a pass: publish a digest rather than the bytes.
    pub(crate) digest_only: bool,
}

impl Capture {
    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "encoding".to_owned(),
            Value::String(self.encoding.as_str().to_owned()),
        );
        object.insert("bytes_total".to_owned(), Value::from(self.bytes_total));
        if self.digest_only {
            object.insert("digest".to_owned(), Value::String(self.digest()));
        } else {
            object.insert("data".to_owned(), Value::String(self.encoded_data()));
        }
        Value::Object(object)
    }

    /// The retained bytes as the `encoding` tag describes them.
    pub(crate) fn encoded_data(&self) -> String {
        match self.encoding {
            Encoding::Utf8 => String::from_utf8_lossy(&self.retained).into_owned(),
            Encoding::Base64 => base64(&self.retained),
        }
    }

    /// `sha256:<hex>` over the retained bytes.
    fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("sha256:{:x}", Sha256::digest(&self.retained))
    }

    /// Build a record from drained bytes. Rue strings are arbitrary byte
    /// sequences written raw, so the encoding is decided by inspection rather
    /// than assumed.
    pub(crate) fn new(retained: Vec<u8>, bytes_total: u64, digest_only: bool) -> Self {
        let encoding = if std::str::from_utf8(&retained).is_ok() {
            Encoding::Utf8
        } else {
            Encoding::Base64
        };
        Self {
            encoding,
            bytes_total,
            retained,
            digest_only,
        }
    }
}

/// Standard base64 with padding, for capture that is not valid UTF-8.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// A failure's source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Location {
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

impl Location {
    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("file".to_owned(), Value::String(self.file.clone()));
        object.insert("line".to_owned(), Value::from(self.line));
        object.insert("column".to_owned(), Value::from(self.column));
        Value::Object(object)
    }
}

/// The structured failure record (ADR-0083 §2).
#[derive(Debug, Clone, Default)]
pub(crate) struct FailureRecord {
    pub(crate) kind: String,
    pub(crate) message: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) location: Option<Location>,
    /// The open, versioned payload ADR-0083 §5.1 reserves for assertion
    /// libraries. Empty until Phase 2.5's structured comparisons.
    pub(crate) payload: Option<String>,
    /// The runner's own explanation, when it could not trust what it read.
    pub(crate) runner_note: Option<String>,
}

impl FailureRecord {
    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("kind".to_owned(), Value::String(self.kind.clone()));
        object.insert("message".to_owned(), Value::String(self.message.clone()));
        if let Some(code) = self.exit_code {
            object.insert("exit_code".to_owned(), Value::from(code));
        }
        if let Some(signal) = self.signal {
            object.insert("signal".to_owned(), Value::from(signal));
        }
        if let Some(location) = &self.location {
            object.insert("location".to_owned(), location.to_json());
        }
        if let Some(payload) = &self.payload {
            object.insert("payload".to_owned(), Value::String(payload.clone()));
        }
        if let Some(note) = &self.runner_note {
            object.insert("runner_note".to_owned(), Value::String(note.clone()));
        }
        Value::Object(object)
    }
}

/// A declared test file outside the compiled closure (ADR-0083 §1).
#[derive(Debug, Clone)]
pub(crate) struct UnimportedFile {
    pub(crate) path: String,
    pub(crate) tests: u32,
    pub(crate) parse_failed: bool,
}

/// Whether a declared candidate inventory was supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateSource {
    Declared,
    None,
}

impl CandidateSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::None => "none",
        }
    }
}

/// One event of the stream.
#[derive(Debug, Clone)]
pub(crate) enum Event {
    RunStarted {
        root: String,
        target: String,
        opt_level: String,
        seed: u64,
        jobs: usize,
        shard: Option<String>,
        selected: usize,
        total: usize,
    },
    TestStarted {
        id: String,
    },
    TestFinished(Box<TestFinished>),
    RunFinished {
        passed: usize,
        failed: usize,
        timeout: usize,
        crash: usize,
        wall_ms: u64,
        unimported_test_files: Option<Vec<UnimportedFile>>,
        test_candidates: CandidateSource,
    },
    /// A `--list --format json` inventory record. The listing stream carries
    /// no `run_started`, so this record carries the schema version itself —
    /// ADR-0061 §6 wants every published stream to name its version in-band.
    Test {
        id: String,
        module: String,
        name: String,
        file: String,
        line: u32,
        column: u32,
    },
}

/// The `test_finished` payload, boxed out of [`Event`] because it dwarfs the
/// other variants.
#[derive(Debug, Clone)]
pub(crate) struct TestFinished {
    pub(crate) id: String,
    pub(crate) verdict: Verdict,
    pub(crate) duration_ms: u64,
    pub(crate) failure: Option<FailureRecord>,
    pub(crate) stdout: Capture,
    pub(crate) stderr: Capture,
    pub(crate) scratch_dir: Option<String>,
    pub(crate) repro: Vec<String>,
}

impl Event {
    /// This event as one NDJSON line, without its terminator.
    pub(crate) fn to_ndjson(&self) -> String {
        serde_json::to_string(&self.to_json()).expect("event values are always serializable")
    }

    fn to_json(&self) -> Value {
        let mut object = Map::new();
        match self {
            Self::RunStarted {
                root,
                target,
                opt_level,
                seed,
                jobs,
                shard,
                selected,
                total,
            } => {
                object.insert("event".to_owned(), Value::String("run_started".to_owned()));
                object.insert(
                    "schema".to_owned(),
                    Value::String(SCHEMA_VERSION.to_owned()),
                );
                object.insert("root".to_owned(), Value::String(root.clone()));
                object.insert("target".to_owned(), Value::String(target.clone()));
                object.insert("opt_level".to_owned(), Value::String(opt_level.clone()));
                object.insert("seed".to_owned(), Value::from(*seed));
                object.insert("jobs".to_owned(), Value::from(*jobs));
                if let Some(shard) = shard {
                    object.insert("shard".to_owned(), Value::String(shard.clone()));
                }
                let mut plan = Map::new();
                plan.insert("selected".to_owned(), Value::from(*selected));
                plan.insert("total".to_owned(), Value::from(*total));
                object.insert("plan".to_owned(), Value::Object(plan));
            }
            Self::TestStarted { id } => {
                object.insert("event".to_owned(), Value::String("test_started".to_owned()));
                object.insert("id".to_owned(), Value::String(id.clone()));
            }
            Self::TestFinished(finished) => {
                let TestFinished {
                    id,
                    verdict,
                    duration_ms,
                    failure,
                    stdout,
                    stderr,
                    scratch_dir,
                    repro,
                } = finished.as_ref();
                object.insert(
                    "event".to_owned(),
                    Value::String("test_finished".to_owned()),
                );
                object.insert("id".to_owned(), Value::String(id.clone()));
                object.insert(
                    "verdict".to_owned(),
                    Value::String(verdict.as_str().to_owned()),
                );
                object.insert("duration_ms".to_owned(), Value::from(*duration_ms));
                let mut capability = Map::new();
                capability.insert(
                    "status".to_owned(),
                    Value::String(CAPABILITY_UNAVAILABLE.to_owned()),
                );
                object.insert("capability_summary".to_owned(), Value::Object(capability));
                if let Some(failure) = failure {
                    object.insert("failure".to_owned(), failure.to_json());
                }
                object.insert("stdout".to_owned(), stdout.to_json());
                object.insert("stderr".to_owned(), stderr.to_json());
                if let Some(scratch) = scratch_dir {
                    object.insert("scratch_dir".to_owned(), Value::String(scratch.clone()));
                }
                object.insert(
                    "repro".to_owned(),
                    Value::Array(repro.iter().cloned().map(Value::String).collect()),
                );
            }
            Self::RunFinished {
                passed,
                failed,
                timeout,
                crash,
                wall_ms,
                unimported_test_files,
                test_candidates,
            } => {
                object.insert("event".to_owned(), Value::String("run_finished".to_owned()));
                object.insert("passed".to_owned(), Value::from(*passed));
                object.insert("failed".to_owned(), Value::from(*failed));
                object.insert("timeout".to_owned(), Value::from(*timeout));
                object.insert("crash".to_owned(), Value::from(*crash));
                object.insert("wall_ms".to_owned(), Value::from(*wall_ms));
                if let Some(files) = unimported_test_files {
                    object.insert(
                        "unimported_test_files".to_owned(),
                        Value::Array(
                            files
                                .iter()
                                .map(|file| {
                                    let mut entry = Map::new();
                                    entry.insert(
                                        "path".to_owned(),
                                        Value::String(file.path.clone()),
                                    );
                                    entry.insert("tests".to_owned(), Value::from(file.tests));
                                    entry.insert(
                                        "parse_failed".to_owned(),
                                        Value::Bool(file.parse_failed),
                                    );
                                    Value::Object(entry)
                                })
                                .collect(),
                        ),
                    );
                }
                object.insert(
                    "test_candidates".to_owned(),
                    Value::String(test_candidates.as_str().to_owned()),
                );
            }
            Self::Test {
                id,
                module,
                name,
                file,
                line,
                column,
            } => {
                object.insert("event".to_owned(), Value::String("test".to_owned()));
                object.insert(
                    "schema".to_owned(),
                    Value::String(SCHEMA_VERSION.to_owned()),
                );
                object.insert("id".to_owned(), Value::String(id.clone()));
                object.insert("module".to_owned(), Value::String(module.clone()));
                object.insert("name".to_owned(), Value::String(name.clone()));
                object.insert("file".to_owned(), Value::String(file.clone()));
                object.insert("line".to_owned(), Value::from(*line));
                object.insert("column".to_owned(), Value::from(*column));
            }
        }
        Value::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_mode::verdict::FailureKind;

    /// Alphabetical key order is a determinism promise, not an accident of the
    /// struct's field order — the same stance `diagnostics.md` takes.
    #[test]
    fn keys_are_serialized_in_alphabetical_order() {
        let line = Event::RunStarted {
            root: "app/main.rue".to_owned(),
            target: "aarch64-macos".to_owned(),
            opt_level: "0".to_owned(),
            seed: 417,
            jobs: 4,
            shard: Some("1/2".to_owned()),
            selected: 3,
            total: 8,
        }
        .to_ndjson();
        assert_eq!(
            line,
            "{\"event\":\"run_started\",\"jobs\":4,\"opt_level\":\"0\",\
             \"plan\":{\"selected\":3,\"total\":8},\"root\":\"app/main.rue\",\
             \"schema\":\"1.0\",\"seed\":417,\"shard\":\"1/2\",\"target\":\"aarch64-macos\"}"
        );
    }

    #[test]
    fn an_absent_shard_is_omitted_rather_than_null() {
        let line = Event::RunStarted {
            root: "m.rue".to_owned(),
            target: "x86-64-linux".to_owned(),
            opt_level: "1".to_owned(),
            seed: 1,
            jobs: 1,
            shard: None,
            selected: 1,
            total: 1,
        }
        .to_ndjson();
        assert!(!line.contains("shard"), "{line}");
    }

    #[test]
    fn a_passing_test_publishes_digests_and_no_scratch_directory() {
        let line = Event::TestFinished(Box::new(TestFinished {
            id: "app/t.rue::ok".to_owned(),
            verdict: Verdict::Pass,
            duration_ms: 2,
            failure: None,
            stdout: Capture::new(b"hi\n".to_vec(), 3, true),
            stderr: Capture::new(Vec::new(), 0, true),
            scratch_dir: None,
            repro: vec!["rue".to_owned(), "test".to_owned()],
        }))
        .to_ndjson();
        assert!(line.contains("\"verdict\":\"pass\""), "{line}");
        assert!(line.contains("\"digest\":\"sha256:"), "{line}");
        assert!(!line.contains("\"data\""), "{line}");
        assert!(!line.contains("scratch_dir"), "{line}");
        assert!(
            line.contains("\"capability_summary\":{\"status\":\"unavailable\"}"),
            "{line}"
        );
    }

    #[test]
    fn a_failing_test_carries_its_bytes_scratch_directory_and_repro() {
        let line = Event::TestFinished(Box::new(TestFinished {
            id: "app/t.rue::bad".to_owned(),
            verdict: Verdict::Fail(FailureKind::Assert),
            duration_ms: 5,
            failure: Some(FailureRecord {
                kind: "assert".to_owned(),
                message: "assertion failed".to_owned(),
                exit_code: Some(101),
                location: Some(Location {
                    file: "app/t.rue".to_owned(),
                    line: 4,
                    column: 5,
                }),
                ..FailureRecord::default()
            }),
            stdout: Capture::new(Vec::new(), 0, false),
            stderr: Capture::new(b"assertion failed\n".to_vec(), 17, false),
            scratch_dir: Some("/tmp/rue-test-1-0".to_owned()),
            repro: vec![
                "rue".to_owned(),
                "test".to_owned(),
                "app/main.rue".to_owned(),
            ],
        }))
        .to_ndjson();
        assert!(line.contains("\"verdict\":\"fail\""), "{line}");
        assert!(line.contains("\"data\":\"assertion failed\\n\""), "{line}");
        assert!(line.contains("\"exit_code\":101"), "{line}");
        assert!(
            line.contains("\"location\":{\"column\":5,\"file\":\"app/t.rue\",\"line\":4}"),
            "{line}"
        );
        assert!(
            line.contains("\"scratch_dir\":\"/tmp/rue-test-1-0\""),
            "{line}"
        );
    }

    /// Rue strings are arbitrary bytes written raw, so capture is lossless
    /// within its window rather than lossy-UTF-8.
    #[test]
    fn invalid_utf8_capture_is_tagged_and_base64_encoded() {
        let capture = Capture::new(vec![0xff, 0xfe, 0x00], 3, false);
        assert_eq!(capture.encoding, Encoding::Base64);
        assert_eq!(capture.encoded_data(), "//4A");
    }

    #[test]
    fn base64_pads_every_chunk_length() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_listing_record_names_its_schema_version() {
        let line = Event::Test {
            id: "app/t.rue::ok".to_owned(),
            module: "app/t.rue".to_owned(),
            name: "ok".to_owned(),
            file: "/w/app/t.rue".to_owned(),
            line: 1,
            column: 1,
        }
        .to_ndjson();
        assert_eq!(
            line,
            "{\"column\":1,\"event\":\"test\",\"file\":\"/w/app/t.rue\",\
             \"id\":\"app/t.rue::ok\",\"line\":1,\"module\":\"app/t.rue\",\
             \"name\":\"ok\",\"schema\":\"1.0\"}"
        );
    }

    #[test]
    fn run_finished_states_the_candidate_source_either_way() {
        let declared = Event::RunFinished {
            passed: 1,
            failed: 0,
            timeout: 0,
            crash: 0,
            wall_ms: 12,
            unimported_test_files: Some(vec![UnimportedFile {
                path: "app/orphan.rue".to_owned(),
                tests: 2,
                parse_failed: false,
            }]),
            test_candidates: CandidateSource::Declared,
        }
        .to_ndjson();
        assert!(
            declared.contains("\"test_candidates\":\"declared\""),
            "{declared}"
        );
        assert!(
            declared.contains(
                "\"unimported_test_files\":[{\"parse_failed\":false,\"path\":\"app/orphan.rue\",\"tests\":2}]"
            ),
            "{declared}"
        );

        let none = Event::RunFinished {
            passed: 0,
            failed: 0,
            timeout: 0,
            crash: 0,
            wall_ms: 0,
            unimported_test_files: None,
            test_candidates: CandidateSource::None,
        }
        .to_ndjson();
        assert!(none.contains("\"test_candidates\":\"none\""), "{none}");
        assert!(!none.contains("unimported_test_files"), "{none}");
    }
}
