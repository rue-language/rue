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

/// What the human renderer is told out of band.
///
/// The event schema is a published surface (`test-events.md`), so presentation
/// policy that is not run data does not become a field on an event. It reaches
/// the renderer here instead, and the NDJSON stream is byte-identical with or
/// without it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Context {
    /// Whether the compiled closure holds more than one user module.
    ///
    /// A closure of one has no second module that could have failed to import a
    /// test file, so the missing-inventory note would answer a question this run
    /// cannot raise. `run_finished.test_candidates` still says `"none"` either
    /// way (RUE-1959).
    pub(crate) multi_module_closure: bool,
}

/// Render one event for a person, or `None` when a person is owed nothing by
/// it.
///
/// Every line it produces is run data. Presentation policy that is not — the
/// missing-inventory notice — is [`notice`]'s, and goes to stderr.
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
            // The unimported-test-file warnings are the runner's own, and
            // stderr carries them once in every format (test-events.md,
            // "Streams"). Rendering them here too would print a second copy
            // on stdout whenever a terminal joins the streams.
            unimported_test_files: _,
            // The missing-inventory notice is the runner's own too, and goes
            // to stderr with them. See `notice`.
            test_candidates: _,
        } => Some(summary(*passed, *failed, *timeout, *crash, *wall_ms)),
    }
}

