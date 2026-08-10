//! Canonical retained-session measurement engine for ADR-0068.
//!
//! Maintained fixture declarations live outside this module. The engine owns
//! isolation, exact mutation, endpoint timing, structural projection, and the
//! fresh-session oracle so later suites cannot accidentally redefine a sample.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rue_compiler::unstable::{
    EndpointQueryWork, EndpointWork, MetricsSnapshot, QueryRuntimeMetrics, QueryValidationMetrics,
};
use rue_compiler::{CompileErrors, CompileOptions, CompileOutput, CompileWarning, OptLevel};
use rue_driver::{FilesystemCompilerHost, HostOpenRequest, SourceLoadError};
use rue_perf_schema::{
    EDIT_REPORT_SCHEMA_VERSION, EditEndpoints, EditManifest, EditOutcome, EditReport,
    EditReportIdentity, EditReportRegime, EditRow, EditSample, EditScenario, ExpectedEditOutcome,
    FailureStage, OptimizationSetting, OracleComparison, OutcomeIdentity, OutcomeKind, PhaseWork,
    RetainedGauges, RetentionSequence, RetentionStep, RetentionStepOutcome, SourceShape,
    StructuralWork, TransformationIdentity, ValidationWork as ReportValidationWork, WorkerMode,
    canonical_json, derive_edit_report, render_edit_report_markdown, validate_edit_report,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixtureManifest {
    schema_version: u32,
    fixture_revision: u32,
    workloads: Vec<FixtureWorkload>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixtureWorkload {
    id: String,
    fixture_root: PathBuf,
    root_source: PathBuf,
    overlays: Vec<OverlayOperation>,
    edits: Vec<DeclaredEdit>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum OverlayOperation {
    Replace {
        logical_file: PathBuf,
        before: String,
        after: String,
    },
    Create {
        logical_file: PathBuf,
        content: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct DeclaredEdit {
    scenario: EditScenario,
    #[serde(flatten)]
    operation: DeclaredOperation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DeclaredOperation {
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

impl FixtureManifest {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text)
            .map_err(|error| format!("invalid incremental fixture manifest: {error}"))
    }

    pub(crate) fn validate(&self, manifest: &EditManifest, repo_root: &Path) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported incremental fixture schema version {}",
                self.schema_version
            ));
        }
        if self.fixture_revision != manifest.fixture_revision {
            return Err(format!(
                "fixture revision {} differs from manifest revision {}",
                self.fixture_revision, manifest.fixture_revision
            ));
        }
        let declared_ids: Vec<_> = self.workloads.iter().map(|workload| &workload.id).collect();
        let manifest_ids: Vec<_> = manifest
            .workloads
            .iter()
            .chain(std::iter::once(&manifest.retention_workload))
            .map(|workload| &workload.id)
            .collect();
        if declared_ids != manifest_ids {
            return Err("fixture workloads differ from the incremental manifest".into());
        }

        let mut edit_ids = BTreeSet::new();
        for workload in &self.workloads {
            validate_relative_path(&workload.fixture_root)?;
            validate_relative_path(&workload.root_source)?;
            let fixture_root = repo_root.join(&workload.fixture_root);
            if !fixture_root.join(&workload.root_source).is_file() {
                return Err(format!(
                    "fixture root source does not exist: {}",
                    fixture_root.join(&workload.root_source).display()
                ));
            }
            let source = workload.fixture_root.join(&workload.root_source);
            let manifest_workload = manifest
                .workloads
                .iter()
                .chain(std::iter::once(&manifest.retention_workload))
                .find(|entry| entry.id == workload.id)
                .expect("workload ids were compared above");
            if source != Path::new(&manifest_workload.source) {
                return Err(format!(
                    "fixture {:?} root does not match the incremental manifest source",
                    workload.id
                ));
            }
            let scenarios: Vec<_> = workload.edits.iter().map(|edit| edit.scenario).collect();
            if scenarios != EditScenario::ALL {
                return Err(format!(
                    "fixture {:?} must declare the exact scenario matrix",
                    workload.id
                ));
            }
            for edit in &workload.edits {
                let operation = edit.operation();
                if !edit_ids.insert(operation.id().to_string()) {
                    return Err(format!(
                        "duplicate incremental edit id {:?}",
                        operation.id()
                    ));
                }
                if matches!(operation, EditOperation::Reobserve { .. })
                    != (edit.scenario == EditScenario::NoOpReobservation)
                {
                    return Err(format!(
                        "fixture {:?} uses the wrong operation kind for {}",
                        workload.id,
                        edit.scenario.wire_name()
                    ));
                }
            }

            // Validate overlays and every A/B edit against an actual isolated
            // copy. Reversing each edit also proves the declared operation can
            // restore the exact revision-A fixture.
            let isolated = tempfile::tempdir()
                .map_err(|error| format!("could not create fixture validation copy: {error}"))?;
            copy_tree(&fixture_root, isolated.path())?;
            apply_overlays(isolated.path(), &workload.overlays)?;
            for edit in &workload.edits {
                let operation = edit.operation();
                apply_operation(isolated.path(), &operation)?;
                if let Some(reverse) = operation.reverse() {
                    apply_operation(isolated.path(), &reverse)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn workload(&self, id: &str) -> &FixtureWorkload {
        self.workloads
            .iter()
            .find(|workload| workload.id == id)
            .expect("validated fixture manifest covers every workload")
    }
}

impl FixtureWorkload {
    fn edit(&self, scenario: EditScenario) -> EditOperation {
        self.edits
            .iter()
            .find(|edit| edit.scenario == scenario)
            .expect("validated fixture covers every scenario")
            .operation()
    }
}

impl DeclaredEdit {
    fn operation(&self) -> EditOperation {
        match &self.operation {
            DeclaredOperation::Reobserve { id } => EditOperation::Reobserve { id: id.clone() },
            DeclaredOperation::Replace {
                id,
                logical_file,
                before,
                after,
            } => EditOperation::Replace {
                id: id.clone(),
                logical_file: logical_file.clone(),
                before: before.clone(),
                after: after.clone(),
            },
        }
    }
}

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
    fn id(&self) -> &str {
        match self {
            Self::Reobserve { id } | Self::Replace { id, .. } => id,
        }
    }

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

    fn reverse(&self) -> Option<Self> {
        match self {
            Self::Reobserve { .. } => None,
            Self::Replace {
                id,
                logical_file,
                before,
                after,
            } => Some(Self::Replace {
                id: format!("{id}-reverse"),
                logical_file: logical_file.clone(),
                before: after.clone(),
                after: before.clone(),
            }),
        }
    }
}

pub(crate) struct SampleRequest<'a> {
    pub fixture_root: &'a Path,
    pub root_source: &'a Path,
    pub baseline_overlays: &'a [OverlayOperation],
    pub source_manifest: Option<&'a Path>,
    pub std_root: Option<&'a Path>,
    pub options: &'a CompileOptions,
    pub worker_mode: WorkerMode,
    pub expected_outcome: ExpectedEditOutcome,
    pub operation: &'a EditOperation,
    pub sample_index: u32,
    pub session_id: String,
    pub collection_order: u32,
    pub fresh_oracle: Option<&'a OutcomeIdentity>,
}

pub(crate) struct SampleObservation {
    pub shape: SourceShape,
    pub sample: EditSample,
    collection_timing: CollectionTiming,
}

#[derive(Debug, Clone, Copy, Default)]
struct CollectionTiming {
    setup_and_baseline: Duration,
    warm: Duration,
    fresh_oracle: Duration,
    total: Duration,
    query_runtime: QueryRuntimeMetrics,
}

impl CollectionTiming {
    fn add(&mut self, other: Self) {
        self.setup_and_baseline += other.setup_and_baseline;
        self.warm += other.warm;
        self.fresh_oracle += other.fresh_oracle;
        self.total += other.total;
        self.query_runtime
            .validation
            .saturating_add_assign(other.query_runtime.validation);
        self.query_runtime.retention_enforcements = self
            .query_runtime
            .retention_enforcements
            .saturating_add(other.query_runtime.retention_enforcements);
        self.query_runtime.retention_scan_entries = self
            .query_runtime
            .retention_scan_entries
            .saturating_add(other.query_runtime.retention_scan_entries);
    }

    fn other(self) -> Duration {
        self.total.saturating_sub(
            self.setup_and_baseline
                .saturating_add(self.warm)
                .saturating_add(self.fresh_oracle),
        )
    }
}

fn elapsed_ms(duration: Duration) -> u128 {
    duration.as_millis()
}

fn validation_summary(work: QueryValidationMetrics) -> String {
    format!(
        "walks {}/{}/{}/{} total/clean/dirty/abort, edges {}/{}, nodes {}/{}/{} hit/miss/cycle (misses {}/{} certificate/proof-reacquisition), registry {}/{}, demands {}/{}/{}/{}/{}, endorsements {}/{}, query terminal leases {}/{} attempts/duplicates, superseded {}, certificates {}",
        work.traversals,
        work.successful_traversals,
        work.dirty_traversals,
        work.aborted_traversals,
        work.input_observations,
        work.dependency_observations,
        work.memo_hits,
        work.memo_misses,
        work.active_cycle_prunes,
        work.certificate_misses,
        work.proof_reacquisition_misses,
        work.registry_misses,
        work.registry_probes,
        work.demand_reuses,
        work.demand_computes,
        work.demand_joins,
        work.demand_aborts,
        work.demands,
        work.endorsement_hits,
        work.endorsement_probes,
        work.terminal_lease_observations,
        work.duplicate_terminal_lease_observations,
        work.superseded,
        work.certificates_published,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportStatus {
    Valid,
    Diverged,
}

struct IncrementalOptions {
    manifest: PathBuf,
    fixtures: PathBuf,
    commit: String,
    repo_root: PathBuf,
    std_root: Option<PathBuf>,
    output: PathBuf,
}

pub(crate) fn run() -> Result<ReportStatus, String> {
    let options = parse_args()?;
    let manifest_text = fs::read_to_string(&options.manifest)
        .map_err(|error| format!("could not read {}: {error}", options.manifest.display()))?;
    let manifest = EditManifest::parse(&manifest_text).map_err(|error| error.to_string())?;
    let fixture_text = fs::read_to_string(&options.fixtures)
        .map_err(|error| format!("could not read {}: {error}", options.fixtures.display()))?;
    let fixtures = FixtureManifest::parse(&fixture_text)?;
    fixtures.validate(&manifest, &options.repo_root)?;
    if !manifest.compiler_args.is_empty() {
        return Err("incremental compiler_args are not supported by this runner version".into());
    }

    let compile_options = CompileOptions {
        target: manifest
            .target
            .parse()
            .map_err(|error| format!("unsupported incremental target: {error}"))?,
        opt_level: match manifest.optimization {
            OptimizationSetting::Default | OptimizationSetting::O0 => OptLevel::O0,
            OptimizationSetting::O1 => OptLevel::O1,
            OptimizationSetting::O2 => OptLevel::O2,
            OptimizationSetting::O3 => OptLevel::O3,
        },
        ..CompileOptions::default()
    };
    let started_at = crate::utc_timestamp();

    let mut rows = Vec::new();
    for workload in &manifest.workloads {
        for scenario in &manifest.scenarios {
            for worker in &manifest.workers {
                rows.push(EditRow {
                    workload: workload.id.clone(),
                    source: workload.source.clone(),
                    shape: SourceShape {
                        files: 0,
                        modules: 0,
                        bytes: 0,
                        lines: 0,
                        tokens: 0,
                        functions: 0,
                    },
                    scenario: scenario.scenario,
                    worker_mode: worker.mode,
                    samples: Vec::with_capacity(manifest.samples_for(scenario.scenario) as usize),
                });
            }
        }
    }

    let collection_started = Instant::now();
    let matrix_started = Instant::now();
    let mut matrix_timing = CollectionTiming::default();
    let scenario_count = manifest.scenarios.len();
    for (workload_index, workload) in manifest.workloads.iter().enumerate() {
        let fixture = fixtures.workload(&workload.id);
        let fixture_root = options.repo_root.join(&fixture.fixture_root);
        let oracle_started = Instant::now();
        let fresh_oracles = manifest
            .scenarios
            .iter()
            .map(|declaration| {
                eprintln!(
                    "rue-bench: preparing fresh oracle for {} {}",
                    workload.id,
                    declaration.scenario.wire_name()
                );
                fresh_fixture_identity(
                    &fixture_root,
                    &fixture.root_source,
                    &fixture.overlays,
                    options.std_root.as_deref(),
                    &compile_options,
                    Some(&fixture.edit(declaration.scenario)),
                    declaration.expected_outcome,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        matrix_timing.fresh_oracle += oracle_started.elapsed();
        for (worker_index, worker) in manifest.workers.iter().enumerate() {
            for sample_index in 0..manifest.timing_samples_per_row {
                for collection_order in 0..scenario_count {
                    let scenario_index =
                        (collection_order + sample_index as usize) % scenario_count;
                    let declaration = &manifest.scenarios[scenario_index];
                    let sample_count = manifest.samples_for(declaration.scenario);
                    if sample_index >= sample_count {
                        continue;
                    }
                    let operation = fixture.edit(declaration.scenario);
                    eprintln!(
                        "rue-bench: incremental {} {} {} sample {}/{}",
                        workload.id,
                        declaration.scenario.wire_name(),
                        worker.mode.wire_name(),
                        sample_index + 1,
                        sample_count
                    );
                    let observation = measure_sample(SampleRequest {
                        fixture_root: &fixture_root,
                        root_source: &fixture.root_source,
                        baseline_overlays: &fixture.overlays,
                        source_manifest: None,
                        std_root: options.std_root.as_deref(),
                        options: &compile_options,
                        worker_mode: worker.mode,
                        expected_outcome: declaration.expected_outcome,
                        operation: &operation,
                        sample_index,
                        session_id: format!(
                            "{}-{}-{}-{}-{}",
                            options.commit,
                            workload.id,
                            declaration.scenario.wire_name(),
                            worker.mode.wire_name(),
                            sample_index
                        ),
                        collection_order: collection_order as u32,
                        fresh_oracle: Some(&fresh_oracles[scenario_index]),
                    })?;
                    validate_structural_expectation(
                        declaration.scenario,
                        &observation.sample.work,
                    )?;
                    eprintln!(
                        "rue-bench: incremental {} {} {} sample {}/{} completed in {} ms \
                         (setup+baseline {} ms, warm {} ms, fresh-oracle {} ms, other {} ms; \
                         validation {}; retention passes {}, scan entries {})",
                        workload.id,
                        declaration.scenario.wire_name(),
                        worker.mode.wire_name(),
                        sample_index + 1,
                        sample_count,
                        elapsed_ms(observation.collection_timing.total),
                        elapsed_ms(observation.collection_timing.setup_and_baseline),
                        elapsed_ms(observation.collection_timing.warm),
                        elapsed_ms(observation.collection_timing.fresh_oracle),
                        elapsed_ms(observation.collection_timing.other()),
                        validation_summary(observation.collection_timing.query_runtime.validation),
                        observation
                            .collection_timing
                            .query_runtime
                            .retention_enforcements,
                        observation
                            .collection_timing
                            .query_runtime
                            .retention_scan_entries,
                    );
                    matrix_timing.add(observation.collection_timing);
                    let row_index = ((workload_index * scenario_count + scenario_index)
                        * manifest.workers.len())
                        + worker_index;
                    let row = &mut rows[row_index];
                    match row.samples.first() {
                        None => row.shape = observation.shape,
                        Some(_) if row.shape != observation.shape => {
                            return Err(format!(
                                "revision-A source shape changed for {} {} {}",
                                workload.id,
                                declaration.scenario.wire_name(),
                                worker.mode.wire_name()
                            ));
                        }
                        Some(_) => {}
                    }
                    row.samples.push(observation.sample);
                }
            }
        }
    }

    eprintln!(
        "rue-bench: incremental edit matrix completed in {} ms \
         (sample totals: setup+baseline {} ms, warm {} ms, fresh-oracle {} ms, other {} ms)",
        elapsed_ms(matrix_started.elapsed()),
        elapsed_ms(matrix_timing.setup_and_baseline),
        elapsed_ms(matrix_timing.warm),
        elapsed_ms(matrix_timing.fresh_oracle),
        elapsed_ms(matrix_timing.other()),
    );

    eprintln!("rue-bench: incremental bounded-retention sequence");
    let retention_started = Instant::now();
    let retention = collect_retention(
        &manifest,
        &fixtures,
        &options.repo_root,
        options.std_root.as_deref(),
        &compile_options,
    )?;
    eprintln!(
        "rue-bench: incremental bounded-retention sequence completed in {} ms",
        elapsed_ms(retention_started.elapsed())
    );
    eprintln!(
        "rue-bench: incremental collection completed in {} ms",
        elapsed_ms(collection_started.elapsed())
    );
    let report = EditReport {
        schema_version: EDIT_REPORT_SCHEMA_VERSION,
        identity: EditReportIdentity {
            fixture_revision: manifest.fixture_revision,
            commit: options.commit,
            started_at,
            finished_at: crate::utc_timestamp(),
            target: manifest.target.clone(),
            environment: crate::environment::fingerprint(),
        },
        regime: EditReportRegime {
            compiler_state: "retained_session".into(),
            os_page_cache: "uncontrolled".into(),
            timing_samples_per_row: manifest.timing_samples_per_row,
            structural_samples_per_row: manifest.structural_samples_per_row,
            retention_revisions: manifest.retention_revisions,
            rotation: manifest.rotation,
            optimization: manifest.optimization,
            compiler_args: manifest.compiler_args.clone(),
        },
        rows,
        retention,
    };
    if let Some(parent) = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    write_validated_report(&options.output, &manifest, &report)
}

fn parse_args() -> Result<IncrementalOptions, String> {
    let mut manifest = None;
    let mut fixtures = None;
    let mut commit = None;
    let mut repo_root = None;
    let mut std_root = None;
    let mut output = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match flag.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value()?)),
            "--fixtures" => fixtures = Some(PathBuf::from(value()?)),
            "--commit" => commit = Some(value()?),
            "--repo-root" => repo_root = Some(PathBuf::from(value()?)),
            "--std-root" => std_root = Some(PathBuf::from(value()?)),
            "--out" => output = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognized incremental argument {other:?}")),
        }
    }
    let commit = commit.ok_or("incremental requires --commit <revision>")?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("incremental commit must be a 40-character hexadecimal hash".into());
    }
    let repo_root = repo_root.unwrap_or_else(|| PathBuf::from("."));
    let std_root = std_root.or_else(|| {
        let candidate = repo_root.join("std");
        candidate.is_dir().then_some(candidate)
    });
    Ok(IncrementalOptions {
        manifest: manifest.unwrap_or_else(|| repo_root.join("performance/incremental.toml")),
        fixtures: fixtures
            .unwrap_or_else(|| repo_root.join("performance/incremental-fixtures.toml")),
        commit,
        repo_root,
        std_root,
        output: output.ok_or("incremental requires --out <path>")?,
    })
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
    let derived = derive_edit_report(manifest, report).map_err(|findings| {
        let details = findings
            .iter()
            .map(|finding| format!("{}: {}", finding.path, finding.detail))
            .collect::<Vec<_>>()
            .join("; ");
        format!("could not derive incremental report: {details}")
    })?;
    let markdown = render_edit_report_markdown(&derived);
    fs::write(path, encoded)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    let markdown_path = path.with_extension("md");
    fs::write(&markdown_path, markdown)
        .map_err(|error| format!("could not write {}: {error}", markdown_path.display()))?;
    eprintln!(
        "rue-bench: wrote {} and {}",
        path.display(),
        markdown_path.display()
    );
    Ok(if validation.divergences.is_empty() {
        ReportStatus::Valid
    } else {
        ReportStatus::Diverged
    })
}

pub(crate) fn measure_sample(request: SampleRequest<'_>) -> Result<SampleObservation, String> {
    let total_started = Instant::now();
    let resolved_workers = resolve_workers(request.worker_mode);
    rue_compiler::configure_thread_pool(resolved_workers as usize);

    let isolated = tempfile::tempdir()
        .map_err(|error| format!("could not create isolated fixture: {error}"))?;
    copy_tree(request.fixture_root, isolated.path())?;
    apply_overlays(isolated.path(), request.baseline_overlays)?;
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
        // The accepted filesystem closure has one source file per Rue module.
        // `modules_considered` is a work counter and may be zero when retained
        // query terminals satisfy the baseline without running the retired
        // whole-program parse projection.
        modules: one_shot.files as u64,
        bytes: one_shot.bytes as u64,
        lines: one_shot.lines as u64,
        tokens: one_shot.tokens as u64,
        functions: one_shot.semantic.cfg.functions_considered as u64,
    };

    apply_operation(isolated.path(), request.operation)?;
    let setup_and_baseline = total_started.elapsed();
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
    let query_runtime = query_runtime_delta(
        baseline_metrics.query_runtime(),
        endpoint_metrics.query_runtime(),
    );
    let validation = report_validation_work(query_runtime.validation);
    let work = structural_work(
        &baseline_metrics,
        &endpoint_metrics,
        endpoint_work,
        &outcome,
    );
    let retention = retained_gauges(endpoint_metrics);
    let warm_identity = outcome_identity(&outcome);
    let warm = match &outcome {
        EditOutcome::Success { endpoints, .. } => Duration::from_nanos(endpoints.runnable_ready_ns),
        EditOutcome::ExpectedDiagnostics {
            diagnostics_ready_ns,
            ..
        } => Duration::from_nanos(*diagnostics_ready_ns),
        EditOutcome::UnexpectedFailure { .. } => started.elapsed(),
    };

    // The correctness oracle is wholly outside the measured interval. The
    // report runner precomputes one fresh identity per exact fixture state;
    // focused callers may still request an inline independent oracle.
    let oracle_started = Instant::now();
    let fresh_identity = match request.fresh_oracle {
        Some(identity) => identity.clone(),
        None => {
            let mut fresh = open_host(&root, manifest.as_deref(), request.std_root)?;
            fresh
                .acquire_reached_toolchain_modules(request.options)
                .map_err(source_load_error)?;
            fresh_identity(&mut fresh, request.options, request.expected_outcome)
        }
    };
    let oracle = compare_identities(warm_identity, fresh_identity);
    let fresh_oracle = oracle_started.elapsed();

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
            validation,
            retention,
            oracle,
        },
        collection_timing: CollectionTiming {
            setup_and_baseline,
            warm,
            fresh_oracle,
            total: total_started.elapsed(),
            query_runtime,
        },
    })
}

