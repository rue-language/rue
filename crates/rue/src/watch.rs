use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rue_compiler::unstable::{CompilationCancellation, SourceInfo};
use rue_compiler::{CompileOptions, LinkerMode};
use rue_driver::{FilesystemCompilerHost, SourceLoadError};

use crate::compile::{CancellableCompileRequest, CompileCycleOutcome, execute_cancellable};
use crate::output;
use crate::{DiagnosticOutput, ErrorFormat};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const QUIET_PERIOD: Duration = Duration::from_millis(75);
const FAILED_REOBSERVE_RETRY: Duration = Duration::from_millis(250);

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
    fn start(inputs: Vec<(PathBuf, u64)>, cancellation: CompilationCancellation) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_changed = changed.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                if inputs_changed(&inputs) {
                    thread_changed.store(true, Ordering::Release);
                    cancellation.cancel();
                    return;
                }
                thread::sleep(POLL_INTERVAL);
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
        self.thread
            .join()
            .expect("watch change monitor thread panicked");
        self.changed.load(Ordering::Acquire)
    }
}

pub(crate) fn run(mut request: WatchRequest) -> ! {
    let mut needs_reobserve = false;
    println!("Watching {} for changes", request.source_path);

    loop {
        let cycle_started = Instant::now();
        if needs_reobserve {
            match request.host.reobserve() {
                Ok(()) => {}
                Err(error) => {
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
                print_source_load_error(error, request.error_format);
                eprintln!(
                    "Watch cycle failed after {} ms; keeping the last successful executable",
                    cycle_started.elapsed().as_millis()
                );
                thread::sleep(FAILED_REOBSERVE_RETRY);
                continue;
            }
        }

        let inputs = request.host.watch_inputs();
        let source_snapshot = request.host.source_snapshot().clone();
        let source_infos = source_snapshot
            .files()
            .map(|source| (source.file_id, SourceInfo::new(source.source, source.path)))
            .collect();
        let diagnostics = DiagnosticOutput::new(request.error_format, source_infos);

        let destination = match output::preflight_destination(
            Path::new(&request.output_path),
            source_snapshot.files().map(|source| source.path),
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
        let outcome = execute_cancellable(CancellableCompileRequest {
            host: &mut request.host,
            options: request.compile_options.clone(),
            destination,
            cancellation,
        });

        let changed_before_publication = monitor.changed() || inputs_changed(&inputs);
        if changed_before_publication {
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
                        Err(error) => diagnostics.print_error(&error.into_compile_error()),
                    }
                }
                CompileCycleOutcome::Canceled => {
                    eprintln!(
                        "Watch cycle canceled after {} ms",
                        cycle_started.elapsed().as_millis()
                    );
                }
                CompileCycleOutcome::Errors(errors) => {
                    diagnostics.print_errors(&errors);
                    eprintln!(
                        "Watch cycle failed after {} ms; keeping the last successful executable",
                        cycle_started.elapsed().as_millis()
                    );
                }
            }
        }

        let changed = monitor.finish() || inputs_changed(&inputs);
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

fn wait_for_change(inputs: &[(PathBuf, u64)]) {
    while !inputs_changed(inputs) {
        thread::sleep(POLL_INTERVAL);
    }
}

fn debounce(inputs: &[(PathBuf, u64)]) {
    let paths = inputs
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let mut previous = current_fingerprints(&paths);
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < QUIET_PERIOD {
        thread::sleep(POLL_INTERVAL);
        let current = current_fingerprints(&paths);
        if current != previous {
            previous = current;
            quiet_since = Instant::now();
        }
    }
}

fn inputs_changed(inputs: &[(PathBuf, u64)]) -> bool {
    inputs
        .iter()
        .any(|(path, expected)| file_hash(path).as_ref() != Some(expected))
}

fn current_fingerprints(paths: &[PathBuf]) -> Vec<Option<u64>> {
    paths.iter().map(|path| file_hash(path)).collect()
}

fn file_hash(path: &Path) -> Option<u64> {
    fs::read(path).ok().map(|bytes| content_hash(&bytes))
}

fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let inputs = vec![(path.clone(), content_hash(b"alpha"))];
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
        let inputs = vec![(path.clone(), content_hash(b"source"))];
        fs::remove_file(path).unwrap();
        assert!(inputs_changed(&inputs));
    }
}
