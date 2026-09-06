use std::path::Path;
use std::time::Instant;

use rue_compiler::unstable::{
    CancellableCompileOutcome, CancellableTestImageOutcome, CompilationCancellation,
    OneShotMetrics, TestCompileFailure, TestImage, TestInventory,
};
use rue_compiler::{CompileOptions, CompileWarning, LinkerMode};
use rue_driver::{FilesystemCompilerHost, WatchInput};

use crate::DiagnosticOutput;
use crate::output::{
    PublicationDestination, PublishError, PublishRequest, publish_executable,
    publish_watch_executable,
};

/// Linked bytes and everything publication needs, held between the compiler's
/// timing root closing and the atomic write.
struct LinkedExecutable {
    target: rue_target::Target,
    warnings: Vec<CompileWarning>,
    metrics: OneShotMetrics,
    linked_bytes: Vec<u8>,
    destination: PublicationDestination,
    observation: PublicationObservation,
}

enum PublicationObservation {
    OneShot,
    Watch(Vec<WatchInput>),
}

pub(crate) struct PublishedExecutable {
    metrics: OneShotMetrics,
}

struct PublicationAttempt {
    warnings: Vec<CompileWarning>,
    result: Result<PublishedExecutable, PublishError>,
}

impl LinkedExecutable {
    /// Finalize and publish the linked bytes after compiler timing/accounting closes.
    fn publish(self) -> PublicationAttempt {
        let LinkedExecutable {
            target,
            warnings,
            metrics,
            linked_bytes,
            destination,
            observation,
        } = self;
        let publication = match observation {
            PublicationObservation::OneShot => publish_executable(PublishRequest {
                destination,
                bytes: &linked_bytes,
                target,
            }),
            PublicationObservation::Watch(inputs) => publish_watch_executable(
                PublishRequest {
                    destination,
                    bytes: &linked_bytes,
                    target,
                },
                &inputs,
            ),
        };
        PublicationAttempt {
            warnings,
            result: publication.map(|()| PublishedExecutable { metrics }),
        }
    }
}

impl PublishedExecutable {
    pub(crate) fn unstable_metrics(&self) -> OneShotMetrics {
        self.metrics
    }
}

/// One compile cycle: destination preflight, compile, publish, warnings, and
/// the success line.
///
/// The batch driver runs it once and the watch loop runs it per revision, so
/// there is one implementation of each of those steps rather than two that
/// drift. What a watch cycle adds — the change monitor, cancellation,
/// re-observation, and the cycle's own progress text — stays in `watch`, which
/// reads the report this returns and decides what to say and whether to keep
/// looping.
///
/// The discovery gate is not a step here: the host's compile-scope entry
/// points refuse an unclosed import graph and hand back discovery's own
/// diagnostics, so a cycle reports the unresolved `@import` through the
/// ordinary compile-failure path whichever driver runs it (RUE-1969).
pub(crate) struct CycleRequest<'a, 'diagnostics> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) options: &'a CompileOptions,
    pub(crate) diagnostics: &'a DiagnosticOutput<'diagnostics>,
    pub(crate) source_path: &'a str,
    pub(crate) output_path: &'a str,
    pub(crate) observation: CycleObservation<'a>,
    pub(crate) announcement: Announcement,
}

/// How this cycle observes its inputs, which decides how the destination is
/// validated, whether compilation can be canceled, and whether publication
/// revalidates the accepted-read closure.
pub(crate) enum CycleObservation<'a> {
    /// A one-shot build: the destination is checked against the closed
    /// snapshot's own paths and the compile runs to completion.
    OneShot,
    /// A watch cycle: the destination is checked against the accepted-read
    /// closure, the compile is cancellable, and publication refuses to install
    /// bytes built from inputs that have since changed. `superseded` reports
    /// whether a newer revision landed while this one compiled; a superseded
    /// cycle publishes nothing and says nothing, because its diagnostics
    /// describe source the user has already replaced.
    Watch {
        inputs: Vec<WatchInput>,
        cancellation: CompilationCancellation,
        superseded: &'a dyn Fn() -> bool,
    },
}