fn query_runtime_delta(
    before: QueryRuntimeMetrics,
    after: QueryRuntimeMetrics,
) -> QueryRuntimeMetrics {
    QueryRuntimeMetrics {
        validation: after.validation.saturating_sub(before.validation),
        retention_enforcements: after
            .retention_enforcements
            .saturating_sub(before.retention_enforcements),
        retention_scan_entries: after
            .retention_scan_entries
            .saturating_sub(before.retention_scan_entries),
    }
}

fn report_validation_work(work: QueryValidationMetrics) -> ReportValidationWork {
    ReportValidationWork {
        traversals: work.traversals,
        successful_traversals: work.successful_traversals,
        dirty_traversals: work.dirty_traversals,
        aborted_traversals: work.aborted_traversals,
        input_observations: work.input_observations,
        dependency_observations: work.dependency_observations,
        registry_probes: work.registry_probes,
        registry_misses: work.registry_misses,
        node_visits: work.node_visits,
        active_cycle_prunes: work.active_cycle_prunes,
        memo_hits: work.memo_hits,
        memo_misses: work.memo_misses,
        certificate_misses: work.certificate_misses,
        proof_reacquisition_misses: work.proof_reacquisition_misses,
        endorsement_probes: work.endorsement_probes,
        endorsement_hits: work.endorsement_hits,
        terminal_lease_observations: work.terminal_lease_observations,
        duplicate_terminal_lease_observations: work.duplicate_terminal_lease_observations,
        demands: work.demands,
        demand_reuses: work.demand_reuses,
        demand_computes: work.demand_computes,
        demand_joins: work.demand_joins,
        demand_aborts: work.demand_aborts,
        superseded: work.superseded,
        certificates_published: work.certificates_published,
    }
}

