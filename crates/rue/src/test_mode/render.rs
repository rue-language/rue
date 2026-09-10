//! The human renderer, as a consumer of the event stream (ADR-0083 §2).
//!
//! It reads the same `Event` values the NDJSON writer serializes, in the same
//! process — never re-parsed text, and never a second computation of what to
//! say. It is deliberately stateless: every number in the summary comes out of
//! the `run_finished` event's own fields rather than a private tally, so a
//! person and a tool cannot be shown two different counts of the same run.
//!
//! Verbosity is asymmetric on purpose. A failure prints whole — structure,
//! location, and captured output — and a pass prints nothing at all. No wall
//! of green.
//!
//! What a failure does *not* print is anything the reader already has
//! (RUE-2166). The reproduction argv is identical across a run but for the
//! selector, so it appears once, as a template, under the summary; a captured
//! stderr holding only the pinned runtime line the header already quoted is
//! dropped; a scratch directory the test left empty is named by its root
//! rather than per test; and one compile error shared by many tests is one
//! block naming them all. The stream is unaffected: every one of those is a
//! decision about what to show, made from event values the NDJSON writer
//! serializes unchanged.

use std::fmt::Write as _;
use std::path::Path;

use super::diff::{DiffOp, Hunk};
use super::events::{CandidateSource, Capture, Comparison, Event, Location, TestFinished};
use super::verdict::{TestExpectation, Verdict};

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

/// The human report of one run, assembled from the events it consumes.
///
/// A failure is held rather than printed the moment it lands, because the
/// report it belongs to is ordered, grouped, and given one shared
/// reproduction, and none of those can be decided while tests are still
/// finishing. Buffering is presentation and nothing more: the NDJSON stream
/// publishes each `test_finished` when it happens, from the same values.
///
/// One `Report` covers one run. A `--watch` cycle builds its own, so a cycle's
/// report never carries the previous cycle's failures.
pub(crate) struct Report {
    /// The non-passing tests so far, in the order the run finished them.
    failures: Vec<TestFinished>,
}

impl Report {
    pub(crate) fn new() -> Self {
        Self {
            failures: Vec::new(),
        }
    }

    /// Render one event for a person, or `None` when a person is owed nothing
    /// by it yet.
    ///
    /// Every line it produces is run data. Presentation policy that is not —
    /// the missing-inventory notice — is [`notice`]'s, and goes to stderr.
    pub(crate) fn observe(&mut self, event: &Event) -> Option<String> {
        match event {
            Event::RunStarted { .. } | Event::TestStarted { .. } => None,
            Event::Test { id, .. } => Some(id.clone()),
            Event::TestFinished(finished) => {
                if is_reported(finished) {
                    self.failures.push((**finished).clone());
                }
                None
            }
            Event::RunFinished {
                passed,
                failed,
                timeout,
                crash,
                compile_error,
                xfail,
                xpass,
                wall_ms,
                // The unimported-test-file warnings are the runner's own, and
                // stderr carries them once in every format (test-events.md,
                // "Streams"). Rendering them here too would print a second copy
                // on stdout whenever a terminal joins the streams.
                unimported_test_files: _,
                // The missing-inventory notice is the runner's own too, and goes
                // to stderr with them. See `notice`.
                test_candidates: _,
            } => Some(self.finish(summary(Counts {
                passed: *passed,
                failed: *failed,
                timeout: *timeout,
                crash: *crash,
                compile_error: *compile_error,
                xfail: *xfail,
                xpass: *xpass,
                wall_ms: *wall_ms,
            }))),
            Event::RunCanceled {
                reported,
                selected,
                wall_ms,
                // The cycle number is for a consumer grouping a tailed stream; a
                // person reading a terminal is already inside the cycle.
                cycle: _,
            } => Some(self.finish(canceled(*reported, *selected, *wall_ms))),
        }
    }

    /// The whole report: the failures in source order, the line the run ended
    /// with, and the trailer they share.
    ///
    /// A canceled cycle gets the same shape as a finished one. The verdicts it
    /// did publish are the only account of that cycle anyone will get, so they
    /// are printed with their reproduction exactly as a finished run's are.
    fn finish(&mut self, closing: String) -> String {
        let failures = std::mem::take(&mut self.failures);
        let mut out = String::new();
        for block in blocks(&failures) {
            out.push_str(&block.text);
            out.push('\n');
        }
        out.push_str(&closing);
        push_trailer(&mut out, &failures);
        out
    }
}

/// Whether a person is owed a report for this finished test.
///
/// An expected failure and an unexpected pass are both reported: one is
/// evidence the marker is still earned, the other is the prompt to remove it.
fn is_reported(finished: &TestFinished) -> bool {
    match finished.expectation {
        Some(TestExpectation::Xfail | TestExpectation::Xpass) => true,
        None => !finished.verdict.is_pass(),
    }
}

