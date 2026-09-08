use std::path::Path;
use std::time::Instant;

use rue_compiler::unstable::{
    CancellableCompileOutcome, CancellableTestImageOutcome, CompilationCancellation,
    OneShotMetrics, TestCandidateInventory, TestCompileFailure, TestImage, TestInventory,
    UnimportedTestFile,
};
use rue_compiler::{
    AcceptedReadManifest, CompileErrors, CompileOptions, CompileOutput, CompileWarning, LinkerMode,
    MultiErrorResult, SourceSnapshot,
};
use rue_driver::{
    AttemptedRead, FilesystemCompilerHost, WatchInput, with_import_migration_helps,
    with_import_migration_helps_batches,
};

use crate::output::{
    PublicationDestination, PublishError, PublishRequest, preflight_destination_with_display,
    preflight_watch_destination_with_display, publish_executable, publish_watch_executable,
};
use crate::{DiagnosticOutput, ErrorFormat};

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

/// The compiler-owned answer to one cycle. It contains no borrow of the
/// retained host, so a caller may reobserve or release that host before it
/// consumes the result for diagnostics, publication, or test execution.
pub(crate) struct OwnedCycleResponse<Artifact> {
    source_snapshot: SourceSnapshot,
    accepted_reads: AcceptedReadManifest,
    attempted_reads: Vec<AttemptedRead>,
    watch_inputs: Vec<WatchInput>,
    published_user_module_count: usize,
    error_format: ErrorFormat,
    options: CompileOptions,
    source_path: Option<String>,
    output_display_path: String,
    unimported_test_files: Option<Result<Vec<UnimportedTestFile>, CompileErrors>>,
    result: OwnedCycleResult<Artifact>,
}

/// Filesystem observations belonging to a compiler cycle, carried with the
/// owned result so later host observations cannot be mistaken for its inputs.
pub(crate) struct OwnedCycleObservations {
    pub(crate) accepted_reads: AcceptedReadManifest,
    pub(crate) attempted_reads: Vec<AttemptedRead>,
    pub(crate) watch_inputs: Vec<WatchInput>,
}

enum OwnedCycleResult<Artifact> {
    Ready {
        artifact: Artifact,
        destination: PublicationDestination,
        observation: PublicationObservation,
    },
    Failed {
        errors: Option<CompileErrors>,
        publication: Option<PublishError>,
    },
    Superseded(Supersession),
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
    drive::<CompileOutput>(
        host,
        options,
        diagnostics,
        source_path,
        Path::new(output_path),
        observation,
        Some(announcement),
    )
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
pub(crate) trait CycleArtifact: Sized {
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

    /// Freeze presentation-only diagnostic facts while the source tree that
    /// produced this artifact is still the active filesystem revision.
    fn prepare(self) -> Self {
        self
    }

    /// Diagnostics the companion carries, printed after the warnings and
    /// before the publication outcome so a failed publication cannot discard
    /// them. The executable has none.
    fn print_companion_diagnostics(
        _companion: &Self::Companion,
        _diagnostics: &DiagnosticOutput<'_>,
    ) {
    }

