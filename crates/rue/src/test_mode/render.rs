//! The human renderer, as a consumer of the event stream (ADR-0083 §2).
//!
//! It reads the same `Event` values the NDJSON writer serializes, in the same
//! process — never re-parsed text, and never a second computation of what to
//! say. It is deliberately stateless: every number in the summary comes out of
//! the `run_finished` event's own fields rather than a private tally, so a
//! person and a tool cannot be shown two different counts of the same run.
//!
//! Verbosity is asymmetric on purpose. A failure prints whole — structure,
//! location, captured output, the retained scratch directory, and the argv that
//! reproduces it alone — and a pass prints nothing at all. No wall of green.

use std::fmt::Write as _;

use super::diff::{DiffOp, Hunk};
use super::events::{CandidateSource, Capture, Comparison, Event, TestFinished};
use super::verdict::Verdict;

/// Render one event for a person, or `None` when a person is owed nothing by
/// it.
pub(crate) fn render(event: &Event) -> Option<String> {
    match event {
        Event::RunStarted { .. } | Event::TestStarted { .. } => None,
        Event::Test { id, .. } => Some(id.clone()),
        Event::TestFinished(finished) => {
            (!finished.verdict.is_pass()).then(|| render_failure(finished))
        }
        Event::RunFinished {
            passed,
            failed,
            timeout,
            crash,
            wall_ms,
            unimported_test_files,
            test_candidates,
        } => {
            let mut out = String::new();
            if let Some(files) = unimported_test_files {
                for file in files {
                    if file.parse_failed {
                        let _ = writeln!(
                            out,
                            "warning: test file '{}' is outside the compiled closure and could not be parsed",
                            file.path
                        );
                    } else {
                        let plural = if file.tests == 1 { "" } else { "s" };
                        let _ = writeln!(
                            out,
                            "warning: test file '{}' declares {} test{plural} but no module in the compiled closure imports it",
                            file.path, file.tests
                        );
                    }
                }
            }
            out.push_str(&summary(*passed, *failed, *timeout, *crash, *wall_ms));
            if *test_candidates == CandidateSource::None {
                out.push('\n');
                out.push_str(
                    "note: no --test-candidates inventory; unimported test files are not detected",
                );
            }
            Some(out)
        }
    }
}

/// `41 passed, 1 failed (0.9s)`, naming the classes that occurred.
///
/// A zero count for timeouts or crashes is left out rather than printed as
/// `0 timed out`: those are not ordinary outcomes, and a line that always
/// mentions them trains a reader to stop seeing them.
fn summary(passed: usize, failed: usize, timeout: usize, crash: usize, wall_ms: u64) -> String {
    let mut parts = vec![format!("{passed} passed")];
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if timeout > 0 {
        parts.push(format!("{timeout} timed out"));
    }
    if crash > 0 {
        parts.push(format!("{crash} crashed"));
    }
    format!("{} ({})", parts.join(", "), seconds(wall_ms))
}

/// A duration a person reads, to a tenth of a second.
fn seconds(millis: u64) -> String {
    format!("{}.{}s", millis / 1000, (millis % 1000) / 100)
}

/// One non-passing test, whole.
fn render_failure(finished: &TestFinished) -> String {
    let TestFinished {
        id,
        verdict,
        failure,
        stdout,
        stderr,
        scratch_dir,
        repro,
        ..
    } = finished;
    let banner = match verdict {
        Verdict::Pass => "PASS",
        Verdict::Fail(_) => "FAIL",
        Verdict::Timeout => "TIMEOUT",
        Verdict::Crash(_) => "CRASH",
    };
    let mut out = format!("{banner} {id}");
    if let Some(failure) = failure {
        out.push_str("\n  ");
        out.push_str(&failure.kind);
        if !failure.message.is_empty() {
            let _ = write!(out, ": {}", failure.message);
        }
        if let Some(location) = &failure.location {
            let _ = write!(
                out,
                "  ({}:{}:{})",
                location.file, location.line, location.column
            );
        }
        if let Some(payload) = &failure.payload {
            if !payload.is_empty() {
                let _ = write!(out, "\n  payload: {payload}");
            }
        }
        if let Some(comparison) = &failure.comparison {
            push_comparison(&mut out, comparison);
        }
        if let Some(note) = &failure.runner_note {
            let _ = write!(out, "\n  note: {note}");
        }
    }
    push_capture(&mut out, "stdout", stdout);
    push_capture(&mut out, "stderr", stderr);
    if let Some(scratch) = scratch_dir {
        let _ = write!(out, "\n  scratch: {scratch}");
    }
    let _ = write!(out, "\n  repro: {}", shell_command(repro));
    out
}