/// `canceled after 3 of 9 tests (0.4s); a newer source revision is available`.
///
/// The two counts are the honest report a `run_finished` could not make: the
/// verdicts this cycle published, and the plan it was working through when the
/// edit landed (RUE-2023).
fn canceled(reported: usize, selected: usize, wall_ms: u64) -> String {
    format!(
        "canceled after {reported} of {selected} test{} ({}); a newer source revision is available",
        if selected == 1 { "" } else { "s" },
        seconds(wall_ms)
    )
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

/// The `run_finished` counts a summary line is built from.
struct Counts {
    passed: usize,
    failed: usize,
    timeout: usize,
    crash: usize,
    compile_error: usize,
    xfail: usize,
    xpass: usize,
    wall_ms: u64,
}

/// `41 passed, 1 failed (0.9s)`, naming the classes that occurred.
///
/// A zero count for timeouts, crashes, or compile errors is left out rather
/// than printed as `0 timed out`: those are not ordinary outcomes, and a line
/// that always mentions them trains a reader to stop seeing them.
fn summary(counts: Counts) -> String {
    let mut parts = vec![format!("{} passed", counts.passed)];
    if counts.failed > 0 {
        parts.push(format!("{} failed", counts.failed));
    }
    if counts.timeout > 0 {
        parts.push(format!("{} timed out", counts.timeout));
    }
    if counts.crash > 0 {
        parts.push(format!("{} crashed", counts.crash));
    }
    if counts.compile_error > 0 {
        parts.push(format!(
            "{} compile error{}",
            counts.compile_error,
            if counts.compile_error == 1 { "" } else { "s" }
        ));
    }
    if counts.xfail > 0 {
        parts.push(format!(
            "{} expected failure{}",
            counts.xfail,
            if counts.xfail == 1 { "" } else { "s" }
        ));
    }
    if counts.xpass > 0 {
        parts.push(format!(
            "{} unexpected pass{}",
            counts.xpass,
            if counts.xpass == 1 { "" } else { "es" }
        ));
    }
    format!("{} ({})", parts.join(", "), seconds(counts.wall_ms))
}

/// A duration a person reads, to a tenth of a second.
fn seconds(millis: u64) -> String {
    format!("{}.{}s", millis / 1000, (millis % 1000) / 100)
}

/// One block of the report, and where a reader would go to look at it.
struct Block {
    /// `(file, line, column, id)` — the failure's site, so the report reads in
    /// the order a person navigates the source.
    at: (String, u32, u32, String),
    text: String,
}

/// The report's blocks, grouped and ordered.
///
/// Ordering is by source position rather than by the order the run finished
/// them: the seed governs *execution*, and a shuffled report is a list a reader
/// has to sort by hand before they can work through a file. The JSON stream
/// still reports in completion order, which is what a consumer tailing a live
/// run needs.
fn blocks(failures: &[TestFinished]) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    // One block per distinct diagnostic, not per test it excluded: a broken
    // helper reached by N tests is one thing that is wrong, and N copies of its
    // message say so N times without adding anything (RUE-2166).
    let mut groups: Vec<(DiagnosticKey, Vec<&TestFinished>)> = Vec::new();
    for failure in failures {
        if !matches!(failure.verdict, Verdict::CompileError) {
            blocks.push(ordinary_block(failure));
            continue;
        }
        let key = diagnostic_key(failure);
        match groups.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(failure),
            None => groups.push((key, vec![failure])),
        }
    }
    for (_, mut members) in groups {
        members.sort_by(|left, right| left.id.cmp(&right.id));
        blocks.push(compile_error_block(&members));
    }
    blocks.sort_by(|left, right| left.at.cmp(&right.at));
    blocks
}

/// What makes two `compile_error` verdicts the same diagnostic: the code the
/// payload leads with, the message, and the site — plus the banner, because an
/// expected failure and an ordinary one are different reports of one message.
type DiagnosticKey = (&'static str, String, String, String, String);

fn diagnostic_key(finished: &TestFinished) -> DiagnosticKey {
    let failure = finished.failure.as_ref();
    (
        banner(finished),
        failure
            .map(|failure| failure.kind.clone())
            .unwrap_or_default(),
        failure
            .map(|failure| failure.message.clone())
            .unwrap_or_default(),
        failure
            .and_then(|failure| failure.payload.clone())
            .unwrap_or_default(),
        failure
            .and_then(|failure| failure.location.as_ref())
            .map(site)
            .unwrap_or_default(),
    )
}

/// The banner a verdict reports under.
fn banner(finished: &TestFinished) -> &'static str {
    match finished.expectation {
        Some(TestExpectation::Xfail) => "XFAIL",
        Some(TestExpectation::Xpass) => "XPASS",
        None => match finished.verdict {
            Verdict::Pass => "PASS",
            Verdict::Fail(_) => "FAIL",
            Verdict::Timeout => "TIMEOUT",
            Verdict::Crash(_) => "CRASH",
            Verdict::CompileError => "COMPILE ERROR",
        },
    }
}

/// `file:line:column`.
fn site(location: &Location) -> String {
    format!("{}:{}:{}", location.file, location.line, location.column)
}

/// Where a failure sends its reader.
///
/// Every failure record carries a location — a frame's own site when it
/// reported one, the test declaration's header otherwise — so a header-located
/// failure orders by the declaration, which is exactly where a reader would go.
/// An unexpected pass has no failure record at all, and orders by the module
/// its ID names, ahead of that module's located failures.
fn position(finished: &TestFinished) -> (String, u32, u32, String) {
    match finished
        .failure
        .as_ref()
        .and_then(|failure| failure.location.as_ref())
    {
        Some(location) => (
            location.file.clone(),
            location.line,
            location.column,
            finished.id.clone(),
        ),
        None => (
            module_of(&finished.id).to_owned(),
            0,
            0,
            finished.id.clone(),
        ),
    }
}

/// The module half of a stable ID.
fn module_of(id: &str) -> &str {
    id.split_once("::").map_or(id, |(module, _)| module)
}

