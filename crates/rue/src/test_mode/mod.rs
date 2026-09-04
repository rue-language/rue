//! `rue test`: the driver's test mode (ADR-0083 §2, §3).
//!
//! One image is linked for the request's whole test closure, and one process is
//! spawned per selected test. Everything the run produces is an event
//! (`events`), decided by one classifier (`verdict`), ordered by one planner
//! (`selection`), executed by one dispatcher (`exec`), and shown to a person by
//! one consumer of those same events (`render`).
//!
//! The schema is `docs/process/test-events.md`. Two stream rules are settled
//! here and stated there because they are easy to get subtly wrong:
//!
//! - **stdout is the runner's surface and stderr is the compiler's.** Compiler
//!   diagnostics keep going where `docs/process/diagnostics.md` puts them,
//!   `--error-format json` included and unchanged, so a consumer can read the
//!   whole of stdout as the event stream.
//! - **No event is emitted before the image exists.** A compile failure is
//!   exit 2 with diagnostics and an empty event stream, never a `run_started`
//!   for a run that never began.

pub(crate) mod diff;
pub(crate) mod events;
pub(crate) mod exec;
pub(crate) mod render;
pub(crate) mod selection;
pub(crate) mod verdict;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rue_compiler::unstable::{TestCandidateInventory, TestInventoryEntry};
use rue_compiler::{CompileOptions, OptLevel};
use rue_driver::FilesystemCompilerHost;
use rue_target::Target;

use events::{
    CandidateSource, Capture, Comparison, Event, FailureRecord, Location, TestFinished,
    UnimportedFile,
};
use exec::{DEFAULT_STREAM_BUDGET, Dispatch};
use selection::Shard;
use verdict::{FailureKind, Verdict};

/// The default per-test wall-clock budget, matching `rue-test-runner`'s
/// (ADR-0083 §3).
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = rue_test_runner::DEFAULT_TIMEOUT_MS;

/// `rue test`'s exit statuses (ADR-0083 §2).
///
/// Agents branch on these, so they are one enum with one documented mapping
/// rather than scattered `exit` calls. The compile-mode driver's own exit paths
/// are untouched: this is a new surface, not a change to the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestExitCode {
    /// Every selected test passed.
    AllPassed = 0,
    /// At least one selected test failed, timed out, or crashed.
    Failures = 1,
    /// The run could not be performed: a compile failure, a link failure, an
    /// ICE, a bad flag combination, or a runner error.
    RunnerError = 2,
    /// The selection was empty. A filter that matches nothing is how a typo
    /// becomes false evidence, so it is an outcome of its own rather than a
    /// vacuous success.
    EmptySelection = 3,
}

impl TestExitCode {
    pub(crate) fn code(self) -> i32 {
        self as i32
    }
}

/// How the run reports itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OutputFormat {
    #[default]
    Human,
    Json,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "human" => Ok(Self::Human),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "invalid --format '{other}' (valid formats: human, json)"
            )),
        }
    }
}

/// The test-mode flags, as parsed.
#[derive(Debug, Clone)]
pub(crate) struct TestOptions {
    pub(crate) list: bool,
    pub(crate) filters: Vec<String>,
    pub(crate) format: OutputFormat,
    pub(crate) timeout_ms: u64,
    pub(crate) shard: Option<Shard>,
    /// `None` until the run derives one from a fresh random source and reports
    /// it in `run_started`.
    pub(crate) seed: Option<u64>,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            list: false,
            filters: Vec::new(),
            format: OutputFormat::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            shard: None,
            seed: None,
        }
    }
}

/// Everything one `rue test` invocation needs from the driver.
pub(crate) struct TestRequest<'a, 'diagnostics> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) compile_options: CompileOptions,
    pub(crate) options: TestOptions,
    pub(crate) diagnostics: &'a crate::DiagnosticOutput<'diagnostics>,
    /// The root source exactly as the command line spelled it, which is what a
    /// repro argv must repeat.
    pub(crate) root: String,
    /// The compile-mode flags a repro argv repeats after the filter and seed.
    pub(crate) repro_flags: Vec<String>,
    pub(crate) jobs: usize,
    pub(crate) target: Target,
    pub(crate) opt_level: OptLevel,
    pub(crate) candidates: Option<TestCandidateInventory>,
}

