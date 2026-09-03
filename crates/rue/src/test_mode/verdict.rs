//! The verdict taxonomy and its one classification function (ADR-0083 §2).
//!
//! Every published verdict is decided here, from an observation of one finished
//! test process: its exit status, its stderr, the frames it wrote on the §5.1
//! failure channel, and whether the runner killed it. Keeping the decision in
//! one total function is what makes the taxonomy testable without spawning
//! anything, and what keeps the human renderer and the event stream from
//! disagreeing — both read the same `Verdict`.
//!
//! Reserved and unproduced in this version, per ADR-0083 §6: the `skipped`
//! verdict (settled OUT of the v1 taxonomy — filtering removes tests from the
//! selection rather than reporting them, and `@skip` is deferred with directive
//! grammar), the `compile_error` and `cached_pass` verdicts, and the `ice`
//! failure kind. `docs/process/test-events.md` documents each as reserved.

use std::fmt;

/// The pinned stderr message of a runtime trap, and the `trap:<class>` name the
/// event stream publishes for it.
///
/// The messages are the ones `crates/rue-runtime/src/error.rs` and
/// `entry.rs` write byte-by-byte before `exit(101)`. They are a contract with
/// the runtime, not a heuristic: `cases/rue_test.toml` runs real programs that
/// take each of these paths through the real runtime, so a runtime that
/// reworded one fails those cases rather than silently reclassifying a trap as
/// a bare `exit`.
const TRAP_MESSAGES: &[(&str, &str)] = &[
    // `__rue_panic_no_msg`. The message-carrying `__rue_panic` writes
    // `panic: <msg>`, which is matched by prefix below.
    ("panic", "panic"),
    ("error: division by zero", "div_by_zero"),
    ("error: integer overflow", "overflow"),
    ("error: integer cast overflow", "intcast_overflow"),
    ("error: index out of bounds", "bounds_check"),
    ("error: invalid UTF-8", "invalid_utf8"),
    ("stack overflow", "stack_overflow"),
];

/// `__rue_assert_failed`'s pinned message.
const ASSERT_MESSAGE: &str = "assertion failed";

/// `__rue_panic`'s prefix, ahead of the user's message text.
const PANIC_PREFIX: &str = "panic: ";

/// The runtime's abort status. Every trap, `@panic`, and failed `@assert`
/// exits with it (ADR-0083 Context: the failure model is abort-only).
const RUNTIME_ERROR_EXIT_CODE: i32 = rue_test_runner::RUNTIME_ERROR_EXIT_CODE;

/// Why a test failed.
///
/// `Reported` carries a kind a failure frame supplied verbatim. ADR-0083 §5.1
/// makes the channel an open protocol — user assertion libraries emit the same
/// records as the built-ins — so a kind the runner does not recognize is
/// published rather than flattened into `exit`. The built-in producers use
/// `unhandled_error`, which is normalized to its own variant on construction so
/// there is exactly one spelling of it in the taxonomy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// Exit 0 with no completion record: the body called `std.exit(0)` before
    /// reaching the dispatcher's epilogue (ADR-0083 §3).
    Incomplete,
    /// A failed `@assert(cond)`.
    Assert,
    /// A failed `@assert_eq(left, right)` (ADR-0083 Phase 2.5). Its frame is
    /// the one that carries `expected` and `actual`.
    AssertEq,
    /// A failed `@assert_ne(left, right)`, whose frame carries the two values
    /// the assertion demanded be different.
    AssertNe,
    /// A runtime trap, named by its class.
    Trap(&'static str),
    /// A `?` in a test body whose failure arm reported through the channel.
    UnhandledError,
    /// Any other nonzero exit.
    Exit,
    /// A capture budget was exhausted and the process group was killed.
    OutputOverflow,
    /// A kind a failure frame supplied verbatim (ADR-0083 §5.1).
    Reported(String),
}

impl FailureKind {
    /// Build the kind a failure frame named, normalizing the built-in spelling.
    pub(crate) fn reported(kind: &str) -> Self {
        match kind {
            "unhandled_error" => Self::UnhandledError,
            "assert" => Self::Assert,
            "assert_eq" => Self::AssertEq,
            "assert_ne" => Self::AssertNe,
            other => Self::Reported(other.to_owned()),
        }
    }
}

