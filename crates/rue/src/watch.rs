use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rue_compiler::unstable::{CompilationCancellation, SourceInfo};
use rue_compiler::{CompileOptions, OptLevel};
#[cfg(test)]
use rue_driver::watch_inputs_changed_with_reader;
use rue_driver::{
    FilesystemCompilerHost, SourceLoadError, WatchFingerprint, WatchInput, watch_inputs_changed,
};
use rue_target::Target;

use crate::compile::{
    Announcement, CycleObservation, CycleReport, CycleRequest, Supersession, drive_cycle,
};
use crate::test_mode;
use crate::{DiagnosticOutput, ErrorFormat, render_source_load_error};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(250);
const QUIET_PERIOD: Duration = Duration::from_millis(75);
const FAILED_REOBSERVE_RETRY: Duration = Duration::from_millis(250);

// Test-only watch protocol. When RUE_WATCH_TEST_PROTOCOL names a file, the
// loop appends one milestone per line. It is intentionally dormant unless the
// CLI integration harness opts in; production users never pay for the file
// opens or the optional delay. The protocol gives end-to-end tests stable
// synchronization without wall-clock sleeps.
const TEST_PROTOCOL_ENV: &str = "RUE_WATCH_TEST_PROTOCOL";
const TEST_COMPILE_DELAY_ENV: &str = "RUE_WATCH_TEST_COMPILE_DELAY_MS";
const TEST_ACQUIRE_DELAY_ENV: &str = "RUE_WATCH_TEST_ACQUIRE_DELAY_MS";
const TEST_BOUNDARY_DELAY_ENV: &str = "RUE_WATCH_TEST_BOUNDARY_DELAY_MS";

pub(crate) fn test_event(event: &str) {
    let Some(path) = std::env::var_os(TEST_PROTOCOL_ENV) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    // One `write(2)`, not two. `writeln!` on an unbuffered `File` issues the
    // payload and the newline separately, and two writers can interleave
    // between them: `test-spawned` is emitted from the run's worker threads
    // rather than from this loop, so under `--jobs N` that produced
    // concatenated lines and a harness undercount. A single append-mode write
    // of this size is atomic (RUE-2023).
    let line = format!("{event}\n");
    let _ = file.write_all(line.as_bytes());
    let _ = file.flush();
}

fn test_compile_delay() {
    let Ok(delay) = std::env::var(TEST_COMPILE_DELAY_ENV) else {
        return;
    };
    let Ok(milliseconds) = delay.parse::<u64>() else {
        return;
    };
    thread::sleep(Duration::from_millis(milliseconds.min(5_000)));
}

// Hold the cycle between re-observation and reached-toolchain acquisition, so
// an edit can deterministically land while acquisition is reading demanded
// modules or re-closing (RUE-1863). Test-only, like the delays above.
fn test_acquire_delay() {
    let Ok(delay) = std::env::var(TEST_ACQUIRE_DELAY_ENV) else {
        return;
    };
    let Ok(milliseconds) = delay.parse::<u64>() else {
        return;
    };
    thread::sleep(Duration::from_millis(milliseconds.min(5_000)));
}

// Widen the unobserved gap between the change monitor stopping and the
// trailing input check. Test-only, like the compile delay above: it exists so
// an edit can be made to land inside that window on purpose.
fn test_boundary_delay() {
    let Ok(delay) = std::env::var(TEST_BOUNDARY_DELAY_ENV) else {
        return;
    };
    let Ok(milliseconds) = delay.parse::<u64>() else {
        return;
    };
    thread::sleep(Duration::from_millis(milliseconds.min(5_000)));
}

/// Which half of a re-observation cycle is running. One `ChangeMonitor` spans
/// both, so the phase — not the monitor — decides which protocol event a
/// supersession or failure reports (RUE-1863).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ObservationPhase {
    Reobserve,
    Acquire,
}

impl ObservationPhase {
    fn superseded_event(self) -> &'static str {
        match self {
            ObservationPhase::Reobserve => "reobserve-superseded",
            ObservationPhase::Acquire => "acquire-superseded",
        }
    }

    /// The milestone for a failure this cycle actually reported to the user.
    fn error_event(self) -> &'static str {
        match self {
            ObservationPhase::Reobserve => "reobserve-error",
            ObservationPhase::Acquire => "acquire-error",
        }
    }

    /// The milestone for a retry that hit the failure already on screen and
    /// said nothing. The retry itself is not silent to the protocol — a
    /// harness can still count attempts — but the user's terminal is
    /// (RUE-2091).
    fn repeated_error_event(self) -> &'static str {
        match self {
            ObservationPhase::Reobserve => "reobserve-error-repeat",
            ObservationPhase::Acquire => "acquire-error-repeat",
        }
    }
}