/// Derive a seed from a fresh random source.
///
/// `RandomState` is seeded by the OS, which is the only entropy the standard
/// library exposes without a dependency. The value is published in
/// `run_started` and repeated in every repro argv, so a shuffle that surfaced a
/// bug is re-runnable.
fn fresh_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// Serializes the run's output so concurrent workers cannot interleave a line,
/// and routes each event to the surface the invocation asked for.
struct Reporter {
    format: OutputFormat,
    /// Presentation policy the runner's notices need and the event schema does
    /// not carry. See `render::Context`.
    context: render::Context,
    stdout: Mutex<()>,
}

impl Reporter {
    fn new(format: OutputFormat, context: render::Context) -> Self {
        Self {
            format,
            context,
            stdout: Mutex::new(()),
        }
    }

    fn emit(&self, event: &Event) {
        let _guard = self
            .stdout
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut out = std::io::stdout().lock();
        match self.format {
            OutputFormat::Json => {
                let _ = writeln!(out, "{}", event.to_ndjson());
            }
            OutputFormat::Human => {
                if let Some(text) = render::render(event) {
                    let _ = writeln!(out, "{text}");
                }
            }
        }
        let _ = out.flush();
        // A notice is the runner's own voice, not run data, so it follows the
        // warnings onto stderr rather than joining the events on stdout. Said
        // under the same lock, after the line it annotates, so a terminal
        // joining the streams reads them in order.
        if self.format == OutputFormat::Human
            && let Some(notice) = render::notice(event, self.context)
        {
            eprintln!("{notice}");
        }
    }
}