/// What the cycle says on success.
pub(crate) enum Announcement {
    /// Nothing: `--benchmark-json` owns stdout and a banner would corrupt it.
    Silent,
    /// The one-shot success line.
    Completed,
    /// The watch success line, which additionally reports how long the cycle
    /// took. Carries the cycle's start, so the time it reports is measured at
    /// the moment the executable actually reached the output path.
    Cycle(Instant),
}

/// Which boundary caught a newer revision. Both refuse to publish; they differ
/// only in what the watcher's progress protocol calls them.
pub(crate) enum Supersession {
    /// Observed after the compile, before anything was written.
    BeforePublication,
    /// The publication guard refused to install bytes built from inputs that
    /// had already changed.
    AtPublication,
}

/// The outcome of a cycle. Every diagnostic it produced is already on the
/// diagnostic stream; the report says what happened so the caller can pick an
/// exit status or a progress line.
pub(crate) enum CycleReport {
    /// The executable reached the output path. Boxed because the one-shot
    /// metrics it carries dwarf every other outcome, and a report is built
    /// once per cycle.
    Published(Box<PublishedExecutable>),
    /// The program was rejected, or the destination or publication was
    /// refused.
    Failed,
    /// A newer source revision arrived before this one could publish. Nothing
    /// was written and nothing was reported.
    Superseded(Supersession),
    /// Compilation was canceled without a newer revision being observed.
    Canceled,
}

pub(crate) fn drive_cycle(request: CycleRequest<'_, '_>) -> CycleReport {
    let CycleRequest {
        host,
        options,
        diagnostics,
        source_path,
        output_path,
        observation,
        announcement,
    } = request;

    // A revision whose import graph did not close valid is reported as that
    // and nothing else: the unresolved `@import` is the user's problem, and
    // the destination preflight's answer about the output path would only bury
    // it (RUE-810). The host refuses such a revision at every compile-scope
    // entry point regardless, so a driver that forgot to ask still cannot
    // reach the compiler with one; asking here only fixes the order.
    if let Some(errors) = host.discovery_refusal() {
        diagnostics.print_errors(&errors);
        return CycleReport::Failed;
    }

    // Closed discovery fixes the complete source identity set, so the
    // destination can be validated before any semantic, codegen, or link work
    // and the set retained for mandatory revalidation immediately before the
    // atomic publication.
    let destination = match preflight(Path::new(output_path), host, &observation) {
        Ok(destination) => destination,
        Err(error) => {
            diagnostics.print_error(&error.into_compile_error());
            return CycleReport::Failed;
        }
    };

    let linked = match observation {
        CycleObservation::OneShot => match host.executable_in_compile_scope(options) {
            Ok(output) => linked_executable(
                output,
                options,
                destination,
                PublicationObservation::OneShot,
            ),
            Err(errors) => {
                diagnostics.print_errors(&errors);
                return CycleReport::Failed;
            }
        },
        CycleObservation::Watch {
            ref inputs,
            ref cancellation,
            superseded,
        } => {
            let outcome =
                host.cancellable_executable_in_compile_scope(options, cancellation.clone());
            // A superseded cycle describes bytes that no longer exist. Check
            // before reporting, so a transient error the user has already
            // fixed never reaches the terminal.
            if superseded() {
                return CycleReport::Superseded(Supersession::BeforePublication);
            }
            match outcome {
                CancellableCompileOutcome::Completed(output) => linked_executable(
                    *output,
                    options,
                    destination,
                    PublicationObservation::Watch(inputs.clone()),
                ),
                CancellableCompileOutcome::Errors(errors) => {
                    diagnostics.print_errors(&errors);
                    return CycleReport::Failed;
                }
                CancellableCompileOutcome::Canceled => return CycleReport::Canceled,
            }
        }
    };

    // Publication runs after the compiler's timing root closes, so it is
    // measured as a driver phase: it breaks down process-minus-root overhead
    // without becoming a second timing root (RUE-786).
    let publication = {
        let _span = tracing::info_span!("output_write", driver_phase = true).entered();
        linked.publish()
    };
    // Warnings live outside the publication result so a failure cannot discard
    // them; present them before inspecting the publication outcome.
    diagnostics.print_warnings(&publication.warnings);
    match publication.result {
        Ok(published) => {
            announce(announcement, source_path, output_path, options);
            CycleReport::Published(Box::new(published))
        }
        Err(PublishError::InputsChanged) => CycleReport::Superseded(Supersession::AtPublication),
        Err(error) => {
            diagnostics.print_error(&error.into_compile_error());
            CycleReport::Failed
        }
    }
}

