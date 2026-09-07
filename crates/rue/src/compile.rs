use std::path::Path;
use std::time::Instant;

use rue_compiler::unstable::{
    CancellableCompileOutcome, CancellableTestImageOutcome, CompilationCancellation,
    OneShotMetrics, TestCompileFailure, TestImage, TestInventory,
};
use rue_compiler::{
    CompileErrors, CompileOptions, CompileOutput, CompileWarning, LinkerMode, MultiErrorResult,
};
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

/// One executable cycle: discovery gate, destination preflight, compile,
/// publish, warnings, and the success line.
///
/// The batch driver runs it once and the watch loop runs it per revision, so
/// there is one implementation of each of those steps rather than two that
/// drift (RUE-1969). What a watch cycle adds — the change monitor,
/// cancellation, re-observation, and the cycle's own progress text — stays in
/// `watch`, which reads the report this returns and decides what to say and
/// whether to keep looping.
///
/// The steps themselves are [`drive`], shared with the test-image cycle; this
/// request adds only what is the executable's alone, the success line.
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
///
/// `Published` carries what the artifact's cycle promises once its bytes are
/// at the output path: the executable's metrics, or the test image's
/// inventory. The other three outcomes are the same for both.
pub(crate) enum CycleReport<Published = PublishedExecutable> {
    /// The artifact reached the output path. Boxed because the one-shot
    /// metrics an executable carries dwarf every other outcome, and a report
    /// is built once per cycle.
    Published(Box<Published>),
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
    let report = drive::<CompileOutput>(
        host,
        options,
        diagnostics,
        Path::new(output_path),
        observation,
    );
    if let CycleReport::Published(_) = &report {
        announce(announcement, source_path, output_path, options);
    }
    report
}

/// What one cycle compiles and publishes.
///
/// The executable cycle and the test-image cycle differ in exactly two places:
/// what the host is asked to compile, and what rides beside the linked bytes
/// from that compile to the report. Everything between — the discovery gate,
/// the destination preflight, the supersession check, publication, the
/// warnings, the publication outcome — is one sequence, [`drive`], written
/// once over this trait rather than once per artifact (RUE-2089). A change to
/// the order of those steps has one home.
trait CycleArtifact: Sized {
    /// What rides beside the linked output from the compile to the report.
    type Companion;
    /// What the report carries once the bytes are at the output path.
    type Published;

    fn compile(
        host: &mut FilesystemCompilerHost,
        options: &CompileOptions,
    ) -> MultiErrorResult<Self>;

    fn compile_cancellable(
        host: &mut FilesystemCompilerHost,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> Compiled<Self>;

    /// Split the artifact into the output every cycle links and publishes and
    /// the part that is this artifact's own.
    fn into_parts(self) -> (CompileOutput, Self::Companion);

    /// Diagnostics the companion carries, printed after the warnings and
    /// before the publication outcome so a failed publication cannot discard
    /// them. The executable has none.
    fn print_companion_diagnostics(
        _companion: &Self::Companion,
        _diagnostics: &DiagnosticOutput<'_>,
    ) {
    }

    fn published(companion: Self::Companion, executable: PublishedExecutable) -> Self::Published;
}

/// A cancellable compile's answer, in one spelling for every artifact.
enum Compiled<Artifact> {
    Completed(Artifact),
    Errors(CompileErrors),
    Canceled,
}

impl CycleArtifact for CompileOutput {
    type Companion = ();
    type Published = PublishedExecutable;

    fn compile(
        host: &mut FilesystemCompilerHost,
        options: &CompileOptions,
    ) -> MultiErrorResult<Self> {
        host.executable_in_compile_scope(options)
    }

    fn compile_cancellable(
        host: &mut FilesystemCompilerHost,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> Compiled<Self> {
        match host.cancellable_executable_in_compile_scope(options, cancellation) {
            CancellableCompileOutcome::Completed(output) => Compiled::Completed(*output),
            CancellableCompileOutcome::Errors(errors) => Compiled::Errors(errors),
            CancellableCompileOutcome::Canceled => Compiled::Canceled,
        }
    }

    fn into_parts(self) -> (CompileOutput, ()) {
        (self, ())
    }

    fn published((): (), executable: PublishedExecutable) -> PublishedExecutable {
        executable
    }
}

/// What a test image carries beside its linked bytes (ADR-0083 §3).
struct TestImageCompanion {
    inventory: TestInventory,
    compile_failures: Vec<TestCompileFailure>,
    failure_diagnostics: CompileErrors,
}

impl CycleArtifact for TestImage {
    type Companion = TestImageCompanion;
    type Published = PublishedTestImage;

    fn compile(
        host: &mut FilesystemCompilerHost,
        options: &CompileOptions,
    ) -> MultiErrorResult<Self> {
        host.test_image_in_compile_scope(options)
    }

    fn compile_cancellable(
        host: &mut FilesystemCompilerHost,
        options: &CompileOptions,
        cancellation: CompilationCancellation,
    ) -> Compiled<Self> {
        match host.cancellable_test_image_in_compile_scope(options, cancellation) {
            CancellableTestImageOutcome::Completed(image) => Compiled::Completed(*image),
            CancellableTestImageOutcome::Errors(errors) => Compiled::Errors(errors),
            CancellableTestImageOutcome::Canceled => Compiled::Canceled,
        }
    }