/// Run `rue test` to an exit status.
pub(crate) fn run(request: TestRequest<'_, '_>) -> TestExitCode {
    let TestRequest {
        host,
        compile_options,
        options,
        diagnostics,
        root,
        repro_flags,
        jobs,
        target,
        opt_level,
        candidates,
    } = request;

    // Before anything can spawn: pin descriptor 3 shut for the life of the
    // process, so no pipe the standard library opens for its own bookkeeping
    // can be allocated there and then be destroyed by a child's `dup2` onto
    // the channel. See `exec::reserve_channel_descriptor`.
    exec::reserve_channel_descriptor();
    // And take responsibility for the children: each test leads its own process
    // group, so a terminal's Ctrl-C reaches this process alone and would
    // otherwise leave every live test running unsupervised.
    exec::install_signal_forwarding();

    let seed = options.seed.unwrap_or_else(fresh_seed);

    if options.list {
        // A listing emits no `run_finished`, so it is owed no closure context.
        let reporter = Reporter::new(options.format, render::Context::default());
        return list(host, &compile_options, &options, diagnostics, &reporter);
    }

    // Nothing is published before the image exists: a compile failure is
    // diagnostics on stderr and exit 2, with an empty event stream.
    let (image, inventory) = match host.test_image_in_compile_scope(&compile_options) {
        Ok(output) => output,
        Err(errors) => {
            diagnostics.print_errors(&crate::with_import_migration_helps(&errors));
            return TestExitCode::RunnerError;
        }
    };
    // Built here rather than above because the closure is only published once
    // the image is: nothing before this point could answer how many modules the
    // program has.
    let reporter = Reporter::new(
        options.format,
        render::Context {
            multi_module_closure: host.published_user_module_count() > 1,
        },
    );
    diagnostics.print_warnings(&image.warnings);

    let total = inventory.entries.len();
    let plan = selection::plan(&inventory.entries, &options.filters, options.shard, seed);
    let started = Instant::now();

    reporter.emit(&Event::RunStarted {
        root: root.clone(),
        target: target.to_string(),
        opt_level: opt_level_digit(opt_level),
        seed,
        jobs,
        shard: options.shard.map(|shard| shard.to_string()),
        selected: plan.len(),
        total,
    });

    if plan.is_empty() {
        let unimported = report_unimported(host, candidates.as_ref(), diagnostics);
        let Ok(unimported) = unimported else {
            return TestExitCode::RunnerError;
        };
        // Said before the terminal event, so a reader of an interleaved
        // terminal sees the reason ahead of the vacuous "0 passed" summary.
        eprintln!("{}", empty_selection_reason(total));
        reporter.emit(&Event::RunFinished {
            passed: 0,
            failed: 0,
            timeout: 0,
            crash: 0,
            wall_ms: elapsed_ms(started),
            unimported_test_files: unimported,
            test_candidates: candidate_source(candidates.as_ref()),
        });
        return TestExitCode::EmptySelection;
    }

    let run_root = exec::run_root(seed);
    let image_path = match publish_image(&image.elf, target, &run_root) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            return TestExitCode::RunnerError;
        }
    };

    let outcome = execute_plan(ExecutionRequest {
        plan: &plan,
        image: &image_path,
        run_root: &run_root,
        seed,
        timeout: Duration::from_millis(options.timeout_ms),
        jobs,
        root: &root,
        repro_flags: &repro_flags,
        reporter: &reporter,
    });
    // The image is the runner's own artifact and is never retained; the run
    // root goes with it unless a failing test left a scratch directory behind,
    // in which case the non-recursive removal fails and the evidence survives.
    let _ = std::fs::remove_file(&image_path);
    let _ = std::fs::remove_dir(&run_root);

    if let Some(error) = outcome.runner_error {
        eprintln!("error: {error}");
        return TestExitCode::RunnerError;
    }

    let Ok(unimported) = report_unimported(host, candidates.as_ref(), diagnostics) else {
        return TestExitCode::RunnerError;
    };
    reporter.emit(&Event::RunFinished {
        passed: outcome.passed,
        failed: outcome.failed,
        timeout: outcome.timeout,
        crash: outcome.crash,
        wall_ms: elapsed_ms(started),
        unimported_test_files: unimported,
        test_candidates: candidate_source(candidates.as_ref()),
    });

    if outcome.failed + outcome.timeout + outcome.crash > 0 {
        TestExitCode::Failures
    } else {
        TestExitCode::AllPassed
    }
}

/// The `opt_level` field: the digit alone, so a consumer reads `"2"` rather
/// than having to strip the `-O` a command line spells it with.
fn opt_level_digit(level: OptLevel) -> String {
    level.name().trim_start_matches('O').to_owned()
}