impl fmt::Display for FailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => f.write_str("incomplete"),
            Self::Assert => f.write_str("assert"),
            Self::AssertEq => f.write_str("assert_eq"),
            Self::AssertNe => f.write_str("assert_ne"),
            Self::Trap(class) => write!(f, "trap:{class}"),
            Self::UnhandledError => f.write_str("unhandled_error"),
            Self::Exit => f.write_str("exit"),
            Self::OutputOverflow => f.write_str("output_overflow"),
            Self::Reported(kind) => f.write_str(kind),
        }
    }
}

/// What one test process produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass,
    Fail(FailureKind),
    /// The per-test wall-clock budget expired and the process group was killed.
    Timeout,
    /// Killed by a signal, SIGPIPE included.
    Crash(i32),
}

impl Verdict {
    /// The `verdict` field's published value.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail(_) => "fail",
            Self::Timeout => "timeout",
            Self::Crash(_) => "crash",
        }
    }

    pub(crate) fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// One frame read from the failure channel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FailureFrame {
    pub(crate) kind: String,
    pub(crate) message: String,
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) column: u32,
    pub(crate) payload: String,
    /// The left operand of a comparison assertion, rendered (ADR-0083 Phase
    /// 2.5). `None` when the frame carried no `expected` field at all, which is
    /// what every non-comparison producer writes.
    ///
    /// Empty is a value, not an absence: `@assert_eq` over two empty strings
    /// fails with two empty renderings, and dropping them would leave a
    /// consumer unable to tell that case from an `@assert`.
    pub(crate) expected: Option<String>,
    /// The right operand, rendered. Present exactly when `expected` is.
    pub(crate) actual: Option<String>,
}

/// What the channel carried, as the classifier needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChannelFrames {
    /// The dispatcher's terminal completion record was observed.
    pub(crate) complete: bool,
    /// The first well-formed `failure` frame. Later ones are additional
    /// reports from the same aborting process; the first is the one that
    /// caused the abort.
    pub(crate) failure: Option<FailureFrame>,
    /// A line on the channel that was not a well-formed frame, with the reason.
    ///
    /// Never discarded: a channel the runner cannot read is a runner defect,
    /// and swallowing it would turn a broken failure report into a bare exit.
    pub(crate) malformed: Option<String>,
}

/// Parse the bytes drained from the failure channel.
///
/// The channel is newline-delimited JSON. A line that is not valid JSON, or
/// carries no known `record`, is recorded as malformed rather than skipped.
pub(crate) fn parse_channel(bytes: &[u8]) -> ChannelFrames {
    let mut frames = ChannelFrames::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let text = match std::str::from_utf8(line) {
            Ok(text) => text,
            Err(_) => {
                note_malformed(&mut frames, "a channel line was not valid UTF-8");
                continue;
            }
        };
        let value: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                note_malformed(
                    &mut frames,
                    &format!("a channel line was not JSON: {error}"),
                );
                continue;
            }
        };
        match value.get("record").and_then(serde_json::Value::as_str) {
            Some("complete") => frames.complete = true,
            Some("failure") => {
                if frames.failure.is_none() {
                    frames.failure = Some(FailureFrame {
                        kind: string_field(&value, "kind"),
                        message: string_field(&value, "message"),
                        file: value
                            .get("location")
                            .map(|location| string_field(location, "file"))
                            .unwrap_or_default(),
                        line: location_number(&value, "line"),
                        column: location_number(&value, "column"),
                        payload: string_field(&value, "payload"),
                        expected: optional_string_field(&value, "expected"),
                        actual: optional_string_field(&value, "actual"),
                    });
                }
            }
            Some(other) => note_malformed(
                &mut frames,
                &format!("a channel line named an unknown record '{other}'"),
            ),
            None => note_malformed(&mut frames, "a channel line carried no 'record' field"),
        }
    }
    frames
}