/// The re-observation failure whose diagnostic is already on the user's
/// terminal.
///
/// A failed re-observation cannot simply wait for an edit: the failure may be
/// an import naming a file that does not exist yet, which is outside the
/// retained closure and so outside anything there is to watch. The loop
/// therefore retries on a timer, and before RUE-2091 each tick reprinted the
/// whole diagnostic — roughly four identical reports per second for as long as
/// a person sat inside a syntax error.
///
/// Two things make a failure worth reporting again, and a bare timer is
/// neither of them:
///
/// - **The message changed.** Identity is the rendered diagnostic byte for
///   byte, which is what the user reads. A different code, a different file, or
///   the same error a line further down is a different report, because the
///   thing it tells someone to look at moved. Anything less than the whole
///   rendering risks swallowing a message the previous one did not contain.
/// - **The source changed.** A revision that still fails identically is worth
///   one line back, because silence after a save is ambiguous: it looks the
///   same as a watcher that never noticed the file. `observation` is the
///   physical state of the retained closure this attempt read, so an edit
///   anywhere in it re-reports even when the compiler's answer is unchanged.
///
/// A clock carries neither signal, so there is no time-based reprint: a
/// stuck-and-unedited watcher stays quiet indefinitely, which is the whole
/// point.
struct ReportedFailure {
    diagnostic: String,
    observation: Vec<WatchObservation>,
}

pub(crate) struct WatchRequest {
    pub(crate) host: FilesystemCompilerHost,
    pub(crate) compile_options: CompileOptions,
    pub(crate) source_path: String,
    pub(crate) error_format: ErrorFormat,
    pub(crate) mode: WatchMode,
}

/// What one accepted source revision produces.
///
/// The loop itself — the change monitor, re-observation, acquisition,
/// cancellation, debouncing, and the cycle boundary — is the same either way;
/// only the cycle body differs, which is what lets `rue test --watch` reuse the
/// retained host instead of paying a fresh process and a full compile per run
/// (RUE-2023).
pub(crate) enum WatchMode {
    /// Publish the user's executable at this path.
    Executable { output_path: String },
    /// Build the request's test image and run its tests.
    Test(Box<TestWatch>),
}

/// The test-mode configuration a watch cycle repeats verbatim.
pub(crate) struct TestWatch {
    pub(crate) options: test_mode::TestOptions,
    pub(crate) repro_flags: Vec<String>,
    pub(crate) repro_env: Vec<(String, String)>,
    pub(crate) jobs: usize,
    pub(crate) target: Target,
    pub(crate) opt_level: OptLevel,
    /// `--test-candidates`, re-read per cycle: the declared list and the files
    /// it names are both ordinary disk state, and a cycle reports what is on
    /// disk now.
    pub(crate) test_candidates_path: Option<String>,
    /// Derived once for the whole process unless `--seed` was given, so
    /// consecutive cycles shuffle the same way and a difference between two of
    /// them is attributable to the edit rather than to the order.
    pub(crate) seed: u64,
}

/// Everything one cycle is canceled through.
///
/// A compile is canceled cooperatively inside the query graph; a run is
/// canceled by killing the process groups it is supervising. One edit ends
/// both, so the change monitor holds both and the two can never disagree about
/// whether this cycle is still wanted (RUE-2023).
#[derive(Clone)]
struct CycleCancellation {
    compilation: CompilationCancellation,
    run: Option<test_mode::RunCancellation>,
}

impl CycleCancellation {
    fn cancel(&self) {
        self.compilation.cancel();
        if let Some(run) = &self.run {
            run.cancel();
        }
    }
}

/// What one cycle did, in the vocabulary the loop's boundary logic needs.
enum CycleStatus {
    /// The cycle produced its artifact: an executable at the output path, or a
    /// completed test run.
    Completed,
    /// The cycle was rejected. Its diagnostics are already out and the loop
    /// keeps watching.
    Failed,
    Superseded(Supersession),
    Canceled,
}