/// One test that ran and did not pass, whole.
fn ordinary_block(finished: &TestFinished) -> Block {
    let TestFinished {
        id,
        expectation,
        failure,
        stdout,
        stderr,
        scratch_dir,
        ..
    } = finished;
    let mut out = format!("{} {id}", banner(finished));
    if matches!(expectation, Some(TestExpectation::Xpass)) {
        out.push_str("\n  test passed unexpectedly; remove the known-bug marker");
    }
    if let Some(failure) = failure {
        out.push_str("\n  ");
        out.push_str(&failure.kind);
        if !failure.message.is_empty() {
            let _ = write!(out, ": {}", failure.message);
        }
        if let Some(location) = &failure.location {
            let _ = write!(out, "  ({})", site(location));
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
    // stdout is the test's own voice and is never suppressed. stderr is shared
    // with the runtime's abort path, and the block is dropped when that is all
    // it holds.
    push_capture(&mut out, "stdout", stdout);
    let message = failure.as_ref().map_or("", |failure| &failure.message);
    if !stderr_only_repeats(stderr, message) {
        push_capture(&mut out, "stderr", stderr);
    }
    if let Some(scratch) = scratch_dir
        && scratch_holds_evidence(scratch)
    {
        let _ = write!(out, "\n  scratch: {scratch}");
    }
    Block {
        at: position(finished),
        text: out,
    }
}

/// One diagnostic that excluded tests from the image, and every test it
/// excluded.
///
/// A test that never ran has no captured output, no scratch directory, and
/// nothing the runner observed: its whole report is the diagnostic and a
/// pointer to the stream that carries them all (ADR-0083 §3).
fn compile_error_block(members: &[&TestFinished]) -> Block {
    let first = members[0];
    let mut out = match members {
        [only] => format!("{} {}", banner(only), only.id),
        _ => format!("{} {} tests", banner(first), members.len()),
    };
    if let Some(failure) = &first.failure {
        out.push_str("\n  ");
        out.push_str(&failure.kind);
        if !failure.message.is_empty() {
            let _ = write!(out, ": {}", failure.message);
        }
        if let Some(location) = &failure.location {
            let _ = write!(out, "  ({})", site(location));
        }
        let count = failure
            .diagnostics
            .as_ref()
            .map_or(0, |diagnostics| diagnostics.len());
        let _ = write!(
            out,
            "\n  {} on stderr; {} excluded from the image",
            if count == 1 {
                "the diagnostic is".to_owned()
            } else {
                format!("{count} diagnostics are")
            },
            if members.len() == 1 {
                "the test was"
            } else {
                "these tests were"
            }
        );
    }
    if members.len() > 1 {
        for member in members {
            let _ = write!(out, "\n    {}", member.id);
        }
    }
    Block {
        at: position(first),
        text: out,
    }
}

/// Whether a captured stderr says only what this failure's header already said.
///
/// The abort-only runtime writes one pinned line and exits: `panic: <message>`
/// for a failed `@assert`, an unhandled `?`, and `@panic`; the trap's own
/// message for a trap; `segmentation fault at 0x…` for a segfault. The failure
/// record's `message` is that line — with the `panic: ` prefix when the line
/// *is* the message, without it when an assertion frame reported the message
/// alone — so a stderr holding nothing else is the header printed twice.
///
/// Anything the test itself wrote keeps the whole block, and so does a stream
/// that overflowed its budget: what was cut is exactly what a reader cannot
/// reconstruct from the header. The rule is for a repeat, not for brevity, and
/// it never applies to stdout, which is the test's alone.
fn stderr_only_repeats(capture: &Capture, message: &str) -> bool {
    if message.is_empty() || capture.bytes_total != capture.retained.len() as u64 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&capture.retained) else {
        return false;
    };
    let text = text.trim();
    text == message || text.strip_prefix(PANIC_PREFIX) == Some(message)
}

/// The prefix `__rue_panic` writes ahead of a message, which an assertion's own
/// failure frame reports without.
const PANIC_PREFIX: &str = "panic: ";

/// Whether a retained scratch directory holds anything worth a line.
///
/// The runner creates the directory, makes it the test's working directory, and
/// writes nothing into it itself — so an empty one is evidence of nothing, and
/// its path is a line per failure that says only what the run root and the
/// naming scheme already say (test-events.md, "Scratch directories and
/// isolation"). A directory that cannot be read is treated as empty: naming a
/// path a reader cannot open is the same non-information.
fn scratch_holds_evidence(directory: &str) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_some())
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

/// What the template puts where a test's stable ID goes.
const ID_PLACEHOLDER: &str = "<id>";

/// The report's trailer: what every failure in it shares.
///
/// The reproduction argv is the same line for every test of a run but for the
/// selector, so it is printed once here rather than once per failure — four
/// hundred characters of absolute paths that a reader has already read. Each
/// failure's ID is its own header, which is exactly what the placeholder wants,
/// and `--exact` stays on the line so a pasted ID still selects one test.
///
/// A run with a single failure prints that failure's complete line instead: the
/// common case is one broken test, and a line that is already paste-ready
/// should not ask for an edit first.
fn push_trailer(out: &mut String, failures: &[TestFinished]) {
    let Some(first) = failures.first() else {
        return;
    };
    if let Some(root) = unnamed_scratch_root(failures) {
        let _ = write!(out, "\nscratch root: {root}");
    }
    match failures {
        [only] => {
            let _ = write!(
                out,
                "\nrepro: {}",
                shell_command(&only.repro_env, &only.repro)
            );
        }
        _ => {
            let _ = write!(
                out,
                "\nrepro: {}",
                shell_command(&first.repro_env, &id_template(&first.repro))
            );
            let _ = write!(
                out,
                "\n  put a failing test's ID from above in place of {}",
                shell_word(ID_PLACEHOLDER)
            );
        }
    }
}