    fn published(
        companion: Self::Companion,
        executable: PublishedExecutable,
        unimported_test_files: Option<Result<Vec<UnimportedTestFile>, CompileErrors>>,
        _source_snapshot: SourceSnapshot,
        _error_format: ErrorFormat,
        _observations: OwnedCycleObservations,
    ) -> Self::Published;
}

/// A cancellable compile's answer, in one spelling for every artifact.
pub(crate) enum Compiled<Artifact> {
    Completed(Artifact),
    Errors(CompileErrors),
    Canceled,
}

fn owned_response<Artifact>(
    host: &FilesystemCompilerHost,
    error_format: ErrorFormat,
    options: &CompileOptions,
    source_path: Option<&str>,
    output_display_path: &str,
    result: OwnedCycleResult<Artifact>,
) -> OwnedCycleResponse<Artifact> {
    OwnedCycleResponse {
        source_snapshot: host.source_snapshot().clone(),
        accepted_reads: host.accepted_reads().clone(),
        attempted_reads: host.attempted_reads().to_vec(),
        watch_inputs: host.watch_inputs(),
        published_user_module_count: host.published_user_module_count(),
        error_format,
        options: options.clone(),
        source_path: source_path.map(str::to_owned),
        output_display_path: output_display_path.to_owned(),
        unimported_test_files: None,
        result,
    }
}

/// Produce an owned cycle answer while the retained host is available. This
/// function performs every compiler query but does no diagnostics, output
/// publication, or test execution; those are client completion operations.
fn produce<Artifact: CycleArtifact>(
    host: &mut FilesystemCompilerHost,
    options: &CompileOptions,
    error_format: ErrorFormat,
    source_path: Option<&str>,
    output_path: &Path,
    observation: CycleObservation<'_>,
) -> OwnedCycleResponse<Artifact> {
    let output_display_path = output_path.to_string_lossy().into_owned();
    let output_path = host.anchor_path(output_path);
    if let Some(errors) = host.discovery_refusal() {
        return owned_response(
            host,
            error_format,
            options,
            source_path,
            &output_display_path,
            OwnedCycleResult::Failed {
                errors: Some(with_import_migration_helps(&errors)),
                publication: None,
            },
        );
    }

    let destination = match preflight(
        &output_path,
        Path::new(&output_display_path),
        host,
        &observation,
    ) {
        Ok(destination) => destination,
        Err(error) => {
            return owned_response(
                host,
                error_format,
                options,
                source_path,
                &output_display_path,
                OwnedCycleResult::Failed {
                    errors: None,
                    publication: Some(error),
                },
            );
        }
    };

    let (artifact, publication_observation) = match observation {
        CycleObservation::OneShot => match Artifact::compile(host, options) {
            Ok(artifact) => (Artifact::prepare(artifact), PublicationObservation::OneShot),
            Err(errors) => {
                return owned_response(
                    host,
                    error_format,
                    options,
                    source_path,
                    &output_display_path,
                    OwnedCycleResult::Failed {
                        errors: Some(with_import_migration_helps(&errors)),
                        publication: None,
                    },
                );
            }
        },
        CycleObservation::Watch {
            inputs,
            cancellation,
            superseded,
        } => {
            let outcome = Artifact::compile_cancellable(host, options, cancellation);
            if superseded() {
                return owned_response(
                    host,
                    error_format,
                    options,
                    source_path,
                    &output_display_path,
                    OwnedCycleResult::Superseded(Supersession::BeforePublication),
                );
            }
            match outcome {
                Compiled::Completed(artifact) => (
                    Artifact::prepare(artifact),
                    PublicationObservation::Watch(inputs),
                ),
                Compiled::Errors(errors) => {
                    return owned_response(
                        host,
                        error_format,
                        options,
                        source_path,
                        &output_display_path,
                        OwnedCycleResult::Failed {
                            errors: Some(with_import_migration_helps(&errors)),
                            publication: None,
                        },
                    );
                }
                Compiled::Canceled => {
                    return owned_response(
                        host,
                        error_format,
                        options,
                        source_path,
                        &output_display_path,
                        OwnedCycleResult::Canceled,
                    );
                }
            }
        }
    };
    owned_response(
        host,
        error_format,
        options,
        source_path,
        &output_display_path,
        OwnedCycleResult::Ready {
            artifact,
            destination,
            observation: publication_observation,
        },
    )
}

impl<Artifact: CycleArtifact> OwnedCycleResponse<Artifact> {
    pub(crate) fn published_user_module_count(&self) -> usize {
        self.published_user_module_count
    }

    fn attach_unimported_test_files(
        &mut self,
        report: Option<Result<Vec<UnimportedTestFile>, CompileErrors>>,
    ) {
        self.unimported_test_files = report;
    }