/// Why exit 3 happened, distinguishing the two ways a selection can be empty.
///
/// "Your filter matched nothing" and "this root declares no tests" are
/// different mistakes with different fixes, and a run that says only the first
/// sends a reader looking for a typo in a correct pattern.
fn empty_selection_reason(total: usize) -> &'static str {
    if total == 0 {
        "error: the compiled closure declares no tests; a test-only file must be reached by an @import"
    } else {
        "error: no tests matched the selection"
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn candidate_source(candidates: Option<&TestCandidateInventory>) -> CandidateSource {
    match candidates {
        Some(_) => CandidateSource::Declared,
        None => CandidateSource::None,
    }
}

/// `--list`: the inventory, with no codegen, no linking, and no execution.
///
/// Filtering and sharding apply — a listing answers "what would this
/// invocation run" — but the shuffle does not. A listing is an inventory, and
/// stable-ID order is the property that makes two listings comparable.
///
/// Membership therefore comes from `selection::select`, the same computation
/// `plan` runs, rather than from a second copy of the predicate here: a listing
/// that could disagree with the run it previews is worse than no listing.
fn list(
    host: &mut FilesystemCompilerHost,
    compile_options: &CompileOptions,
    options: &TestOptions,
    diagnostics: &crate::DiagnosticOutput<'_>,
    reporter: &Reporter,
) -> TestExitCode {
    let inventory = match host.test_inventory(compile_options) {
        Ok(inventory) => inventory,
        Err(errors) => {
            diagnostics.print_errors(&crate::with_import_migration_helps(&errors));
            return TestExitCode::RunnerError;
        }
    };
    let selected = selection::select(&inventory.entries, &options.filters, options.shard);
    if selected.is_empty() {
        eprintln!("{}", empty_selection_reason(inventory.entries.len()));
        return TestExitCode::EmptySelection;
    }
    for entry in selected {
        reporter.emit(&Event::Test {
            id: entry.id.clone(),
            module: entry.module.clone(),
            name: entry.name.clone(),
            file: entry.file.clone(),
            line: entry.line,
            column: entry.column,
        });
    }
    TestExitCode::AllPassed
}

/// Write the linked image where it can be executed.
///
/// This goes through the driver's ordinary publication path rather than a bare
/// `fs::write`, because publication is where target-specific finalization
/// happens — notably ad-hoc Mach-O signing, without which the image would not
/// run at all on Apple silicon.
fn publish_image(
    elf: &[u8],
    target: Target,
    run_root: &std::path::Path,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(run_root)
        .map_err(|error| format!("error: could not create the test run directory: {error}"))?;
    let path = run_root.join("rue-test-image");
    let destination = crate::output::preflight_destination(&path, std::iter::empty())
        .map_err(|error| publish_failure("stage", error))?;
    crate::output::publish_executable(crate::output::PublishRequest {
        destination,
        bytes: elf,
        target,
    })
    .map_err(|error| publish_failure("publish", error))?;
    Ok(path)
}

/// A staging failure, rendered through the same message a publication failure
/// carries on the compile path.
fn publish_failure(verb: &str, error: crate::output::PublishError) -> String {
    format!(
        "error: could not {verb} the test image: {}",
        error.into_compile_error().kind
    )
}

struct ExecutionRequest<'a> {
    plan: &'a [TestInventoryEntry],
    image: &'a std::path::Path,
    run_root: &'a std::path::Path,
    seed: u64,
    timeout: Duration,
    jobs: usize,
    root: &'a str,
    repro_flags: &'a [String],
    reporter: &'a Reporter,
}

#[derive(Default)]
struct ExecutionOutcome {
    passed: usize,
    failed: usize,
    timeout: usize,
    crash: usize,
    runner_error: Option<String>,
}

/// Run the plan across a bounded pool of workers.
///
/// Work is claimed from a shared cursor rather than partitioned up front:
/// tests have wildly different durations and the MVP has no duration history
/// to bin-pack with (that arrives with the deferred scheduling ADR), so a
/// static split would leave workers idle behind one slow test.
fn execute_plan(request: ExecutionRequest<'_>) -> ExecutionOutcome {
    let cursor = AtomicUsize::new(0);
    let passed = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let timed_out = AtomicUsize::new(0);
    let crashed = AtomicUsize::new(0);
    let runner_error: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..request.jobs.max(1) {
            let cursor = &cursor;
            let passed = &passed;
            let failed = &failed;
            let timed_out = &timed_out;
            let crashed = &crashed;
            let runner_error = &runner_error;
            let request = &request;
            scope.spawn(move || {
                loop {
                    let index = cursor.fetch_add(1, Ordering::SeqCst);
                    let Some(entry) = request.plan.get(index) else {
                        return;
                    };
                    if runner_error
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .is_some()
                    {
                        return;
                    }
                    request.reporter.emit(&Event::TestStarted {
                        id: entry.id.clone(),
                    });
                    let execution = exec::run_one(Dispatch {
                        image: request.image,
                        run_root: request.run_root,
                        ordinal: entry.ordinal,
                        seed: request.seed,
                        timeout: request.timeout,
                        stream_budget: DEFAULT_STREAM_BUDGET,
                    });
                    let execution = match execution {
                        Ok(execution) => execution,
                        Err(error) => {
                            let mut slot = runner_error
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            if slot.is_none() {
                                *slot = Some(format!("could not run test '{}': {error}", entry.id));
                            }
                            return;
                        }
                    };
                    match &execution.classification.verdict {
                        Verdict::Pass => passed.fetch_add(1, Ordering::Relaxed),
                        Verdict::Fail(_) => failed.fetch_add(1, Ordering::Relaxed),
                        Verdict::Timeout => timed_out.fetch_add(1, Ordering::Relaxed),
                        Verdict::Crash(_) => crashed.fetch_add(1, Ordering::Relaxed),
                    };
                    let event = finish_event(
                        entry,
                        execution,
                        request.root,
                        request.repro_flags,
                        request.seed,
                        request.timeout,
                    );
                    request.reporter.emit(&event);
                }
            });
        }
    });

    ExecutionOutcome {
        passed: passed.into_inner(),
        failed: failed.into_inner(),
        timeout: timed_out.into_inner(),
        crash: crashed.into_inner(),
        runner_error: runner_error
            .into_inner()
            .unwrap_or_else(|error| error.into_inner()),
    }
}