fn validate_structural_expectation(
    scenario: EditScenario,
    work: &StructuralWork,
) -> Result<(), String> {
    let fail = |detail: &str| {
        Err(format!(
            "{} structural expectation failed: {detail}; observed {work:?}",
            scenario.wire_name()
        ))
    };
    match scenario {
        EditScenario::NoOpReobservation => {
            if work.import_discovery.computed != 0
                || work.parsing.computed != 0
                || work.program.computed != 0
                || work.semantic.computed != 0
                || work.program.invalidated != 0
                || work.cfg.computed != 0
                || work.codegen.computed != 0
                || work.object_projection.computed != 0
            {
                return fail("a compiler artifact recomputed");
            }
        }
        EditScenario::UnreachableBody => {
            if work.semantic.computed != 0
                || work.cfg.computed != 0
                || work.codegen.computed != 0
                || work.object_projection.computed != 0
            {
                return fail("an unreachable edit recomputed a reached terminal");
            }
        }
        EditScenario::ReachedBodyOnly => {
            if work.cfg.computed != 1
                || work.codegen.computed != 1
                || work.object_projection.computed != 1
            {
                return fail("the edited body cone did not reach every backend endpoint");
            }
        }
        EditScenario::CallableSignature => {
            if work.cfg.computed != 2
                || work.codegen.computed != 2
                || work.object_projection.computed != 2
            {
                return fail("the changed callable and its direct consumer did not recompute");
            }
        }
        EditScenario::LayoutAbi => {
            if work.cfg.computed == 0
                || work.codegen.computed == 0
                || work.object_projection.computed == 0
            {
                return fail("layout and ABI consumers did not recompute");
            }
        }
        EditScenario::ImportSet => {
            if work.source_observation.computed == 0
                || work.program.invalidated == 0
                || work.codegen.computed == 0
                || work.object_projection.computed == 0
            {
                return fail("the changed import cone did not reach its consumers");
            }
        }
        EditScenario::ReachabilityDeletion => {
            if work.cfg.computed != 1
                || work.codegen.computed != 1
                || work.object_projection.computed != 1
                || work.codegen.reused == 0
                || work.object_projection.reused == 0
            {
                return fail("unaffected rooted backend units were not reused");
            }
        }
        EditScenario::ErrorIntroduction => {
            if work.source_observation.computed == 0
                || work.program.invalidated == 0
                || work.cfg.computed != 0
                || work.codegen.computed != 0
                || work.object_projection.computed != 0
                || work.linking.computed != 0
            {
                return fail("diagnostic work escaped into a successful downstream endpoint");
            }
        }
    }
    Ok(())
}