fn preflight(
    path: &Path,
    host: &FilesystemCompilerHost,
    observation: &CycleObservation<'_>,
) -> Result<PublicationDestination, PublishError> {
    match observation {
        CycleObservation::OneShot => crate::output::preflight_destination(
            path,
            host.source_snapshot().files().map(|source| source.path),
        ),
        CycleObservation::Watch { inputs, .. } => {
            crate::output::preflight_watch_destination(path, inputs)
        }
    }
}

fn linked_executable(
    output: rue_compiler::CompileOutput,
    options: &CompileOptions,
    destination: PublicationDestination,
    observation: PublicationObservation,
) -> LinkedExecutable {
    let metrics = output.unstable_metrics();
    LinkedExecutable {
        target: options.target,
        warnings: output.warnings,
        metrics,
        linked_bytes: output.elf,
        destination,
        observation,
    }
}

fn announce(
    announcement: Announcement,
    source_path: &str,
    output_path: &str,
    options: &CompileOptions,
) {
    let target = options.target;
    let linker = linker_name(&options.linker);
    match announcement {
        Announcement::Silent => {}
        Announcement::Completed => {
            println!("Compiled {source_path} -> {output_path} (target: {target}, linker: {linker})")
        }
        Announcement::Cycle(started) => println!(
            "Compiled {source_path} -> {output_path} in {} ms (target: {target}, linker: {linker})",
            started.elapsed().as_millis()
        ),
    }
}

fn linker_name(linker: &LinkerMode) -> &str {
    match linker {
        LinkerMode::Internal => "internal",
        LinkerMode::System(command) => command,
    }
}

/// What a published test image hands the runner (ADR-0083 §3).
///
/// The linked bytes are already at the cycle's image path; what the run still
/// needs is the inventory that assigned the dispatch ordinals and the tests the
/// image could not hold.
pub(crate) struct PublishedTestImage {
    pub(crate) inventory: TestInventory,
    pub(crate) compile_failures: Vec<TestCompileFailure>,
}

/// One test-image cycle, the test-mode twin of [`CycleRequest`].
///
/// Same steps in the same order as [`drive_cycle`] — discovery gate,
/// destination preflight, compile, publish, warnings — over the request's test
/// root set instead of its executable one, so `rue test` and `rue test --watch`
/// reach the image through one orchestration rather than two (RUE-2023). What
/// a watch cycle adds around it stays in `watch`, exactly as it does for the
/// executable cycle; what a test cycle adds *after* it — the run — is
/// `test_mode`'s.
pub(crate) struct TestCycleRequest<'a, 'diagnostics> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) options: &'a CompileOptions,
    pub(crate) diagnostics: &'a DiagnosticOutput<'diagnostics>,
    /// Where the linked image is staged. Always inside the run's own private
    /// directory, never a path the user named: `rue test` refuses `-o` for
    /// exactly this reason, so no cycle here can publish over a user artifact.
    pub(crate) image_path: &'a Path,
    pub(crate) observation: CycleObservation<'a>,
}