/// The two operands of a comparison assertion, and where they differ.
///
/// Both values are always printed, because "these two are not equal" is only
/// half the report; the third element is what the runner computed about them,
/// and it is drawn from the same `diff` the event stream publishes rather than
/// recomputed here. A single-line pair gets a caret under the first differing
/// character, which is the whole answer for the common case of one wrong digit;
/// a multi-line pair gets the `-`/`+` listing, because a caret into a wall of
/// text locates nothing.
fn push_comparison(out: &mut String, comparison: &Comparison) {
    let multi_line = comparison.left.contains('\n') || comparison.right.contains('\n');
    if !multi_line {
        let _ = write!(out, "\n  left:  {}", comparison.left);
        let _ = write!(out, "\n  right: {}", comparison.right);
        if let Some(column) = first_difference(&comparison.diff) {
            let _ = write!(out, "\n  {}^", " ".repeat(LABEL_WIDTH - 2 + column));
        }
        return;
    }
    push_block(out, "left", &comparison.left);
    push_block(out, "right", &comparison.right);
    out.push_str("\n  diff:");
    for hunk in &comparison.diff {
        let marker = match hunk.op {
            DiffOp::Equal => ' ',
            DiffOp::Delete => '-',
            DiffOp::Insert => '+',
        };
        for line in hunk.text.lines() {
            let _ = write!(out, "\n    {marker} {line}");
        }
    }
}

/// Width of the `left:  ` / `right: ` labels, including the two-space indent
/// every line of a failure carries.
const LABEL_WIDTH: usize = 9;

/// The character offset of the first difference, or `None` when the two values
/// are identical — which is exactly how an `@assert_ne` failure looks.
fn first_difference(diff: &[Hunk]) -> Option<usize> {
    let mut offset = 0;
    for hunk in diff {
        if hunk.op != DiffOp::Equal {
            return Some(offset);
        }
        offset += hunk.text.chars().count();
    }
    None
}

/// One labelled multi-line value, its lines indented under the label.
fn push_block(out: &mut String, label: &str, value: &str) {
    let _ = write!(out, "\n  {label}:");
    for line in value.lines() {
        let _ = write!(out, "\n    {line}");
    }
}

/// A captured stream, indented under its failure, or nothing when it is empty.
fn push_capture(out: &mut String, label: &str, capture: &Capture) {
    if capture.bytes_total == 0 {
        return;
    }
    let _ = write!(out, "\n  --- {label} ({} bytes) ---", capture.bytes_total);
    for line in capture.encoded_data().lines() {
        let _ = write!(out, "\n  {line}");
    }
    if capture.bytes_total > capture.retained.len() as u64 {
        let _ = write!(
            out,
            "\n  ... {} further bytes were not retained",
            capture.bytes_total - capture.retained.len() as u64
        );
    }
}