/// The runner's own notice for this event, or `None` when a run is owed none.
///
/// A notice is not run data, so it goes where the runner's warnings go: stderr,
/// once, and never repeated on stdout, where a terminal joining the streams
/// would show two copies of one line (test-events.md, "Streams"). Only the
/// human format is owed one at all — `--format json` publishes the same fact as
/// `run_finished.test_candidates`.
pub(crate) fn notice(event: &Event, context: Context) -> Option<&'static str> {
    match event {
        // RUE-1959: a closure of one user module has no second module that
        // could have failed to import a test file, so the note would answer a
        // question this run cannot raise. `test_candidates` is unaffected.
        Event::RunFinished {
            test_candidates: CandidateSource::None,
            ..
        } if context.multi_module_closure => {
            Some("note: no --test-candidates inventory; unimported test files are not detected")
        }
        _ => None,
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
        repro_env,
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
    let _ = write!(out, "\n  repro: {}", shell_command(repro_env, repro));
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

/// How many lines of one captured stream a person is shown before the middle is
/// elided, and the byte ceiling that applies first when the lines are long.
///
/// The retained window is a megabyte per stream, which is the right size for a
/// machine and about thirteen thousand lines for a person: a failure printed
/// whole would push its own `scratch:` and `repro:` lines out of the scrollback
/// it exists to be read in. `--format json` carries the window losslessly, so
/// nothing is lost by bounding what the terminal gets.
const DISPLAY_LINES: usize = 64;
const DISPLAY_BYTES: usize = 8 * 1024;
/// The elision keeps the start, where a test says what it was doing, and the
/// end, where it says how it stopped.
const HEAD_LINES: usize = 48;
const TAIL_LINES: usize = 16;

/// A captured stream, indented under its failure, or nothing when it is empty.
fn push_capture(out: &mut String, label: &str, capture: &Capture) {
    if capture.bytes_total == 0 {
        return;
    }
    let _ = write!(out, "\n  --- {label} ({} bytes) ---", capture.bytes_total);
    let data = capture.encoded_data();
    let lines: Vec<&str> = data.lines().collect();
    let (head, tail) = display_window(&lines, data.len());
    for line in &lines[..head] {
        let _ = write!(out, "\n  {line}");
    }
    let omitted = lines.len() - head - tail;
    if omitted > 0 {
        let shown: usize = lines[..head]
            .iter()
            .chain(&lines[lines.len() - tail..])
            .map(|line| line.len() + 1)
            .sum();
        let plural = if omitted == 1 { "" } else { "s" };
        let _ = write!(
            out,
            "\n  ... {omitted} line{plural} ({} bytes) omitted here; --format json carries the whole capture ...",
            data.len().saturating_sub(shown)
        );
    }
    for line in &lines[lines.len() - tail..] {
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

/// How many lines to print from each end of a capture.
///
/// This is a display bound, distinct from the retention window the capture
/// arrived with: a stream can be short enough to be retained whole and still be
/// too long to print. Lines are taken whole — a byte budget that lands mid-line
/// stops before it rather than cutting, so what is printed is always something
/// the test actually wrote.
fn display_window(lines: &[&str], bytes: usize) -> (usize, usize) {
    if lines.len() <= DISPLAY_LINES && bytes <= DISPLAY_BYTES {
        return (lines.len(), 0);
    }
    let tail_budget = DISPLAY_BYTES / 4;
    let head_budget = DISPLAY_BYTES - tail_budget;
    let head = whole_lines_within(lines.iter().copied(), head_budget, HEAD_LINES);
    let tail = whole_lines_within(lines.iter().rev().copied(), tail_budget, TAIL_LINES);
    // A capture shorter than head plus tail is only here because of the byte
    // budget; the two ends must still not overlap into a doubled line.
    (head, tail.min(lines.len() - head))
}

/// How many of `lines` fit in `budget` bytes, at most `limit` of them, counting
/// each line's newline and stopping before the line that would exceed it.
fn whole_lines_within<'a>(
    lines: impl Iterator<Item = &'a str>,
    budget: usize,
    limit: usize,
) -> usize {
    let mut used = 0;
    let mut taken = 0;
    for line in lines.take(limit) {
        used += line.len() + 1;
        if used > budget {
            break;
        }
        taken += 1;
    }
    taken
}

/// The repro as a line a person can paste: the environment the run depended
/// on, then the argv.
///
/// The assignments lead because that is where a shell accepts them, and each
/// one quotes only its value — quoting the name half would stop the shell from
/// reading the word as an assignment at all.
///
/// Quoting is presentation only: the argv and the `repro_env` object the event
/// stream publishes are the authoritative forms, because a test name may
/// contain any byte a shell would argue about and a consumer should never have
/// to unquote to re-execute.
fn shell_command(env: &[(String, String)], argv: &[String]) -> String {
    env.iter()
        .map(|(name, value)| format!("{name}={}", shell_word(value)))
        .chain(argv.iter().map(|argument| shell_word(argument)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One argument, quoted only when a shell would read it as more than itself.
fn shell_word(argument: &str) -> String {
    let safe = argument.bytes().all(|byte| {
        matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'=' | b':')
    });
    if argument.is_empty() || !safe {
        format!("'{}'", argument.replace('\'', "'\\''"))
    } else {
        argument.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_mode::events::{Comparison, FailureRecord, Location, UnimportedFile};
    use crate::test_mode::verdict::FailureKind;

    /// The notice an ordinary multi-module program is owed: the closure-size
    /// policy governs the missing-inventory note and nothing else.
    fn notice_multi(event: &Event) -> Option<&'static str> {
        super::notice(
            event,
            Context {
                multi_module_closure: true,
            },
        )
    }

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
                "/opt/rue/bin/rue".to_owned(),
                "test".to_owned(),
                "/work/app/main.rue".to_owned(),
                "--filter".to_owned(),
                "app/t.rue::parses a port".to_owned(),
            ],
            repro_env: vec![("RUE_STD_PATH".to_owned(), "/opt/rue/std".to_owned())],
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
        assert!(super::render(&finished(Verdict::Pass, None)).is_none());
    }

    /// The head events are machine bookkeeping; a person is shown failures and
    /// a summary, not a running commentary.
    #[test]
    fn run_and_test_start_print_nothing() {
        assert!(
            super::render(&Event::TestStarted {
                id: "app/t.rue::ok".to_owned()
            })
            .is_none()
        );
        assert!(
            super::render(&Event::RunStarted {
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
        let rendered = super::render(&finished(
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
        // The environment leads and every path is absolute, so the line runs
        // in a clean shell from any directory (RUE-2020).
        assert!(
            rendered.contains(
                "repro: RUE_STD_PATH=/opt/rue/std /opt/rue/bin/rue test /work/app/main.rue \
                 --filter 'app/t.rue::parses a port'"
            ),
            "{rendered}"
        );
    }

    /// A single-line comparison prints both values aligned, and a caret under
    /// the first character that differs. The caret's column comes out of the
    /// same `diff` the event stream publishes, so the two surfaces cannot
    /// disagree about where the difference is.
    #[test]
    fn a_single_line_comparison_prints_both_values_and_a_caret() {
        let rendered = super::render(&finished(
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
        let rendered = super::render(&finished(
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
        let rendered = super::render(&finished(
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
        let rendered = super::render(&finished(
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
        let timeout = super::render(&finished(Verdict::Timeout, None)).expect("a timeout renders");
        assert!(timeout.starts_with("TIMEOUT "), "{timeout}");
        let crash = super::render(&finished(Verdict::Crash(11), None)).expect("a crash renders");
        assert!(crash.starts_with("CRASH "), "{crash}");
    }

    /// Every number a person reads comes from the event, so the two surfaces
    /// cannot report different counts for the same run.
    #[test]
    fn the_summary_names_only_the_classes_that_occurred() {
        assert!(
            super::render(&run_finished(2, 0, 0, 0))
                .unwrap()
                .starts_with("2 passed (0.9s)")
        );
        assert!(
            super::render(&run_finished(2, 1, 0, 0))
                .unwrap()
                .starts_with("2 passed, 1 failed (0.9s)")
        );
        assert!(
            super::render(&run_finished(2, 1, 1, 1))
                .unwrap()
                .starts_with("2 passed, 1 failed, 1 timed out, 1 crashed (0.9s)")
        );
    }

    fn no_inventory() -> Event {
        Event::RunFinished {
            passed: 0,
            failed: 0,
            timeout: 0,
            crash: 0,
            wall_ms: 100,
            unimported_test_files: None,
            test_candidates: CandidateSource::None,
        }
    }

    /// Where an orphan is possible, the runner cannot detect one without an
    /// inventory and says so, rather than leaving silence to be read as "none
    /// found". It says so as a notice, so stdout carries the summary alone.
    #[test]
    fn a_multi_module_run_without_a_candidate_inventory_says_so() {
        let rendered = super::render(&no_inventory()).expect("a summary renders");
        assert_eq!(rendered, "0 passed (0.1s)");
        assert_eq!(
            notice_multi(&no_inventory()),
            Some("note: no --test-candidates inventory; unimported test files are not detected")
        );
    }

    /// A closure of one user module has no second module that could have failed
    /// to import a test file, so the note would answer a question this run
    /// cannot raise — noise under every filtered rerun pasted from a `repro:`
    /// line. The event's `test_candidates` is unchanged.
    #[test]
    fn a_single_module_run_is_owed_no_note_about_candidates() {
        let context = Context {
            multi_module_closure: false,
        };
        let rendered = super::render(&no_inventory()).expect("a summary renders");
        assert_eq!(rendered, "0 passed (0.1s)");
        assert_eq!(super::notice(&no_inventory(), context), None);
    }

    /// Only a run's terminal event can be owed a notice, so nothing repeats it
    /// per test.
    #[test]
    fn no_event_but_the_run_summary_carries_a_notice() {
        assert_eq!(notice_multi(&finished(Verdict::Pass, None)), None);
        assert_eq!(
            notice_multi(&Event::TestStarted {
                id: "app/t.rue::parses a port".to_owned(),
            }),
            None
        );
    }

    /// The runner already warns about these on stderr, in both formats. The
    /// human renderer writes to stdout, so repeating them here would show a
    /// person two copies of one warning on a terminal that joins the streams.
    /// A run that supplied an inventory is owed no notice either: it did look.
    #[test]
    fn orphaned_test_files_are_left_to_the_stderr_warning() {
        let event = Event::RunFinished {
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
        };
        let rendered = super::render(&event).expect("a summary renders");
        assert_eq!(rendered, "1 passed (0.0s)");
        assert!(!rendered.contains("warning:"), "{rendered}");
        assert!(!rendered.contains("app/orphan.rue"), "{rendered}");
        assert!(!rendered.contains("could not be parsed"), "{rendered}");
        assert_eq!(notice_multi(&event), None);
    }

    #[test]
    fn a_listing_entry_renders_as_its_bare_identity() {
        assert_eq!(
            super::render(&Event::Test {
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
            shell_command(&[], &["rue".to_owned(), "it's fine".to_owned()]),
            "rue 'it'\\''s fine'"
        );
        assert_eq!(
            shell_command(&[], &["--seed".to_owned(), "417".to_owned()]),
            "--seed 417"
        );
    }

    /// An assignment quotes only its value: quoting the name half would stop
    /// the shell from reading the word as an assignment at all.
    #[test]
    fn an_environment_assignment_leads_and_quotes_only_its_value() {
        assert_eq!(
            shell_command(
                &[("RUE_STD_PATH".to_owned(), "/a std/lib".to_owned())],
                &["/opt/rue/bin/rue".to_owned(), "test".to_owned()]
            ),
            "RUE_STD_PATH='/a std/lib' /opt/rue/bin/rue test"
        );
        // The empty spelling means "no toolchain std", and survives as such.
        assert_eq!(
            shell_command(
                &[("RUE_STD_PATH".to_owned(), String::new())],
                &["rue".to_owned()]
            ),
            "RUE_STD_PATH='' rue"
        );
    }

    /// The retention window is a megabyte; the display bound is a screenful.
    /// A flooding test's failure has to stay readable in a terminal, which
    /// means its own `scratch:` and `repro:` lines must survive the capture
    /// above them.
    #[test]
    fn a_long_capture_is_shown_as_a_head_a_tail_and_what_was_skipped() {
        let data: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let total = data.len() as u64;
        let mut out = String::new();
        push_capture(
            &mut out,
            "stdout",
            &Capture::new(data.into_bytes(), total, false),
        );

        assert!(out.contains("\n  line 0"), "{out}");
        assert!(out.contains("\n  line 47"), "{out}");
        assert!(!out.contains("\n  line 48"), "{out}");
        assert!(out.contains("\n  line 19984"), "{out}");
        assert!(out.contains("\n  line 19999"), "{out}");
        assert!(
            out.contains(
                "19936 lines (208340 bytes) omitted here; --format json carries the whole capture"
            ),
            "{out}"
        );
        // Head, tail, the header, the one omission line, and the empty line
        // the leading newline makes: a screenful.
        assert_eq!(out.lines().count(), 48 + 16 + 3);
        // Nothing was retained past the window, so no second truncation line.
        assert!(!out.contains("not retained"), "{out}");
    }

    /// The bound only elides what would not fit. A capture under both limits is
    /// printed exactly as the test wrote it.
    #[test]
    fn a_capture_within_both_bounds_is_printed_whole() {
        let data: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let total = data.len() as u64;
        let mut out = String::new();
        push_capture(
            &mut out,
            "stdout",
            &Capture::new(data.into_bytes(), total, false),
        );

        assert!(out.contains("\n  line 0"), "{out}");
        assert!(out.contains("\n  line 59"), "{out}");
        assert!(!out.contains("omitted here"), "{out}");
        assert_eq!(out.lines().count(), 60 + 2);
    }

    /// The byte ceiling binds before the line count when lines are long, and it
    /// still stops between lines rather than through one.
    #[test]
    fn the_byte_ceiling_binds_first_and_never_cuts_a_line() {
        let data: String = (0..40).map(|i| format!("{i:0>500}\n")).collect();
        let total = data.len() as u64;
        let mut out = String::new();
        push_capture(
            &mut out,
            "stdout",
            &Capture::new(data.into_bytes(), total, false),
        );

        assert!(out.contains("omitted here"), "{out}");
        for line in out.lines().skip(2).filter(|line| !line.contains("omitted")) {
            assert_eq!(line.len(), 502, "a whole line, indented: {line}");
        }
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