/// One repro argv with its selector replaced by the placeholder.
///
/// The substitution is on the argv, before quoting, so `shell_command` remains
/// the one authority on what a shell would argue about — including the angle
/// brackets, which is why the placeholder comes back quoted.
fn id_template(argv: &[String]) -> Vec<String> {
    let mut argv = argv.to_vec();
    if let Some(index) = argv.iter().position(|word| word == "--filter")
        && let Some(selector) = argv.get_mut(index + 1)
    {
        *selector = ID_PLACEHOLDER.to_owned();
    }
    argv
}

/// The run directory holding the scratch directories no failure named, or
/// `None` when every retained directory got its own line.
///
/// Per-test directories are named `rue-test-<seed>-<ordinal>` under it, so the
/// root plus the naming scheme is the whole address of an empty one.
fn unnamed_scratch_root(failures: &[TestFinished]) -> Option<String> {
    failures
        .iter()
        .filter_map(|failure| failure.scratch_dir.as_deref())
        .filter(|directory| !scratch_holds_evidence(directory))
        .find_map(|directory| Path::new(directory).parent())
        .map(|root| root.display().to_string())
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
    use crate::test_mode::events::{Comparison, FailureRecord, UnimportedFile};
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

    /// What one event alone prints, through a report of its own.
    fn rendered(event: &Event) -> Option<String> {
        let mut report = Report::new();
        report.observe(event)
    }

    /// The whole report a run of these events produces, joined the way the
    /// reporter writes it.
    fn report_of(events: &[Event]) -> String {
        let mut report = Report::new();
        events
            .iter()
            .filter_map(|event| report.observe(event))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The block one finished test contributes, without the run's trailer.
    fn block_of(event: &Event) -> String {
        let Event::TestFinished(finished) = event else {
            unreachable!("only a finished test has a block");
        };
        super::blocks(std::slice::from_ref(finished.as_ref()))
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn finished(verdict: Verdict, failure: Option<FailureRecord>) -> Event {
        Event::TestFinished(Box::new(TestFinished {
            id: "app/t.rue::parses a port".to_owned(),
            verdict,
            expectation: None,
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
                "--exact".to_owned(),
            ],
            repro_env: vec![("RUE_STD_PATH".to_owned(), "/opt/rue/std".to_owned())],
        }))
    }

    /// A second failing test, at a site of its own, so a report has an order to
    /// get right.
    fn finished_at(id: &str, file: &str, line: u32, column: u32) -> Event {
        let Event::TestFinished(mut finished) = finished(
            Verdict::Fail(FailureKind::Assert),
            Some(FailureRecord {
                kind: "assert".to_owned(),
                message: "assertion failed".to_owned(),
                location: Some(Location {
                    file: file.to_owned(),
                    line,
                    column,
                }),
                ..FailureRecord::default()
            }),
        ) else {
            unreachable!();
        };
        finished.id = id.to_owned();
        finished.repro[4] = id.to_owned();
        Event::TestFinished(finished)
    }

    /// The `compile_error` shape: no captures, no scratch directory, and the
    /// diagnostics the compiler decided it with.
    fn compile_error_finished(id: &str, diagnostics: usize) -> Event {
        Event::TestFinished(Box::new(TestFinished {
            id: id.to_owned(),
            verdict: Verdict::CompileError,
            expectation: None,
            duration_ms: 0,
            failure: Some(FailureRecord {
                kind: "compile_error".to_owned(),
                message: "type mismatch: expected i32, found bool".to_owned(),
                location: Some(Location {
                    file: "app/t.rue".to_owned(),
                    line: 4,
                    column: 5,
                }),
                payload: Some("E0206: type mismatch: expected i32, found bool".to_owned()),
                diagnostics: Some(vec![serde_json::Value::Null; diagnostics]),
                ..FailureRecord::default()
            }),
            stdout: Capture::new(Vec::new(), 0, false),
            stderr: Capture::new(Vec::new(), 0, false),
            scratch_dir: None,
            repro: vec![
                "/opt/rue/bin/rue".to_owned(),
                "test".to_owned(),
                "/work/app/main.rue".to_owned(),
            ],
            repro_env: Vec::new(),
        }))
    }

    /// A test that never ran has nothing observed to print: its report is the
    /// banner, the diagnostic, and a pointer to stderr (ADR-0083 §3). It keeps
    /// the asymmetric-verbosity shape every other failure has.
    #[test]
    fn a_compile_error_names_its_diagnostic_and_points_at_stderr() {
        let rendered = block_of(&compile_error_finished("app/t.rue::parses a port", 1));
        assert_eq!(
            rendered,
            concat!(
                "COMPILE ERROR app/t.rue::parses a port\n",
                "  compile_error: type mismatch: expected i32, found bool  (app/t.rue:4:5)\n",
                "  the diagnostic is on stderr; the test was excluded from the image",
            )
        );
        assert!(
            !rendered.contains("stdout") && !rendered.contains("scratch"),
            "a test that never ran captured nothing: {rendered}"
        );
        assert!(
            block_of(&compile_error_finished("app/t.rue::parses a port", 3))
                .contains("3 diagnostics are on stderr"),
            "the count is the reader's cue that stderr has more"
        );
    }

    /// RUE-2166: a broken helper reached by three tests is one thing that is
    /// wrong. It gets one block, naming every test it excluded, rather than
    /// three copies of one message. The `run_finished` counts are unchanged —
    /// three tests did not compile, however many diagnostics say so.
    #[test]
    fn one_diagnostic_is_one_block_naming_every_test_it_excluded() {
        let rendered = report_of(&[
            compile_error_finished("app/t.rue::ccc", 1),
            compile_error_finished("app/t.rue::aaa", 1),
            compile_error_finished("app/t.rue::bbb", 1),
            Event::RunFinished {
                passed: 0,
                failed: 0,
                timeout: 0,
                crash: 0,
                compile_error: 3,
                xfail: 0,
                xpass: 0,
                wall_ms: 100,
                unimported_test_files: None,
                test_candidates: CandidateSource::Declared,
            },
        ]);
        assert!(
            rendered.starts_with(concat!(
                "COMPILE ERROR 3 tests\n",
                "  compile_error: type mismatch: expected i32, found bool  (app/t.rue:4:5)\n",
                "  the diagnostic is on stderr; these tests were excluded from the image\n",
                "    app/t.rue::aaa\n",
                "    app/t.rue::bbb\n",
                "    app/t.rue::ccc\n",
            )),
            "{rendered}"
        );
        assert_eq!(rendered.matches("type mismatch").count(), 1, "{rendered}");
        assert!(
            rendered.contains("0 passed, 3 compile errors ("),
            "{rendered}"
        );
    }

    /// Two different diagnostics are two different things to fix, so grouping
    /// stops at the message: same code, same message, same site, one block.
    #[test]
    fn two_distinct_diagnostics_stay_two_blocks() {
        let Event::TestFinished(mut other) = compile_error_finished("app/t.rue::bbb", 1) else {
            unreachable!();
        };
        if let Some(failure) = other.failure.as_mut() {
            failure.message = "unknown type 'Nope'".to_owned();
            failure.payload = Some("E0101: unknown type 'Nope'".to_owned());
        }
        let blocks = super::blocks(&[
            {
                let Event::TestFinished(first) = compile_error_finished("app/t.rue::aaa", 1) else {
                    unreachable!();
                };
                *first
            },
            *other,
        ]);
        assert_eq!(blocks.len(), 2, "one block per distinct diagnostic");
    }

    /// `compile_error` is its own summary class: "did not compile" and "ran and
    /// failed" send a reader to different places, and stderr is where the first
    /// one is explained.
    #[test]
    fn the_summary_counts_compile_errors_separately() {
        let summarized = |compile_error| {
            rendered(&Event::RunFinished {
                passed: 2,
                failed: 1,
                timeout: 0,
                crash: 0,
                compile_error,
                xfail: 0,
                xpass: 0,
                wall_ms: 900,
                unimported_test_files: None,
                test_candidates: CandidateSource::Declared,
            })
            .expect("a summary renders")
        };
        assert_eq!(summarized(0), "2 passed, 1 failed (0.9s)");
        assert_eq!(summarized(1), "2 passed, 1 failed, 1 compile error (0.9s)");
        assert_eq!(summarized(4), "2 passed, 1 failed, 4 compile errors (0.9s)");
    }

    #[test]
    fn the_summary_names_expected_failures_and_unexpected_passes() {
        let rendered = rendered(&Event::RunFinished {
            passed: 2,
            failed: 0,
            timeout: 0,
            crash: 0,
            compile_error: 0,
            xfail: 1,
            xpass: 1,
            wall_ms: 900,
            unimported_test_files: None,
            test_candidates: CandidateSource::Declared,
        })
        .expect("a summary renders");
        assert_eq!(
            rendered,
            "2 passed, 1 expected failure, 1 unexpected pass (0.9s)"
        );
    }

    fn run_finished(passed: usize, failed: usize, timeout: usize, crash: usize) -> Event {
        Event::RunFinished {
            passed,
            failed,
            timeout,
            crash,
            compile_error: 0,
            xfail: 0,
            xpass: 0,
            wall_ms: 900,
            unimported_test_files: Some(Vec::new()),
            test_candidates: CandidateSource::Declared,
        }
    }

    /// No wall of green: a pass prints nothing, and adds nothing to the report
    /// its run ends with.
    #[test]
    fn a_pass_prints_nothing() {
        assert!(rendered(&finished(Verdict::Pass, None)).is_none());
        assert_eq!(
            report_of(&[finished(Verdict::Pass, None), run_finished(1, 0, 0, 0)]),
            "1 passed (0.9s)"
        );
    }

    #[test]
    fn expected_failure_classifications_are_visible_to_humans() {
        let Event::TestFinished(mut xfail) = finished(
            Verdict::Fail(FailureKind::Assert),
            Some(FailureRecord {
                kind: "assert".to_owned(),
                message: "assertion failed".to_owned(),
                ..FailureRecord::default()
            }),
        ) else {
            unreachable!();
        };
        xfail.expectation = Some(TestExpectation::Xfail);
        assert!(block_of(&Event::TestFinished(xfail)).starts_with("XFAIL "));

        let Event::TestFinished(mut xpass) = finished(Verdict::Pass, None) else {
            unreachable!();
        };
        xpass.expectation = Some(TestExpectation::Xpass);
        let rendered = report_of(&[
            Event::TestFinished(xpass),
            Event::RunFinished {
                passed: 0,
                failed: 0,
                timeout: 0,
                crash: 0,
                compile_error: 0,
                xfail: 0,
                xpass: 1,
                wall_ms: 900,
                unimported_test_files: None,
                test_candidates: CandidateSource::Declared,
            },
        ]);
        assert!(rendered.starts_with("XPASS app/t.rue::parses a port\n"));
        assert!(rendered.contains("remove the known-bug marker"));
        assert!(rendered.contains("repro:"));
        assert!(rendered.contains("checking"));
    }

    /// The head events are machine bookkeeping; a person is shown failures and
    /// a summary, not a running commentary.
    #[test]
    fn run_and_test_start_print_nothing() {
        assert!(
            rendered(&Event::TestStarted {
                id: "app/t.rue::ok".to_owned()
            })
            .is_none()
        );
        assert!(
            rendered(&Event::RunStarted {
                root: "m.rue".to_owned(),
                target: "x86-64-linux".to_owned(),
                opt_level: "0".to_owned(),
                seed: 1,
                jobs: 1,
                shard: None,
                selected: 1,
                total: 1,
                cycle: None,
            })
            .is_none()
        );
    }

    /// A canceled cycle is the one thing a watch consumer must not read as a
    /// silent end, so it prints in place of the summary it replaces
    /// (RUE-2023) — and with it, the failures the cycle did publish.
    #[test]
    fn a_canceled_cycle_reports_what_it_managed() {
        let rendered = rendered(&Event::RunCanceled {
            cycle: 2,
            reported: 3,
            selected: 9,
            wall_ms: 400,
        })
        .expect("a canceled cycle is never silent");
        assert_eq!(
            rendered,
            "canceled after 3 of 9 tests (0.4s); a newer source revision is available"
        );
        let single = rendered_canceled();
        assert!(single.contains("0 of 1 test ("), "{single}");
    }

    fn rendered_canceled() -> String {
        rendered(&Event::RunCanceled {
            cycle: 1,
            reported: 0,
            selected: 1,
            wall_ms: 0,
        })
        .expect("a canceled cycle is never silent")
    }

    /// A cycle abandoned mid-run still reports the failures it published, with
    /// their reproduction: those verdicts are the only account of that cycle
    /// anyone will get.
    #[test]
    fn a_canceled_cycle_still_carries_its_failures() {
        let rendered = report_of(&[
            finished(
                Verdict::Fail(FailureKind::Assert),
                Some(FailureRecord {
                    kind: "assert".to_owned(),
                    message: "assertion failed".to_owned(),
                    ..FailureRecord::default()
                }),
            ),
            Event::RunCanceled {
                cycle: 2,
                reported: 1,
                selected: 9,
                wall_ms: 400,
            },
        ]);
        assert!(
            rendered.starts_with("FAIL app/t.rue::parses a port\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\ncanceled after 1 of 9 tests"),
            "{rendered}"
        );
        assert!(rendered.contains("\nrepro: RUE_STD_PATH="), "{rendered}");
    }

    #[test]
    fn a_failure_prints_structure_and_output() {
        let rendered = block_of(&finished(
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
        ));
        assert!(
            rendered.starts_with("FAIL app/t.rue::parses a port"),
            "{rendered}"
        );
        assert!(
            rendered.contains("assert: assertion failed  (app/t.rue:7:5)"),
            "{rendered}"
        );
        assert!(rendered.contains("--- stdout (9 bytes) ---"), "{rendered}");
        // This fixture's stderr holds the pinned line and nothing else, so the
        // block is the header a second time and is dropped; the two tests below
        // pin that rule on its own.
        assert!(!rendered.contains("--- stderr"), "{rendered}");
        // The repro is the report's, once, not this block's (RUE-2166).
        assert!(!rendered.contains("repro:"), "{rendered}");
    }

    /// A run with one failure is the common case, and its line is already
    /// paste-ready: it gets the complete argv rather than a template to edit.
    /// The environment leads and every path is absolute, so the line runs in a
    /// clean shell from any directory (RUE-2020).
    #[test]
    fn a_single_failure_keeps_its_whole_repro_line() {
        let rendered = report_of(&[
            finished(
                Verdict::Fail(FailureKind::Assert),
                Some(FailureRecord {
                    kind: "assert".to_owned(),
                    message: "assertion failed".to_owned(),
                    ..FailureRecord::default()
                }),
            ),
            run_finished(0, 1, 0, 0),
        ]);
        assert!(
            rendered.ends_with(
                "\nrepro: RUE_STD_PATH=/opt/rue/std /opt/rue/bin/rue test /work/app/main.rue \
                 --filter 'app/t.rue::parses a port' --exact"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("in place of"), "{rendered}");
        assert_eq!(rendered.matches("repro:").count(), 1, "{rendered}");
    }

    /// RUE-2166: every repro of a run is the same four hundred characters but
    /// for the selector, so the report carries one template and each failure's
    /// header is the ID that goes in it. `--exact` stays on the line so a
    /// pasted ID that is a prefix of another still selects one test.
    #[test]
    fn many_failures_share_one_repro_template() {
        let rendered = report_of(&[
            finished_at("app/t.rue::aaa", "app/t.rue", 3, 1),
            finished_at("app/t.rue::bbb", "app/t.rue", 9, 1),
            run_finished(0, 2, 0, 0),
        ]);
        assert!(
            rendered.contains(
                "\nrepro: RUE_STD_PATH=/opt/rue/std /opt/rue/bin/rue test /work/app/main.rue \
                 --filter '<id>' --exact\n  put a failing test's ID from above in place of '<id>'"
            ),
            "{rendered}"
        );
        assert_eq!(rendered.matches("repro:").count(), 1, "{rendered}");
        assert!(
            !rendered.contains("--filter 'app/t.rue::aaa'"),
            "{rendered}"
        );
    }

    /// RUE-2166: the report reads in the order a person navigates the source,
    /// not in the order the shuffle happened to finish the tests. The seed
    /// still governs execution, and the JSON stream still reports in completion
    /// order.
    #[test]
    fn failures_are_reported_in_source_order() {
        let rendered = report_of(&[
            finished_at("app/z.rue::early", "app/z.rue", 2, 1),
            finished_at("app/t.rue::later", "app/t.rue", 40, 1),
            finished_at("app/t.rue::earlier", "app/t.rue", 7, 9),
            finished_at("app/t.rue::same line", "app/t.rue", 7, 2),
            run_finished(0, 4, 0, 0),
        ]);
        let order: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("FAIL "))
            .collect();
        assert_eq!(
            order,
            vec![
                "FAIL app/t.rue::same line",
                "FAIL app/t.rue::earlier",
                "FAIL app/t.rue::later",
                "FAIL app/z.rue::early",
            ],
            "{rendered}"
        );
    }

    /// An unexpected pass has no failure record and so no site. It orders by
    /// the module its ID names, ahead of that module's located failures.
    #[test]
    fn a_failure_without_a_site_orders_by_its_module() {
        let Event::TestFinished(mut xpass) = finished(Verdict::Pass, None) else {
            unreachable!();
        };
        xpass.expectation = Some(TestExpectation::Xpass);
        xpass.id = "app/t.rue::fixed".to_owned();
        let rendered = report_of(&[
            finished_at("app/t.rue::broken", "app/t.rue", 7, 1),
            Event::TestFinished(xpass),
            run_finished(0, 1, 0, 0),
        ]);
        assert!(
            rendered.starts_with("XPASS app/t.rue::fixed\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\nFAIL app/t.rue::broken\n"),
            "{rendered}"
        );
    }

    /// RUE-2166: the runtime's abort path writes one pinned line, and the
    /// header already quotes it. A stderr holding only that line is the header
    /// printed twice, whether the line carries the `panic: ` prefix or the
    /// failure frame reported the message without it.
    #[test]
    fn a_stderr_that_only_repeats_the_header_is_left_out() {
        let repeated = |stderr: &[u8], message: &str| {
            let Event::TestFinished(mut finished) = finished(
                Verdict::Fail(FailureKind::Assert),
                Some(FailureRecord {
                    kind: "assert".to_owned(),
                    message: message.to_owned(),
                    ..FailureRecord::default()
                }),
            ) else {
                unreachable!();
            };
            let total = stderr.len() as u64;
            finished.stderr = Capture::new(stderr.to_vec(), total, false);
            block_of(&Event::TestFinished(finished))
        };
        // `panic: <message>`: what `__rue_panic` writes for an assertion or a
        // `?` whose frame reported the message alone.
        assert!(
            !repeated(b"panic: x should exceed five\n", "x should exceed five").contains("stderr"),
        );
        // The trap's own message, which the record carries verbatim.
        assert!(
            !repeated(b"error: division by zero\n", "error: division by zero").contains("stderr"),
        );
        assert!(
            !repeated(
                b"segmentation fault at 0xdead\n",
                "segmentation fault at 0xdead"
            )
            .contains("stderr"),
        );
        // stdout is the test's own voice and is never dropped for repeating
        // anything.
        assert!(
            repeated(b"panic: boom\n", "boom").contains("--- stdout (9 bytes) ---"),
            "stdout survives the stderr rule"
        );
    }

    /// Anything else on stderr is the test's, and the block is kept whole: a
    /// line the reader cannot reconstruct from the header is the whole reason
    /// the capture exists.
    #[test]
    fn a_stderr_with_more_than_the_header_is_kept_whole() {
        let kept = |stderr: &[u8], total: u64, message: &str| {
            let Event::TestFinished(mut finished) = finished(
                Verdict::Fail(FailureKind::Assert),
                Some(FailureRecord {
                    kind: "assert".to_owned(),
                    message: message.to_owned(),
                    ..FailureRecord::default()
                }),
            ) else {
                unreachable!();
            };
            finished.stderr = Capture::new(stderr.to_vec(), total, false);
            block_of(&Event::TestFinished(finished))
        };
        let rendered = kept(b"connecting\npanic: boom\n", 23, "boom");
        assert!(rendered.contains("--- stderr (23 bytes) ---"), "{rendered}");
        assert!(rendered.contains("\n  connecting"), "{rendered}");
        // A stream cut at its budget keeps its block too: what was dropped is
        // exactly what the header cannot stand in for.
        let truncated = kept(b"panic: boom", 4096, "boom");
        assert!(
            truncated.contains("--- stderr (4096 bytes) ---"),
            "{truncated}"
        );
    }

    /// A single-line comparison prints both values aligned, and a caret under
    /// the first character that differs. The caret's column comes out of the
    /// same `diff` the event stream publishes, so the two surfaces cannot
    /// disagree about where the difference is.
    #[test]
    fn a_single_line_comparison_prints_both_values_and_a_caret() {
        let rendered = block_of(&finished(
            Verdict::Fail(FailureKind::AssertEq),
            Some(FailureRecord {
                kind: "assert_eq".to_owned(),
                message: "assertion failed: left == right".to_owned(),
                exit_code: Some(101),
                comparison: Some(Comparison::new("41".to_owned(), "42".to_owned())),
                ..FailureRecord::default()
            }),
        ));
        assert!(
            rendered.contains("\n  left:  41\n  right: 42\n          ^\n"),
            "{rendered}"
        );
    }

    /// An `@assert_ne` failure has two identical values, so there is no first
    /// difference and no caret is drawn — the two values *are* the report.
    #[test]
    fn identical_values_print_no_caret() {
        let rendered = block_of(&finished(
            Verdict::Fail(FailureKind::AssertNe),
            Some(FailureRecord {
                kind: "assert_ne".to_owned(),
                message: "assertion failed: left != right".to_owned(),
                comparison: Some(Comparison::new("41".to_owned(), "41".to_owned())),
                ..FailureRecord::default()
            }),
        ));
        assert!(
            rendered.contains("\n  left:  41\n  right: 41\n  --- stdout"),
            "{rendered}"
        );
    }

    /// A multi-line pair gets the `-`/`+` listing instead: a caret into a wall
    /// of text locates nothing.
    #[test]
    fn a_multi_line_comparison_prints_a_hunk_listing() {
        let rendered = block_of(&finished(
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
        ));
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
        let rendered = block_of(&finished(
            Verdict::Fail(FailureKind::Exit),
            Some(FailureRecord {
                kind: "exit".to_owned(),
                message: "the test exited with status 101".to_owned(),
                runner_note: Some("the test failure channel could not be read".to_owned()),
                ..FailureRecord::default()
            }),
        ));
        assert!(
            rendered.contains("note: the test failure channel could not be read"),
            "{rendered}"
        );
    }

    #[test]
    fn timeouts_and_crashes_carry_their_own_banners() {
        let timeout = block_of(&finished(Verdict::Timeout, None));
        assert!(timeout.starts_with("TIMEOUT "), "{timeout}");
        let crash = block_of(&finished(Verdict::Crash(11), None));
        assert!(crash.starts_with("CRASH "), "{crash}");
    }

    /// RUE-2166: the runner writes nothing into a scratch directory itself, so
    /// an empty one is evidence of nothing and its per-test name is derivable
    /// from the run root. A directory the test left something in is named where
    /// the reader is already looking.
    #[test]
    fn a_scratch_directory_is_named_only_when_it_holds_something() {
        let root = std::env::temp_dir().join(format!("rue-render-2166-{}", std::process::id()));
        let scratch = root.join("rue-test-417-2");
        std::fs::create_dir_all(&scratch).expect("a scratch directory");
        let with_scratch = |directory: &std::path::Path| {
            let Event::TestFinished(mut finished) = finished(
                Verdict::Fail(FailureKind::Assert),
                Some(FailureRecord {
                    kind: "assert".to_owned(),
                    message: "assertion failed".to_owned(),
                    ..FailureRecord::default()
                }),
            ) else {
                unreachable!();
            };
            finished.scratch_dir = Some(directory.display().to_string());
            Event::TestFinished(finished)
        };

        let empty = report_of(&[with_scratch(&scratch), run_finished(0, 1, 0, 0)]);
        assert!(!empty.contains("\n  scratch: "), "{empty}");
        assert!(
            empty.contains(&format!("\nscratch root: {}", root.display())),
            "{empty}"
        );

        std::fs::write(scratch.join("out.txt"), b"kept").expect("evidence");
        let kept = report_of(&[with_scratch(&scratch), run_finished(0, 1, 0, 0)]);
        assert!(
            kept.contains(&format!("\n  scratch: {}", scratch.display())),
            "{kept}"
        );
        assert!(!kept.contains("scratch root:"), "{kept}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every number a person reads comes from the event, so the two surfaces
    /// cannot report different counts for the same run.
    #[test]
    fn the_summary_names_only_the_classes_that_occurred() {
        assert!(
            rendered(&run_finished(2, 0, 0, 0))
                .unwrap()
                .starts_with("2 passed (0.9s)")
        );
        assert!(
            rendered(&run_finished(2, 1, 0, 0))
                .unwrap()
                .starts_with("2 passed, 1 failed (0.9s)")
        );
        assert!(
            rendered(&run_finished(2, 1, 1, 1))
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
            compile_error: 0,
            xfail: 0,
            xpass: 0,
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
        let rendered = rendered(&no_inventory()).expect("a summary renders");
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
        let rendered = rendered(&no_inventory()).expect("a summary renders");
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
            compile_error: 0,
            xfail: 0,
            xpass: 0,
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
        let rendered = rendered(&event).expect("a summary renders");
        assert_eq!(rendered, "1 passed (0.0s)");
        assert!(!rendered.contains("warning:"), "{rendered}");
        assert!(!rendered.contains("app/orphan.rue"), "{rendered}");
        assert!(!rendered.contains("could not be parsed"), "{rendered}");
        assert_eq!(notice_multi(&event), None);
    }

    #[test]
    fn a_listing_entry_renders_as_its_bare_identity() {
        assert_eq!(
            rendered(&Event::Test {
                id: "app/t.rue::ok".to_owned(),
                module: "app/t.rue".to_owned(),
                name: "ok".to_owned(),
                file: "app/t.rue".to_owned(),
                line: 1,
                column: 1,
                known_bug: None,
                known_bug_on: Vec::new(),
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

    /// The template substitutes on the argv, before quoting, so `shell_command`
    /// stays the one authority on what a shell would argue about — which is why
    /// the placeholder comes back quoted.
    #[test]
    fn the_template_replaces_only_the_selector() {
        let argv = vec![
            "rue".to_owned(),
            "test".to_owned(),
            "/work/main.rue".to_owned(),
            "--filter".to_owned(),
            "app/t.rue::ok".to_owned(),
            "--exact".to_owned(),
        ];
        assert_eq!(
            shell_command(&[], &id_template(&argv)),
            "rue test /work/main.rue --filter '<id>' --exact"
        );
        // A listing repro carries no selector, and comes back untouched.
        let bare = vec!["rue".to_owned(), "test".to_owned()];
        assert_eq!(id_template(&bare), bare);
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
    /// means the summary and repro below it must survive the capture above.
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