struct ChangeMonitor {
    stop: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchObservation {
    requested_route: Vec<WatchSymlinkObservation>,
    requested_canonical: Option<PathBuf>,
    requested_fingerprint: Option<WatchFingerprint>,
    canonical_fingerprint: Option<WatchFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchSymlinkObservation {
    path: PathBuf,
    target: PathBuf,
    identity: Option<(u64, u64)>,
}

impl ChangeMonitor {
    fn start(inputs: Vec<WatchInput>, cancellation: CycleCancellation) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_changed = changed.clone();
        let thread = thread::spawn(move || {
            let mut poll = PollBackoff::new();
            while !thread_stop.load(Ordering::Acquire) {
                if inputs_changed(&inputs) {
                    thread_changed.store(true, Ordering::Release);
                    test_event("change-detected");
                    cancellation.cancel();
                    return;
                }
                thread::park_timeout(poll.next_delay());
            }
        });
        Self {
            stop,
            changed,
            thread,
        }
    }

    /// Monitor for edits landing after `baseline` — the debounced disk state
    /// this cycle's re-observation is about to read (RUE-1830). The compile
    /// monitor's comparison against the last committed observation would trip
    /// immediately here: while re-observing, the pending edit IS the reason
    /// the cycle runs, so only a post-baseline edit supersedes the attempt.
    /// The thread stays silent on the test protocol; the superseded cycle
    /// announces itself through its own milestone.
    fn start_reobservation(
        inputs: Vec<WatchInput>,
        cancellation: CompilationCancellation,
    ) -> (Self, Vec<WatchObservation>) {
        let baseline = current_observations(&inputs);
        let thread_baseline = baseline.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_changed = changed.clone();
        let thread = thread::spawn(move || {
            let mut poll = PollBackoff::new();
            while !thread_stop.load(Ordering::Acquire) {
                if current_observations(&inputs) != thread_baseline {
                    thread_changed.store(true, Ordering::Release);
                    cancellation.cancel();
                    return;
                }
                thread::park_timeout(poll.next_delay());
            }
        });
        (
            Self {
                stop,
                changed,
                thread,
            },
            baseline,
        )
    }

    fn changed(&self) -> bool {
        self.changed.load(Ordering::Acquire)
    }

    fn finish(self) -> bool {
        self.stop.store(true, Ordering::Release);
        // Idle polling backs off, but completing a compile must never wait for
        // the monitor's current timeout to expire.
        self.thread.thread().unpark();
        self.thread
            .join()
            .expect("watch change monitor thread panicked");
        self.changed.load(Ordering::Acquire)
    }
}

pub(crate) fn run(request: WatchRequest) -> ! {
    let WatchRequest {
        mut host,
        compile_options,
        source_path,
        error_format,
        mut mode,
    } = request;
    let mut needs_reobserve = false;
    let mut cycle: u64 = 0;
    let mut reported_failure: Option<ReportedFailure> = None;

    if let WatchMode::Test(_) = &mode {
        // Once for the process, before anything can spawn: descriptor 3 is
        // pinned shut so no pipe the standard library opens for its own
        // bookkeeping can be allocated there and then destroyed by a child's
        // `dup2` onto the failure channel (`exec::reserve_channel_descriptor`).
        // Per cycle would be pointless — the reservation is idempotent and
        // never released — and per spawn would be a race.
        test_mode::reserve_channel_descriptor();
        // A watch process has no natural end, so being asked to stop IS its
        // result: it reports the last completed cycle's status rather than
        // dying of the signal a one-shot run dies of (ADR-0083 §2).
        test_mode::install_watch_signal_exit();
    }

    match &mode {
        // stdout is the event stream in test mode, so the loop's own voice
        // goes where every other runner notice goes.
        WatchMode::Test(_) => eprintln!("Watching {source_path} for changes"),
        WatchMode::Executable { .. } => println!("Watching {source_path} for changes"),
    }
    test_event("ready");

    loop {
        let cycle_started = Instant::now();
        if needs_reobserve {
            // Observe edits WHILE the cycle re-observes AND acquires: both
            // halves block on filesystem reads across several waves, and an
            // edit landing in either must supersede the stale attempt promptly
            // instead of waiting for compilation proper to notice it
            // (RUE-1830, RUE-1863). One monitor spans both so the window has
            // no unobserved seam between them.
            let stale_inputs = host.watch_inputs();
            let cancellation = CompilationCancellation::new();
            let (monitor, observation_baseline) =
                ChangeMonitor::start_reobservation(stale_inputs.clone(), cancellation.clone());
            let superseded = || cancellation.is_canceled();
            test_event("reobserve-started");
            let mut phase = ObservationPhase::Reobserve;
            let mut observed = host.reobserve_superseding(&superseded);
            if observed.is_ok() {
                test_event("reobserve-ok");
                phase = ObservationPhase::Acquire;
                test_acquire_delay();
                observed = host
                    .acquire_reached_toolchain_modules_superseding(&compile_options, &superseded);
            }
            let monitor_changed = monitor.finish();
            let changed =
                monitor_changed || current_observations(&stale_inputs) != observation_baseline;
            if changed || matches!(&observed, Err(SourceLoadError::Superseded)) {
                test_event(phase.superseded_event());
                print_cycle_status(
                    &mode,
                    error_format,
                    format!(
                        "Watch re-observation superseded after {} ms; a newer source revision is available",
                        cycle_started.elapsed().as_millis()
                    ),
                );
                // Each phase commits either nothing or one coherent close. Let
                // the burst settle, then re-observe the exact physical routes
                // from the newest bytes; `needs_reobserve` remains set.
                debounce(&stale_inputs);
                continue;
            }
            match observed {
                Ok(()) => {
                    test_event("acquire-ok");
                    // A revision that loads again ends the failure state. The
                    // next failure is news even if it renders exactly like the
                    // last one did — reverting to previously broken bytes must
                    // report, and the fingerprints alone cannot tell that
                    // revert from a retry, because they are content hashes.
                    reported_failure = None;
                }
                Err(SourceLoadError::Superseded) => {
                    unreachable!("supersession was handled before source errors")
                }
                Err(error) => {
                    let diagnostic = render_source_load_error(error, error_format);
                    let repeat = reported_failure.as_ref().is_some_and(|reported| {
                        reported.diagnostic == diagnostic
                            && reported.observation == observation_baseline
                    });
                    if repeat {
                        test_event(phase.repeated_error_event());
                    } else {
                        test_event(phase.error_event());
                        // Diagnostics are stderr's in both formats.
                        eprintln!("{diagnostic}");
                        print_cycle_status(
                            &mode,
                            error_format,
                            format!(
                                "Watch cycle failed after {} ms; {}",
                                cycle_started.elapsed().as_millis(),
                                failure_consequence(&mode)
                            ),
                        );
                        reported_failure = Some(ReportedFailure {
                            diagnostic,
                            observation: observation_baseline,
                        });
                    }
                    thread::sleep(FAILED_REOBSERVE_RETRY);
                    continue;
                }
            }
        }

        let inputs = host.watch_inputs();
        let source_snapshot = host.source_snapshot().clone();
        let source_infos = source_snapshot
            .files()
            .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
            .collect();
        let diagnostics = DiagnosticOutput::new(error_format, source_infos);

        let cancellation = CompilationCancellation::new();
        // A test cycle stays abandonable past its compile: one edit cancels
        // whichever half of the cycle it lands in, so the monitor carries the
        // authority to kill this cycle's running tests alongside the one that
        // cancels its compilation (RUE-2023). An executable cycle has no
        // execution phase and so carries none.
        let run_cancellation =
            matches!(mode, WatchMode::Test(_)).then(test_mode::RunCancellation::new);
        let monitor = ChangeMonitor::start(
            inputs.clone(),
            CycleCancellation {
                compilation: cancellation.clone(),
                run: run_cancellation.clone(),
            },
        );
        test_event("compile-started");
        test_compile_delay();
        // A cycle is superseded once the monitor has seen an edit, or once a
        // fresh read of the closure disagrees with what this cycle observed.
        let superseded = || monitor.changed() || inputs_changed(&inputs);
        let observation = CycleObservation::Watch {
            inputs: inputs.clone(),
            cancellation,
            superseded: &superseded,
        };
        let status = match &mut mode {
            WatchMode::Executable { output_path } => executable_status(drive_cycle(CycleRequest {
                host: &mut host,
                options: &compile_options,
                diagnostics: &diagnostics,
                source_path: &source_path,
                output_path,
                observation,
                announcement: Announcement::Cycle(cycle_started),
            })),
            WatchMode::Test(config) => {
                cycle += 1;
                test_status(test_watch_cycle(TestWatchCycle {
                    host: &mut host,
                    compile_options: &compile_options,
                    config,
                    diagnostics: &diagnostics,
                    source_path: &source_path,
                    cycle,
                    cancellation: run_cancellation
                        .as_ref()
                        .expect("a test cycle always holds a run cancellation"),
                    observation,
                }))
            }
        };

        let mut publication_changed = false;
        match status {
            CycleStatus::Completed => {
                // The milestone names the artifact reaching disk, which both
                // modes do: an executable at the output path, a test image in
                // the cycle's own run directory. A test cycle's run milestones
                // are `test_mode`'s and were emitted inside it.
                test_event("published");
                announce_watching(&mode);
            }
            CycleStatus::Superseded(boundary) => {
                // The compile monitor and the publication guard both refuse to
                // publish a stale revision; the milestone says which of them
                // caught it, and only the publication guard's answer feeds the
                // cycle-boundary decision below.
                publication_changed = matches!(boundary, Supersession::AtPublication);
                test_event(match boundary {
                    Supersession::AtPublication => "canceled-at-publication",
                    Supersession::BeforePublication => "canceled-before-publication",
                });
                print_cycle_status(
                    &mode,
                    error_format,
                    format!(
                        "Watch cycle canceled after {} ms; a newer source revision is available",
                        cycle_started.elapsed().as_millis()
                    ),
                );
            }
            CycleStatus::Canceled => {
                test_event("canceled");
                print_cycle_status(
                    &mode,
                    error_format,
                    format!(
                        "Watch cycle canceled after {} ms",
                        cycle_started.elapsed().as_millis()
                    ),
                );
            }
            CycleStatus::Failed => {
                test_event("compile-error");
                print_cycle_status(
                    &mode,
                    error_format,
                    format!(
                        "Watch cycle failed after {} ms; {}",
                        cycle_started.elapsed().as_millis(),
                        failure_consequence(&mode)
                    ),
                );
                // A failed cycle ends the same way a completed one does: the
                // loop goes back to waiting, and a person is told so.
                announce_watching(&mode);
            }
        }

        let monitor_changed = monitor.finish();
        // The monitor has stopped and nothing observes the inputs again until
        // the trailing check below. That gap is where RUE-1783 lived, and it is
        // normally too short to hit deliberately -- so the harness can widen it
        // to make the race reproducible instead of hoping a loaded runner
        // supplies it.
        test_boundary_delay();
        let changed = watch_cycle_changed(
            publication_changed,
            monitor_changed,
            inputs_changed(&inputs),
        );
        match cycle_boundary_action(changed, monitor_changed) {
            CycleBoundary::Wait => wait_for_change(&inputs),
            CycleBoundary::Announce => test_event("change-detected"),
            CycleBoundary::AlreadyAnnounced => {}
        }
        debounce(&inputs);
        needs_reobserve = true;
    }
}

/// What a failed cycle leaves the user with, which is the one thing the two
/// modes' status lines disagree about.
///
/// An executable watch keeps the last executable it published on disk. A test
/// watch published nothing to keep: this revision reached no `run_finished`,
/// and whatever the last completed cycle reported is still on the stream above
/// rather than reprinted here.
///
/// "No summary" rather than "no tests ran", because the one failure that can
/// reach here after the image existed — a test the runner could not execute at
/// all — did run some. What is true of every failed cycle is that it published
/// no summary.
fn failure_consequence(mode: &WatchMode) -> &'static str {
    match mode {
        WatchMode::Executable { .. } => "keeping the last successful executable",
        WatchMode::Test(_) => "no summary for this revision",
    }
}