fn collect_retention(
    manifest: &EditManifest,
    fixtures: &FixtureManifest,
    repo_root: &Path,
    std_root: Option<&Path>,
    options: &CompileOptions,
) -> Result<RetentionSequence, String> {
    let workload = &manifest.retention_workload;
    let fixture = fixtures.workload(&workload.id);
    let fixture_root = repo_root.join(&fixture.fixture_root);
    let resolved_workers = resolve_workers(WorkerMode::Automatic);
    rue_compiler::configure_thread_pool(resolved_workers as usize);

    let body = fixture.edit(EditScenario::ReachedBodyOnly);
    let error = fixture.edit(EditScenario::ErrorIntroduction);
    let reachability = fixture.edit(EditScenario::ReachabilityDeletion);
    let imports = fixture.edit(EditScenario::ImportSet);
    let transitions = vec![
        body.clone(),
        body.reverse().expect("body edit is reversible"),
        error.clone(),
        error.reverse().expect("error edit is reversible"),
        reachability.clone(),
        reachability
            .reverse()
            .expect("reachability edit is reversible"),
        imports.clone(),
        imports.reverse().expect("import edit is reversible"),
    ];
    let states = [
        ("reached-body-b", Some(&body), ExpectedEditOutcome::Success),
        ("baseline", None, ExpectedEditOutcome::Success),
        ("error-b", Some(&error), ExpectedEditOutcome::Diagnostics),
        ("baseline", None, ExpectedEditOutcome::Success),
        (
            "reachability-deleted",
            Some(&reachability),
            ExpectedEditOutcome::Success,
        ),
        ("baseline", None, ExpectedEditOutcome::Success),
        ("import-added", Some(&imports), ExpectedEditOutcome::Success),
        ("baseline", None, ExpectedEditOutcome::Success),
    ];
    let baseline_identity = fresh_fixture_identity(
        &fixture_root,
        &fixture.root_source,
        &fixture.overlays,
        std_root,
        options,
        None,
        ExpectedEditOutcome::Success,
    )?;
    let mut fresh_states = Vec::with_capacity(states.len());
    for (_, operation, expected) in &states {
        fresh_states.push(match operation {
            Some(operation) => fresh_fixture_identity(
                &fixture_root,
                &fixture.root_source,
                &fixture.overlays,
                std_root,
                options,
                Some(operation),
                *expected,
            )?,
            None => baseline_identity.clone(),
        });
    }

    let isolated = tempfile::tempdir()
        .map_err(|error| format!("could not create retention fixture: {error}"))?;
    copy_tree(&fixture_root, isolated.path())?;
    apply_overlays(isolated.path(), &fixture.overlays)?;
    let root = isolated_path(isolated.path(), &fixture.root_source)?;
    let mut warm = open_host(&root, None, std_root)?;
    warm.acquire_reached_toolchain_modules(options)
        .map_err(source_load_error)?;
    run_success(&mut warm, options)
        .map_err(|errors| format!("retention baseline did not compile: {errors}"))?;
    let initial_query_evictions = warm.unstable_metrics().retention().query_evictions as u64;

    let mut revisions = Vec::with_capacity(manifest.retention_revisions as usize);
    for revision_index in 0..manifest.retention_revisions {
        let state_index = revision_index as usize % transitions.len();
        apply_operation(isolated.path(), &transitions[state_index])?;
        warm.reobserve().map_err(source_load_error)?;
        warm.acquire_reached_toolchain_modules(options)
            .map_err(source_load_error)?;
        let expected = states[state_index].2;
        let identity = match run_success(&mut warm, options) {
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
        };
        let outcome = if expected == ExpectedEditOutcome::Diagnostics {
            RetentionStepOutcome::Diagnostics {
                identity: identity.clone(),
            }
        } else {
            RetentionStepOutcome::Success {
                identity: identity.clone(),
            }
        };
        revisions.push(RetentionStep {
            revision_index,
            state_id: states[state_index].0.into(),
            outcome,
            oracle: Some(compare_identities(
                identity,
                fresh_states[state_index].clone(),
            )),
            retention: retained_gauges(warm.unstable_metrics()),
        });
        let completed = revision_index + 1;
        if completed % 100 == 0 || completed == manifest.retention_revisions {
            let gauges = &revisions
                .last()
                .expect("the completed retention revision was just recorded")
                .retention;
            eprintln!(
                "rue-bench: incremental bounded-retention revision {completed}/{} \
                 (current {} bytes, peak {} bytes, observations {}/{})",
                manifest.retention_revisions,
                gauges.current_bytes,
                gauges.peak_bytes,
                gauges
                    .dependency_observations
                    .saturating_add(gauges.input_observations),
                gauges.observation_budget,
            );
        }
    }
    let query_evictions = (warm.unstable_metrics().retention().query_evictions as u64)
        .saturating_sub(initial_query_evictions);
    Ok(RetentionSequence {
        workload: workload.id.clone(),
        worker_mode: WorkerMode::Automatic,
        resolved_workers,
        query_evictions,
        revisions,
    })
}