/// Turn one finished process into its `test_finished` event.
///
/// The scratch directory is deleted on a pass and retained on anything else,
/// with its path in the event: the abort-only runtime means destructors do not
/// run on a failing path, so the directory plus process death is what
/// teardown-on-failure amounts to (ADR-0083 §5.4).
fn finish_event(
    entry: &TestInventoryEntry,
    execution: exec::Execution,
    root: &str,
    repro_flags: &[String],
    seed: u64,
    timeout: Duration,
) -> Event {
    let verdict = execution.classification.verdict.clone();
    let passed = verdict.is_pass();
    if passed {
        let _ = std::fs::remove_dir_all(&execution.scratch_dir);
    }
    let failure = (!passed).then(|| failure_record(entry, &verdict, &execution, timeout));
    Event::TestFinished(Box::new(TestFinished {
        id: entry.id.clone(),
        verdict,
        duration_ms: u64::try_from(execution.duration.as_millis()).unwrap_or(u64::MAX),
        failure,
        stdout: Capture::new(execution.stdout, execution.stdout_total, passed),
        stderr: Capture::new(execution.stderr, execution.stderr_total, passed),
        scratch_dir: (!passed).then(|| execution.scratch_dir.display().to_string()),
        repro: repro_argv(root, &entry.id, seed, repro_flags),
    }))
}

fn failure_record(
    entry: &TestInventoryEntry,
    verdict: &Verdict,
    execution: &exec::Execution,
    timeout: Duration,
) -> FailureRecord {
    let frame = execution.frames.failure.as_ref();
    let (kind, message) = match verdict {
        Verdict::Pass => unreachable!("a pass carries no failure record"),
        Verdict::Timeout => (
            "timeout".to_owned(),
            format!(
                "the test exceeded its {} ms budget; the process group was killed",
                timeout.as_millis()
            ),
        ),
        Verdict::Crash(signal) => (
            "signal".to_owned(),
            format!("the test was killed by signal {signal}"),
        ),
        Verdict::Fail(kind) => (kind.to_string(), failure_message(kind, execution, frame)),
    };
    // The declaration's span is the default location; a frame that carries its
    // own site (the `?` failure arm, or an assertion library reporting its
    // caller) supersedes it.
    let location = match frame.filter(|frame| !frame.file.is_empty()) {
        Some(frame) => Location {
            file: frame.file.clone(),
            line: frame.line,
            column: frame.column,
        },
        None => Location {
            file: entry.file.clone(),
            line: entry.line,
            column: entry.column,
        },
    };
    FailureRecord {
        kind,
        message,
        exit_code: execution.exit_code,
        signal: execution.signal,
        location: Some(location),
        payload: frame
            .map(|frame| frame.payload.clone())
            .filter(|payload| !payload.is_empty()),
        comparison: frame_comparison(frame),
        runner_note: execution.classification.runner_note.clone(),
    }
}