/// Say the loop is idle again after a cycle a person watched go by.
///
/// Said after a cycle that ENDED — completed or failed — and not after one a
/// newer revision superseded or canceled, because that loop is not idle: the
/// next cycle starts immediately.
///
/// Human format only: `--format json` delimits its cycles with `run_finished`
/// and `run_canceled`, and a consumer of that stream is owed no prose.
fn announce_watching(mode: &WatchMode) {
    if let WatchMode::Test(config) = mode
        && config.options.format == test_mode::OutputFormat::Human
    {
        eprintln!("watching…");
    }
}

fn executable_status(report: CycleReport) -> CycleStatus {
    match report {
        CycleReport::Published(_) => CycleStatus::Completed,
        CycleReport::Failed => CycleStatus::Failed,
        CycleReport::Superseded(boundary) => CycleStatus::Superseded(boundary),
        CycleReport::Canceled => CycleStatus::Canceled,
    }
}

/// Fold a test cycle's outcome into the loop's vocabulary, and publish the
/// status a later interrupt exits with.
///
/// `rue test --watch` produces an exit code only when it is asked to stop, and
/// that code is the last cycle that actually COMPLETED reporting itself: a
/// cycle that could not build its image, or that an edit abandoned, leaves the
/// previous answer standing rather than overwriting it with "the run did not
/// happen" (ADR-0083 §2, RUE-2023).
///
/// A completed cycle's own status carries through unchanged, `EmptySelection`
/// included. "Your filter matched nothing" and "tests failed" are the different
/// mistakes ADR-0083 §2 gives `3` its own code for, and a watcher stopped after
/// such a cycle owes an agent that distinction exactly as a one-shot run does.
fn test_status(outcome: test_mode::CycleOutcome) -> CycleStatus {
    match outcome {
        // Not a completed cycle: the image failed, or the runner did. This is
        // the one status that must not reach the atomic — it would claim the
        // last completed cycle never happened.
        test_mode::CycleOutcome::Finished(test_mode::TestExitCode::RunnerError) => {
            CycleStatus::Failed
        }
        test_mode::CycleOutcome::Finished(exit) => {
            test_mode::set_watch_exit_status(exit);
            CycleStatus::Completed
        }
        test_mode::CycleOutcome::Canceled => CycleStatus::Canceled,
        test_mode::CycleOutcome::Superseded(boundary) => CycleStatus::Superseded(boundary),
    }
}

