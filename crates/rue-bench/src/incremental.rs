//! Canonical retained-session measurement engine for ADR-0068.
//!
//! Maintained fixture declarations live outside this module. The engine owns
//! isolation, exact mutation, endpoint timing, structural projection, and the
//! fresh-session oracle so later suites cannot accidentally redefine a sample.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rue_compiler::unstable::{EndpointQueryWork, EndpointWork, MetricsSnapshot};
use rue_compiler::{CompileErrors, CompileOptions, CompileOutput, CompileWarning};
use rue_driver::{FilesystemCompilerHost, HostOpenRequest, SourceLoadError};
use rue_perf_schema::{
    EditEndpoints, EditManifest, EditOutcome, EditReport, EditSample, ExpectedEditOutcome,
    FailureStage, OracleComparison, OutcomeIdentity, OutcomeKind, PhaseWork, RetainedGauges,
    SourceShape, StructuralWork, TransformationIdentity, WorkerMode, canonical_json,
    validate_edit_report,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub(crate) enum EditOperation {
    Reobserve {
        id: String,
    },
    Replace {
        id: String,
        logical_file: PathBuf,
        before: String,
        after: String,
    },
}

impl EditOperation {
    pub(crate) fn identity(&self) -> TransformationIdentity {
        match self {
            Self::Reobserve { id } => TransformationIdentity::Reobserve { id: id.clone() },
            Self::Replace {
                id,
                logical_file,
                before,
                after,
            } => TransformationIdentity::Replace {
                id: id.clone(),
                logical_file: logical_file.to_string_lossy().into_owned(),
                before_sha256: sha256(before.as_bytes()),
                after_sha256: sha256(after.as_bytes()),
            },
        }
    }
}

pub(crate) struct SampleRequest<'a> {
    pub fixture_root: &'a Path,
    pub root_source: &'a Path,
    pub source_manifest: Option<&'a Path>,
    pub std_root: Option<&'a Path>,
    pub options: &'a CompileOptions,
    pub worker_mode: WorkerMode,
    pub expected_outcome: ExpectedEditOutcome,
    pub operation: &'a EditOperation,
    pub sample_index: u32,
    pub session_id: String,
    pub collection_order: u32,
}

pub(crate) struct SampleObservation {
    pub shape: SourceShape,
    pub sample: EditSample,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportStatus {
    Valid,
    Diverged,
}

impl ReportStatus {
    /// Divergence is publishable evidence but must still fail the collection
    /// step, distinct from malformed input that produces no report.
    pub(crate) const fn exit_code(self) -> u8 {
        match self {
            Self::Valid => 0,
            Self::Diverged => 3,
        }
    }
}

/// Structural invalidity produces no latency artifact. A compiler divergence
/// is serialized as explicit failing evidence for the workflow to publish.
pub(crate) fn write_validated_report(
    path: &Path,
    manifest: &EditManifest,
    report: &EditReport,
) -> Result<ReportStatus, String> {
    let validation = validate_edit_report(manifest, report);
    if !validation.errors.is_empty() {
        let details = validation
            .errors
            .iter()
            .map(|finding| format!("{}: {}", finding.path, finding.detail))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "incremental report is structurally invalid: {details}"
        ));
    }
    let encoded = canonical_json(report)
        .map_err(|error| format!("could not serialize incremental report: {error}"))?;
    fs::write(path, encoded)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(if validation.divergences.is_empty() {
        ReportStatus::Valid
    } else {
        ReportStatus::Diverged
    })
}