fn fresh_fixture_identity(
    fixture_root: &Path,
    root_source: &Path,
    overlays: &[OverlayOperation],
    std_root: Option<&Path>,
    options: &CompileOptions,
    operation: Option<&EditOperation>,
    expected: ExpectedEditOutcome,
) -> Result<OutcomeIdentity, String> {
    let isolated = tempfile::tempdir()
        .map_err(|error| format!("could not create fresh fixture oracle: {error}"))?;
    copy_tree(fixture_root, isolated.path())?;
    apply_overlays(isolated.path(), overlays)?;
    if let Some(operation) = operation {
        apply_operation(isolated.path(), operation)?;
    }
    let root = isolated_path(isolated.path(), root_source)?;
    let mut host = open_host(&root, None, std_root)?;
    host.acquire_reached_toolchain_modules(options)
        .map_err(source_load_error)?;
    Ok(fresh_identity(&mut host, options, expected))
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

fn apply_overlays(root: &Path, overlays: &[OverlayOperation]) -> Result<(), String> {
    for overlay in overlays {
        match overlay {
            OverlayOperation::Replace {
                logical_file,
                before,
                after,
            } => apply_operation(
                root,
                &EditOperation::Replace {
                    id: "baseline-overlay".into(),
                    logical_file: logical_file.clone(),
                    before: before.clone(),
                    after: after.clone(),
                },
            )?,
            OverlayOperation::Create {
                logical_file,
                content,
            } => {
                let path = isolated_path(root, logical_file)?;
                if path.exists() {
                    return Err(format!(
                        "baseline overlay refuses to overwrite {}",
                        logical_file.display()
                    ));
                }
                let parent = path.parent().expect("an isolated path has a parent");
                if !parent.is_dir() {
                    return Err(format!(
                        "baseline overlay parent does not exist for {}",
                        logical_file.display()
                    ));
                }
                fs::write(&path, content)
                    .map_err(|error| format!("could not create {}: {error}", path.display()))?;
            }
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(format!(
            "fixture path must be relative and contained: {}",
            path.display()
        ));
    }
    Ok(())
}

fn isolated_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    validate_relative_path(relative)?;
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

    #[test]
    fn collection_timing_attributes_only_time_outside_declared_stages() {
        let timing = CollectionTiming {
            setup_and_baseline: Duration::from_millis(20),
            warm: Duration::from_millis(5),
            fresh_oracle: Duration::from_millis(10),
            total: Duration::from_millis(40),
            query_runtime: QueryRuntimeMetrics::default(),
        };
        assert_eq!(timing.other(), Duration::from_millis(5));

        let overlapping_clocks = CollectionTiming {
            total: Duration::from_millis(1),
            ..timing
        };
        assert_eq!(overlapping_clocks.other(), Duration::ZERO);
    }

    fn fixture(source: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rue"), source).unwrap();
        dir
    }

    fn maintained_fixtures() -> (EditManifest, FixtureManifest) {
        let manifest_text = fs::read_to_string("performance/incremental.toml")
            .expect("checked-in incremental manifest is readable");
        let manifest = EditManifest::parse(&manifest_text).expect("checked-in manifest is valid");
        let fixture_text = fs::read_to_string("performance/incremental-fixtures.toml")
            .expect("checked-in fixture manifest is readable");
        let fixtures = FixtureManifest::parse(&fixture_text).expect("fixture manifest parses");
        fixtures
            .validate(&manifest, Path::new("."))
            .expect("maintained fixture operations are exact and reversible");
        (manifest, fixtures)
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
            baseline_overlays: &[],
            source_manifest: None,
            std_root: None,
            options,
            worker_mode: WorkerMode::One,
            expected_outcome,
            operation,
            sample_index: 0,
            session_id: "test-session".into(),
            collection_order: 0,
            fresh_oracle: None,
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
        assert!(
            observation.shape.files > 0
                && observation.shape.modules > 0
                && observation.shape.bytes > 0
                && observation.shape.lines > 0
                && observation.shape.tokens > 0
                && observation.shape.functions > 0,
            "compiler-derived source shape must be complete: {:?}",
            observation.shape
        );
    }

    #[test]
    fn successful_edit_accepts_a_precomputed_fresh_oracle() {
        let fixture = fixture("fn main() -> i32 { 1 }\n");
        let operation = EditOperation::Replace {
            id: "body".into(),
            logical_file: "main.rue".into(),
            before: "{ 1 }".into(),
            after: "{ 2 }".into(),
        };
        let options = CompileOptions::default();
        let fresh_oracle = fresh_fixture_identity(
            fixture.path(),
            Path::new("main.rue"),
            &[],
            None,
            &options,
            Some(&operation),
            ExpectedEditOutcome::Success,
        )
        .unwrap();
        let mut request = request(
            fixture.path(),
            &operation,
            &options,
            ExpectedEditOutcome::Success,
        );
        request.fresh_oracle = Some(&fresh_oracle);

        let observation = measure_sample(request).unwrap();

        assert!(matches!(
            observation.sample.oracle,
            OracleComparison::Matched { .. }
        ));
    }

    #[test]
    fn maintained_fixture_manifest_covers_the_exact_versioned_matrix() {
        let (manifest, fixtures) = maintained_fixtures();
        assert_eq!(fixtures.fixture_revision, manifest.fixture_revision);
        assert_eq!(fixtures.workloads.len(), 3);
        assert!(
            fixtures
                .workload("mosaic")
                .edit(EditScenario::LayoutAbi)
                .id()
                .starts_with("mosaic-")
        );
        assert!(
            fixtures
                .workload("lattice")
                .edit(EditScenario::ImportSet)
                .id()
                .starts_with("lattice-")
        );
        assert!(
            fixtures
                .workload("retention")
                .edit(EditScenario::ReachedBodyOnly)
                .id()
                .starts_with("retention-")
        );
    }

    #[test]
    #[ignore = "full maintained-program warm/fresh verification belongs to the slow measurement lane"]
    fn maintained_transformations_reach_their_declared_outcomes() {
        let (manifest, fixtures) = maintained_fixtures();
        let workload_selector =
            std::env::var("RUE_INCREMENTAL_TEST_WORKLOAD").unwrap_or_else(|_| "all".into());
        let scenario_selector =
            std::env::var("RUE_INCREMENTAL_TEST_SCENARIO").unwrap_or_else(|_| "all".into());
        let options = CompileOptions {
            target: manifest
                .target
                .parse()
                .expect("manifest target is supported"),
            ..CompileOptions::default()
        };
        for workload in manifest
            .workloads
            .iter()
            .filter(|workload| workload_selector == "all" || workload.id == workload_selector)
        {
            let fixture = fixtures.workload(&workload.id);
            let fixture_root = Path::new(".").join(&fixture.fixture_root);
            for declaration in manifest.scenarios.iter().filter(|declaration| {
                scenario_selector == "all" || declaration.scenario.wire_name() == scenario_selector
            }) {
                let operation = fixture.edit(declaration.scenario);
                let observation = measure_sample(SampleRequest {
                    fixture_root: &fixture_root,
                    root_source: &fixture.root_source,
                    baseline_overlays: &fixture.overlays,
                    source_manifest: None,
                    std_root: Some(Path::new("std")),
                    options: &options,
                    worker_mode: WorkerMode::One,
                    expected_outcome: declaration.expected_outcome,
                    operation: &operation,
                    sample_index: 0,
                    session_id: format!(
                        "test-{}-{}",
                        workload.id,
                        declaration.scenario.wire_name()
                    ),
                    collection_order: 0,
                    fresh_oracle: None,
                })
                .unwrap_or_else(|error| {
                    panic!(
                        "{} {} failed: {error}",
                        workload.id,
                        declaration.scenario.wire_name()
                    )
                });
                eprintln!(
                    "rue-bench: maintained fixture {} {} completed in {} ms \
                     (setup+baseline {} ms, warm {} ms, fresh-oracle {} ms, other {} ms; \
                     validation {}; retention passes {}, scan entries {})",
                    workload.id,
                    declaration.scenario.wire_name(),
                    elapsed_ms(observation.collection_timing.total),
                    elapsed_ms(observation.collection_timing.setup_and_baseline),
                    elapsed_ms(observation.collection_timing.warm),
                    elapsed_ms(observation.collection_timing.fresh_oracle),
                    elapsed_ms(observation.collection_timing.other()),
                    validation_summary(observation.collection_timing.query_runtime.validation),
                    observation
                        .collection_timing
                        .query_runtime
                        .retention_enforcements,
                    observation
                        .collection_timing
                        .query_runtime
                        .retention_scan_entries,
                );
                assert!(matches!(
                    observation.sample.oracle,
                    OracleComparison::Matched { .. }
                ));
                validate_structural_expectation(declaration.scenario, &observation.sample.work)
                    .unwrap();
            }
        }
    }

    #[test]
    #[ignore = "the full 1000-revision retention witness belongs to the slow measurement lane"]
    fn maintained_retention_sequence_reaches_every_declared_state() {
        let (mut manifest, fixtures) = maintained_fixtures();
        manifest.retention_revisions = std::env::var("RUE_INCREMENTAL_TEST_RETENTION_REVISIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(16);
        let options = CompileOptions {
            target: manifest
                .target
                .parse()
                .expect("manifest target is supported"),
            ..CompileOptions::default()
        };
        let started = Instant::now();
        let retention = collect_retention(
            &manifest,
            &fixtures,
            Path::new("."),
            Some(Path::new("std")),
            &options,
        )
        .unwrap();
        eprintln!(
            "rue-bench: maintained retention diagnostic completed {} revisions with {} query \
             evictions in {} ms",
            retention.revisions.len(),
            retention.query_evictions,
            elapsed_ms(started.elapsed()),
        );
        assert_eq!(
            retention.revisions.len(),
            manifest.retention_revisions as usize
        );
        assert!(
            retention
                .revisions
                .iter()
                .all(|revision| matches!(revision.oracle, Some(OracleComparison::Matched { .. })))
        );
        if manifest.retention_revisions >= 1_000 {
            assert!(
                retention.query_evictions > 0,
                "the full retention witness must exercise query cleanup"
            );
        }
    }

    #[test]
    fn fixture_manifest_rejects_unknown_edit_fields() {
        let text = fs::read_to_string("performance/incremental-fixtures.toml").unwrap();
        let malformed = text.replacen(
            "id = \"mosaic-no-op-v1\"",
            "id = \"mosaic-no-op-v1\"\nunknown_fixture_field = true",
            1,
        );
        assert!(FixtureManifest::parse(&malformed).is_err());
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
                timing_samples_per_row: manifest.timing_samples_per_row,
                structural_samples_per_row: manifest.structural_samples_per_row,
                retention_revisions: manifest.retention_revisions,
                rotation: RotationRule::LeftBySample,
                optimization: OptimizationSetting::Default,
                compiler_args: Vec::new(),
            },
            rows: Vec::new(),
            retention: RetentionSequence {
                workload: manifest.retention_workload.id.clone(),
                worker_mode: WorkerMode::Automatic,
                resolved_workers: 1,
                query_evictions: 0,
                revisions: Vec::new(),
            },
        };
        let output = tempfile::tempdir().unwrap().path().join("report.json");
        assert!(write_validated_report(&output, &manifest, &report).is_err());
        assert!(!output.exists());
        assert!(!output.with_extension("md").exists());
    }
}
