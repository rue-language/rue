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
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rue_compiler::unstable::{SourceInfo, TestCandidateInventory, TestInventoryEntry};
use rue_compiler::{AcceptedReadManifest, CompileErrors, CompileOptions, OptLevel, SourceSnapshot};
use rue_driver::{AttemptedRead, FilesystemCompilerHost, WatchInput};
use rue_target::Target;

use events::{
    CandidateSource, Capture, Comparison, Event, FailureRecord, Location, TestFinished,
    UnimportedFile,
};
use exec::{DEFAULT_STREAM_BUDGET, Dispatch};
use selection::Shard;
use verdict::{FailureKind, TestExpectation, Verdict};

// What the watch loop drives a test cycle with. Test mode owns the run; the
// loop owns the process's lifetime, the change monitor, and the signals
// (RUE-2023).
pub(crate) use exec::{
    RunCancellation, install_watch_signal_exit, reserve_channel_descriptor, set_watch_exit_status,
};

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
    /// Match filters against complete stable IDs rather than substrings.
    pub(crate) exact: bool,
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
            exact: false,
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
    /// The root source exactly as the command line spelled it, which is what
    /// `run_started` publishes.
    pub(crate) root: String,
    /// The root anchored at the invocation directory for repros.
    pub(crate) repro_root: String,
    /// The compile-mode flags a repro argv repeats after the filter and seed.
    pub(crate) repro_flags: Vec<String>,
    /// The environment assignments a repro must be run under, sorted by name.
    ///
    /// Environment is not argv, so a variable that decided the run — the
    /// standard library `RUE_STD_PATH` names — would otherwise be silently
    /// missing from a pasted reproduction (RUE-2020).
    pub(crate) repro_env: Vec<(String, String)>,
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
pub(crate) fn fresh_seed() -> u64 {
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
        repro_root,
        repro_flags,
        repro_env,
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

    match run_cycle(CycleRequest {
        host,
        compile_options: &compile_options,
        options: &options,
        diagnostics,
        root: &root,
        repro_root: &repro_root,
        repro_flags: &repro_flags,
        repro_env: &repro_env,
        jobs,
        target,
        opt_level,
        candidates: candidates.as_ref(),
        seed,
        cycle: None,
        cancellation: None,
        observation: crate::compile::CycleObservation::OneShot,
    }) {
        CycleOutcome::Finished(exit) => exit,
        // A one-shot cycle observes nothing that could supersede it and holds
        // no cancellation anyone else can trip; those outcomes belong to the
        // watch loop alone (RUE-2023).
        CycleOutcome::Canceled | CycleOutcome::Superseded(_) => TestExitCode::RunnerError,
    }
}

/// Everything one test cycle needs.
///
/// One-shot mode drives exactly one of these; `rue test --watch` drives one per
/// accepted source revision on the retained host, which is what removes the
/// per-run recompile (RUE-2023). Everything that differs between the two is a
/// field here — the cycle number, the run cancellation, and how the image is
/// observed — so there is one cycle body rather than a batch one and a watch
/// one that drift.
pub(crate) struct CycleRequest<'a, 'diagnostics> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) compile_options: &'a CompileOptions,
    pub(crate) options: &'a TestOptions,
    pub(crate) diagnostics: &'a crate::DiagnosticOutput<'diagnostics>,
    pub(crate) root: &'a str,
    pub(crate) repro_root: &'a str,
    pub(crate) repro_flags: &'a [String],
    pub(crate) repro_env: &'a [(String, String)],
    pub(crate) jobs: usize,
    pub(crate) target: Target,
    pub(crate) opt_level: OptLevel,
    /// Re-acquired per cycle under `--watch`: `--test-candidates` names files
    /// on disk, and the answer to "what does this target own" changes with the
    /// same edits everything else here does.
    pub(crate) candidates: Option<&'a TestCandidateInventory>,
    /// Fixed for the life of the process unless `--seed` was given, so
    /// consecutive cycles shuffle the same way and a difference between two of
    /// them is attributable to the edit rather than to the order.
    pub(crate) seed: u64,
    /// The 1-based watch cycle number, or `None` for a one-shot run.
    pub(crate) cycle: Option<u64>,
    /// The watch loop's authority to end this cycle's execution phase.
    pub(crate) cancellation: Option<&'a exec::RunCancellation>,
    pub(crate) observation: crate::compile::CycleObservation<'a>,
}

/// How a cycle ended.
pub(crate) enum CycleOutcome {
    /// The cycle published `run_finished` (or failed before any event could be
    /// published) and this is the status a one-shot run would exit with.
    Finished(TestExitCode),
    /// The cycle was abandoned. If it had reached the run, `run_canceled` says
    /// so on the event stream; if it was still building the image, nothing was
    /// published at all, because no `run_started` had opened the cycle.
    Canceled,
    /// A newer source revision arrived before the image could be published.
    Superseded(crate::compile::Supersession),
}