pub(crate) fn measure_sample(request: SampleRequest<'_>) -> Result<SampleObservation, String> {
    let resolved_workers = resolve_workers(request.worker_mode);
    rue_compiler::configure_thread_pool(resolved_workers as usize);

    let isolated = tempfile::tempdir()
        .map_err(|error| format!("could not create isolated fixture: {error}"))?;
    copy_tree(request.fixture_root, isolated.path())?;
    let root = isolated_path(isolated.path(), request.root_source)?;
    let manifest = request
        .source_manifest
        .map(|path| isolated_path(isolated.path(), path))
        .transpose()?;

    let mut warm = open_host(&root, manifest.as_deref(), request.std_root)?;
    warm.acquire_reached_toolchain_modules(request.options)
        .map_err(source_load_error)?;
    let baseline = run_success(&mut warm, request.options)
        .map_err(|errors| format!("revision A did not reach runnable-ready: {errors}"))?;
    let baseline_metrics = warm.unstable_metrics();
    let one_shot = baseline.unstable_metrics();
    let shape = SourceShape {
        files: one_shot.files as u64,
        modules: one_shot.parsed.modules_considered as u64,
        bytes: one_shot.bytes as u64,
        lines: one_shot.lines as u64,
        tokens: one_shot.tokens as u64,
        functions: one_shot.semantic.cfg.functions_considered as u64,
    };

    apply_operation(isolated.path(), request.operation)?;
    let started = Instant::now();
    warm.reobserve().map_err(source_load_error)?;
    warm.acquire_reached_toolchain_modules(request.options)
        .map_err(source_load_error)?;

    let (outcome, endpoint_work) = match warm.codegen_ready(request.options) {
        Ok(codegen) => {
            let codegen_ready_ns = elapsed_ns(started);
            if request.expected_outcome == ExpectedEditOutcome::Diagnostics {
                return Err(
                    "diagnostics scenario unexpectedly reached the codegen-ready endpoint".into(),
                );
            }
            let codegen_work = codegen.unstable_work();
            match warm.objects_ready(codegen) {
                Ok(objects) => {
                    let objects_ready_ns = elapsed_ns(started);
                    let work = objects.unstable_work();
                    match warm.runnable_ready(objects) {
                        Ok(output) => {
                            let runnable_ready_ns = elapsed_ns(started);
                            (
                                success_outcome(
                                    output,
                                    EditEndpoints {
                                        codegen_ready_ns,
                                        objects_ready_ns,
                                        runnable_ready_ns,
                                    },
                                ),
                                work,
                            )
                        }
                        Err(errors) => (unexpected_failure(FailureStage::Linking, &errors), work),
                    }
                }
                Err(errors) => (
                    unexpected_failure(FailureStage::Objects, &errors),
                    codegen_work,
                ),
            }
        }
        Err(errors) if request.expected_outcome == ExpectedEditOutcome::Diagnostics => (
            EditOutcome::ExpectedDiagnostics {
                diagnostics_ready_ns: elapsed_ns(started),
                diagnostics: errors_identity(&errors),
                warnings: sha256(&[]),
            },
            EndpointWork::default(),
        ),
        Err(errors) => (
            unexpected_failure(FailureStage::Codegen, &errors),
            EndpointWork::default(),
        ),
    };
    let endpoint_metrics = warm.unstable_metrics();
    let work = structural_work(
        &baseline_metrics,
        &endpoint_metrics,
        endpoint_work,
        &outcome,
    );
    let retention = retained_gauges(endpoint_metrics);
    let warm_identity = outcome_identity(&outcome);

    // The correctness oracle owns a new session over revision B and is wholly
    // outside the measured interval.
    let mut fresh = open_host(&root, manifest.as_deref(), request.std_root)?;
    fresh
        .acquire_reached_toolchain_modules(request.options)
        .map_err(source_load_error)?;
    let fresh_identity = fresh_identity(&mut fresh, request.options, request.expected_outcome);
    let oracle = compare_identities(warm_identity, fresh_identity);

    Ok(SampleObservation {
        shape,
        sample: EditSample {
            sample_index: request.sample_index,
            session_id: request.session_id,
            collection_order: request.collection_order,
            resolved_workers,
            transformation: request.operation.identity(),
            outcome,
            work,
            retention,
            oracle,
        },
    })
}

fn open_host(
    root: &Path,
    manifest: Option<&Path>,
    std_root: Option<&Path>,
) -> Result<FilesystemCompilerHost, String> {
    FilesystemCompilerHost::open(HostOpenRequest {
        root_source: root
            .to_str()
            .ok_or_else(|| format!("fixture root is not UTF-8: {}", root.display()))?,
        source_manifest_path: manifest.and_then(Path::to_str),
        std_root,
    })
    .map_err(source_load_error)
}