/// One `rue test --watch` cycle: the declared candidate list as it stands now,
/// then the ordinary test cycle over this revision's image.
struct TestWatchCycle<'a, 'diagnostics> {
    host: &'a mut FilesystemCompilerHost,
    compile_options: &'a CompileOptions,
    config: &'a TestWatch,
    diagnostics: &'a DiagnosticOutput<'diagnostics>,
    source_path: &'a str,
    cycle: u64,
    cancellation: &'a test_mode::RunCancellation,
    observation: CycleObservation<'a>,
}

fn test_watch_cycle(request: TestWatchCycle<'_, '_>) -> test_mode::CycleOutcome {
    let TestWatchCycle {
        host,
        compile_options,
        config,
        diagnostics,
        source_path,
        cycle,
        cancellation,
        observation,
    } = request;
    // Re-read per cycle: the list is disk state and so are the files it names,
    // so a candidate added, removed, or newly broken since the last cycle is
    // reported by this one. The warning it produces is printed by the cycle
    // itself, once, in each cycle whose report is non-empty (RUE-2023).
    let candidates = match &config.test_candidates_path {
        Some(path) => match rue_driver::load_declared_candidates(path) {
            Ok(declared) => match host.acquire_test_candidates(&declared) {
                Ok(inventory) => Some(inventory),
                Err(errors) => {
                    diagnostics.print_errors(&errors);
                    return test_mode::CycleOutcome::Finished(test_mode::TestExitCode::RunnerError);
                }
            },
            Err(message) => {
                eprintln!("{message}");
                return test_mode::CycleOutcome::Finished(test_mode::TestExitCode::RunnerError);
            }
        },
        None => None,
    };
    test_mode::run_cycle(test_mode::CycleRequest {
        host,
        compile_options,
        options: &config.options,
        diagnostics,
        root: source_path,
        repro_flags: &config.repro_flags,
        repro_env: &config.repro_env,
        jobs: config.jobs,
        target: config.target,
        opt_level: config.opt_level,
        candidates: candidates.as_ref(),
        seed: config.seed,
        cycle: Some(cycle),
        cancellation: Some(cancellation),
        observation,
    })
}