/// Build the image for one revision and run the plan over it.
pub(crate) fn run_cycle(request: CycleRequest<'_, '_>) -> CycleOutcome {
    let CycleRequest {
        host,
        compile_options,
        options,
        diagnostics,
        root,
        repro_root,
        repro_flags,
        repro_env,
        jobs,
        target,
        opt_level,
        candidates,
        seed,
        cycle,
        cancellation,
        observation,
    } = request;

    // One private directory per cycle, so a failing test's retained scratch
    // directory from cycle N survives cycle N+1 (RUE-2023).
    let run_root = exec::run_root(seed, cycle);
    if let Err(error) = std::fs::create_dir_all(&run_root) {
        eprintln!("error: could not create the test run directory: {error}");
        return CycleOutcome::Finished(TestExitCode::RunnerError);
    }
    let image_path = run_root.join("rue-test-image");

    // Nothing is published before the image exists: a compile failure outside
    // every test closure is diagnostics on stderr and exit 2, with an empty
    // event stream. A failure INSIDE one is not that failure — the image still
    // exists, built from the tests that did analyze (ADR-0083 §3). The cycle
    // that decides all of that is `compile`'s, shared with the executable
    // build and with executable watch (RUE-1969, RUE-2023).
    let response = crate::compile::produce_test_cycle(crate::compile::TestCycleRequest {
        host: &mut *host,
        options: compile_options,
        error_format: diagnostics.format(),
        candidates,
        image_path: &image_path,
        observation,
    });
    let multi_module_closure = response.published_user_module_count() > 1;
    let image = match response.complete(None) {
        crate::compile::CycleReport::Published(image) => *image,
        crate::compile::TestCycleReport::Failed => {
            discard_run_root(&image_path, &run_root);
            return CycleOutcome::Finished(TestExitCode::RunnerError);
        }
        crate::compile::TestCycleReport::Canceled => {
            discard_run_root(&image_path, &run_root);
            return CycleOutcome::Canceled;
        }
        crate::compile::TestCycleReport::Superseded(boundary) => {
            discard_run_root(&image_path, &run_root);
            return CycleOutcome::Superseded(boundary);
        }
    };
    // Built here rather than above because the closure is only published once
    // the image is: nothing before this point could answer how many modules the
    // program has.
    let reporter = Reporter::new(
        options.format,
        render::Context {
            multi_module_closure,
        },
    );
    let diagnostics = diagnostics_for_snapshot(image.error_format, &image.source_snapshot);
    let compile_errors = CompileErrorVerdicts::new(&image.compile_failures, &diagnostics);

    let total = image.inventory.entries.len();
    let plan = selection::plan(
        &image.inventory.entries,
        &options.filters,
        options.exact,
        options.shard,
        seed,
    );
    let started = Instant::now();

    reporter.emit(&Event::RunStarted {
        root: root.to_owned(),
        target: target.to_string(),
        opt_level: opt_level_digit(opt_level),
        seed,
        jobs,
        shard: options.shard.map(|shard| shard.to_string()),
        selected: plan.len(),
        total,
        cycle,
    });
    watch_milestone("run-started");

    if plan.is_empty() {
        discard_run_root(&image_path, &run_root);
        let unimported = match report_unimported(image.unimported_test_files.as_ref(), &diagnostics)
        {
            Ok(unimported) => unimported,
            Err(_) => return CycleOutcome::Finished(TestExitCode::RunnerError),
        };
        // Said before the terminal event, so a reader of an interleaved
        // terminal sees the reason ahead of the vacuous "0 passed" summary.
        eprintln!("{}", empty_selection_reason(total));
        reporter.emit(&Event::RunFinished {
            passed: 0,
            failed: 0,
            timeout: 0,
            crash: 0,
            compile_error: 0,
            xfail: 0,
            xpass: 0,
            wall_ms: elapsed_ms(started),
            unimported_test_files: unimported,
            test_candidates: candidate_source(candidates),
        });
        watch_milestone("run-finished");
        return CycleOutcome::Finished(TestExitCode::EmptySelection);
    }

    // A repro is pasted into some other shell, from some other directory, so it
    // names the compiler and the root by absolute path rather than by whatever
    // spelling this invocation happened to use (RUE-2020).
    let repro_program = repro_program();
    let outcome = execute_plan(ExecutionRequest {
        plan: &plan,
        compile_errors: &compile_errors,
        target,
        image: &image_path,
        run_root: &run_root,
        seed,
        timeout: Duration::from_millis(options.timeout_ms),
        jobs,
        repro_program: &repro_program,
        repro_root,
        repro_flags,
        repro_env,
        reporter: &reporter,
        cancellation,
    });
    // The image is the runner's own artifact and is never retained; the run
    // root goes with it unless a failing test left a scratch directory behind,
    // in which case the non-recursive removal fails and the evidence survives.
    discard_run_root(&image_path, &run_root);

    // An edit that landed mid-run killed the tests that were still going, so
    // the counts describe neither the whole plan nor the verdicts that would
    // have followed. The cycle says it was abandoned and says how far it got;
    // it does not publish a `run_finished` (RUE-2023).
    if outcome.canceled {
        reporter.emit(&Event::RunCanceled {
            // A run is only cancellable through a watch cycle's own
            // cancellation, and a watch cycle always has a number.
            cycle: cycle.unwrap_or(0),
            reported: outcome.reported(),
            selected: plan.len(),
            wall_ms: elapsed_ms(started),
        });
        watch_milestone("run-canceled");
        return CycleOutcome::Canceled;
    }

    if let Some(error) = outcome.runner_error {
        eprintln!("error: {error}");
        return CycleOutcome::Finished(TestExitCode::RunnerError);
    }

    let unimported = match report_unimported(image.unimported_test_files.as_ref(), &diagnostics) {
        Ok(unimported) => unimported,
        Err(_) => return CycleOutcome::Finished(TestExitCode::RunnerError),
    };
    reporter.emit(&Event::RunFinished {
        passed: outcome.passed,
        failed: outcome.failed,
        timeout: outcome.timeout,
        crash: outcome.crash,
        compile_error: outcome.compile_error,
        xfail: outcome.xfail,
        xpass: outcome.xpass,
        wall_ms: elapsed_ms(started),
        unimported_test_files: unimported,
        test_candidates: candidate_source(candidates),
    });
    watch_milestone("run-finished");

    // A `compile_error` test is a failed test, not a failed run: exit 1 with
    // the other tests' verdicts, never the 2 that says nothing ran
    // (ADR-0083 §3).
    CycleOutcome::Finished(
        if outcome.failed + outcome.timeout + outcome.crash + outcome.compile_error + outcome.xpass
            > 0
        {
            TestExitCode::Failures
        } else {
            TestExitCode::AllPassed
        },
    )
}