/// The repro argv as a line a person can paste.
///
/// Quoting is presentation only: the argv the event stream publishes is the
/// authoritative form, because a test name may contain any byte a shell would
/// argue about and a consumer should never have to unquote to re-execute.
fn shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            let safe = argument.bytes().all(|byte| {
                matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'=' | b':')
            });
            if argument.is_empty() || !safe {
                format!("'{}'", argument.replace('\'', "'\\''"))
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_mode::events::{Comparison, FailureRecord, Location, UnimportedFile};
    use crate::test_mode::verdict::FailureKind;

    fn finished(verdict: Verdict, failure: Option<FailureRecord>) -> Event {
        Event::TestFinished(Box::new(TestFinished {
            id: "app/t.rue::parses a port".to_owned(),
            verdict,
            duration_ms: 3,
            failure,
            stdout: Capture::new(b"checking\n".to_vec(), 9, false),
            stderr: Capture::new(b"assertion failed\n".to_vec(), 17, false),
            scratch_dir: Some("/tmp/rue-test-417-2".to_owned()),
            repro: vec![
                "rue".to_owned(),
                "test".to_owned(),
                "app/main.rue".to_owned(),
                "--filter".to_owned(),
                "app/t.rue::parses a port".to_owned(),
            ],
        }))
    }

    fn run_finished(passed: usize, failed: usize, timeout: usize, crash: usize) -> Event {
        Event::RunFinished {
            passed,
            failed,
            timeout,
            crash,
            wall_ms: 900,
            unimported_test_files: Some(Vec::new()),
            test_candidates: CandidateSource::Declared,
        }
    }

    /// No wall of green: a pass prints nothing.
    #[test]
    fn a_pass_prints_nothing() {
        assert!(render(&finished(Verdict::Pass, None)).is_none());
    }

    /// The head events are machine bookkeeping; a person is shown failures and
    /// a summary, not a running commentary.
    #[test]
    fn run_and_test_start_print_nothing() {
        assert!(
            render(&Event::TestStarted {
                id: "app/t.rue::ok".to_owned()
            })
            .is_none()
        );
        assert!(
            render(&Event::RunStarted {
                root: "m.rue".to_owned(),
                target: "x86-64-linux".to_owned(),
                opt_level: "0".to_owned(),
                seed: 1,
                jobs: 1,
                shard: None,
                selected: 1,
                total: 1,
            })
            .is_none()
        );
    }

    #[test]
    fn a_failure_prints_structure_output_scratch_and_repro() {
        let rendered = render(&finished(
            Verdict::Fail(FailureKind::Assert),
            Some(FailureRecord {
                kind: "assert".to_owned(),
                message: "assertion failed".to_owned(),
                exit_code: Some(101),
                location: Some(Location {
                    file: "app/t.rue".to_owned(),
                    line: 7,
                    column: 5,
                }),
                ..FailureRecord::default()
            }),
        ))
        .expect("a failure renders");
        assert!(
            rendered.starts_with("FAIL app/t.rue::parses a port"),
            "{rendered}"
        );
        assert!(
            rendered.contains("assert: assertion failed  (app/t.rue:7:5)"),
            "{rendered}"
        );
        assert!(rendered.contains("--- stdout (9 bytes) ---"), "{rendered}");
        assert!(rendered.contains("--- stderr (17 bytes) ---"), "{rendered}");
        assert!(
            rendered.contains("scratch: /tmp/rue-test-417-2"),
            "{rendered}"
        );
        assert!(
            rendered.contains("repro: rue test app/main.rue --filter 'app/t.rue::parses a port'"),
            "{rendered}"
        );
    }

    /// A single-line comparison prints both values aligned, and a caret under
    /// the first character that differs. The caret's column comes out of the
    /// same `diff` the event stream publishes, so the two surfaces cannot
    /// disagree about where the difference is.
    #[test]
    fn a_single_line_comparison_prints_both_values_and_a_caret() {
        let rendered = render(&finished(
            Verdict::Fail(FailureKind::AssertEq),
            Some(FailureRecord {
                kind: "assert_eq".to_owned(),
                message: "assertion failed: left == right".to_owned(),
                exit_code: Some(101),
                comparison: Some(Comparison::new("41".to_owned(), "42".to_owned())),
                ..FailureRecord::default()
            }),
        ))
        .expect("a failure renders");
        assert!(
            rendered.contains("\n  left:  41\n  right: 42\n          ^\n"),
            "{rendered}"
        );
    }

    /// An `@assert_ne` failure has two identical values, so there is no first
    /// difference and no caret is drawn — the two values *are* the report.
    #[test]
    fn identical_values_print_no_caret() {
        let rendered = render(&finished(
            Verdict::Fail(FailureKind::AssertNe),
            Some(FailureRecord {
                kind: "assert_ne".to_owned(),
                message: "assertion failed: left != right".to_owned(),
                comparison: Some(Comparison::new("41".to_owned(), "41".to_owned())),
                ..FailureRecord::default()
            }),
        ))
        .expect("a failure renders");
        assert!(
            rendered.contains("\n  left:  41\n  right: 41\n  --- stdout"),
            "{rendered}"
        );
    }

    /// A multi-line pair gets the `-`/`+` listing instead: a caret into a wall
    /// of text locates nothing.
    #[test]
    fn a_multi_line_comparison_prints_a_hunk_listing() {
        let rendered = render(&finished(
            Verdict::Fail(FailureKind::AssertEq),
            Some(FailureRecord {
                kind: "assert_eq".to_owned(),
                message: "assertion failed: left == right".to_owned(),
                comparison: Some(Comparison::new(
                    "alpha\nbeta\ngamma\n".to_owned(),
                    "alpha\nBETA\ngamma\n".to_owned(),
                )),
                ..FailureRecord::default()
            }),
        ))
        .expect("a failure renders");
        assert!(
            rendered.contains(
                "\n  left:\n    alpha\n    beta\n    gamma\
                 \n  right:\n    alpha\n    BETA\n    gamma\
                 \n  diff:\n      alpha\n    - beta\n    + BETA\n      gamma\n"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains('^'), "{rendered}");
    }

    /// A runner-level note is never dropped from the human surface either: it
    /// is the only account of a failure report the runner could not read.
    #[test]
    fn a_runner_note_is_printed_with_its_failure() {
        let rendered = render(&finished(
            Verdict::Fail(FailureKind::Exit),
            Some(FailureRecord {
                kind: "exit".to_owned(),
                message: "the test exited with status 101".to_owned(),
                runner_note: Some("the test failure channel could not be read".to_owned()),
                ..FailureRecord::default()
            }),
        ))
        .expect("a failure renders");
        assert!(
            rendered.contains("note: the test failure channel could not be read"),
            "{rendered}"
        );
    }

    #[test]
    fn timeouts_and_crashes_carry_their_own_banners() {
        let timeout = render(&finished(Verdict::Timeout, None)).expect("a timeout renders");
        assert!(timeout.starts_with("TIMEOUT "), "{timeout}");
        let crash = render(&finished(Verdict::Crash(11), None)).expect("a crash renders");
        assert!(crash.starts_with("CRASH "), "{crash}");
    }

    /// Every number a person reads comes from the event, so the two surfaces
    /// cannot report different counts for the same run.
    #[test]
    fn the_summary_names_only_the_classes_that_occurred() {
        assert!(
            render(&run_finished(2, 0, 0, 0))
                .unwrap()
                .starts_with("2 passed (0.9s)")
        );
        assert!(
            render(&run_finished(2, 1, 0, 0))
                .unwrap()
                .starts_with("2 passed, 1 failed (0.9s)")
        );
        assert!(
            render(&run_finished(2, 1, 1, 1))
                .unwrap()
                .starts_with("2 passed, 1 failed, 1 timed out, 1 crashed (0.9s)")
        );
    }

    /// Without an inventory the runner cannot detect orphaned test files, and
    /// says so rather than leaving silence to be read as "none found".
    #[test]
    fn a_run_without_a_candidate_inventory_says_so() {
        let rendered = render(&Event::RunFinished {
            passed: 0,
            failed: 0,
            timeout: 0,
            crash: 0,
            wall_ms: 100,
            unimported_test_files: None,
            test_candidates: CandidateSource::None,
        })
        .expect("a summary renders");
        assert!(rendered.contains("0 passed (0.1s)"), "{rendered}");
        assert!(
            rendered.contains(
                "note: no --test-candidates inventory; unimported test files are not detected"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn orphaned_test_files_are_rendered_as_warnings() {
        let rendered = render(&Event::RunFinished {
            passed: 1,
            failed: 0,
            timeout: 0,
            crash: 0,
            wall_ms: 0,
            unimported_test_files: Some(vec![
                UnimportedFile {
                    path: "app/orphan.rue".to_owned(),
                    tests: 1,
                    parse_failed: false,
                },
                UnimportedFile {
                    path: "app/broken.rue".to_owned(),
                    tests: 0,
                    parse_failed: true,
                },
            ]),
            test_candidates: CandidateSource::Declared,
        })
        .expect("a summary renders");
        assert!(
            rendered.contains("declares 1 test but no module in the compiled closure imports it"),
            "{rendered}"
        );
        assert!(rendered.contains("could not be parsed"), "{rendered}");
        assert!(
            !rendered.contains("note: no --test-candidates"),
            "{rendered}"
        );
    }

    #[test]
    fn a_listing_entry_renders_as_its_bare_identity() {
        assert_eq!(
            render(&Event::Test {
                id: "app/t.rue::ok".to_owned(),
                module: "app/t.rue".to_owned(),
                name: "ok".to_owned(),
                file: "app/t.rue".to_owned(),
                line: 1,
                column: 1,
            }),
            Some("app/t.rue::ok".to_owned())
        );
    }

    #[test]
    fn repro_quoting_survives_a_name_with_a_quote_in_it() {
        assert_eq!(
            shell_command(&["rue".to_owned(), "it's fine".to_owned()]),
            "rue 'it'\\''s fine'"
        );
        assert_eq!(
            shell_command(&["--seed".to_owned(), "417".to_owned()]),
            "--seed 417"
        );
    }

    #[test]
    fn truncated_capture_says_how_much_was_dropped() {
        let mut out = String::new();
        push_capture(
            &mut out,
            "stdout",
            &Capture::new(b"kept".to_vec(), 1000, false),
        );
        assert!(out.contains("--- stdout (1000 bytes) ---"), "{out}");
        assert!(out.contains("996 further bytes were not retained"), "{out}");
    }
}