/// The comparison a failure frame carried, or `None` when it carried none
/// (ADR-0083 Phase 2.5).
///
/// `left` and `right` travel together or not at all: a frame with one and
/// not the other is a producer's mistake, and half a comparison is not a
/// comparison. The diff between them is computed here, once, so the event
/// stream and the human rendering read the same one.
fn frame_comparison(frame: Option<&verdict::FailureFrame>) -> Option<Comparison> {
    let frame = frame?;
    let (left, right) = frame.left.clone().zip(frame.right.clone())?;
    Some(Comparison::new(left, right))
}

fn failure_message(
    kind: &FailureKind,
    execution: &exec::Execution,
    frame: Option<&verdict::FailureFrame>,
) -> String {
    if let Some(frame) = frame {
        if !frame.message.is_empty() {
            return frame.message.clone();
        }
    }
    match kind {
        FailureKind::Incomplete => {
            "the test exited 0 without the dispatcher's completion record".to_owned()
        }
        FailureKind::OutputOverflow(overflow) => format!(
            "{} exceeded its {}-byte retention budget; the process group was killed",
            overflow.stream.name(),
            overflow.budget
        ),
        FailureKind::Assert
        | FailureKind::AssertEq
        | FailureKind::AssertNe
        | FailureKind::Trap(_) => last_message_line(&execution.stderr),
        FailureKind::Exit => match execution.exit_code {
            Some(code) => format!("the test exited with status {code}"),
            None => "the test did not exit normally".to_owned(),
        },
        FailureKind::UnhandledError | FailureKind::Reported(_) => {
            last_message_line(&execution.stderr)
        }
    }
}

/// The last non-empty line of a trapping test's stderr: the pinned runtime
/// message the verdict was classified from.
fn last_message_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}

/// The argv that reproduces exactly this one test (ADR-0083 §3).
///
/// It selects by the full stable ID, never the bare name: two modules may
/// declare tests with the same name, and a repro that re-runs both is not a
/// repro. The seed travels with it so a shuffle-dependent failure comes back.
fn repro_argv(root: &str, id: &str, seed: u64, flags: &[String]) -> Vec<String> {
    let mut argv = vec![
        "rue".to_owned(),
        "test".to_owned(),
        root.to_owned(),
        "--filter".to_owned(),
        id.to_owned(),
        "--seed".to_owned(),
        seed.to_string(),
    ];
    argv.extend(flags.iter().cloned());
    argv
}