/// Remove the cycle's image and, if nothing retained a scratch directory
/// inside it, the run root itself.
///
/// The removal is deliberately non-recursive: a failing test's scratch
/// directory is evidence, and the directory that holds it survives with it
/// (ADR-0083 §5.4).
fn discard_run_root(image_path: &Path, run_root: &Path) {
    let _ = std::fs::remove_file(image_path);
    let _ = std::fs::remove_dir(run_root);
}

/// Report a run milestone on the watch loop's test protocol.
///
/// Dormant unless `RUE_WATCH_TEST_PROTOCOL` names a file, exactly like every
/// other milestone: this is the seam that lets a watch-mode CLI case wait for
/// a cycle's run to start, finish, or be abandoned instead of sleeping.
fn watch_milestone(event: &str) {
    crate::watch::test_event(event);
}

/// The `compile_error` verdicts a run publishes, keyed by ordinal.
///
/// Built once, before the run, because the compiler decided them: a test whose
/// closure failed to analyze is excluded from the image, so there is no process
/// to observe and nothing about it can change while the run proceeds
/// (ADR-0083 §3).
struct CompileErrorVerdicts {
    /// `ordinal -> (first diagnostic's message and site, the JSON copies)`.
    by_ordinal: std::collections::BTreeMap<u32, CompileErrorVerdict>,
}

struct CompileErrorVerdict {
    /// Preserve typed infrastructure/ICE classification before rendering.
    xfail_eligible: bool,
    /// The diagnostics rendered for a person, as the failure record's payload.
    payload: String,
    /// The first diagnostic's message, which is the record's `message`.
    message: String,
    /// The first diagnostic's primary span, which is the record's `location`.
    location: Option<Location>,
    /// Every diagnostic as `--error-format json` would publish it.
    diagnostics: Vec<serde_json::Value>,
}

impl CompileErrorVerdicts {
    fn new(
        failures: &[rue_compiler::unstable::TestCompileFailure],
        diagnostics: &crate::DiagnosticOutput<'_>,
    ) -> Self {
        let batches = failures
            .iter()
            .map(|failure| &failure.errors)
            .collect::<Vec<_>>();
        let rendered = diagnostics.json_prepared_diagnostic_batches(&batches);
        Self {
            by_ordinal: failures
                .iter()
                .zip(rendered)
                .map(|(failure, json)| {
                    let first = json.first();
                    (
                        failure.entry.ordinal,
                        CompileErrorVerdict {
                            xfail_eligible: compile_errors_allow_xfail(&failure.errors),
                            payload: diagnostic_payload(&json),
                            message: first
                                .and_then(|diagnostic| diagnostic.get("message"))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            location: first.and_then(primary_location),
                            diagnostics: json,
                        },
                    )
                })
                .collect(),
        }
    }

    fn get(&self, ordinal: u32) -> Option<&CompileErrorVerdict> {
        self.by_ordinal.get(&ordinal)
    }
}

fn compile_errors_allow_xfail(errors: &rue_error::CompileErrors) -> bool {
    use rue_error::ErrorKind;
    !errors.is_empty()
        && errors.iter().all(|error| {
            !matches!(
                error.kind,
                ErrorKind::InternalError(_)
                    | ErrorKind::InternalCodegenError(_)
                    | ErrorKind::CompilerProducerInvariant(_)
                    | ErrorKind::CompilerResourceExhaustion(_)
                    | ErrorKind::OutputPublication(_)
                    | ErrorKind::InvalidCompilerInput(_)
                    | ErrorKind::UnsatisfiedTrustedToolchainInput(_)
                    | ErrorKind::StdLibNotFound
                    | ErrorKind::LinkError(_)
                    | ErrorKind::UnsupportedTarget(_)
            )
        })
}