fn run_success(
    host: &mut FilesystemCompilerHost,
    options: &CompileOptions,
) -> Result<CompileOutput, CompileErrors> {
    let codegen = host.codegen_ready(options)?;
    let objects = host.objects_ready(codegen)?;
    host.runnable_ready(objects)
}

fn fresh_identity(
    host: &mut FilesystemCompilerHost,
    options: &CompileOptions,
    expected: ExpectedEditOutcome,
) -> OutcomeIdentity {
    match run_success(host, options) {
        Ok(output) if expected == ExpectedEditOutcome::Success => output_identity(&output),
        Ok(output) => OutcomeIdentity {
            kind: OutcomeKind::UnexpectedFailure,
            diagnostics: sha256(&[]),
            warnings: warnings_identity(&output.warnings),
            executable: Some(sha256(&output.elf)),
        },
        Err(errors) if expected == ExpectedEditOutcome::Diagnostics => OutcomeIdentity {
            kind: OutcomeKind::Diagnostics,
            diagnostics: errors_identity(&errors),
            warnings: sha256(&[]),
            executable: None,
        },
        Err(errors) => OutcomeIdentity {
            kind: OutcomeKind::UnexpectedFailure,
            diagnostics: errors_identity(&errors),
            warnings: sha256(&[]),
            executable: None,
        },
    }
}

fn success_outcome(output: CompileOutput, endpoints: EditEndpoints) -> EditOutcome {
    EditOutcome::Success {
        endpoints,
        diagnostics: sha256(&[]),
        warnings: warnings_identity(&output.warnings),
        executable: sha256(&output.elf),
    }
}

fn unexpected_failure(stage: FailureStage, errors: &CompileErrors) -> EditOutcome {
    EditOutcome::UnexpectedFailure {
        stage,
        diagnostics: errors_identity(errors),
        warnings: sha256(&[]),
    }
}

fn output_identity(output: &CompileOutput) -> OutcomeIdentity {
    OutcomeIdentity {
        kind: OutcomeKind::Success,
        diagnostics: sha256(&[]),
        warnings: warnings_identity(&output.warnings),
        executable: Some(sha256(&output.elf)),
    }
}

fn outcome_identity(outcome: &EditOutcome) -> OutcomeIdentity {
    match outcome {
        EditOutcome::Success {
            diagnostics,
            warnings,
            executable,
            ..
        } => OutcomeIdentity {
            kind: OutcomeKind::Success,
            diagnostics: diagnostics.clone(),
            warnings: warnings.clone(),
            executable: Some(executable.clone()),
        },
        EditOutcome::ExpectedDiagnostics {
            diagnostics,
            warnings,
            ..
        } => OutcomeIdentity {
            kind: OutcomeKind::Diagnostics,
            diagnostics: diagnostics.clone(),
            warnings: warnings.clone(),
            executable: None,
        },
        EditOutcome::UnexpectedFailure {
            diagnostics,
            warnings,
            ..
        } => OutcomeIdentity {
            kind: OutcomeKind::UnexpectedFailure,
            diagnostics: diagnostics.clone(),
            warnings: warnings.clone(),
            executable: None,
        },
    }
}

fn compare_identities(warm: OutcomeIdentity, fresh: OutcomeIdentity) -> OracleComparison {
    if warm == fresh {
        OracleComparison::Matched { warm, fresh }
    } else {
        let first_difference = if warm.kind != fresh.kind {
            format!(
                "outcome kind differs: warm {:?}, fresh {:?}",
                warm.kind, fresh.kind
            )
        } else if warm.diagnostics != fresh.diagnostics {
            "diagnostic identity differs".to_string()
        } else if warm.warnings != fresh.warnings {
            "warning identity differs".to_string()
        } else {
            "executable identity differs".to_string()
        };
        OracleComparison::Diverged {
            warm,
            fresh,
            first_difference,
        }
    }
}