/// Watch-cycle status is not a diagnostic. In JSON mode it must not share
/// stderr with the diagnostic stream, where every non-empty line is a JSON
/// array; text mode retains the established stderr wording and placement.
fn print_watch_status(error_format: ErrorFormat, message: impl std::fmt::Display) {
    match error_format {
        ErrorFormat::Text => eprintln!("{message}"),
        ErrorFormat::Json => println!("{message}"),
    }
}

/// Cycle status, on the stream this mode can spare.
///
/// An executable watch keeps the placement above. A test watch cannot: stdout
/// is the event stream there, and `--format json` promises a consumer that
/// every line of it parses. Its status goes to stderr in both formats, which is
/// where `rue test`'s other notices already go (test-events.md, "Streams").
fn print_cycle_status(
    mode: &WatchMode,
    error_format: ErrorFormat,
    message: impl std::fmt::Display,
) {
    match mode {
        WatchMode::Test(_) => eprintln!("{message}"),
        WatchMode::Executable { .. } => print_watch_status(error_format, message),
    }
}

fn wait_for_change(inputs: &[WatchInput]) {
    let mut poll = PollBackoff::new();
    loop {
        if inputs_changed(inputs) {
            test_event("change-detected");
            break;
        }
        thread::sleep(poll.next_delay());
    }
}

fn debounce(inputs: &[WatchInput]) {
    let mut previous = current_observations(inputs);
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < QUIET_PERIOD {
        thread::sleep(POLL_INTERVAL);
        let current = current_observations(inputs);
        if current != previous {
            previous = current;
            quiet_since = Instant::now();
        }
    }
}

fn inputs_changed(inputs: &[WatchInput]) -> bool {
    watch_inputs_changed(inputs)
}

fn watch_cycle_changed(
    publication_changed: bool,
    monitor_changed: bool,
    observed_changed: bool,
) -> bool {
    publication_changed || monitor_changed || observed_changed
}

/// What the cycle boundary owes the test protocol once the cycle has settled.
///
/// `change-detected` is the protocol's single "a newer revision exists"
/// milestone, but a cycle can learn that three different ways: the in-flight
/// `ChangeMonitor`, the publication guard, or the trailing `inputs_changed`
/// observation. Only the monitor announces itself.
///
/// That asymmetry was a race (RUE-1783). The milestone was emitted solely from
/// `wait_for_change`, which the loop reaches only when the cycle ends with no
/// change pending — so whether it was emitted depended on whether an edit
/// landed before or after the trailing check. An edit landing *before* it left
/// the milestone unsent while the watcher went on to rebuild and publish
/// perfectly correctly, so a harness waiting on the milestone hung against a
/// watcher that was doing its job. It reproduced on slow runners because a
/// longer compile widens the window in which the edit can land early.
///
/// Announcing here makes the milestone unconditional: every cycle that ends
/// knowing about a change reports it exactly once, whichever path found it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CycleBoundary {
    /// Nothing has changed yet; block until something does. `wait_for_change`
    /// emits the milestone itself when it returns.
    Wait,
    /// A change is already pending and nobody has announced it.
    Announce,
    /// The monitor thread announced this change when it cancelled the compile.
    AlreadyAnnounced,
}

fn cycle_boundary_action(changed: bool, monitor_changed: bool) -> CycleBoundary {
    match (changed, monitor_changed) {
        (false, _) => CycleBoundary::Wait,
        (true, true) => CycleBoundary::AlreadyAnnounced,
        (true, false) => CycleBoundary::Announce,
    }
}