fn note_malformed(frames: &mut ChannelFrames, reason: &str) {
    if frames.malformed.is_none() {
        frames.malformed = Some(reason.to_owned());
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A string field that may be absent, keeping absence distinct from empty.
fn optional_string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn location_number(value: &serde_json::Value, key: &str) -> u32 {
    value
        .get("location")
        .and_then(|location| location.get(key))
        .and_then(serde_json::Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .unwrap_or(0)
}

/// How the runner's own supervision ended a test, before its exit status is
/// consulted at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Supervision {
    /// The process exited on its own.
    Exited,
    /// The wall-clock budget expired; the group was killed.
    TimedOut,
    /// A capture budget was exhausted; the group was killed.
    OutputOverflow,
}

/// Everything the classifier reads about one finished test process.
#[derive(Debug, Clone)]
pub(crate) struct Observation<'a> {
    pub(crate) supervision: Supervision,
    /// `Ok(code)` for an ordinary exit, `Err(signal)` for a signal death.
    pub(crate) status: Result<i32, i32>,
    pub(crate) stderr: &'a [u8],
    pub(crate) frames: &'a ChannelFrames,
}

/// The verdict, and the runner's own note when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classification {
    pub(crate) verdict: Verdict,
    /// A runner-level explanation attached to the failure record. Present when
    /// the runner itself could not trust what it read.
    pub(crate) runner_note: Option<String>,
}

/// Decide one test's verdict.
///
/// The order of the arms is the taxonomy's precedence and is deliberate.
/// Supervision outcomes come first because the runner's own kill is what
/// produced the signal death that would otherwise read as a crash. A malformed
/// channel is next: an unreadable failure report must never be reported as a
/// pass or as a bare exit with no explanation. Only then is the process's own
/// account of itself consulted — a failure frame ahead of the exit status,
/// since the frame carries structure the status cannot.
pub(crate) fn classify(observation: Observation<'_>) -> Classification {
    let frames = observation.frames;
    match observation.supervision {
        Supervision::TimedOut => {
            return Classification {
                verdict: Verdict::Timeout,
                runner_note: None,
            };
        }
        Supervision::OutputOverflow => {
            return Classification {
                verdict: Verdict::Fail(FailureKind::OutputOverflow),
                runner_note: None,
            };
        }
        Supervision::Exited => {}
    }

    if let Some(reason) = &frames.malformed {
        return Classification {
            verdict: Verdict::Fail(FailureKind::Exit),
            runner_note: Some(format!(
                "the test failure channel could not be read: {reason}"
            )),
        };
    }

    if let Some(failure) = &frames.failure {
        return Classification {
            verdict: Verdict::Fail(FailureKind::reported(&failure.kind)),
            runner_note: None,
        };
    }

    let code = match observation.status {
        Ok(code) => code,
        Err(signal) => {
            return Classification {
                verdict: Verdict::Crash(signal),
                runner_note: None,
            };
        }
    };

    if code == 0 {
        return if frames.complete {
            Classification {
                verdict: Verdict::Pass,
                runner_note: None,
            }
        } else {
            Classification {
                verdict: Verdict::Fail(FailureKind::Incomplete),
                runner_note: None,
            }
        };
    }

    if code == RUNTIME_ERROR_EXIT_CODE {
        if let Some(kind) = classify_runtime_message(observation.stderr) {
            return Classification {
                verdict: Verdict::Fail(kind),
                runner_note: None,
            };
        }
    }

    Classification {
        verdict: Verdict::Fail(FailureKind::Exit),
        runner_note: None,
    }
}