fn structural_work(
    before: &MetricsSnapshot,
    after: &MetricsSnapshot,
    endpoint: EndpointWork,
    outcome: &EditOutcome,
) -> StructuralWork {
    let parse = after.parse_metrics();
    let merge = query_delta(before.merge(), after.merge());
    let rir = query_delta(before.rir(), after.rir());
    let semantic = query_delta(before.semantic(), after.semantic());
    let imports = query_delta(before.imports(), after.imports());
    let mut program = merge;
    add_work(&mut program, &rir);
    program.invalidated = delta(
        before.downstream_invalidations(),
        after.downstream_invalidations(),
    );
    program.evicted = delta(
        before.retention().query_evictions,
        after.retention().query_evictions,
    );
    StructuralWork {
        source_observation: PhaseWork {
            computed: delta(before.updates(), after.updates()),
            ..Default::default()
        },
        import_discovery: imports,
        parsing: PhaseWork {
            computed: parse.modules_reparsed as u64,
            reused: parse.modules_reused as u64,
            invalidated: parse.modules_rebound as u64,
            ..Default::default()
        },
        program,
        semantic: PhaseWork {
            invalidated: delta(
                before.semantic_entries_invalidated(),
                after.semantic_entries_invalidated(),
            ),
            ..semantic
        },
        cfg: endpoint_phase(endpoint.cfg),
        codegen: endpoint_phase(endpoint.codegen),
        object_projection: endpoint_phase(endpoint.object_projection),
        linking: PhaseWork {
            computed: u64::from(matches!(outcome, EditOutcome::Success { .. })),
            ..Default::default()
        },
    }
}

fn query_delta(
    before: rue_compiler::unstable::QueryMetrics,
    after: rue_compiler::unstable::QueryMetrics,
) -> PhaseWork {
    PhaseWork {
        computed: delta(before.executions, after.executions),
        reused: delta(before.reuses, after.reuses),
        ..Default::default()
    }
}

fn endpoint_phase(work: EndpointQueryWork) -> PhaseWork {
    PhaseWork {
        computed: work.computed as u64,
        reused: work.reused as u64,
        joined: work.joined as u64,
        canceled: work.canceled as u64,
        ..Default::default()
    }
}

fn add_work(target: &mut PhaseWork, other: &PhaseWork) {
    target.computed += other.computed;
    target.reused += other.reused;
    target.joined += other.joined;
    target.invalidated += other.invalidated;
    target.canceled += other.canceled;
    target.evicted += other.evicted;
}

fn retained_gauges(metrics: MetricsSnapshot) -> RetainedGauges {
    let retention = metrics.retention();
    let input_observations = retention.retained_module_input_views
        + retention.retained_module_source_stamps
        + retention.retained_import_input_views
        + retention.retained_import_context_stamps
        + retention.retained_import_topology_stamps
        + retention.retained_import_provenance_stamps
        + retention.retained_import_observation_stamps;
    let observations = retention.dependency_pins.saturating_add(input_observations);
    RetainedGauges {
        current_bytes: retention.retained_bytes as u64,
        peak_bytes: retention.peak_retained_bytes.max(retention.retained_bytes) as u64,
        soft_budget_bytes: retention.retained_byte_budget as u64,
        protected_overflow_bytes: retention
            .retained_bytes
            .saturating_sub(retention.retained_byte_budget)
            as u64,
        dependency_observations: retention.dependency_pins as u64,
        input_observations: input_observations as u64,
        observation_budget: retention.dependency_pin_budget as u64,
        protected_overflow_observations: observations
            .saturating_sub(retention.dependency_pin_budget)
            as u64,
    }
}