/// Capture the physical state at the beginning of a retained re-observation.
///
/// The accepted `WatchInput` still names the committed closure the loader is
/// allowed to revisit, but its embedded fingerprint is necessarily stale after
/// the edit that requested this cycle. Monitoring against that fingerprint
/// would cancel every attempt immediately. This separate baseline observes the
/// current requested route and both requested/canonical bytes, so only a later
/// edit supersedes the attempt, including symlink retargets and appearances of
/// previously absent candidates.
fn current_observations(inputs: &[WatchInput]) -> Vec<WatchObservation> {
    inputs
        .iter()
        .map(|input| WatchObservation {
            requested_route: current_symlink_route(input.requested_path()),
            requested_canonical: fs::canonicalize(input.requested_path()).ok(),
            requested_fingerprint: WatchFingerprint::read(input.requested_path()),
            canonical_fingerprint: WatchFingerprint::read(input.canonical_path()),
        })
        .collect()
}

fn current_symlink_route(path: &Path) -> Vec<WatchSymlinkObservation> {
    let mut current = PathBuf::new();
    let mut route = Vec::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let Ok(target) = fs::read_link(&current) else {
            continue;
        };
        let identity = fs::symlink_metadata(&current)
            .ok()
            .and_then(|metadata| symlink_identity(&metadata));
        route.push(WatchSymlinkObservation {
            path: current.clone(),
            target,
            identity,
        });
    }
    route
}