    pub(crate) fn complete(
        self,
        announcement: Option<Announcement>,
    ) -> CycleReport<Artifact::Published> {
        let OwnedCycleResponse {
            source_snapshot,
            published_user_module_count: _,
            error_format,
            options,
            source_path,
            output_display_path,
            unimported_test_files,
            result,
            accepted_reads,
            attempted_reads,
            watch_inputs,
        } = self;
        let source_infos = source_snapshot
            .files()
            .map(|source| {
                (
                    source.file_id,
                    rue_compiler::unstable::SourceInfo::new(source.source, source.path),
                )
            })
            .collect();
        let diagnostics = DiagnosticOutput::new(error_format, source_infos);
        let (artifact, destination, observation) = match result {
            OwnedCycleResult::Ready {
                artifact,
                destination,
                observation,
            } => (artifact, destination, observation),
            OwnedCycleResult::Failed {
                errors,
                publication,
            } => {
                if let Some(errors) = errors {
                    diagnostics.print_prepared_errors(&errors);
                }
                if let Some(error) = publication {
                    diagnostics.print_error(&error.into_compile_error());
                }
                return CycleReport::Failed;
            }
            OwnedCycleResult::Superseded(boundary) => {
                return CycleReport::Superseded(boundary);
            }
            OwnedCycleResult::Canceled => return CycleReport::Canceled,
        };
        let (output, companion) = artifact.into_parts();
        let linked = linked_executable(output, &options, destination, observation);
        let publication = {
            let _span = tracing::info_span!("output_write", driver_phase = true).entered();
            linked.publish()
        };
        diagnostics.print_warnings(&publication.warnings);
        Artifact::print_companion_diagnostics(&companion, &diagnostics);
        match publication.result {
            Ok(executable) => {
                if let Some(announcement) = announcement {
                    if let Some(source_path) = source_path {
                        announce(announcement, &source_path, &output_display_path, &options);
                    }
                }
                CycleReport::Published(Box::new(Artifact::published(
                    companion,
                    executable,
                    unimported_test_files,
                    source_snapshot.clone(),
                    error_format,
                    OwnedCycleObservations {
                        accepted_reads,
                        attempted_reads,
                        watch_inputs,
                    },
                )))
            }
            Err(PublishError::InputsChanged) => {
                CycleReport::Superseded(Supersession::AtPublication)
            }
            Err(error) => {
                diagnostics.print_error(&error.into_compile_error());
                CycleReport::Failed
            }
        }
    }
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

    fn published(
        (): (),
        executable: PublishedExecutable,
        _: Option<Result<Vec<UnimportedTestFile>, CompileErrors>>,
        _: SourceSnapshot,
        _: ErrorFormat,
        observations: OwnedCycleObservations,
    ) -> PublishedExecutable {
        consume_cycle_observations(observations);
        executable
    }
}

/// What a test image carries beside its linked bytes (ADR-0083 §3).
pub(crate) struct TestImageCompanion {
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
            diagnostics.print_prepared_errors(&companion.failure_diagnostics);
        }
    }

    fn prepare(mut self) -> Self {
        let mut batches = vec![&self.failure_diagnostics];
        batches.extend(self.compile_failures.iter().map(|failure| &failure.errors));
        let mut prepared = with_import_migration_helps_batches(&batches).into_iter();
        self.failure_diagnostics = prepared
            .next()
            .expect("aggregate failure diagnostics have one prepared batch");
        for failure in &mut self.compile_failures {
            failure.errors = prepared
                .next()
                .expect("every test failure has one prepared batch");
        }
        self
    }

    fn published(
        companion: TestImageCompanion,
        _: PublishedExecutable,
        unimported_test_files: Option<Result<Vec<UnimportedTestFile>, CompileErrors>>,
        source_snapshot: SourceSnapshot,
        error_format: ErrorFormat,
        observations: OwnedCycleObservations,
    ) -> PublishedTestImage {
        consume_cycle_observations(observations);
        PublishedTestImage {
            inventory: companion.inventory,
            compile_failures: companion.compile_failures,
            unimported_test_files,
            source_snapshot,
            error_format,
        }
    }
}