fn apply_operation(root: &Path, operation: &EditOperation) -> Result<(), String> {
    let EditOperation::Replace {
        logical_file,
        before,
        after,
        ..
    } = operation
    else {
        return Ok(());
    };
    let path = isolated_path(root, logical_file)?;
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let matches = source.match_indices(before).count();
    if matches != 1 {
        return Err(format!(
            "edit fragment occurs {matches} times in {}; expected exactly once",
            logical_file.display()
        ));
    }
    let edited = source.replacen(before, after, 1);
    fs::write(&path, edited).map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn isolated_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(format!(
            "fixture path must be relative and contained: {}",
            relative.display()
        ));
    }
    Ok(root.join(relative))
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source)
        .map_err(|error| format!("could not read fixture {}: {error}", source.display()))?
    {
        let entry = entry.map_err(|error| format!("could not read fixture entry: {error}"))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("could not create {}: {error}", target.display()))?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("could not copy {}: {error}", target.display()))?;
        } else {
            return Err(format!(
                "fixture contains unsupported non-file entry {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn resolve_workers(mode: WorkerMode) -> u32 {
    match mode {
        WorkerMode::One => 1,
        WorkerMode::Automatic => std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1) as u32,
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    let elapsed = started.elapsed().as_nanos();
    elapsed.min(u128::from(u64::MAX)).max(1) as u64
}

fn delta(before: usize, after: usize) -> u64 {
    after.saturating_sub(before) as u64
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn warnings_identity(warnings: &[CompileWarning]) -> String {
    let mut rendered = String::new();
    for warning in warnings {
        rendered.push_str(&format!(
            "{:?}|{}|{:?}\n",
            warning.span(),
            warning,
            warning.diagnostic()
        ));
    }
    sha256(rendered.as_bytes())
}

fn errors_identity(errors: &CompileErrors) -> String {
    let mut rendered = String::new();
    for error in errors.iter() {
        rendered.push_str(&format!(
            "{:?}|{}|{:?}|{:?}\n",
            error.span(),
            error,
            error.kind,
            error.diagnostic()
        ));
    }
    sha256(rendered.as_bytes())
}

fn source_load_error(error: SourceLoadError) -> String {
    match error {
        SourceLoadError::Message(message) => message,
        SourceLoadError::Compiler { errors, .. } => errors.to_string(),
        SourceLoadError::Toolchain(error) => format!("{error:?}"),
        SourceLoadError::HermeticDenial(error) => format!("{error:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_perf_schema::{
        EDIT_REPORT_SCHEMA_VERSION, EditReportIdentity, EditReportRegime, EnvironmentFingerprint,
        OptimizationSetting, RetentionSequence, RotationRule,
    };

    fn fixture(source: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rue"), source).unwrap();
        dir
    }

    fn request<'a>(
        fixture: &'a Path,
        operation: &'a EditOperation,
        options: &'a CompileOptions,
        expected_outcome: ExpectedEditOutcome,
    ) -> SampleRequest<'a> {
        SampleRequest {
            fixture_root: fixture,
            root_source: Path::new("main.rue"),
            source_manifest: None,
            std_root: None,
            options,
            worker_mode: WorkerMode::One,
            expected_outcome,
            operation,
            sample_index: 0,
            session_id: "test-session".into(),
            collection_order: 0,
        }
    }

    #[test]
    fn successful_edit_records_cumulative_endpoints_and_fresh_oracle() {
        let fixture = fixture("fn main() -> i32 { 1 }\n");
        let operation = EditOperation::Replace {
            id: "body".into(),
            logical_file: "main.rue".into(),
            before: "{ 1 }".into(),
            after: "{ 2 }".into(),
        };
        let options = CompileOptions::default();
        let observation = measure_sample(request(
            fixture.path(),
            &operation,
            &options,
            ExpectedEditOutcome::Success,
        ))
        .unwrap();
        let EditOutcome::Success { endpoints, .. } = observation.sample.outcome else {
            panic!("expected successful outcome")
        };
        assert!(endpoints.codegen_ready_ns <= endpoints.objects_ready_ns);
        assert!(endpoints.objects_ready_ns <= endpoints.runnable_ready_ns);
        assert!(matches!(
            observation.sample.oracle,
            OracleComparison::Matched { .. }
        ));
        assert_eq!(observation.shape.files, 1);
    }

    #[test]
    fn diagnostics_edit_records_only_the_diagnostics_endpoint() {
        let fixture = fixture("fn main() -> i32 { 1 }\n");
        let operation = EditOperation::Replace {
            id: "error".into(),
            logical_file: "main.rue".into(),
            before: "{ 1 }".into(),
            after: "{ missing }".into(),
        };
        let options = CompileOptions::default();
        let observation = measure_sample(request(
            fixture.path(),
            &operation,
            &options,
            ExpectedEditOutcome::Diagnostics,
        ))
        .unwrap();
        assert!(matches!(
            observation.sample.outcome,
            EditOutcome::ExpectedDiagnostics { .. }
        ));
        assert!(matches!(
            observation.sample.oracle,
            OracleComparison::Matched { .. }
        ));
    }

    #[test]
    fn malformed_or_nonunique_edit_produces_no_sample() {
        let fixture = fixture("fn main() -> i32 { 1 + 1 }\n");
        let operation = EditOperation::Replace {
            id: "ambiguous".into(),
            logical_file: "main.rue".into(),
            before: "1".into(),
            after: "2".into(),
        };
        let options = CompileOptions::default();
        let error = measure_sample(request(
            fixture.path(),
            &operation,
            &options,
            ExpectedEditOutcome::Success,
        ))
        .err()
        .unwrap();
        assert!(error.contains("occurs 2 times"));
    }

    #[test]
    fn no_op_reobservation_reuses_backend_work() {
        let fixture = fixture("fn main() -> i32 { 1 }\n");
        let operation = EditOperation::Reobserve { id: "noop".into() };
        let options = CompileOptions::default();
        let observation = measure_sample(request(
            fixture.path(),
            &operation,
            &options,
            ExpectedEditOutcome::Success,
        ))
        .unwrap();
        assert_eq!(observation.sample.work.codegen.computed, 0);
        assert!(observation.sample.work.codegen.reused > 0);
        assert_eq!(observation.sample.work.object_projection.computed, 0);
        assert!(observation.sample.work.object_projection.reused > 0);
    }

    #[test]
    fn divergence_is_publishable_but_maps_to_a_nonzero_status() {
        let warm = OutcomeIdentity {
            kind: OutcomeKind::Success,
            diagnostics: sha256(&[]),
            warnings: sha256(&[]),
            executable: Some(sha256(b"warm")),
        };
        let fresh = OutcomeIdentity {
            executable: Some(sha256(b"fresh")),
            ..warm.clone()
        };
        assert!(matches!(
            compare_identities(warm, fresh),
            OracleComparison::Diverged { .. }
        ));
        assert_ne!(ReportStatus::Diverged.exit_code(), 0);
    }

    #[test]
    fn structurally_invalid_report_is_not_serialized() {
        let manifest_text = fs::read_to_string("performance/incremental.toml")
            .expect("checked-in incremental manifest is readable");
        let manifest = EditManifest::parse(&manifest_text).expect("checked-in manifest is valid");
        let report = EditReport {
            schema_version: EDIT_REPORT_SCHEMA_VERSION,
            identity: EditReportIdentity {
                fixture_revision: manifest.fixture_revision,
                commit: "c".repeat(40),
                started_at: "2026-08-07T00:00:00Z".into(),
                finished_at: "2026-08-07T01:00:00Z".into(),
                target: manifest.target.clone(),
                environment: EnvironmentFingerprint {
                    runner_label: "local".into(),
                    runner_image: "local".into(),
                    runner_image_version: "local".into(),
                    cpu_model: "local".into(),
                    core_count: 1,
                    memory_bytes: 1,
                    kernel_version: "local".into(),
                    os_version: "local".into(),
                    architecture: "aarch64".into(),
                },
            },
            regime: EditReportRegime {
                compiler_state: "retained_session".into(),
                os_page_cache: "uncontrolled".into(),
                samples_per_row: manifest.samples_per_row,
                retention_revisions: manifest.retention_revisions,
                rotation: RotationRule::LeftBySample,
                optimization: OptimizationSetting::Default,
                compiler_args: Vec::new(),
            },
            rows: Vec::new(),
            retention: RetentionSequence {
                workload: "mosaic".into(),
                worker_mode: WorkerMode::One,
                resolved_workers: 1,
                revisions: Vec::new(),
            },
        };
        let output = tempfile::tempdir().unwrap().path().join("report.json");
        assert!(write_validated_report(&output, &manifest, &report).is_err());
        assert!(!output.exists());
    }
}