#[cfg(unix)]
fn symlink_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn symlink_identity(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
fn inputs_changed_with_reader<F>(inputs: &[WatchInput], read: F) -> bool
where
    F: FnMut(&Path) -> Option<WatchFingerprint>,
{
    watch_inputs_changed_with_reader(inputs, read)
}

#[derive(Clone, Copy, Debug)]
struct PollBackoff {
    next: Duration,
}

impl PollBackoff {
    fn new() -> Self {
        Self {
            next: POLL_INTERVAL,
        }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self
            .next
            .checked_mul(2)
            .unwrap_or(MAX_POLL_INTERVAL)
            .min(MAX_POLL_INTERVAL);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_publication_change_latches_reobserve() {
        assert!(watch_cycle_changed(true, false, false));
        assert!(watch_cycle_changed(true, false, true));
        assert!(!watch_cycle_changed(false, false, false));
    }

    // RUE-1783. The milestone used to be emitted only from `wait_for_change`,
    // so a cycle that already knew about a change skipped it entirely and a
    // harness waiting on it hung against a correctly-working watcher.
    #[test]
    fn a_settled_cycle_waits_and_lets_wait_for_change_announce() {
        assert_eq!(
            cycle_boundary_action(false, false),
            CycleBoundary::Wait,
            "with nothing pending the loop must block, not announce"
        );
    }

    #[test]
    fn a_change_found_at_the_boundary_is_announced_exactly_once() {
        // The trailing `inputs_changed` observation and the publication guard
        // both reach here with the monitor quiet: nobody has announced yet.
        assert_eq!(
            cycle_boundary_action(true, false),
            CycleBoundary::Announce,
            "an edit that landed before the trailing check must still be reported"
        );
    }

    #[test]
    fn a_change_the_monitor_already_reported_is_not_announced_twice() {
        assert_eq!(
            cycle_boundary_action(true, true),
            CycleBoundary::AlreadyAnnounced,
            "the monitor emits the milestone when it cancels; a second is a phantom revision"
        );
    }

    // Whichever path detects it, a cycle that ends knowing about a change must
    // never fall through to `wait_for_change` -- that would block on a change
    // that has already happened, which is the hang RUE-1783 filed.
    #[test]
    fn no_pending_change_is_ever_waited_on() {
        for monitor_changed in [false, true] {
            assert_ne!(
                cycle_boundary_action(true, monitor_changed),
                CycleBoundary::Wait,
                "a known change must not send the loop back to sleep"
            );
        }
    }

    #[test]
    fn detects_content_changes_even_when_file_length_is_unchanged() {
        let path = std::env::temp_dir().join(format!(
            "rue-watch-fingerprint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, b"alpha").unwrap();
        let fingerprint = WatchFingerprint::from_bytes(b"alpha");
        let inputs = vec![WatchInput::new(path.clone(), path.clone(), fingerprint)];
        assert!(!inputs_changed(&inputs));
        fs::write(&path, b"bravo").unwrap();
        assert!(inputs_changed(&inputs));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn reobserve_baseline_accepts_the_triggering_edit_and_detects_the_next_one() {
        let path = std::env::temp_dir().join(format!(
            "rue-watch-reobserve-baseline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, b"one").unwrap();
        let inputs = vec![WatchInput::new(
            path.clone(),
            path.clone(),
            WatchFingerprint::from_bytes(b"one"),
        )];

        fs::write(&path, b"two").unwrap();
        let baseline = current_observations(&inputs);
        assert_eq!(
            current_observations(&inputs),
            baseline,
            "the edit which requested re-observation is the attempt baseline, not a new cancellation"
        );

        fs::write(&path, b"six").unwrap();
        assert_ne!(
            current_observations(&inputs),
            baseline,
            "a later same-length edit must supersede the in-flight re-observation"
        );
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reobserve_baseline_distinguishes_repeated_dangling_directory_retargets() {
        let root = std::env::temp_dir().join(format!(
            "rue-watch-reobserve-route-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        let leaf = real.join("leaf.rue");
        fs::write(&leaf, b"source").unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink("real", &alias).unwrap();
        let requested = alias.join("leaf.rue");
        let canonical = fs::canonicalize(&requested).unwrap();
        let inputs = vec![WatchInput::new(
            requested,
            canonical,
            WatchFingerprint::from_bytes(b"source"),
        )];

        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("missing-a", &alias).unwrap();
        let baseline = current_observations(&inputs);
        assert!(
            baseline[0]
                .requested_route
                .iter()
                .any(|component| component.target == Path::new("missing-a")),
            "the baseline must retain the raw dangling directory route"
        );
        assert!(baseline[0].requested_fingerprint.is_none());

        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("missing-b", &alias).unwrap();
        assert_ne!(
            current_observations(&inputs),
            baseline,
            "a second dangling directory retarget must supersede the in-flight observation"
        );

        fs::remove_file(alias).unwrap();
        fs::remove_file(leaf).unwrap();
        fs::remove_dir(real).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn detects_deleted_inputs() {
        let path = std::env::temp_dir().join(format!(
            "rue-watch-deletion-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, b"source").unwrap();
        let fingerprint = WatchFingerprint::from_bytes(b"source");
        let inputs = vec![WatchInput::new(path.clone(), path.clone(), fingerprint)];
        fs::remove_file(path).unwrap();
        assert!(inputs_changed(&inputs));
    }

    #[test]
    fn expected_absence_is_unchanged_until_candidate_appears() {
        let path = std::env::temp_dir().join(format!(
            "rue-watch-expected-absence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let inputs = vec![WatchInput::expected_absence(path.clone())];
        assert!(!inputs_changed(&inputs));
        fs::write(&path, b"new candidate").unwrap();
        assert!(inputs_changed(&inputs));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn expected_absence_changes_when_a_non_file_candidate_appears() {
        let path = std::env::temp_dir().join(format!(
            "rue-watch-expected-absence-directory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let inputs = vec![WatchInput::expected_absence(path.clone())];
        fs::create_dir(&path).unwrap();
        assert!(inputs_changed(&inputs));
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn backs_off_idle_polls_and_caps_the_delay() {
        let mut poll = PollBackoff::new();
        assert_eq!(poll.next_delay(), Duration::from_millis(25));
        assert_eq!(poll.next_delay(), Duration::from_millis(50));
        assert_eq!(poll.next_delay(), Duration::from_millis(100));
        assert_eq!(poll.next_delay(), Duration::from_millis(200));
        assert_eq!(poll.next_delay(), Duration::from_millis(250));
        assert_eq!(poll.next_delay(), Duration::from_millis(250));
    }

    #[test]
    fn a_new_activity_cycle_restarts_at_low_latency() {
        let mut idle_cycle = PollBackoff::new();
        for _ in 0..8 {
            idle_cycle.next_delay();
        }
        assert_eq!(idle_cycle.next_delay(), MAX_POLL_INTERVAL);

        let mut next_cycle = PollBackoff::new();
        assert_eq!(next_cycle.next_delay(), POLL_INTERVAL);
    }

    #[test]
    fn reads_one_physical_path_for_requested_and_canonical_aliases() {
        let root = std::env::temp_dir().join(format!(
            "rue-watch-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let canonical = root.join("source.rue");
        let requested = root.join("alias.rue");
        fs::write(&canonical, b"source").unwrap();
        let canonical = fs::canonicalize(canonical).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&canonical, &requested).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&canonical, &requested).unwrap();

        let fingerprint = WatchFingerprint::from_bytes(b"source");
        let inputs = vec![
            WatchInput::new(requested.clone(), canonical.clone(), fingerprint),
            WatchInput::new(canonical.clone(), canonical.clone(), fingerprint),
        ];
        let mut reads = 0;
        assert!(!inputs_changed_with_reader(&inputs, |path| {
            reads += 1;
            WatchFingerprint::read(path)
        }));
        assert_eq!(reads, 1);

        let other = root.join("other.rue");
        fs::write(&other, b"other").unwrap();
        fs::remove_file(requested).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, root.join("alias.rue")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&other, root.join("alias.rue")).unwrap();
        assert!(inputs_changed(&inputs));
        fs::remove_file(root.join("alias.rue")).unwrap();
        fs::remove_file(canonical).unwrap();
        fs::remove_file(other).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