/// The failure record's `payload` for a `compile_error`: one line per
/// diagnostic, `<code>: <message>` where a diagnostic is coded.
///
/// A rendering rather than the diagnostics themselves, because the structured
/// form travels in `diagnostics` and the authoritative form is already on
/// stderr. This is the one-string summary the open payload field is for.
fn diagnostic_payload(diagnostics: &[serde_json::Value]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let message = diagnostic
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            match diagnostic.get("code").and_then(serde_json::Value::as_str) {
                Some(code) if !code.is_empty() => format!("{code}: {message}"),
                _ => message.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A diagnostic's primary span as a failure record's location.
///
/// `spans[0]` is the primary one when there is any span at all
/// (diagnostics.md), and the coordinates are already the 1-based line and
/// Unicode-scalar column a failure record promises, so a report and a
/// diagnostic can never disagree about where something is.
fn primary_location(diagnostic: &serde_json::Value) -> Option<Location> {
    let span = diagnostic.get("spans")?.as_array()?.first()?;
    Some(Location {
        file: span.get("file")?.as_str()?.to_owned(),
        line: u32::try_from(span.get("line")?.as_u64()?).unwrap_or(0),
        column: u32::try_from(span.get("column")?.as_u64()?).unwrap_or(0),
    })
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
/// that could disagree with the run it previews is worse than no listing. For
/// the same reason it prints the closure's analysis diagnostics on stderr as
/// the run path does, while still listing every declaration and exiting `0`.
struct OwnedListingResponse {
    source_snapshot: SourceSnapshot,
    accepted_reads: AcceptedReadManifest,
    attempted_reads: Vec<AttemptedRead>,
    watch_inputs: Vec<WatchInput>,
    error_format: crate::ErrorFormat,
    result: Result<rue_compiler::unstable::TestListing, CompileErrors>,
}

fn produce_listing(
    host: &mut FilesystemCompilerHost,
    compile_options: &CompileOptions,
    error_format: crate::ErrorFormat,
) -> OwnedListingResponse {
    let source_snapshot = host.source_snapshot().clone();
    let accepted_reads = host.accepted_reads().clone();
    let attempted_reads = host.attempted_reads().to_vec();
    let watch_inputs = host.watch_inputs();
    let result = host
        .test_inventory(compile_options)
        .map_err(|errors| rue_driver::with_import_migration_helps(&errors))
        .map(|mut listing| {
            listing.failure_diagnostics =
                rue_driver::with_import_migration_helps(&listing.failure_diagnostics);
            listing
        });
    OwnedListingResponse {
        source_snapshot,
        accepted_reads,
        attempted_reads,
        watch_inputs,
        error_format,
        result,
    }
}

fn complete_listing(
    response: OwnedListingResponse,
    options: &TestOptions,
    reporter: &Reporter,
) -> TestExitCode {
    let OwnedListingResponse {
        source_snapshot,
        accepted_reads,
        attempted_reads,
        watch_inputs,
        error_format,
        result,
    } = response;
    drop((accepted_reads, attempted_reads, watch_inputs));
    let diagnostics = diagnostics_for_snapshot(error_format, &source_snapshot);
    let listing = match result {
        Ok(listing) => listing,
        Err(errors) => {
            diagnostics.print_prepared_errors(&errors);
            return TestExitCode::RunnerError;
        }
    };
    let rue_compiler::unstable::TestListing {
        inventory,
        failure_diagnostics,
    } = listing;
    // A listing still lists every declaration and still succeeds, but it says
    // what the run would say about the bodies that did not analyze: on stderr,
    // in the run's own `--error-format`, through the same renderer. A listing
    // that swallowed them would be the papercut the run path refuses to be —
    // a reader inspecting a suite would see nothing wrong with tests the run
    // will report as `compile_error`.
    if !failure_diagnostics.is_empty() {
        diagnostics.print_prepared_errors(&failure_diagnostics);
    }
    let selected = selection::select(
        &inventory.entries,
        &options.filters,
        options.exact,
        options.shard,
    );
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
            known_bug: entry
                .expected_failures
                .iter()
                .find(|marker| marker.platform.is_none())
                .map(|marker| marker.issue.clone()),
            known_bug_on: entry
                .expected_failures
                .iter()
                .filter_map(|marker| {
                    marker
                        .platform
                        .as_ref()
                        .map(|platform| (platform.clone(), marker.issue.clone()))
                })
                .collect(),
        });
    }
    TestExitCode::AllPassed
}

fn list(
    host: &mut FilesystemCompilerHost,
    compile_options: &CompileOptions,
    options: &TestOptions,
    diagnostics: &crate::DiagnosticOutput<'_>,
    reporter: &Reporter,
) -> TestExitCode {
    let response = produce_listing(host, compile_options, diagnostics.format());
    complete_listing(response, options, reporter)
}

struct ExecutionRequest<'a> {
    plan: &'a [TestInventoryEntry],
    target: Target,
    /// The verdicts the compiler already decided, by ordinal. A plan entry
    /// found here is reported without spawning anything: it has no body in the
    /// image (ADR-0083 §3).
    compile_errors: &'a CompileErrorVerdicts,
    image: &'a std::path::Path,
    run_root: &'a std::path::Path,
    seed: u64,
    timeout: Duration,
    jobs: usize,
    repro_program: &'a str,
    repro_root: &'a str,
    repro_flags: &'a [String],
    repro_env: &'a [(String, String)],
    reporter: &'a Reporter,
    /// A watch cycle's authority to abandon this run. `None` for a one-shot
    /// run, which nothing outside itself can end (RUE-2023).
    cancellation: Option<&'a exec::RunCancellation>,
}

#[derive(Default)]
struct ExecutionOutcome {
    passed: usize,
    failed: usize,
    timeout: usize,
    crash: usize,
    compile_error: usize,
    xfail: usize,
    xpass: usize,
    runner_error: Option<String>,
    /// An edit landed while the plan was running, so the run stopped short of
    /// it (RUE-2023).
    canceled: bool,
}