fn consume_cycle_observations(
    OwnedCycleObservations {
        accepted_reads,
        attempted_reads,
        watch_inputs,
    }: OwnedCycleObservations,
) {
    // The publication boundary owns these records even when the published
    // artifact does not expose them. Destructuring here makes that ownership
    // explicit and keeps the response from silently borrowing a later host
    // observation.
    drop((accepted_reads, attempted_reads, watch_inputs));
}

/// The one cycle both artifacts run: discovery gate, destination preflight,
/// compile, publish, warnings, publication outcome.
fn drive<Artifact: CycleArtifact>(
    host: &mut FilesystemCompilerHost,
    options: &CompileOptions,
    diagnostics: &DiagnosticOutput<'_>,
    source_path: &str,
    output_path: &Path,
    observation: CycleObservation<'_>,
    announcement: Option<Announcement>,
) -> CycleReport<Artifact::Published> {
    produce::<Artifact>(
        host,
        options,
        diagnostics.format(),
        Some(source_path),
        output_path,
        observation,
    )
    .complete(announcement)
}

fn preflight(
    path: &Path,
    display_path: &Path,
    host: &FilesystemCompilerHost,
    observation: &CycleObservation<'_>,
) -> Result<PublicationDestination, PublishError> {
    match observation {
        CycleObservation::OneShot => preflight_destination_with_display(
            path,
            display_path,
            // Snapshot paths also serve diagnostics and can retain relative
            // display spellings. Only accepted filesystem observations name
            // the source identities independently of the process cwd.
            host.accepted_reads()
                .iter()
                .flat_map(|read| [read.requested_path(), read.canonical_path()]),
        ),
        CycleObservation::Watch { inputs, .. } => {
            preflight_watch_destination_with_display(path, display_path, inputs)
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
    pub(crate) unimported_test_files: Option<Result<Vec<UnimportedTestFile>, CompileErrors>>,
    pub(crate) source_snapshot: SourceSnapshot,
    pub(crate) error_format: ErrorFormat,
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
pub(crate) struct TestCycleRequest<'a> {
    pub(crate) host: &'a mut FilesystemCompilerHost,
    pub(crate) options: &'a CompileOptions,
    pub(crate) error_format: ErrorFormat,
    pub(crate) candidates: Option<&'a TestCandidateInventory>,
    /// Where the linked image is staged. Always inside the run's own private
    /// directory, never a path the user named: `rue test` refuses `-o` for
    /// exactly this reason, so no cycle here can publish over a user artifact.
    pub(crate) image_path: &'a Path,
    pub(crate) observation: CycleObservation<'a>,
}

/// The outcome of a test-image cycle: [`CycleReport`] carrying the image's
/// inventory instead of the executable's metrics.
pub(crate) type TestCycleReport = CycleReport<PublishedTestImage>;

pub(crate) fn produce_test_cycle(request: TestCycleRequest<'_>) -> OwnedCycleResponse<TestImage> {
    let TestCycleRequest {
        host,
        options,
        error_format,
        image_path,
        candidates,
        observation,
        ..
    } = request;
    let mut response =
        produce::<TestImage>(host, options, error_format, None, image_path, observation);
    let report = if matches!(response.result, OwnedCycleResult::Ready { .. }) {
        candidates.map(|candidates| {
            host.unimported_test_files(candidates)
                .map_err(|errors| with_import_migration_helps(&errors))
        })
    } else {
        None
    };
    response.attach_unimported_test_files(report);
    response
}

#[cfg(test)]
#[path = "compile_owned_tests.rs"]
mod owned_tests;