/// Render the unimported-test-file warnings and collect them for the event.
///
/// Returns `Err(())` when the report itself failed; its diagnostics have
/// already been presented.
fn report_unimported(
    host: &mut FilesystemCompilerHost,
    candidates: Option<&TestCandidateInventory>,
    diagnostics: &crate::DiagnosticOutput<'_>,
) -> Result<Option<Vec<UnimportedFile>>, ()> {
    let Some(candidates) = candidates else {
        return Ok(None);
    };
    let files = match host.unimported_test_files(candidates) {
        Ok(files) => files,
        Err(errors) => {
            diagnostics.print_errors(&errors);
            return Err(());
        }
    };
    let warnings: Vec<rue_compiler::CompileWarning> = files
        .iter()
        .map(|file| {
            let kind = if file.parse_failed {
                rue_error::WarningKind::UnimportedTestFileUnparsable {
                    path: file.path.clone(),
                }
            } else {
                rue_error::WarningKind::UnimportedTestFile {
                    path: file.path.clone(),
                    tests: file.tests,
                }
            };
            rue_compiler::CompileWarning::without_span(kind)
        })
        .collect();
    if !warnings.is_empty() {
        diagnostics.print_warnings(&warnings);
    }
    Ok(Some(
        files
            .into_iter()
            .map(|file| UnimportedFile {
                path: file.path,
                tests: file.tests,
                parse_failed: file.parse_failed,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Agents branch on these numbers; they are pinned rather than implied.
    #[test]
    fn exit_codes_are_the_documented_mapping() {
        assert_eq!(TestExitCode::AllPassed.code(), 0);
        assert_eq!(TestExitCode::Failures.code(), 1);
        assert_eq!(TestExitCode::RunnerError.code(), 2);
        assert_eq!(TestExitCode::EmptySelection.code(), 3);
    }

    #[test]
    fn formats_parse_their_two_spellings_and_nothing_else() {
        assert_eq!(
            "human".parse::<OutputFormat>().unwrap(),
            OutputFormat::Human
        );
        assert_eq!("json".parse::<OutputFormat>().unwrap(), OutputFormat::Json);
        assert!(
            "ndjson"
                .parse::<OutputFormat>()
                .unwrap_err()
                .contains("human, json")
        );
    }

    /// The repro selects by the full stable ID and repeats the run's seed and
    /// compile flags, so re-running it lands on the same one test.
    #[test]
    fn a_repro_argv_names_the_stable_id_the_seed_and_the_flags() {
        let argv = repro_argv(
            "app/main.rue",
            "app/t.rue::parses a port",
            417,
            &[
                "--target".to_owned(),
                "x86-64-linux".to_owned(),
                "-O1".to_owned(),
                "--preview".to_owned(),
                "test_infra".to_owned(),
                "--timeout-ms".to_owned(),
                "500".to_owned(),
            ],
        );
        assert_eq!(
            argv,
            vec![
                "rue",
                "test",
                "app/main.rue",
                "--filter",
                "app/t.rue::parses a port",
                "--seed",
                "417",
                "--target",
                "x86-64-linux",
                "-O1",
                "--preview",
                "test_infra",
                "--timeout-ms",
                "500",
            ]
        );
    }

    /// The comparison fields are one unit: both, or neither. A frame carrying
    /// only one of them publishes nothing rather than half a report a consumer
    /// would have to guess at.
    #[test]
    fn a_comparison_needs_both_of_its_operands() {
        let frame = |left: Option<&str>, right: Option<&str>| verdict::FailureFrame {
            kind: "assert_eq".to_owned(),
            left: left.map(str::to_owned),
            right: right.map(str::to_owned),
            ..verdict::FailureFrame::default()
        };
        assert!(frame_comparison(None).is_none());
        assert!(frame_comparison(Some(&frame(None, None))).is_none());
        assert!(frame_comparison(Some(&frame(Some("41"), None))).is_none());
        assert!(frame_comparison(Some(&frame(None, Some("42")))).is_none());
        let both = frame_comparison(Some(&frame(Some("41"), Some("42")))).expect("a comparison");
        assert_eq!(both.left, "41");
        assert_eq!(both.right, "42");
        assert_eq!(both.diff.len(), 3, "{:?}", both.diff);
        // Two empty renderings are still a comparison: empty is a value.
        let empty = frame_comparison(Some(&frame(Some(""), Some("")))).expect("a comparison");
        assert!(empty.diff.is_empty());
    }

    #[test]
    fn the_last_stderr_line_is_the_message_a_trap_reports() {
        assert_eq!(
            last_message_line(b"working\nassertion failed\n"),
            "assertion failed"
        );
        assert_eq!(last_message_line(b""), "");
    }

    /// A fresh seed is genuinely fresh: two runs in one process must not agree.
    #[test]
    fn fresh_seeds_differ_between_runs() {
        assert_ne!(fresh_seed(), fresh_seed());
    }

    /// A wrong filter and a root with no tests are different mistakes.
    #[test]
    fn an_empty_selection_names_which_emptiness_it_was() {
        assert!(empty_selection_reason(0).contains("declares no tests"));
        assert!(empty_selection_reason(0).contains("@import"));
        assert!(empty_selection_reason(7).contains("no tests matched the selection"));
    }

    /// The digit alone, so a consumer reads `"2"` rather than stripping `-O`.
    #[test]
    fn the_opt_level_field_is_the_bare_digit() {
        assert_eq!(opt_level_digit(OptLevel::O0), "0");
        assert_eq!(opt_level_digit(OptLevel::O3), "3");
    }
}