impl ExecutionOutcome {
    /// Verdicts this run actually published, which is what a canceled cycle
    /// reports in place of counts by class.
    fn reported(&self) -> usize {
        self.passed
            + self.failed
            + self.timeout
            + self.crash
            + self.compile_error
            + self.xfail
            + self.xpass
    }
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
    let uncompiled = AtomicUsize::new(0);
    let xfailed = AtomicUsize::new(0);
    let xpassed = AtomicUsize::new(0);
    let runner_error: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..request.jobs.max(1) {
            let cursor = &cursor;
            let passed = &passed;
            let failed = &failed;
            let timed_out = &timed_out;
            let crashed = &crashed;
            let uncompiled = &uncompiled;
            let xfailed = &xfailed;
            let xpassed = &xpassed;
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
                    // Read before anything is published, so an abandoned cycle
                    // never opens a `test_started` it can only leave dangling.
                    if canceled(request.cancellation) {
                        return;
                    }
                    request.reporter.emit(&Event::TestStarted {
                        id: entry.id.clone(),
                    });
                    // Reported in the plan's own order, from the same worker
                    // pool, so a `compile_error` lands where the shuffle put it
                    // rather than in a block of its own before the run.
                    if let Some(verdict) = request.compile_errors.get(entry.ordinal) {
                        let expectation = classify_expected(
                            entry,
                            request.target,
                            &verdict::Classification {
                                verdict: Verdict::CompileError,
                                runner_note: None,
                            },
                            !verdict.xfail_eligible,
                        );
                        if expectation == Some(TestExpectation::Xfail) {
                            xfailed.fetch_add(1, Ordering::Relaxed);
                        } else {
                            uncompiled.fetch_add(1, Ordering::Relaxed);
                        }
                        request.reporter.emit(&compile_error_event(
                            entry,
                            verdict,
                            expectation,
                            &Repro {
                                program: request.repro_program,
                                root: request.repro_root,
                                flags: request.repro_flags,
                                env: request.repro_env,
                            },
                            request.seed,
                        ));
                        continue;
                    }
                    crate::watch::test_event("test-spawned");
                    let execution = exec::run_one(Dispatch {
                        image: request.image,
                        run_root: request.run_root,
                        ordinal: entry.ordinal,
                        seed: request.seed,
                        timeout: request.timeout,
                        stream_budget: DEFAULT_STREAM_BUDGET,
                        cancellation: request.cancellation,
                    });
                    // The cancellation killed this test's process group, so
                    // whatever it reported is an artifact of the kill rather
                    // than a verdict about the program. The `test_started`
                    // above stands unfinished, and `run_canceled` is what
                    // tells a consumer why (RUE-2023).
                    if canceled(request.cancellation) {
                        // This one directory goes with the verdict that was
                        // never published. Retention is for evidence a reader
                        // was pointed at (ADR-0083 §5.4), and no event names
                        // this path; the abort-only runtime left nothing in it
                        // to inspect either, because the process was SIGKILLed.
                        // Only this ordinal's directory: a sibling test's
                        // retained scratch in the same run root belongs to a
                        // verdict that WAS published, which is why the run root
                        // itself is still removed non-recursively.
                        let _ = std::fs::remove_dir_all(exec::scratch_path(
                            request.run_root,
                            request.seed,
                            entry.ordinal,
                        ));
                        return;
                    }
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
                    let expected = classify_expected(
                        entry,
                        request.target,
                        &execution.classification,
                        execution.signal.is_some(),
                    );
                    match expected {
                        Some(TestExpectation::Xfail) => {
                            xfailed.fetch_add(1, Ordering::Relaxed);
                        }
                        Some(TestExpectation::Xpass) => {
                            xpassed.fetch_add(1, Ordering::Relaxed);
                        }
                        None => {
                            match &execution.classification.verdict {
                                Verdict::Pass => passed.fetch_add(1, Ordering::Relaxed),
                                Verdict::Fail(_) => failed.fetch_add(1, Ordering::Relaxed),
                                Verdict::Timeout => timed_out.fetch_add(1, Ordering::Relaxed),
                                Verdict::Crash(_) => crashed.fetch_add(1, Ordering::Relaxed),
                                // Decided by the compiler and reported above, so no
                                // process can classify as one.
                                Verdict::CompileError => unreachable!(
                                    "a compile_error verdict never reaches a dispatched process"
                                ),
                            };
                        }
                    };
                    let event = finish_event(
                        entry,
                        execution,
                        expected,
                        &Repro {
                            program: request.repro_program,
                            root: request.repro_root,
                            flags: request.repro_flags,
                            env: request.repro_env,
                        },
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
        compile_error: uncompiled.into_inner(),
        xfail: xfailed.into_inner(),
        xpass: xpassed.into_inner(),
        runner_error: runner_error
            .into_inner()
            .unwrap_or_else(|error| error.into_inner()),
        canceled: canceled(request.cancellation),
    }
}

/// Whether a watch cycle has abandoned the run this worker is serving.
fn canceled(cancellation: Option<&exec::RunCancellation>) -> bool {
    cancellation.is_some_and(exec::RunCancellation::is_canceled)
}

fn classify_expected(
    entry: &TestInventoryEntry,
    target: Target,
    classification: &verdict::Classification,
    fatal_failure: bool,
) -> Option<TestExpectation> {
    if fatal_failure || !entry_has_expected_failure(entry, target) {
        return None;
    }
    if is_xfail_failure(classification) {
        Some(TestExpectation::Xfail)
    } else if classification.verdict.is_pass() {
        Some(TestExpectation::Xpass)
    } else {
        None
    }
}

/// Whether this test has an expected-failure marker for the target executing
/// the image. The inventory retains all platform-scoped markers so listings
/// remain host-independent; execution is the boundary where one applies.
fn entry_has_expected_failure(entry: &TestInventoryEntry, target: Target) -> bool {
    entry.expected_failures.iter().any(|marker| {
        marker
            .platform
            .as_deref()
            .is_none_or(|platform| platform == target.name())
    })
}

/// Only ordinary test failures are suppressible by an expected-failure marker.
/// Runner notes, incomplete dispatches, output overflow, timeouts, and crashes
/// remain infrastructure failures and must still fail the run visibly.
fn is_xfail_failure(classification: &verdict::Classification) -> bool {
    classification.runner_note.is_none()
        && matches!(
            classification.verdict,
            Verdict::Fail(FailureKind::Assert)
                | Verdict::Fail(FailureKind::AssertEq)
                | Verdict::Fail(FailureKind::AssertNe)
                | Verdict::Fail(FailureKind::Trap(_))
                | Verdict::Fail(FailureKind::UnhandledError)
                | Verdict::Fail(FailureKind::Reported(_))
                | Verdict::Fail(FailureKind::Exit)
                | Verdict::CompileError
        )
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
    expectation: Option<TestExpectation>,
    repro: &Repro<'_>,
    seed: u64,
    timeout: Duration,
) -> Event {
    let verdict = execution.classification.verdict.clone();
    let passed = verdict.is_pass() && expectation.is_none();
    if passed {
        let _ = std::fs::remove_dir_all(&execution.scratch_dir);
    }
    let failure =
        (!verdict.is_pass()).then(|| failure_record(entry, &verdict, &execution, timeout));
    Event::TestFinished(Box::new(TestFinished {
        id: entry.id.clone(),
        verdict,
        expectation,
        duration_ms: u64::try_from(execution.duration.as_millis()).unwrap_or(u64::MAX),
        failure,
        stdout: Capture::new(execution.stdout, execution.stdout_total, passed),
        stderr: Capture::new(execution.stderr, execution.stderr_total, passed),
        scratch_dir: (!passed).then(|| execution.scratch_dir.display().to_string()),
        repro: repro.argv(&entry.id, seed),
        repro_env: repro.env.to_vec(),
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
        Verdict::CompileError => {
            unreachable!("a compile_error builds its own record from the diagnostics")
        }
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
        diagnostics: None,
    }
}

/// The `test_finished` event of a test that was never run because its closure
/// failed to analyze (ADR-0083 §3).
///
/// It carries the same shape every other verdict does — a duration, a
/// capability summary, capture records, and the argv that reproduces it — so a
/// consumer branches on `verdict` and nothing else. The duration is zero and
/// the captures are empty because no process existed; there is no scratch
/// directory for the same reason. The repro is still the run that would show
/// the diagnostics again.
fn compile_error_event(
    entry: &TestInventoryEntry,
    verdict: &CompileErrorVerdict,
    expectation: Option<TestExpectation>,
    repro: &Repro<'_>,
    seed: u64,
) -> Event {
    Event::TestFinished(Box::new(TestFinished {
        id: entry.id.clone(),
        verdict: Verdict::CompileError,
        expectation,
        duration_ms: 0,
        failure: Some(FailureRecord {
            kind: FailureKind::CompileError.to_string(),
            message: verdict.message.clone(),
            exit_code: None,
            signal: None,
            // The first diagnostic's own site, falling back to the test
            // declaration's header the way every other failure does: a
            // diagnostic with no location in the user's program (a panic, an
            // output-publication failure) still has to name the test.
            location: Some(verdict.location.clone().unwrap_or_else(|| Location {
                file: entry.file.clone(),
                line: entry.line,
                column: entry.column,
            })),
            payload: (!verdict.payload.is_empty()).then(|| verdict.payload.clone()),
            comparison: None,
            runner_note: None,
            diagnostics: Some(verdict.diagnostics.clone()),
        }),
        stdout: Capture::new(Vec::new(), 0, false),
        stderr: Capture::new(Vec::new(), 0, false),
        scratch_dir: None,
        repro: repro.argv(&entry.id, seed),
        repro_env: repro.env.to_vec(),
    }))
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
        FailureKind::OutputOverflow(overflow) => overflow.describe(),
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
        FailureKind::CompileError => {
            unreachable!("a compile_error message is the first diagnostic's, not a process's")
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

/// Everything a reproduction of one test is assembled from (ADR-0083 §3).
///
/// The whole point of the promise is a line that runs without thought, so the
/// program and the root are already absolute here and the environment that
/// decided the run travels alongside the argv (RUE-2020).
struct Repro<'a> {
    program: &'a str,
    root: &'a str,
    flags: &'a [String],
    env: &'a [(String, String)],
}

impl Repro<'_> {
    /// The argv that reproduces exactly this one test.
    ///
    /// It selects by the full stable ID, never the bare name: two modules may
    /// declare tests with the same name, and a repro that re-runs both is not
    /// a repro. The seed travels with it so a shuffle-dependent failure comes
    /// back.
    fn argv(&self, id: &str, seed: u64) -> Vec<String> {
        let mut argv = vec![
            self.program.to_owned(),
            "test".to_owned(),
            self.root.to_owned(),
            "--filter".to_owned(),
            id.to_owned(),
            // A stable ID can be a prefix of another stable ID. Keep the
            // runner's exact selector mode on the published argv so this
            // reproduction names precisely the failing test.
            "--exact".to_owned(),
            "--seed".to_owned(),
            seed.to_string(),
        ];
        argv.extend(self.flags.iter().cloned());
        argv
    }
}

/// The running compiler, as a path another shell can execute.
///
/// `rue` is rarely on `PATH` while a compiler is being worked on, and the
/// binary a repro must re-run is *this* one, not whichever the reader's
/// installation would resolve. This one is an installed artifact rather than a
/// project input, so it is canonicalized: the identity that matters is the file
/// on disk, and a launcher symlink may be replaced under it. The literal `rue`
/// remains the fallback for a platform that cannot answer `current_exe` at all
/// — a bare name is a worse repro than an absolute path, but a better one than
/// nothing.
fn repro_program() -> String {
    match std::env::current_exe() {
        Ok(exe) => canonical_spelling(&exe),
        Err(_) => "rue".to_owned(),
    }
}

/// A project input spelled so it resolves from any working directory, and
/// names the same project it named here.
///
/// Lexical and cwd-joined, never canonicalized. The compiler's project root
/// and module identities are lexical — `source_loader` takes the root's parent
/// from `normalize_lexical_path` and "retains the lexical project spelling for
/// durable caller identities" — so resolving a symlinked root file to its
/// target hands the repro a *different* `@import` closure. A run rooted at
/// `link/main.rue -> real/main.rue` selects `link/tests.rue`, and a repro
/// naming `real/main.rue` selects nothing at all (RUE-2020).
///
/// `std::path::absolute` is exactly that operation: absolute, symlink- and
/// `..`-preserving, resolving only against the current directory.
pub(crate) fn absolute_spelling(path: &Path) -> String {
    match std::path::absolute(path) {
        Ok(absolute) => absolute.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// Anchor a project input at the request cwd without canonicalizing it.
/// Symlink and `..` routes are part of a source repro's meaning.
pub(crate) fn absolute_spelling_at(path: &Path, working_directory: &Path) -> String {
    if path.is_absolute() {
        path.display().to_string()
    } else {
        working_directory.join(path).display().to_string()
    }
}

/// An installed artifact spelled by the identity the filesystem gives it.
///
/// The counterpart to [`absolute_spelling`], for paths that are toolchain
/// locations rather than project inputs: the compiler binary, and the
/// standard-library root, which `source_loader::capture_std_root`
/// canonicalizes for the same reason — on macOS a `/var/folders` prefix is a
/// symlink to `/private/var/folders`, and only the resolved spelling compares
/// against the paths discovery actually reads. Falls back to the lexical
/// absolute spelling when the filesystem cannot answer, which is the
/// missing-or-unreadable-toolchain case.
fn canonical_spelling(path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(canonical) => canonical.display().to_string(),
        Err(_) => absolute_spelling(path),
    }
}

/// The standard-library root as a repro must name it.
///
/// Exposed for the driver, which captures `RUE_STD_PATH` and owns the empty
/// spelling's meaning.
pub(crate) fn std_root_spelling(path: &Path) -> String {
    canonical_spelling(path)
}

/// Render the unimported-test-file warnings and collect them for the event.
///
/// Returns `Err(())` when the report itself failed; its diagnostics have
/// already been presented.
fn report_unimported(
    report: Option<
        &Result<Vec<rue_compiler::unstable::UnimportedTestFile>, rue_compiler::CompileErrors>,
    >,
    diagnostics: &crate::DiagnosticOutput<'_>,
) -> Result<Option<Vec<UnimportedFile>>, ()> {
    let Some(report) = report else {
        return Ok(None);
    };
    let files = match report {
        Ok(files) => files,
        Err(errors) => {
            diagnostics.print_prepared_errors(errors);
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
            .iter()
            .map(|file| UnimportedFile {
                path: file.path.clone(),
                tests: file.tests,
                parse_failed: file.parse_failed,
            })
            .collect(),
    ))
}

fn diagnostics_for_snapshot(
    format: crate::ErrorFormat,
    snapshot: &rue_compiler::SourceSnapshot,
) -> crate::DiagnosticOutput<'_> {
    let sources = snapshot
        .files()
        .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
        .collect();
    crate::DiagnosticOutput::new(format, sources)
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
    /// compile flags, so re-running it lands on the same one test. The program
    /// and the root are the absolute spellings the run resolved, so the line
    /// runs from any directory (RUE-2020).
    #[test]
    fn a_repro_argv_names_the_program_the_root_the_stable_id_and_the_flags() {
        let flags = [
            "--target".to_owned(),
            "x86-64-linux".to_owned(),
            "-O1".to_owned(),
            "--preview".to_owned(),
            "test_infra".to_owned(),
            "--timeout-ms".to_owned(),
            "500".to_owned(),
        ];
        let repro = Repro {
            program: "/opt/rue/bin/rue",
            root: "/work/app/main.rue",
            flags: &flags,
            env: &[],
        };
        assert_eq!(
            repro.argv("app/t.rue::parses a port", 417),
            vec![
                "/opt/rue/bin/rue",
                "test",
                "/work/app/main.rue",
                "--filter",
                "app/t.rue::parses a port",
                "--exact",
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

    /// A relative spelling is resolved against the current directory rather
    /// than repeated, because a repro is pasted somewhere else.
    #[test]
    fn an_absolute_spelling_resolves_from_any_directory() {
        let directory = std::env::current_dir().expect("a test process has a working directory");
        let resolved = absolute_spelling(Path::new("no/such/file.rue"));
        assert_eq!(
            resolved,
            directory.join("no/such/file.rue").display().to_string()
        );
        assert!(Path::new(&resolved).is_absolute());

        // An absolute path that does not exist is already its own answer.
        assert_eq!(
            absolute_spelling(Path::new("/no/such/root.rue")),
            "/no/such/root.rue"
        );

        // `..` is preserved rather than resolved: this is a lexical operation,
        // and the compiler's own project identity is lexical too.
        assert_eq!(
            absolute_spelling(Path::new("/a/b/../c/root.rue")),
            "/a/b/../c/root.rue"
        );
    }

    /// The root keeps the spelling the run resolved it by, symlink and all.
    ///
    /// Canonicalizing it would resolve a symlinked root FILE to its target,
    /// whose directory is a different project root with a different `@import`
    /// closure — the run selects `link/tests.rue` and the repro would select
    /// `real/tests.rue`, which is to say nothing at all (RUE-2020).
    #[test]
    fn a_symlinked_root_keeps_its_own_spelling() {
        let temporary = tempfile::tempdir().expect("a temp directory");
        let real = temporary.path().join("real");
        let link = temporary.path().join("link");
        std::fs::create_dir_all(&real).expect("create real/");
        std::fs::create_dir_all(&link).expect("create link/");
        std::fs::write(real.join("main.rue"), "fn main() -> i32 { 0 }\n").expect("write root");
        let linked_root = link.join("main.rue");
        std::os::unix::fs::symlink(real.join("main.rue"), &linked_root).expect("symlink the root");

        // The link is a real, resolvable file, so a canonicalizing spelling
        // would silently answer with the target's directory.
        assert_eq!(
            absolute_spelling(&linked_root),
            linked_root.display().to_string()
        );
        assert_ne!(
            absolute_spelling(&linked_root),
            canonical_spelling(&linked_root)
        );
        assert_eq!(
            canonical_spelling(&linked_root),
            std::fs::canonicalize(real.join("main.rue"))
                .expect("the target canonicalizes")
                .display()
                .to_string()
        );
    }

    /// `argv[0]` is this compiler, by absolute path: `rue` is rarely on `PATH`
    /// while one is being worked on. The binary is an installed artifact
    /// rather than a project input, so it is canonicalized.
    #[test]
    fn the_repro_program_is_an_absolute_path_to_the_running_binary() {
        let program = repro_program();
        assert!(
            Path::new(&program).is_absolute(),
            "expected an absolute program path, got {program}"
        );
        assert_eq!(
            program,
            canonical_spelling(&std::env::current_exe().expect("this platform has a current_exe"))
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

    #[test]
    fn expected_failure_markers_apply_only_to_their_target() {
        let entry = TestInventoryEntry {
            id: "app/t.rue::marked".to_owned(),
            module: "app/t.rue".to_owned(),
            name: "marked".to_owned(),
            file: "app/t.rue".to_owned(),
            line: 1,
            column: 1,
            ordinal: 0,
            expected_failures: vec![rue_compiler::unstable::TestExpectedFailure {
                issue: "RUE-123".to_owned(),
                platform: Some("aarch64-macos".to_owned()),
            }],
        };
        assert!(entry_has_expected_failure(&entry, Target::Aarch64Macos));
        assert!(!entry_has_expected_failure(&entry, Target::X86_64Linux));
        let observed = verdict::Classification {
            verdict: Verdict::Fail(FailureKind::Assert),
            runner_note: None,
        };
        assert_eq!(
            classify_expected(&entry, Target::Aarch64Macos, &observed, false),
            Some(TestExpectation::Xfail)
        );
        // A failure frame can outrank a signal in the observation classifier;
        // it must not make that signal eligible for expected-failure handling.
        assert_eq!(
            classify_expected(&entry, Target::Aarch64Macos, &observed, true),
            None
        );
    }

    #[test]
    fn compile_error_markers_cannot_hide_internal_or_environmental_errors() {
        use rue_error::{CompileError, CompileErrors, ErrorKind};
        let ordinary = CompileError::without_span(ErrorKind::ParseError("bad body".into()));
        assert!(compile_errors_allow_xfail(&CompileErrors::from(
            ordinary.clone()
        )));
        for kind in [
            ErrorKind::InternalError("ICE".into()),
            ErrorKind::InternalCodegenError("ICE".into()),
            ErrorKind::CompilerProducerInvariant("ICE".into()),
            ErrorKind::CompilerResourceExhaustion("allocation".into()),
            ErrorKind::OutputPublication("write".into()),
            ErrorKind::InvalidCompilerInput("input".into()),
            ErrorKind::UnsatisfiedTrustedToolchainInput("std".into()),
            ErrorKind::StdLibNotFound,
            ErrorKind::LinkError("link".into()),
            ErrorKind::UnsupportedTarget("target".into()),
        ] {
            let mut errors = CompileErrors::from(ordinary.clone());
            errors.push(CompileError::without_span(kind));
            assert!(!compile_errors_allow_xfail(&errors), "{errors:?}");
        }
    }

    #[test]
    fn expected_failures_do_not_hide_runner_failures() {
        let ordinary = verdict::Classification {
            verdict: Verdict::Fail(FailureKind::Assert),
            runner_note: None,
        };
        assert!(is_xfail_failure(&ordinary));
        assert!(!is_xfail_failure(&verdict::Classification {
            verdict: Verdict::Fail(FailureKind::Incomplete),
            runner_note: None,
        }));
        assert!(!is_xfail_failure(&verdict::Classification {
            verdict: Verdict::Fail(FailureKind::Exit),
            runner_note: Some("malformed channel".to_owned()),
        }));
    }

    #[test]
    fn expected_failures_do_not_hide_overflow_beside_a_complete_frame() {
        let frames = verdict::ChannelFrames {
            failure: Some(verdict::FailureFrame {
                kind: "assert".to_owned(),
                ..verdict::FailureFrame::default()
            }),
            ..verdict::ChannelFrames::default()
        };
        let classification = verdict::classify(verdict::Observation {
            supervision: verdict::Supervision::OutputOverflow(verdict::Overflow {
                stream: verdict::CaptureStream::Stderr,
                budget: 1024,
            }),
            status: Ok(101),
            stderr: b"assertion failed",
            frames: &frames,
        });
        assert_eq!(classification.verdict, Verdict::Fail(FailureKind::Assert));
        assert!(!is_xfail_failure(&classification));
    }

    #[test]
    fn owned_listing_survives_host_drop_with_partial_failures() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("main.rue");
        std::fs::write(
            &root,
            "test \"broken\" { let value: i32 = true; let _ = value; }\n\
             test \"fine\" { let _value = 1; }\n",
        )
        .unwrap();
        let context =
            rue_driver::HostPathContext::from_working_directory(directory.path()).unwrap();
        let mut host = rue_driver::FilesystemCompilerHost::open(rue_driver::HostOpenRequest {
            root_source: "main.rue",
            source_manifest_path: None,
            std_root: None,
            compiler_config: rue_compiler::CompilerSessionConfig::with_workers(1).unwrap(),
            path_context: &context,
        })
        .unwrap();
        let response = produce_listing(
            &mut host,
            &CompileOptions {
                root_selection: rue_compiler::RootSelection::Tests,
                ..CompileOptions::default()
            },
            crate::ErrorFormat::Json,
        );
        let Ok(listing) = &response.result else {
            panic!("listing should preserve surviving declarations");
        };
        assert_eq!(listing.inventory.entries.len(), 2);
        assert!(!listing.failure_diagnostics.is_empty());
        std::fs::write(&root, "test \"replacement\" { let _value = 2; }\n").unwrap();
        host.reobserve().unwrap();
        drop(host);
        let reporter = Reporter::new(OutputFormat::Json, render::Context::default());
        assert_eq!(
            complete_listing(response, &TestOptions::default(), &reporter),
            TestExitCode::AllPassed
        );
    }
}