/// Recognize a pinned runtime abort message on a trapping process's stderr.
///
/// The last non-empty line is what is matched, not the whole stream: a test
/// that printed diagnostics of its own before tripping an assertion still
/// trapped, and classifying it as a bare `exit` because it was chatty would
/// lose the one piece of structure the abort-only runtime gives us. The match
/// against that line is exact (or, for `@panic("msg")`, exact on the pinned
/// prefix), so a reworded runtime message is a failed CLI case rather than a
/// silent reclassification.
fn classify_runtime_message(stderr: &[u8]) -> Option<FailureKind> {
    let text = String::from_utf8_lossy(stderr);
    let last = text.lines().rev().find(|line| !line.trim().is_empty())?;
    let last = last.trim_end();
    if last == ASSERT_MESSAGE {
        return Some(FailureKind::Assert);
    }
    if last.starts_with(PANIC_PREFIX) {
        return Some(FailureKind::Trap("panic"));
    }
    TRAP_MESSAGES
        .iter()
        .find(|(message, _)| *message == last)
        .map(|(_, class)| FailureKind::Trap(class))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(status: Result<i32, i32>, stderr: &str, frames: &ChannelFrames) -> Classification {
        classify(Observation {
            supervision: Supervision::Exited,
            status,
            stderr: stderr.as_bytes(),
            frames,
        })
    }

    fn complete() -> ChannelFrames {
        ChannelFrames {
            complete: true,
            ..ChannelFrames::default()
        }
    }

    #[test]
    fn exit_zero_with_a_completion_record_passes() {
        assert_eq!(observe(Ok(0), "", &complete()).verdict, Verdict::Pass);
    }

    /// The `std.exit(0)`-before-the-epilogue case: a clean exit that never
    /// reached the dispatcher's completion record is false evidence, not a
    /// pass (ADR-0083 §3).
    #[test]
    fn exit_zero_without_a_completion_record_is_incomplete() {
        assert_eq!(
            observe(Ok(0), "", &ChannelFrames::default()).verdict,
            Verdict::Fail(FailureKind::Incomplete)
        );
    }

    #[test]
    fn a_failed_assert_is_classified_from_its_pinned_message() {
        assert_eq!(
            observe(Ok(101), "assertion failed\n", &ChannelFrames::default()).verdict,
            Verdict::Fail(FailureKind::Assert)
        );
    }

    #[test]
    fn every_pinned_trap_message_names_its_class() {
        let expected = [
            ("panic\n", "panic"),
            ("panic: boom\n", "panic"),
            ("error: division by zero\n", "div_by_zero"),
            ("error: integer overflow\n", "overflow"),
            ("error: integer cast overflow\n", "intcast_overflow"),
            ("error: index out of bounds\n", "bounds_check"),
            ("error: invalid UTF-8\n", "invalid_utf8"),
            ("stack overflow\n", "stack_overflow"),
        ];
        for (stderr, class) in expected {
            assert_eq!(
                observe(Ok(101), stderr, &ChannelFrames::default()).verdict,
                Verdict::Fail(FailureKind::Trap(class)),
                "stderr {stderr:?}"
            );
        }
    }

    /// A test that printed before it trapped still trapped.
    #[test]
    fn output_before_a_trap_does_not_hide_its_class() {
        assert_eq!(
            observe(
                Ok(101),
                "checking the parser\nassertion failed\n",
                &ChannelFrames::default()
            )
            .verdict,
            Verdict::Fail(FailureKind::Assert)
        );
    }

    /// Exit 101 is not enough on its own: an unrecognized message is an
    /// ordinary nonzero exit, never a fabricated trap class.
    #[test]
    fn an_unrecognized_message_at_the_runtime_status_is_an_exit() {
        assert_eq!(
            observe(
                Ok(101),
                "something else entirely\n",
                &ChannelFrames::default()
            )
            .verdict,
            Verdict::Fail(FailureKind::Exit)
        );
    }

    #[test]
    fn any_other_nonzero_exit_is_an_exit_failure() {
        assert_eq!(
            observe(Ok(3), "", &ChannelFrames::default()).verdict,
            Verdict::Fail(FailureKind::Exit)
        );
    }

    #[test]
    fn a_signal_death_is_a_crash_carrying_its_signal() {
        assert_eq!(
            observe(Err(13), "", &ChannelFrames::default()).verdict,
            Verdict::Crash(13)
        );
    }

    /// A failure frame outranks the exit status: it carries structure the
    /// status cannot, and the abort path behind it is the same 101.
    #[test]
    fn a_failure_frame_outranks_the_exit_classification() {
        let frames = ChannelFrames {
            failure: Some(FailureFrame {
                kind: "unhandled_error".to_owned(),
                message: "Err(NotFound)".to_owned(),
                file: "app/tests.rue".to_owned(),
                line: 7,
                column: 5,
                payload: String::new(),
                expected: None,
                actual: None,
            }),
            ..ChannelFrames::default()
        };
        assert_eq!(
            observe(Ok(101), "panic: unhandled error\n", &frames).verdict,
            Verdict::Fail(FailureKind::UnhandledError)
        );
    }

    /// ADR-0083 §5.1 makes the channel an open protocol, so a kind the runner
    /// does not know is published verbatim rather than flattened.
    #[test]
    fn a_library_supplied_kind_is_published_verbatim() {
        let frames = ChannelFrames {
            failure: Some(FailureFrame {
                kind: "expect_eq".to_owned(),
                ..FailureFrame::default()
            }),
            ..ChannelFrames::default()
        };
        let verdict = observe(Ok(101), "", &frames).verdict;
        assert_eq!(
            verdict,
            Verdict::Fail(FailureKind::Reported("expect_eq".to_owned()))
        );
        let Verdict::Fail(kind) = verdict else {
            unreachable!()
        };
        assert_eq!(kind.to_string(), "expect_eq");
    }

    #[test]
    fn supervision_outcomes_precede_the_exit_status() {
        let timed_out = classify(Observation {
            supervision: Supervision::TimedOut,
            status: Err(9),
            stderr: b"",
            frames: &ChannelFrames::default(),
        });
        assert_eq!(timed_out.verdict, Verdict::Timeout);

        let overflowed = classify(Observation {
            supervision: Supervision::OutputOverflow,
            status: Err(9),
            stderr: b"",
            frames: &ChannelFrames::default(),
        });
        assert_eq!(
            overflowed.verdict,
            Verdict::Fail(FailureKind::OutputOverflow)
        );
    }

    /// A channel the runner could not read is never silently ignored — least
    /// of all when the process otherwise looks like a pass.
    #[test]
    fn a_malformed_frame_fails_the_test_with_a_runner_note() {
        let frames = parse_channel(b"{\"record\":\"complete\"\n");
        assert!(frames.malformed.is_some());
        let classification = observe(Ok(0), "", &frames);
        assert_eq!(classification.verdict, Verdict::Fail(FailureKind::Exit));
        let note = classification.runner_note.expect("a note is required");
        assert!(note.contains("failure channel"), "{note}");
    }

    #[test]
    fn a_line_with_no_record_field_is_malformed() {
        let frames = parse_channel(b"{\"schema\":\"1.0\"}\n");
        assert!(
            frames
                .malformed
                .as_deref()
                .is_some_and(|note| note.contains("no 'record' field")),
            "{frames:?}"
        );
    }

    #[test]
    fn frames_parse_completion_and_failure_records() {
        let bytes = concat!(
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"unhandled_error\",",
            "\"message\":\"Err(NotFound)\",\"location\":{\"file\":\"a.rue\",\"line\":3,",
            "\"column\":9},\"payload\":\"\"}\n",
            "{\"record\":\"complete\",\"schema\":\"1.0\"}\n",
        );
        let frames = parse_channel(bytes.as_bytes());
        assert!(frames.complete);
        assert!(frames.malformed.is_none());
        let failure = frames.failure.expect("a failure frame");
        assert_eq!(failure.kind, "unhandled_error");
        assert_eq!(failure.message, "Err(NotFound)");
        assert_eq!(failure.file, "a.rue");
        assert_eq!(failure.line, 3);
        assert_eq!(failure.column, 9);
        assert_eq!(failure.expected, None);
        assert_eq!(failure.actual, None);
    }

    /// A comparison frame carries `expected` and `actual` and no payload
    /// (ADR-0083 Phase 2.5), and its kind is one the taxonomy names rather than
    /// a verbatim one.
    #[test]
    fn a_comparison_frame_parses_its_two_operands() {
        let bytes = concat!(
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert_eq\",",
            "\"message\":\"assertion failed: left == right\",",
            "\"location\":{\"file\":\"a.rue\",\"line\":3,\"column\":5},",
            "\"expected\":\"41\",\"actual\":\"42\"}\n",
        );
        let frames = parse_channel(bytes.as_bytes());
        assert!(frames.malformed.is_none());
        let failure = frames.failure.expect("a failure frame");
        assert_eq!(failure.expected.as_deref(), Some("41"));
        assert_eq!(failure.actual.as_deref(), Some("42"));
        assert_eq!(failure.payload, "");
        assert_eq!(
            FailureKind::reported(&failure.kind),
            FailureKind::AssertEq,
            "a built-in producer's kind is normalized, not published verbatim"
        );
    }

    /// Empty is a value, not an absence: `@assert_eq` over two empty renderings
    /// must still publish both sides, or a consumer cannot tell that failure
    /// from a bare `@assert`.
    #[test]
    fn empty_operands_stay_distinct_from_absent_ones() {
        let bytes = concat!(
            "{\"record\":\"failure\",\"schema\":\"1.0\",\"kind\":\"assert_ne\",",
            "\"message\":\"assertion failed: left != right\",",
            "\"location\":{\"file\":\"a.rue\",\"line\":1,\"column\":1},",
            "\"expected\":\"\",\"actual\":\"\"}\n",
        );
        let failure = parse_channel(bytes.as_bytes())
            .failure
            .expect("a failure frame");
        assert_eq!(failure.expected.as_deref(), Some(""));
        assert_eq!(failure.actual.as_deref(), Some(""));
    }

    /// A rendering that is not valid UTF-8 never becomes a frame at all: the
    /// whole channel line is rejected upstream, and the verdict carries the
    /// runner's note rather than a re-encoded operand. That is what keeps the
    /// diff's inputs `str` all the way down, with no second encoding tag.
    #[test]
    fn a_non_utf8_channel_line_is_malformed_rather_than_re_encoded() {
        let mut bytes = b"{\"record\":\"failure\",\"kind\":\"assert_eq\",\"expected\":\"".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\",\"actual\":\"\"}\n");
        let frames = parse_channel(&bytes);
        assert!(frames.failure.is_none());
        assert_eq!(
            frames.malformed.as_deref(),
            Some("a channel line was not valid UTF-8")
        );
        let classified = classify(Observation {
            supervision: Supervision::Exited,
            status: Ok(101),
            stderr: b"",
            frames: &frames,
        });
        assert_eq!(classified.verdict, Verdict::Fail(FailureKind::Exit));
        assert!(classified.runner_note.is_some());
    }

    /// A blank channel is the ordinary shape for a test image run with no
    /// descriptor 3 at all, and must not be mistaken for a broken one.
    #[test]
    fn an_empty_channel_is_not_malformed() {
        let frames = parse_channel(b"");
        assert_eq!(frames, ChannelFrames::default());
        assert_eq!(parse_channel(b"\n\n"), ChannelFrames::default());
    }

    #[test]
    fn failure_kinds_render_their_published_spelling() {
        assert_eq!(FailureKind::Incomplete.to_string(), "incomplete");
        assert_eq!(FailureKind::Assert.to_string(), "assert");
        assert_eq!(FailureKind::AssertEq.to_string(), "assert_eq");
        assert_eq!(FailureKind::AssertNe.to_string(), "assert_ne");
        assert_eq!(FailureKind::Trap("panic").to_string(), "trap:panic");
        assert_eq!(FailureKind::UnhandledError.to_string(), "unhandled_error");
        assert_eq!(FailureKind::Exit.to_string(), "exit");
        assert_eq!(FailureKind::OutputOverflow.to_string(), "output_overflow");
    }

    #[test]
    fn verdicts_render_their_published_spelling() {
        assert_eq!(Verdict::Pass.as_str(), "pass");
        assert_eq!(Verdict::Fail(FailureKind::Exit).as_str(), "fail");
        assert_eq!(Verdict::Timeout.as_str(), "timeout");
        assert_eq!(Verdict::Crash(11).as_str(), "crash");
    }
}