/// The outcome of a test-image cycle, on the same terms as [`CycleReport`].
pub(crate) enum TestCycleReport {
    /// The image reached the cycle's image path and can be run.
    Published(Box<PublishedTestImage>),
    /// The request was rejected outside every test closure, or the image could
    /// not be staged. Diagnostics are already on the diagnostic stream.
    Failed,
    /// A newer source revision arrived before the image could be published.
    Superseded(Supersession),
    /// The image build was canceled without a newer revision being observed.
    Canceled,
}

pub(crate) fn drive_test_cycle(request: TestCycleRequest<'_, '_>) -> TestCycleReport {
    let TestCycleRequest {
        host,
        options,
        diagnostics,
        image_path,
        observation,
    } = request;

    // The same gate the executable cycle opens with, and for the same reason:
    // an unresolved `@import` is the user's problem and is reported as itself
    // rather than as whatever the staging preflight would have said (RUE-810).
    if let Some(errors) = host.discovery_refusal() {
        diagnostics.print_errors(&errors);
        return TestCycleReport::Failed;
    }

    let destination = match preflight(image_path, host, &observation) {
        Ok(destination) => destination,
        Err(error) => {
            diagnostics.print_error(&error.into_compile_error());
            return TestCycleReport::Failed;
        }
    };

    let image = match observation {
        CycleObservation::OneShot => match host.test_image_in_compile_scope(options) {
            Ok(image) => image,
            Err(errors) => {
                diagnostics.print_errors(&errors);
                return TestCycleReport::Failed;
            }
        },
        CycleObservation::Watch {
            ref cancellation,
            superseded,
            ..
        } => {
            let outcome =
                host.cancellable_test_image_in_compile_scope(options, cancellation.clone());
            // A superseded cycle describes an image the user's source no longer
            // asks for. Check before reporting, so a transient error already
            // fixed on disk never reaches the terminal.
            if superseded() {
                return TestCycleReport::Superseded(Supersession::BeforePublication);
            }
            match outcome {
                CancellableTestImageOutcome::Completed(image) => *image,
                CancellableTestImageOutcome::Errors(errors) => {
                    diagnostics.print_errors(&errors);
                    return TestCycleReport::Failed;
                }
                CancellableTestImageOutcome::Canceled => return TestCycleReport::Canceled,
            }
        }
    };

    let TestImage {
        output,
        inventory,
        compile_failures,
        failure_diagnostics,
    } = image;
    let metrics = output.unstable_metrics();
    let linked = LinkedExecutable {
        target: options.target,
        warnings: output.warnings,
        metrics,
        linked_bytes: output.elf,
        destination,
        observation: match observation {
            CycleObservation::OneShot => PublicationObservation::OneShot,
            CycleObservation::Watch { inputs, .. } => PublicationObservation::Watch(inputs),
        },
    };
    let publication = {
        let _span = tracing::info_span!("output_write", driver_phase = true).entered();
        linked.publish()
    };
    // Warnings and the closure's own analysis failures live outside the
    // publication result so a failed publication cannot discard them, exactly
    // as the executable cycle treats its warnings.
    //
    // stderr is the authoritative diagnostic stream and carries the failures
    // exactly as a whole-request failure would have: once, in the run's own
    // `--error-format`, before any event. The copies inside the `compile_error`
    // events are the attribution (ADR-0083 §3). They are published here rather
    // than by the run, and so whatever the selection turns out to be, because
    // the closure that produced them is the WHOLE closure: a filter narrows the
    // run set, never the analysis root set, and a filtered run that silently
    // swallowed a broken test file would be the papercut the
    // unimported-test-file warning exists to prevent.
    diagnostics.print_warnings(&publication.warnings);
    if !failure_diagnostics.is_empty() {
        diagnostics.print_errors(&failure_diagnostics);
    }
    match publication.result {
        Ok(_) => TestCycleReport::Published(Box::new(PublishedTestImage {
            inventory,
            compile_failures,
        })),
        Err(PublishError::InputsChanged) => {
            TestCycleReport::Superseded(Supersession::AtPublication)
        }
        Err(error) => {
            diagnostics.print_error(&error.into_compile_error());
            TestCycleReport::Failed
        }
    }
}