    fn into_parts(self) -> (CompileOutput, TestImageCompanion) {
        let TestImage {
            output,
            inventory,
            compile_failures,
            failure_diagnostics,
        } = self;
        (
            output,
            TestImageCompanion {
                inventory,
                compile_failures,
                failure_diagnostics,
            },
        )
    }

    /// stderr is the authoritative diagnostic stream and carries the closure's
    /// own analysis failures exactly as a whole-request failure would have:
    /// once, in the run's own `--error-format`, before any event. The copies
    /// inside the `compile_error` events are the attribution (ADR-0083 §3).
    /// They are published here rather than by the run, and so whatever the
    /// selection turns out to be, because the closure that produced them is
    /// the WHOLE closure: a filter narrows the run set, never the analysis
    /// root set, and a filtered run that silently swallowed a broken test file
    /// would be the papercut the unimported-test-file warning exists to
    /// prevent.
    fn print_companion_diagnostics(
        companion: &TestImageCompanion,
        diagnostics: &DiagnosticOutput<'_>,
    ) {
        if !companion.failure_diagnostics.is_empty() {
            diagnostics.print_errors(&companion.failure_diagnostics);
        }
    }

    fn published(companion: TestImageCompanion, _: PublishedExecutable) -> PublishedTestImage {
        PublishedTestImage {
            inventory: companion.inventory,
            compile_failures: companion.compile_failures,
        }
    }
}

/// The one cycle both artifacts run: discovery gate, destination preflight,
/// compile, publish, warnings, publication outcome.
fn drive<Artifact: CycleArtifact>(
    host: &mut FilesystemCompilerHost,
    options: &CompileOptions,
    diagnostics: &DiagnosticOutput<'_>,
    output_path: &Path,
    observation: CycleObservation<'_>,
) -> CycleReport<Artifact::Published> {
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
    let destination = match preflight(output_path, host, &observation) {
        Ok(destination) => destination,
        Err(error) => {
            diagnostics.print_error(&error.into_compile_error());
            return CycleReport::Failed;
        }
    };

    let (artifact, observation) = match observation {
        CycleObservation::OneShot => match Artifact::compile(host, options) {
            Ok(artifact) => (artifact, PublicationObservation::OneShot),
            Err(errors) => {
                diagnostics.print_errors(&errors);
                return CycleReport::Failed;
            }
        },
        CycleObservation::Watch {
            inputs,
            cancellation,
            superseded,
        } => {
            let outcome = Artifact::compile_cancellable(host, options, cancellation);
            // A superseded cycle describes bytes that no longer exist. Check
            // before reporting, so a transient error the user has already
            // fixed never reaches the terminal.
            if superseded() {
                return CycleReport::Superseded(Supersession::BeforePublication);
            }
            match outcome {
                Compiled::Completed(artifact) => (artifact, PublicationObservation::Watch(inputs)),
                Compiled::Errors(errors) => {
                    diagnostics.print_errors(&errors);
                    return CycleReport::Failed;
                }
                Compiled::Canceled => return CycleReport::Canceled,
            }
        }
    };
    let (output, companion) = artifact.into_parts();
    let linked = linked_executable(output, options, destination, observation);

    // Publication runs after the compiler's timing root closes, so it is
    // measured as a driver phase: it breaks down process-minus-root overhead
    // without becoming a second timing root (RUE-786).
    let publication = {
        let _span = tracing::info_span!("output_write", driver_phase = true).entered();
        linked.publish()
    };
    // Warnings, and whatever diagnostics the artifact carries beside them,
    // live outside the publication result so a failure cannot discard them;
    // present them before inspecting the publication outcome.
    diagnostics.print_warnings(&publication.warnings);
    Artifact::print_companion_diagnostics(&companion, diagnostics);
    match publication.result {
        Ok(executable) => {
            CycleReport::Published(Box::new(Artifact::published(companion, executable)))
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
/// The same [`drive`] as the executable cycle — discovery gate, destination
/// preflight, compile, publish, warnings — over the request's test root set
/// instead of its executable one, so `rue test` and `rue test --watch` reach
/// the image through the one orchestration `rue build` and `rue build --watch`
/// use (RUE-2023, RUE-2089). What a watch cycle adds around it stays in
/// `watch`, exactly as it does for the executable cycle; what a test cycle
/// adds *after* it — the run — is `test_mode`'s.
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

/// The outcome of a test-image cycle: [`CycleReport`] carrying the image's
/// inventory instead of the executable's metrics.
pub(crate) type TestCycleReport = CycleReport<PublishedTestImage>;

pub(crate) fn drive_test_cycle(request: TestCycleRequest<'_, '_>) -> TestCycleReport {
    let TestCycleRequest {
        host,
        options,
        diagnostics,
        image_path,
        observation,
    } = request;
    drive::<TestImage>(host, options, diagnostics, image_path, observation)
}
