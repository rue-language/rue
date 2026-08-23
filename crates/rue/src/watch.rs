#[cfg(test)]
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rue_compiler::unstable::{CompilationCancellation, SourceInfo};
use rue_compiler::{CompileOptions, LinkerMode};
#[cfg(test)]
use rue_driver::watch_inputs_changed_with_reader;
use rue_driver::{
    FilesystemCompilerHost, SourceLoadError, WatchFingerprint, WatchInput,
    watch_input_fingerprints, watch_inputs_changed,
};

use crate::compile::{CancellableCompileRequest, CompileCycleOutcome, execute_cancellable};
use crate::output;
use crate::{DiagnosticOutput, ErrorFormat};

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

fn test_event(event: &str) {
    let Some(path) = std::env::var_os(TEST_PROTOCOL_ENV) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{event}");
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

pub(crate) struct WatchRequest {
    pub(crate) host: FilesystemCompilerHost,
    pub(crate) compile_options: CompileOptions,
    pub(crate) source_path: String,
    pub(crate) output_path: String,
    pub(crate) error_format: ErrorFormat,
}

struct ChangeMonitor {
    stop: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}

impl ChangeMonitor {
    fn start(inputs: Vec<WatchInput>, cancellation: CompilationCancellation) -> Self {
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

pub(crate) fn run(mut request: WatchRequest) -> ! {
    let mut needs_reobserve = false;
    println!("Watching {} for changes", request.source_path);
    test_event("ready");

    loop {
        let cycle_started = Instant::now();
        if needs_reobserve {
            match request.host.reobserve() {
                Ok(()) => test_event("reobserve-ok"),
                Err(error) => {
                    test_event("reobserve-error");
                    print_source_load_error(error, request.error_format);
                    eprintln!(
                        "Watch cycle failed after {} ms; keeping the last successful executable",
                        cycle_started.elapsed().as_millis()
                    );
                    thread::sleep(FAILED_REOBSERVE_RETRY);
                    continue;
                }
            }
            if let Err(error) = request
                .host
                .acquire_reached_toolchain_modules(&request.compile_options)
            {
                test_event("acquire-error");
                print_source_load_error(error, request.error_format);
                eprintln!(
                    "Watch cycle failed after {} ms; keeping the last successful executable",
                    cycle_started.elapsed().as_millis()
                );
                thread::sleep(FAILED_REOBSERVE_RETRY);
                continue;
            }
            test_event("acquire-ok");
        }

        let inputs = request.host.watch_inputs();
        let source_snapshot = request.host.source_snapshot().clone();
        let source_infos = source_snapshot
            .files()
            .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
            .collect();
        let diagnostics = DiagnosticOutput::new(request.error_format, source_infos);

        let destination = match output::preflight_watch_destination(
            Path::new(&request.output_path),
            &inputs,
        ) {
            Ok(destination) => destination,
            Err(output::PublishError::WouldClobberSource) => {
                eprintln!(
                    "Error: output path '{}' is also an input source file; refusing to overwrite it",
                    request.output_path
                );
                wait_for_change(&inputs);
                debounce(&inputs);
                needs_reobserve = true;
                continue;
            }
            Err(error) => {
                diagnostics.print_error(&error.into_compile_error());
                wait_for_change(&inputs);
                debounce(&inputs);
                needs_reobserve = true;
                continue;
            }
        };

        let cancellation = CompilationCancellation::new();
        let monitor = ChangeMonitor::start(inputs.clone(), cancellation.clone());
        test_event("compile-started");
        test_compile_delay();
        let outcome = execute_cancellable(CancellableCompileRequest {
            host: &mut request.host,
            options: request.compile_options.clone(),
            destination,
            cancellation,
            watch_inputs: inputs.clone(),
        });

        let changed_before_publication = monitor.changed() || inputs_changed(&inputs);
        let mut publication_changed = false;
        if changed_before_publication {
            test_event("canceled-before-publication");
            eprintln!(
                "Watch cycle canceled after {} ms; a newer source revision is available",
                cycle_started.elapsed().as_millis()
            );
        } else {
            match outcome {
                CompileCycleOutcome::Linked(output) => {
                    let publication = (*output).publish();
                    diagnostics.print_warnings(&publication.warnings);
                    match publication.result {
                        Ok(_) => {
                            test_event("published");
                            println!(
                                "Compiled {} -> {} in {} ms (target: {}, linker: {})",
                                request.source_path,
                                request.output_path,
                                cycle_started.elapsed().as_millis(),
                                request.compile_options.target,
                                linker_name(&request.compile_options.linker),
                            );
                        }
                        Err(output::PublishError::WouldClobberSource) => eprintln!(
                            "Error: output path '{}' became an input source; keeping the last successful executable",
                            request.output_path
                        ),
                        Err(output::PublishError::InputsChanged) => {
                            publication_changed = true;
                            test_event("canceled-at-publication");
                            eprintln!(
                                "Watch cycle canceled after {} ms; a newer source revision is available",
                                cycle_started.elapsed().as_millis()
                            );
                        }
                        Err(error) => diagnostics.print_error(&error.into_compile_error()),
                    }
                }
                CompileCycleOutcome::Canceled => {
                    test_event("canceled");
                    eprintln!(
                        "Watch cycle canceled after {} ms",
                        cycle_started.elapsed().as_millis()
                    );
                }
                CompileCycleOutcome::Errors(errors) => {
                    test_event("compile-error");
                    diagnostics.print_errors(&errors);
                    eprintln!(
                        "Watch cycle failed after {} ms; keeping the last successful executable",
                        cycle_started.elapsed().as_millis()
                    );
                }
            }
        }

        let monitor_changed = monitor.finish();
        let changed = watch_cycle_changed(
            publication_changed,
            monitor_changed,
            inputs_changed(&inputs),
        );
        if !changed {
            wait_for_change(&inputs);
        }
        debounce(&inputs);
        needs_reobserve = true;
    }
}

fn linker_name(linker: &LinkerMode) -> &str {
    match linker {
        LinkerMode::Internal => "internal",
        LinkerMode::System(command) => command,
    }
}

fn print_source_load_error(error: SourceLoadError, error_format: ErrorFormat) {
    match error {
        SourceLoadError::Message(message) => eprintln!("{message}"),
        SourceLoadError::Toolchain(error) => eprintln!("{error}"),
        SourceLoadError::HermeticDenial(error) => eprintln!("{error}"),
        SourceLoadError::Compiler { snapshot, errors } => {
            let infos = snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .files()
                        .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
                        .collect()
                })
                .unwrap_or_default();
            DiagnosticOutput::new(error_format, infos).print_errors(&errors);
        }
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
    let mut previous = current_fingerprints(inputs);
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < QUIET_PERIOD {
        thread::sleep(POLL_INTERVAL);
        let current = current_fingerprints(inputs);
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

fn current_fingerprints(inputs: &[WatchInput]) -> Vec<Option<WatchFingerprint>> {
    watch_input_fingerprints(inputs)
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
