//! Retained-session edit-performance records (ADR-0068).
//!
//! These records are deliberately separate from ADR-0067's fresh-process
//! history. The manifest declares maintained workloads, edit classes, worker
//! modes, sampling, retention, and the reference host. A raw report stores
//! only integer observations. Validation separates malformed/incomplete runs
//! from compiler divergences so a warm/fresh mismatch can be published as a
//! loud failing artifact without being mistaken for a latency sample.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{EnvironmentFingerprint, median, median_absolute_deviation};

/// Version of the retained-session raw report wire format.
pub const EDIT_REPORT_SCHEMA_VERSION: u32 = 1;

/// One retained-session edit class from ADR-0068's initial matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditScenario {
    /// Re-observe the accepted-read closure without changing a source file.
    NoOpReobservation,
    /// Edit a body that is parsed but not in the rooted reached set.
    UnreachableBody,
    /// Edit one reached body without changing its callable interface.
    ReachedBodyOnly,
    /// Change one callable signature.
    CallableSignature,
    /// Change a used type's physical layout or ABI.
    LayoutAbi,
    /// Add or remove an import edge.
    ImportSet,
    /// Remove a previously reached function from the rooted image.
    ReachabilityDeletion,
    /// Turn a valid baseline into a deterministic compiler error.
    ErrorIntroduction,
}

impl EditScenario {
    /// Every initial scenario in canonical manifest order.
    pub const ALL: [Self; 8] = [
        Self::NoOpReobservation,
        Self::UnreachableBody,
        Self::ReachedBodyOnly,
        Self::CallableSignature,
        Self::LayoutAbi,
        Self::ImportSet,
        Self::ReachabilityDeletion,
        Self::ErrorIntroduction,
    ];

    /// Stable wire name used in diagnostics and Markdown.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::NoOpReobservation => "no_op_reobservation",
            Self::UnreachableBody => "unreachable_body",
            Self::ReachedBodyOnly => "reached_body_only",
            Self::CallableSignature => "callable_signature",
            Self::LayoutAbi => "layout_abi",
            Self::ImportSet => "import_set",
            Self::ReachabilityDeletion => "reachability_deletion",
            Self::ErrorIntroduction => "error_introduction",
        }
    }
}

/// The two worker modes every retained-session row must measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerMode {
    /// Exactly one query worker (`-j1`).
    One,
    /// Production automatic worker resolution (`-j0`).
    Automatic,
}

impl WorkerMode {
    /// Canonical worker-mode order.
    pub const ALL: [Self; 2] = [Self::One, Self::Automatic];

    /// Stable display name.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Automatic => "automatic",
        }
    }
}

/// Expected result of one edit class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedEditOutcome {
    /// Compilation reaches all three successful endpoints.
    Success,
    /// Compilation reaches the canonical diagnostics endpoint only.
    Diagnostics,
}

/// Deterministic scenario-order rotation between sample indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationRule {
    /// Sample `n` rotates the manifest scenario order left by `n` positions.
    LeftBySample,
}

/// Optimization setting pinned by the edit manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationSetting {
    /// The compiler's production default.
    Default,
    /// Explicit `-O0`.
    O0,
    /// Explicit `-O1`.
    O1,
    /// Explicit `-O2`.
    O2,
    /// Explicit `-O3`.
    O3,
}

/// One declared edit scenario and the structural question it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditScenarioDeclaration {
    /// Scenario identity.
    pub scenario: EditScenario,
    /// Whether revision B should succeed or diagnose.
    pub expected_outcome: ExpectedEditOutcome,
    /// Human-readable structural claim carried into the report.
    pub structural_claim: String,
}

/// One query-worker mode declared by the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDeclaration {
    /// Worker mode identity.
    pub mode: WorkerMode,
    /// Exact compiler argument selecting this mode.
    pub compiler_arg: String,
}

/// A maintained program participating in the edit suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditWorkload {
    /// Stable short name.
    pub id: String,
    /// Root source relative to the repository.
    pub source: String,
    /// The maintained-program question this row answers.
    pub question: String,
}

/// Hardware and operating-system class used by the numerical linker gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostClass {
    /// Stable, versioned identity for this reference class.
    pub id: String,
    /// Runner ownership class, for example `github-hosted`.
    pub runner_label: String,
    /// Pinned operating-system image label.
    pub runner_image: String,
    /// Pinned machine architecture.
    pub architecture: String,
    /// Exact visible logical CPU count.
    pub logical_cores: u32,
    /// Minimum visible memory required for this class.
    pub minimum_memory_bytes: u64,
}

/// Manifest-owned host fingerprint for the Lattice linker decision row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceHost {
    /// Hardware and OS class.
    pub class: HostClass,
    /// Compilation target whose Lattice row may decide the gate.
    pub target: String,
    /// Exact worker count to which `-j0` must resolve.
    pub automatic_workers: u32,
}

impl ReferenceHost {
    /// Whether an observed environment is eligible for the numerical gate.
    pub fn admits(
        &self,
        environment: &EnvironmentFingerprint,
        target: &str,
        automatic_workers: u32,
    ) -> bool {
        environment.runner_label == self.class.runner_label
            && environment.runner_image == self.class.runner_image
            && environment.architecture == self.class.architecture
            && environment.core_count == self.class.logical_cores
            && environment.memory_bytes >= self.class.minimum_memory_bytes
            && target == self.target
            && automatic_workers == self.automatic_workers
    }
}

/// Versioned retained-session suite declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditManifest {
    /// Manifest syntax version.
    pub schema_version: u32,
    /// Raw report syntax version emitted under this manifest.
    pub report_schema_version: u32,
    /// Revision of maintained-program edit fixtures.
    pub fixture_revision: u32,
    /// Independent retained sessions per workload/scenario/worker row.
    pub samples_per_row: u32,
    /// Revisions in the bounded long-edit sequence.
    pub retention_revisions: u32,
    /// Deterministic interleaving rule.
    pub rotation: RotationRule,
    /// Compilation target.
    pub target: String,
    /// Optimization setting.
    pub optimization: OptimizationSetting,
    /// Behavior-affecting arguments common to both worker modes.
    #[serde(default)]
    pub compiler_args: Vec<String>,
    /// Initial edit scenario matrix, in rotation order.
    pub scenarios: Vec<EditScenarioDeclaration>,
    /// Worker modes, in report order.
    pub workers: Vec<WorkerDeclaration>,
    /// Maintained programs, in report order.
    pub workloads: Vec<EditWorkload>,
    /// Host allowed to decide the numerical linker gate.
    pub reference_host: ReferenceHost,
}

/// Why an edit manifest could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditManifestError {
    /// Human-readable, stable failure detail.
    pub detail: String,
}

impl std::fmt::Display for EditManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.detail.fmt(f)
    }
}

impl std::error::Error for EditManifestError {}

impl EditManifest {
    /// Parse and strictly validate a retained-session manifest.
    pub fn parse(text: &str) -> Result<Self, EditManifestError> {
        let manifest: Self = toml::from_str(text).map_err(|error| EditManifestError {
            detail: format!("invalid incremental manifest: {error}"),
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate an already-deserialized manifest.
    pub fn validate(&self) -> Result<(), EditManifestError> {
        let fail = |detail: String| Err(EditManifestError { detail });
        if self.schema_version != 1 {
            return fail(format!(
                "unsupported incremental manifest schema version {}",
                self.schema_version
            ));
        }
        if self.report_schema_version != EDIT_REPORT_SCHEMA_VERSION {
            return fail(format!(
                "manifest report schema {} does not match supported version {}",
                self.report_schema_version, EDIT_REPORT_SCHEMA_VERSION
            ));
        }
        if self.fixture_revision == 0 {
            return fail("fixture revision must be nonzero".to_string());
        }
        if self.samples_per_row < 5 {
            return fail("retained-session rows require at least five samples".to_string());
        }
        if self.retention_revisions < 1_000 {
            return fail(
                "the bounded retention sequence requires at least 1000 revisions".to_string(),
            );
        }
        if self.target.trim().is_empty() {
            return fail("the incremental target is empty".to_string());
        }
        if self.compiler_args.iter().any(|arg| {
            arg == "-j" || arg == "--jobs" || arg.starts_with("-j") || arg.starts_with("--jobs=")
        }) {
            return fail("common compiler arguments must not select a worker mode".to_string());
        }

        let scenarios: Vec<_> = self.scenarios.iter().map(|entry| entry.scenario).collect();
        if scenarios != EditScenario::ALL {
            return fail(format!(
                "scenario matrix must be exactly {:?}, got {scenarios:?}",
                EditScenario::ALL
            ));
        }
        for declaration in &self.scenarios {
            if declaration.structural_claim.trim().is_empty() {
                return fail(format!(
                    "scenario {} has an empty structural claim",
                    declaration.scenario.wire_name()
                ));
            }
            let expected = if declaration.scenario == EditScenario::ErrorIntroduction {
                ExpectedEditOutcome::Diagnostics
            } else {
                ExpectedEditOutcome::Success
            };
            if declaration.expected_outcome != expected {
                return fail(format!(
                    "scenario {} declares the wrong expected outcome",
                    declaration.scenario.wire_name()
                ));
            }
        }

        let workers: Vec<_> = self.workers.iter().map(|entry| entry.mode).collect();
        if workers != WorkerMode::ALL {
            return fail(format!(
                "worker modes must be exactly {:?}, got {workers:?}",
                WorkerMode::ALL
            ));
        }
        for worker in &self.workers {
            let expected = match worker.mode {
                WorkerMode::One => "-j1",
                WorkerMode::Automatic => "-j0",
            };
            if worker.compiler_arg != expected {
                return fail(format!(
                    "worker mode {} must use {expected}",
                    worker.mode.wire_name()
                ));
            }
        }

        if self.workloads.is_empty() {
            return fail("the incremental manifest declares no workloads".to_string());
        }
        let mut workload_ids = BTreeSet::new();
        for workload in &self.workloads {
            if workload.id.trim().is_empty() || !workload_ids.insert(&workload.id) {
                return fail(format!(
                    "invalid or duplicate workload id {:?}",
                    workload.id
                ));
            }
            if workload.source.is_empty() || workload.source.starts_with('/') {
                return fail(format!(
                    "workload {:?} must use a repository-relative source",
                    workload.id
                ));
            }
            if workload.question.trim().is_empty() {
                return fail(format!("workload {:?} has no question", workload.id));
            }
        }

        let reference = &self.reference_host;
        if reference.class.id.trim().is_empty()
            || reference.class.runner_label.trim().is_empty()
            || reference.class.runner_image.trim().is_empty()
            || reference.class.architecture.trim().is_empty()
            || reference.class.logical_cores == 0
            || reference.class.minimum_memory_bytes == 0
            || reference.automatic_workers == 0
        {
            return fail("reference-host identity is incomplete".to_string());
        }
        if reference.target != self.target {
            return fail("reference-host target differs from the suite target".to_string());
        }
        if reference.automatic_workers != reference.class.logical_cores {
            return fail(
                "reference-host automatic workers must equal its visible logical cores".to_string(),
            );
        }
        Ok(())
    }

    fn scenario(&self, scenario: EditScenario) -> &EditScenarioDeclaration {
        self.scenarios
            .iter()
            .find(|entry| entry.scenario == scenario)
            .expect("validated manifest has every scenario")
    }

    fn workload(&self, id: &str) -> Option<&EditWorkload> {
        self.workloads.iter().find(|entry| entry.id == id)
    }
}

/// Compiler-derived shape of a maintained source closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceShape {
    /// Physical files in the accepted-read closure.
    pub files: u64,
    /// Parsed modules.
    pub modules: u64,
    /// Source bytes.
    pub bytes: u64,
    /// Source lines.
    pub lines: u64,
    /// Lexer tokens.
    pub tokens: u64,
    /// Source and synthesized functions considered for CFG construction.
    pub functions: u64,
}

/// Identity of the exact revision-A to revision-B operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransformationIdentity {
    /// No source bytes changed; only canonical filesystem re-observation ran.
    Reobserve {
        /// Stable fixture operation id.
        id: String,
    },
    /// One unique source fragment was replaced.
    Replace {
        /// Stable fixture operation id.
        id: String,
        /// Logical path within the isolated fixture.
        logical_file: String,
        /// SHA-256 of the expected revision-A fragment.
        before_sha256: String,
        /// SHA-256 of the revision-B replacement fragment.
        after_sha256: String,
    },
}

impl TransformationIdentity {
    fn id(&self) -> &str {
        match self {
            Self::Reobserve { id } | Self::Replace { id, .. } => id,
        }
    }
}

/// Counts for one compiler phase's incremental work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseWork {
    /// Artifacts newly computed.
    pub computed: u64,
    /// Retained artifacts reused without joining live work.
    pub reused: u64,
    /// Requests joined to already-running work.
    pub joined: u64,
    /// Previously retained artifacts invalidated.
    pub invalidated: u64,
    /// Requests canceled before publication.
    pub canceled: u64,
    /// Retained artifacts evicted.
    pub evicted: u64,
}

/// Fixed phase inventory for one measured successor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuralWork {
    /// Filesystem and accepted-read observations.
    pub source_observation: PhaseWork,
    /// Import discovery and reachability closure.
    pub import_discovery: PhaseWork,
    /// Lexing and parsing.
    pub parsing: PhaseWork,
    /// Canonical program construction and lowering.
    pub program: PhaseWork,
    /// Semantic and type queries.
    pub semantic: PhaseWork,
    /// CFG construction and optimization.
    pub cfg: PhaseWork,
    /// Machine-code production.
    pub codegen: PhaseWork,
    /// Per-unit object projection.
    pub object_projection: PhaseWork,
    /// Fresh image planning and linking.
    pub linking: PhaseWork,
}

impl StructuralWork {
    fn phases(&self) -> [&PhaseWork; 9] {
        [
            &self.source_observation,
            &self.import_discovery,
            &self.parsing,
            &self.program,
            &self.semantic,
            &self.cfg,
            &self.codegen,
            &self.object_projection,
            &self.linking,
        ]
    }

    fn totals(&self) -> Option<[u64; 6]> {
        self.phases()
            .iter()
            .try_fold([0_u64; 6], |mut totals, phase| {
                totals[0] = totals[0].checked_add(phase.computed)?;
                totals[1] = totals[1].checked_add(phase.reused)?;
                totals[2] = totals[2].checked_add(phase.joined)?;
                totals[3] = totals[3].checked_add(phase.invalidated)?;
                totals[4] = totals[4].checked_add(phase.canceled)?;
                totals[5] = totals[5].checked_add(phase.evicted)?;
                Some(totals)
            })
    }
}

/// Retained-session memory and observation gauges at an endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedGauges {
    /// Current retained artifact charge.
    pub current_bytes: u64,
    /// Peak retained artifact charge observed during this successor.
    pub peak_bytes: u64,
    /// Configured soft artifact budget.
    pub soft_budget_bytes: u64,
    /// Protected charge temporarily permitted beyond the soft budget.
    pub protected_overflow_bytes: u64,
    /// Retained dependency-edge observations.
    pub dependency_observations: u64,
    /// Retained source/input observations.
    pub input_observations: u64,
    /// Configured soft observation budget.
    pub observation_budget: u64,
    /// Protected observations temporarily permitted beyond the soft budget.
    pub protected_overflow_observations: u64,
}

/// Cumulative successful-compilation endpoint timings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditEndpoints {
    /// Re-observation start through rooted CodegenUnit collection.
    pub codegen_ready_ns: u64,
    /// Re-observation start through object-projection collection.
    pub objects_ready_ns: u64,
    /// Re-observation start through fresh linked executable bytes.
    pub runnable_ready_ns: u64,
}

/// Stage at which an unexpected failure occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    /// Filesystem re-observation or import discovery.
    Reobservation,
    /// Semantic or diagnostic query.
    Diagnostics,
    /// Rooted codegen collection.
    Codegen,
    /// Object projection.
    Objects,
    /// Linking.
    Linking,
}

/// Warm retained-host result for one edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOutcome {
    /// All successful endpoints completed.
    Success {
        /// Cumulative endpoint timings.
        endpoints: EditEndpoints,
        /// Stable ordered diagnostic identity (normally the empty identity).
        diagnostics: String,
        /// Stable ordered warning identity.
        warnings: String,
        /// Hash of the linked executable bytes.
        executable: String,
    },
    /// The declared error-introduction edit produced canonical diagnostics.
    ExpectedDiagnostics {
        /// Re-observation start through the failed canonical query.
        diagnostics_ready_ns: u64,
        /// Stable ordered diagnostic identity.
        diagnostics: String,
        /// Stable ordered warning identity.
        warnings: String,
    },
    /// A scenario failed before its declared endpoint.
    UnexpectedFailure {
        /// Stage that failed.
        stage: FailureStage,
        /// Stable ordered diagnostic identity.
        diagnostics: String,
        /// Stable ordered warning identity.
        warnings: String,
    },
}

impl EditOutcome {
    fn identity(&self) -> OutcomeIdentity {
        match self {
            Self::Success {
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
            Self::ExpectedDiagnostics {
                diagnostics,
                warnings,
                ..
            } => OutcomeIdentity {
                kind: OutcomeKind::Diagnostics,
                diagnostics: diagnostics.clone(),
                warnings: warnings.clone(),
                executable: None,
            },
            Self::UnexpectedFailure {
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
}

/// Stable classification of a warm or fresh compiler result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// Successful executable result.
    Success,
    /// Expected deterministic diagnostics.
    Diagnostics,
    /// Failure outside the scenario contract.
    UnexpectedFailure,
    /// A long-sequence request canceled before publication.
    Canceled,
}

/// Comparison identity independent of timing and structural counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeIdentity {
    /// Outcome classification.
    pub kind: OutcomeKind,
    /// Stable ordered diagnostic identity.
    pub diagnostics: String,
    /// Stable ordered warning identity.
    pub warnings: String,
    /// Executable hash, present exactly for successful outcomes.
    pub executable: Option<String>,
}

/// Warm/fresh correctness evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum OracleComparison {
    /// Warm and fresh outcome identities agree exactly.
    Matched {
        /// Retained-session result.
        warm: OutcomeIdentity,
        /// Fresh-session result.
        fresh: OutcomeIdentity,
    },
    /// Warm and fresh outcome identities disagree.
    Diverged {
        /// Retained-session result.
        warm: OutcomeIdentity,
        /// Fresh-session result.
        fresh: OutcomeIdentity,
        /// First differing diagnostic, warning, outcome kind, or executable.
        first_difference: String,
    },
}

impl OracleComparison {
    fn warm(&self) -> &OutcomeIdentity {
        match self {
            Self::Matched { warm, .. } | Self::Diverged { warm, .. } => warm,
        }
    }
}

/// One independent retained-session sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditSample {
    /// Zero-based sample index within its row.
    pub sample_index: u32,
    /// Globally unique host/session identity.
    pub session_id: String,
    /// This scenario's position in the sample's rotated collection order.
    pub collection_order: u32,
    /// Exact worker count after resolving the declared mode.
    pub resolved_workers: u32,
    /// Exact A/B operation identity.
    pub transformation: TransformationIdentity,
    /// Warm result and its declared endpoint timings.
    pub outcome: EditOutcome,
    /// Exact structural work by compiler phase.
    pub work: StructuralWork,
    /// Retained memory and observation gauges at the endpoint.
    pub retention: RetainedGauges,
    /// Fresh-session correctness comparison performed outside timing.
    pub oracle: OracleComparison,
}

/// One workload/scenario/worker row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditRow {
    /// Maintained workload identity.
    pub workload: String,
    /// Root source recorded for inspection.
    pub source: String,
    /// Compiler-derived revision-A shape.
    pub shape: SourceShape,
    /// Edit class.
    pub scenario: EditScenario,
    /// Query-worker mode.
    pub worker_mode: WorkerMode,
    /// Independent raw observations.
    pub samples: Vec<EditSample>,
}

/// Identity and host of one raw edit report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditReportIdentity {
    /// Manifest fixture revision.
    pub fixture_revision: u32,
    /// Compiler source revision measured.
    pub commit: String,
    /// UTC collection start.
    pub started_at: String,
    /// UTC collection completion.
    pub finished_at: String,
    /// Compilation target.
    pub target: String,
    /// Exact host fingerprint.
    pub environment: EnvironmentFingerprint,
}

/// Measurement regime copied into the raw report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditReportRegime {
    /// Always `retained_session`.
    pub compiler_state: String,
    /// Always `uncontrolled`.
    pub os_page_cache: String,
    /// Manifest sampling count.
    pub samples_per_row: u32,
    /// Manifest long-sequence revision count.
    pub retention_revisions: u32,
    /// Manifest rotation rule.
    pub rotation: RotationRule,
    /// Manifest optimization setting.
    pub optimization: OptimizationSetting,
    /// Manifest common compiler arguments.
    pub compiler_args: Vec<String>,
}

/// Result stored for one long-sequence revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetentionStepOutcome {
    /// Successful warm result.
    Success { identity: OutcomeIdentity },
    /// Expected deterministic compiler diagnostics.
    Diagnostics { identity: OutcomeIdentity },
    /// Request canceled before any successful artifact published.
    Canceled,
}

/// One revision in the bounded long-edit retention witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionStep {
    /// Zero-based revision index.
    pub revision_index: u32,
    /// Stable identity of the fixture state selected at this revision.
    pub state_id: String,
    /// Actual warm outcome.
    pub outcome: RetentionStepOutcome,
    /// Precomputed fresh comparison; absent exactly for canceled requests.
    pub oracle: Option<OracleComparison>,
    /// Gauges after the request/protection release point.
    pub retention: RetainedGauges,
}

/// Bounded long-edit service-viability row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionSequence {
    /// Representative maintained workload.
    pub workload: String,
    /// Worker mode used for the sequence.
    pub worker_mode: WorkerMode,
    /// Exact worker count used.
    pub resolved_workers: u32,
    /// Ordered revision observations.
    pub revisions: Vec<RetentionStep>,
}

/// Complete raw retained-session report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditReport {
    /// Raw report syntax version.
    pub schema_version: u32,
    /// Run identity and exact host.
    pub identity: EditReportIdentity,
    /// Measurement regime.
    pub regime: EditReportRegime,
    /// Canonically ordered single-edit rows.
    pub rows: Vec<EditRow>,
    /// Long-edit bounded-retention sequence.
    pub retention: RetentionSequence,
}

/// One structural invalidity or compiler divergence found in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationFinding {
    /// Stable report location.
    pub path: String,
    /// Human-readable detail.
    pub detail: String,
}

/// Result of strict retained-session report validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditValidation {
    /// Infrastructure/schema invalidities; these suppress all derived latency.
    pub errors: Vec<ValidationFinding>,
    /// Warm/fresh mismatches; these remain publishable failing evidence.
    pub divergences: Vec<ValidationFinding>,
}

impl EditValidation {
    /// Whether the raw object is complete and structurally meaningful.
    pub fn is_structurally_valid(&self) -> bool {
        self.errors.is_empty()
    }

    /// Whether collection completed with no compiler correctness mismatch.
    pub fn is_success(&self) -> bool {
        self.errors.is_empty() && self.divergences.is_empty()
    }
}

fn finding(path: impl Into<String>, detail: impl Into<String>) -> ValidationFinding {
    ValidationFinding {
        path: path.into(),
        detail: detail.into(),
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    let digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    let dashes = [4, 7];
    let colons = [13, 16];
    digits.iter().all(|&index| bytes[index].is_ascii_digit())
        && dashes.iter().all(|&index| bytes[index] == b'-')
        && colons.iter().all(|&index| bytes[index] == b':')
        && bytes[10] == b'T'
        && bytes[19] == b'Z'
}

fn validate_identity(path: &str, identity: &OutcomeIdentity, errors: &mut Vec<ValidationFinding>) {
    let executable_expected = identity.kind == OutcomeKind::Success;
    if identity.executable.is_some() != executable_expected {
        errors.push(finding(
            path,
            "executable identity must be present exactly for successful outcomes",
        ));
    }
    if !is_sha256(&identity.diagnostics) || !is_sha256(&identity.warnings) {
        errors.push(finding(
            path,
            "diagnostic and warning identities must be lowercase SHA-256 values (including empty-set hashes)",
        ));
    }
    if let Some(executable) = &identity.executable
        && !is_sha256(executable)
    {
        errors.push(finding(
            path,
            "executable identity is not a lowercase SHA-256",
        ));
    }
}

fn validate_oracle(
    path: &str,
    expected_warm: &OutcomeIdentity,
    oracle: &OracleComparison,
    errors: &mut Vec<ValidationFinding>,
    divergences: &mut Vec<ValidationFinding>,
) {
    if oracle.warm() != expected_warm {
        errors.push(finding(
            path,
            "oracle warm identity differs from the measured outcome",
        ));
    }
    match oracle {
        OracleComparison::Matched { warm, fresh } => {
            validate_identity(&format!("{path}.warm"), warm, errors);
            validate_identity(&format!("{path}.fresh"), fresh, errors);
            if warm != fresh {
                errors.push(finding(path, "matched oracle carries unequal identities"));
            }
        }
        OracleComparison::Diverged {
            warm,
            fresh,
            first_difference,
        } => {
            validate_identity(&format!("{path}.warm"), warm, errors);
            validate_identity(&format!("{path}.fresh"), fresh, errors);
            if warm == fresh || first_difference.trim().is_empty() {
                errors.push(finding(
                    path,
                    "divergent oracle requires unequal identities and a first difference",
                ));
            } else {
                divergences.push(finding(path, first_difference.clone()));
            }
        }
    }
}

fn validate_gauges(path: &str, gauges: &RetainedGauges, errors: &mut Vec<ValidationFinding>) {
    if gauges.current_bytes > gauges.peak_bytes {
        errors.push(finding(
            path,
            "current retained bytes exceed the recorded peak",
        ));
    }
    match gauges
        .soft_budget_bytes
        .checked_add(gauges.protected_overflow_bytes)
    {
        Some(limit) if gauges.current_bytes > limit => errors.push(finding(
            path,
            "current retained bytes exceed soft budget plus protected overflow",
        )),
        None => errors.push(finding(path, "retained-byte budget arithmetic overflowed")),
        Some(_) => {}
    }
    let observations = gauges
        .dependency_observations
        .checked_add(gauges.input_observations);
    let observation_limit = gauges
        .observation_budget
        .checked_add(gauges.protected_overflow_observations);
    match (observations, observation_limit) {
        (Some(observations), Some(limit)) if observations > limit => errors.push(finding(
            path,
            "retained observations exceed soft budget plus protected overflow",
        )),
        (None, _) | (_, None) => {
            errors.push(finding(path, "retained-observation arithmetic overflowed"));
        }
        _ => {}
    }
}

/// Strictly validate a raw report against its manifest.
pub fn validate_edit_report(manifest: &EditManifest, report: &EditReport) -> EditValidation {
    let mut errors = Vec::new();
    let mut divergences = Vec::new();
    if let Err(error) = manifest.validate() {
        errors.push(finding("manifest", error.to_string()));
        return EditValidation {
            errors,
            divergences,
        };
    }
    if report.schema_version != manifest.report_schema_version {
        errors.push(finding(
            "schema_version",
            "report schema version differs from manifest",
        ));
    }
    if report.identity.fixture_revision != manifest.fixture_revision {
        errors.push(finding(
            "identity.fixture_revision",
            "report fixture revision differs from manifest",
        ));
    }
    if report.identity.target != manifest.target {
        errors.push(finding(
            "identity.target",
            "report target differs from manifest",
        ));
    }
    if !is_commit(&report.identity.commit) {
        errors.push(finding(
            "identity.commit",
            "compiler commit is not a 40-character hexadecimal hash",
        ));
    }
    if !is_utc_timestamp(&report.identity.started_at)
        || !is_utc_timestamp(&report.identity.finished_at)
        || report.identity.started_at >= report.identity.finished_at
    {
        errors.push(finding(
            "identity",
            "report timestamps must be nonempty and strictly ordered UTC strings",
        ));
    }
    let regime = &report.regime;
    if regime.compiler_state != "retained_session"
        || regime.os_page_cache != "uncontrolled"
        || regime.samples_per_row != manifest.samples_per_row
        || regime.retention_revisions != manifest.retention_revisions
        || regime.rotation != manifest.rotation
        || regime.optimization != manifest.optimization
        || regime.compiler_args != manifest.compiler_args
    {
        errors.push(finding("regime", "report regime differs from manifest"));
    }

    let expected_rows: Vec<_> = manifest
        .workloads
        .iter()
        .flat_map(|workload| {
            manifest.scenarios.iter().flat_map(move |scenario| {
                manifest
                    .workers
                    .iter()
                    .map(move |worker| (workload.id.as_str(), scenario.scenario, worker.mode))
            })
        })
        .collect();
    let actual_rows: Vec<_> = report
        .rows
        .iter()
        .map(|row| (row.workload.as_str(), row.scenario, row.worker_mode))
        .collect();
    if actual_rows != expected_rows {
        errors.push(finding(
            "rows",
            "rows are missing, duplicated, unexpected, or not in manifest order",
        ));
    }

    let scenario_count = manifest.scenarios.len() as u32;
    let mut sessions = BTreeSet::new();
    let mut shapes: BTreeMap<&str, &SourceShape> = BTreeMap::new();
    let automatic_resolved = report
        .rows
        .iter()
        .find(|row| row.worker_mode == WorkerMode::Automatic)
        .and_then(|row| row.samples.first())
        .map(|sample| sample.resolved_workers)
        .unwrap_or(0);
    let reference_eligible = manifest.reference_host.admits(
        &report.identity.environment,
        &report.identity.target,
        automatic_resolved,
    );

    for (row_index, row) in report.rows.iter().enumerate() {
        let row_path = format!("rows[{row_index}]");
        let Some(workload) = manifest.workload(&row.workload) else {
            errors.push(finding(&row_path, "row names an unknown workload"));
            continue;
        };
        if row.source != workload.source {
            errors.push(finding(&row_path, "row source differs from manifest"));
        }
        if row.shape.files == 0
            || row.shape.modules == 0
            || row.shape.bytes == 0
            || row.shape.lines == 0
            || row.shape.tokens == 0
            || row.shape.functions == 0
        {
            errors.push(finding(
                &row_path,
                "row has an empty compiler-derived source shape",
            ));
        }
        if let Some(previous) = shapes.insert(&row.workload, &row.shape)
            && previous != &row.shape
        {
            errors.push(finding(
                &row_path,
                "revision-A source shape changed within a workload",
            ));
        }
        if row.samples.len() != manifest.samples_per_row as usize {
            errors.push(finding(
                &row_path,
                "row has the wrong number of independent samples",
            ));
        }
        let scenario_index = manifest
            .scenarios
            .iter()
            .position(|entry| entry.scenario == row.scenario)
            .unwrap_or(0) as u32;
        for (position, sample) in row.samples.iter().enumerate() {
            let sample_path = format!("{row_path}.samples[{position}]");
            if sample.sample_index != position as u32 {
                errors.push(finding(&sample_path, "sample indices are not canonical"));
            }
            if sample.session_id.trim().is_empty() || !sessions.insert(&sample.session_id) {
                errors.push(finding(
                    &sample_path,
                    "sample session identity is empty or reused by another row",
                ));
            }
            let expected_order = (scenario_index + scenario_count
                - (sample.sample_index % scenario_count))
                % scenario_count;
            if sample.collection_order != expected_order {
                errors.push(finding(
                    &sample_path,
                    "sample collection order violates left-by-sample rotation",
                ));
            }
            match row.worker_mode {
                WorkerMode::One if sample.resolved_workers != 1 => errors.push(finding(
                    &sample_path,
                    "one-worker row did not resolve to exactly one worker",
                )),
                WorkerMode::Automatic if sample.resolved_workers == 0 => errors.push(finding(
                    &sample_path,
                    "automatic-worker row did not record a resolved worker count",
                )),
                WorkerMode::Automatic
                    if sample.resolved_workers != report.identity.environment.core_count =>
                {
                    errors.push(finding(
                        &sample_path,
                        "automatic-worker row differs from the host's detected parallelism",
                    ));
                }
                WorkerMode::Automatic
                    if reference_eligible
                        && sample.resolved_workers != manifest.reference_host.automatic_workers =>
                {
                    errors.push(finding(
                        &sample_path,
                        "reference-host automatic row changed resolved worker count",
                    ));
                }
                _ => {}
            }
            if sample.transformation.id().trim().is_empty() {
                errors.push(finding(&sample_path, "transformation id is empty"));
            }
            match (&row.scenario, &sample.transformation) {
                (EditScenario::NoOpReobservation, TransformationIdentity::Reobserve { .. }) => {}
                (EditScenario::NoOpReobservation, TransformationIdentity::Replace { .. }) => errors
                    .push(finding(
                        &sample_path,
                        "no-op re-observation must not claim a source replacement",
                    )),
                (_, TransformationIdentity::Reobserve { .. }) => errors.push(finding(
                    &sample_path,
                    "a mutating scenario must record a source replacement",
                )),
                (
                    _,
                    TransformationIdentity::Replace {
                        logical_file,
                        before_sha256,
                        after_sha256,
                        ..
                    },
                ) => {
                    if logical_file.is_empty()
                        || logical_file.starts_with('/')
                        || !is_sha256(before_sha256)
                        || !is_sha256(after_sha256)
                        || before_sha256 == after_sha256
                    {
                        errors.push(finding(
                            &sample_path,
                            "replacement identity has an invalid path or fragment hashes",
                        ));
                    }
                }
            }

            let declaration = manifest.scenario(row.scenario);
            match (&declaration.expected_outcome, &sample.outcome) {
                (
                    ExpectedEditOutcome::Success,
                    EditOutcome::Success {
                        endpoints,
                        diagnostics,
                        warnings,
                        executable,
                    },
                ) => {
                    if endpoints.codegen_ready_ns == 0
                        || endpoints.codegen_ready_ns > endpoints.objects_ready_ns
                        || endpoints.objects_ready_ns > endpoints.runnable_ready_ns
                    {
                        errors.push(finding(
                            &sample_path,
                            "successful endpoints are zero or not cumulative/monotonic",
                        ));
                    }
                    if diagnostics.is_empty() || warnings.is_empty() || !is_sha256(executable) {
                        errors.push(finding(
                            &sample_path,
                            "successful outcome identities are incomplete",
                        ));
                    }
                }
                (
                    ExpectedEditOutcome::Diagnostics,
                    EditOutcome::ExpectedDiagnostics {
                        diagnostics_ready_ns,
                        diagnostics,
                        warnings,
                    },
                ) => {
                    if *diagnostics_ready_ns == 0 || diagnostics.is_empty() || warnings.is_empty() {
                        errors.push(finding(
                            &sample_path,
                            "diagnostics outcome is missing its endpoint or identities",
                        ));
                    }
                }
                (_, EditOutcome::UnexpectedFailure { .. }) => errors.push(finding(
                    &sample_path,
                    "scenario failed before its declared endpoint",
                )),
                _ => errors.push(finding(
                    &sample_path,
                    "scenario outcome does not match the manifest declaration",
                )),
            }
            validate_gauges(
                &format!("{sample_path}.retention"),
                &sample.retention,
                &mut errors,
            );
            if sample.work.totals().is_none() {
                errors.push(finding(
                    &format!("{sample_path}.work"),
                    "structural work totals overflowed",
                ));
            }
            let identity = sample.outcome.identity();
            validate_oracle(
                &format!("{sample_path}.oracle"),
                &identity,
                &sample.oracle,
                &mut errors,
                &mut divergences,
            );
        }
    }

    let sequence = &report.retention;
    if manifest.workload(&sequence.workload).is_none() {
        errors.push(finding(
            "retention.workload",
            "retention row names an unknown workload",
        ));
    }
    if sequence.worker_mode != WorkerMode::Automatic
        || sequence.resolved_workers == 0
        || sequence.resolved_workers != report.identity.environment.core_count
        || sequence.resolved_workers != automatic_resolved
    {
        errors.push(finding(
            "retention",
            "retention row must use the same host-resolved automatic worker count as the edit rows",
        ));
    }
    if sequence.revisions.len() != manifest.retention_revisions as usize {
        errors.push(finding(
            "retention.revisions",
            "retention row has the wrong revision count",
        ));
    }
    for (position, step) in sequence.revisions.iter().enumerate() {
        let path = format!("retention.revisions[{position}]");
        if step.revision_index != position as u32 || step.state_id.trim().is_empty() {
            errors.push(finding(
                &path,
                "retention revision index or state identity is invalid",
            ));
        }
        validate_gauges(&format!("{path}.retention"), &step.retention, &mut errors);
        match (&step.outcome, &step.oracle) {
            (RetentionStepOutcome::Canceled, None) => {}
            (RetentionStepOutcome::Canceled, Some(_)) => errors.push(finding(
                &path,
                "canceled retention revision must not publish oracle evidence",
            )),
            (RetentionStepOutcome::Success { identity }, Some(oracle)) => {
                if identity.kind != OutcomeKind::Success {
                    errors.push(finding(
                        &path,
                        "successful retention identity has wrong kind",
                    ));
                }
                validate_oracle(
                    &format!("{path}.oracle"),
                    identity,
                    oracle,
                    &mut errors,
                    &mut divergences,
                );
            }
            (RetentionStepOutcome::Diagnostics { identity }, Some(oracle)) => {
                if identity.kind != OutcomeKind::Diagnostics {
                    errors.push(finding(
                        &path,
                        "diagnostic retention identity has wrong kind",
                    ));
                }
                validate_oracle(
                    &format!("{path}.oracle"),
                    identity,
                    oracle,
                    &mut errors,
                    &mut divergences,
                );
            }
            (_, None) => errors.push(finding(
                &path,
                "non-canceled retention revision lacks fresh-oracle evidence",
            )),
        }
    }
    if let Some(last) = sequence.revisions.last() {
        if last.retention.protected_overflow_bytes != 0
            || last.retention.protected_overflow_observations != 0
            || last.retention.current_bytes > last.retention.soft_budget_bytes
            || last
                .retention
                .dependency_observations
                .saturating_add(last.retention.input_observations)
                > last.retention.observation_budget
        {
            errors.push(finding(
                "retention.revisions[last]",
                "final retention gauges did not return within soft bounds after protection release",
            ));
        }
    }

    EditValidation {
        errors,
        divergences,
    }
}

/// Median and MAD for one raw integer field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointSummary {
    /// Median nanoseconds or count.
    pub median: u64,
    /// Median absolute deviation.
    pub mad: u64,
}

fn summarize(values: &[u64]) -> EndpointSummary {
    EndpointSummary {
        median: median(values).expect("validated row has samples"),
        mad: median_absolute_deviation(values).expect("validated row has samples"),
    }
}

/// Derived totals across the fixed structural phase inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StructuralWorkSummary {
    /// Total newly computed artifacts.
    pub computed: EndpointSummary,
    /// Total reused retained artifacts.
    pub reused: EndpointSummary,
    /// Total joined requests.
    pub joined: EndpointSummary,
    /// Total invalidations.
    pub invalidated: EndpointSummary,
    /// Total cancellations.
    pub canceled: EndpointSummary,
    /// Total evictions.
    pub evicted: EndpointSummary,
}

/// Fresh-link band derived from successful cumulative endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkBandSummary {
    /// Median and MAD of per-sample objects-ready to runnable-ready deltas.
    pub duration_ns: EndpointSummary,
    /// Median link share in basis points, derived per sample.
    pub share_basis_points: EndpointSummary,
}

/// One deterministic derived row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditRowSummary {
    /// Maintained workload.
    pub workload: String,
    /// Edit class.
    pub scenario: EditScenario,
    /// Worker mode.
    pub worker_mode: WorkerMode,
    /// Diagnostics-ready timing for the error row.
    pub diagnostics_ready_ns: Option<EndpointSummary>,
    /// Codegen-ready timing for successful rows.
    pub codegen_ready_ns: Option<EndpointSummary>,
    /// Objects-ready timing for successful rows.
    pub objects_ready_ns: Option<EndpointSummary>,
    /// Runnable-ready timing for successful rows.
    pub runnable_ready_ns: Option<EndpointSummary>,
    /// Fresh-link band for successful rows.
    pub link_band: Option<LinkBandSummary>,
    /// Structural totals across phases.
    pub work: StructuralWorkSummary,
}

/// Deterministic derived edit report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditSummary {
    /// Derived format version.
    pub schema_version: u32,
    /// Fixture revision.
    pub fixture_revision: u32,
    /// Compiler commit.
    pub commit: String,
    /// Target.
    pub target: String,
    /// Whether this exact run may decide the numerical linker gate.
    pub reference_host_eligible: bool,
    /// Summaries in manifest row order. Empty on compiler divergence.
    pub rows: Vec<EditRowSummary>,
    /// Warm/fresh mismatches retained for the failing artifact.
    pub divergences: Vec<ValidationFinding>,
    /// Number of long-sequence revisions.
    pub retention_revisions: u32,
    /// Maximum current retained charge in the long sequence.
    pub retention_max_current_bytes: u64,
    /// Maximum peak retained charge in the long sequence.
    pub retention_peak_bytes: u64,
    /// Final retained charge after protection release.
    pub retention_final_bytes: u64,
}

fn derived_work(samples: &[EditSample]) -> StructuralWorkSummary {
    let totals: Vec<[u64; 6]> = samples
        .iter()
        .map(|sample| {
            sample
                .work
                .totals()
                .expect("validation rejected structural-work overflow")
        })
        .collect();
    let column = |index: usize| totals.iter().map(|row| row[index]).collect::<Vec<_>>();
    StructuralWorkSummary {
        computed: summarize(&column(0)),
        reused: summarize(&column(1)),
        joined: summarize(&column(2)),
        invalidated: summarize(&column(3)),
        canceled: summarize(&column(4)),
        evicted: summarize(&column(5)),
    }
}

/// Validate and derive medians/MAD from raw observations.
///
/// Structural invalidity returns `Err` and must publish no partial latency
/// report. Compiler divergence returns a successful derived object with no
/// latency rows and the divergence details needed by a failing artifact.
pub fn derive_edit_report(
    manifest: &EditManifest,
    report: &EditReport,
) -> Result<EditSummary, Vec<ValidationFinding>> {
    let validation = validate_edit_report(manifest, report);
    if !validation.errors.is_empty() {
        return Err(validation.errors);
    }
    let automatic_workers = report
        .rows
        .iter()
        .find(|row| row.worker_mode == WorkerMode::Automatic)
        .and_then(|row| row.samples.first())
        .map(|sample| sample.resolved_workers)
        .unwrap_or(0);
    let reference_host_eligible = manifest.reference_host.admits(
        &report.identity.environment,
        &report.identity.target,
        automatic_workers,
    );
    let rows = if validation.divergences.is_empty() {
        report
            .rows
            .iter()
            .map(|row| {
                let mut diagnostics = Vec::new();
                let mut codegen = Vec::new();
                let mut objects = Vec::new();
                let mut runnable = Vec::new();
                let mut link = Vec::new();
                let mut share = Vec::new();
                for sample in &row.samples {
                    match &sample.outcome {
                        EditOutcome::Success { endpoints, .. } => {
                            codegen.push(endpoints.codegen_ready_ns);
                            objects.push(endpoints.objects_ready_ns);
                            runnable.push(endpoints.runnable_ready_ns);
                            let band = endpoints
                                .runnable_ready_ns
                                .saturating_sub(endpoints.objects_ready_ns);
                            link.push(band);
                            share.push(
                                ((u128::from(band) * 10_000)
                                    / u128::from(endpoints.runnable_ready_ns))
                                    as u64,
                            );
                        }
                        EditOutcome::ExpectedDiagnostics {
                            diagnostics_ready_ns,
                            ..
                        } => diagnostics.push(*diagnostics_ready_ns),
                        EditOutcome::UnexpectedFailure { .. } => {
                            unreachable!("validation rejected unexpected failure")
                        }
                    }
                }
                EditRowSummary {
                    workload: row.workload.clone(),
                    scenario: row.scenario,
                    worker_mode: row.worker_mode,
                    diagnostics_ready_ns: (!diagnostics.is_empty())
                        .then(|| summarize(&diagnostics)),
                    codegen_ready_ns: (!codegen.is_empty()).then(|| summarize(&codegen)),
                    objects_ready_ns: (!objects.is_empty()).then(|| summarize(&objects)),
                    runnable_ready_ns: (!runnable.is_empty()).then(|| summarize(&runnable)),
                    link_band: (!link.is_empty()).then(|| LinkBandSummary {
                        duration_ns: summarize(&link),
                        share_basis_points: summarize(&share),
                    }),
                    work: derived_work(&row.samples),
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let retention_max_current_bytes = report
        .retention
        .revisions
        .iter()
        .map(|step| step.retention.current_bytes)
        .max()
        .unwrap_or(0);
    let retention_peak_bytes = report
        .retention
        .revisions
        .iter()
        .map(|step| step.retention.peak_bytes)
        .max()
        .unwrap_or(0);
    let retention_final_bytes = report
        .retention
        .revisions
        .last()
        .map(|step| step.retention.current_bytes)
        .unwrap_or(0);
    Ok(EditSummary {
        schema_version: 1,
        fixture_revision: report.identity.fixture_revision,
        commit: report.identity.commit.clone(),
        target: report.identity.target.clone(),
        reference_host_eligible,
        rows,
        divergences: validation.divergences,
        retention_revisions: report.retention.revisions.len() as u32,
        retention_max_current_bytes,
        retention_peak_bytes,
        retention_final_bytes,
    })
}

/// Render a deterministic Markdown view from a derived report.
pub fn render_edit_report_markdown(summary: &EditSummary) -> String {
    let mut out = String::new();
    out.push_str("# Retained-session compiler performance\n\n");
    out.push_str(&format!(
        "- fixture revision: {}\n- compiler: `{}`\n- target: `{}`\n- reference-host eligible: {}\n\n",
        summary.fixture_revision,
        summary.commit,
        summary.target,
        if summary.reference_host_eligible { "yes" } else { "no" }
    ));
    if !summary.divergences.is_empty() {
        out.push_str("## Compiler divergence\n\n");
        for divergence in &summary.divergences {
            out.push_str(&format!("- `{}`: {}\n", divergence.path, divergence.detail));
        }
        out.push('\n');
    } else {
        out.push_str("| workload | scenario | workers | diagnostics ns | codegen ns | objects ns | runnable ns | link ns | link bp | computed | reused |\n");
        out.push_str(
            "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        );
        for row in &summary.rows {
            let value = |summary: &Option<EndpointSummary>| {
                summary
                    .as_ref()
                    .map(|value| format!("{} ± {}", value.median, value.mad))
                    .unwrap_or_else(|| "—".to_string())
            };
            let link = row
                .link_band
                .as_ref()
                .map(|value| format!("{} ± {}", value.duration_ns.median, value.duration_ns.mad))
                .unwrap_or_else(|| "—".to_string());
            let share = row
                .link_band
                .as_ref()
                .map(|value| {
                    format!(
                        "{} ± {}",
                        value.share_basis_points.median, value.share_basis_points.mad
                    )
                })
                .unwrap_or_else(|| "—".to_string());
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} ± {} | {} ± {} |\n",
                row.workload,
                row.scenario.wire_name(),
                row.worker_mode.wire_name(),
                value(&row.diagnostics_ready_ns),
                value(&row.codegen_ready_ns),
                value(&row.objects_ready_ns),
                value(&row.runnable_ready_ns),
                link,
                share,
                row.work.computed.median,
                row.work.computed.mad,
                row.work.reused.median,
                row.work.reused.mad,
            ));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "Retention sequence: {} revisions; max current {} bytes; peak {} bytes; final {} bytes.\n",
        summary.retention_revisions,
        summary.retention_max_current_bytes,
        summary.retention_peak_bytes,
        summary.retention_final_bytes,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKED_IN_MANIFEST: &str = include_str!("incremental.toml");

    const MANIFEST: &str = r#"
schema_version = 1
report_schema_version = 1
fixture_revision = 3
samples_per_row = 5
retention_revisions = 1000
rotation = "left_by_sample"
target = "x86-64-linux"
optimization = "default"
compiler_args = []

[[scenarios]]
scenario = "no_op_reobservation"
expected_outcome = "success"
structural_claim = "no compiler artifact recomputes"
[[scenarios]]
scenario = "unreachable_body"
expected_outcome = "success"
structural_claim = "rooted terminals remain green"
[[scenarios]]
scenario = "reached_body_only"
expected_outcome = "success"
structural_claim = "one body cone recomputes"
[[scenarios]]
scenario = "callable_signature"
expected_outcome = "success"
structural_claim = "exact ABI consumers invalidate"
[[scenarios]]
scenario = "layout_abi"
expected_outcome = "success"
structural_claim = "exact layout consumers invalidate"
[[scenarios]]
scenario = "import_set"
expected_outcome = "success"
structural_claim = "changed discovery cone recomputes"
[[scenarios]]
scenario = "reachability_deletion"
expected_outcome = "success"
structural_claim = "removed units leave the image"
[[scenarios]]
scenario = "error_introduction"
expected_outcome = "diagnostics"
structural_claim = "only the diagnostic cone recomputes"

[[workers]]
mode = "one"
compiler_arg = "-j1"
[[workers]]
mode = "automatic"
compiler_arg = "-j0"

[[workloads]]
id = "mosaic"
source = "examples/mosaic/main.rue"
question = "medium maintained program"
[[workloads]]
id = "lattice"
source = "examples/lattice/main.rue"
question = "largest maintained program"

[reference_host]
target = "x86-64-linux"
automatic_workers = 4
[reference_host.class]
id = "github-public-ubuntu-24.04-x64-4cpu-16gb-v1"
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"
architecture = "x86_64"
logical_cores = 4
minimum_memory_bytes = 15000000000
"#;

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn environment() -> EnvironmentFingerprint {
        EnvironmentFingerprint {
            runner_label: "github-hosted".to_string(),
            runner_image: "ubuntu-24.04".to_string(),
            runner_image_version: "20260801.1".to_string(),
            cpu_model: "hosted x64".to_string(),
            core_count: 4,
            memory_bytes: 16_000_000_000,
            kernel_version: "6.11".to_string(),
            os_version: "Ubuntu 24.04".to_string(),
            architecture: "x86_64".to_string(),
        }
    }

    fn gauges(index: u32) -> RetainedGauges {
        RetainedGauges {
            current_bytes: 1_000 + u64::from(index),
            peak_bytes: 2_000 + u64::from(index),
            soft_budget_bytes: 10_000,
            protected_overflow_bytes: 0,
            dependency_observations: 100,
            input_observations: 20,
            observation_budget: 1_000,
            protected_overflow_observations: 0,
        }
    }

    fn success_identity() -> OutcomeIdentity {
        OutcomeIdentity {
            kind: OutcomeKind::Success,
            diagnostics: hash('0'),
            warnings: hash('1'),
            executable: Some(hash('2')),
        }
    }

    fn diagnostics_identity() -> OutcomeIdentity {
        OutcomeIdentity {
            kind: OutcomeKind::Diagnostics,
            diagnostics: hash('3'),
            warnings: hash('1'),
            executable: None,
        }
    }

    fn sample(
        scenario: EditScenario,
        scenario_index: u32,
        mode: WorkerMode,
        sample_index: u32,
        workload: &str,
    ) -> EditSample {
        let identity = if scenario == EditScenario::ErrorIntroduction {
            diagnostics_identity()
        } else {
            success_identity()
        };
        EditSample {
            sample_index,
            session_id: format!(
                "{workload}-{}-{}-{sample_index}",
                scenario.wire_name(),
                mode.wire_name()
            ),
            collection_order: (scenario_index + 8 - sample_index % 8) % 8,
            resolved_workers: if mode == WorkerMode::One { 1 } else { 4 },
            transformation: if scenario == EditScenario::NoOpReobservation {
                TransformationIdentity::Reobserve {
                    id: format!("{workload}-noop"),
                }
            } else {
                TransformationIdentity::Replace {
                    id: format!("{workload}-{}", scenario.wire_name()),
                    logical_file: "main.rue".to_string(),
                    before_sha256: hash('a'),
                    after_sha256: hash('b'),
                }
            },
            outcome: if scenario == EditScenario::ErrorIntroduction {
                EditOutcome::ExpectedDiagnostics {
                    diagnostics_ready_ns: 50 + u64::from(sample_index),
                    diagnostics: identity.diagnostics.clone(),
                    warnings: identity.warnings.clone(),
                }
            } else {
                EditOutcome::Success {
                    endpoints: EditEndpoints {
                        codegen_ready_ns: 100 + u64::from(sample_index),
                        objects_ready_ns: 150 + u64::from(sample_index),
                        runnable_ready_ns: 200 + u64::from(sample_index),
                    },
                    diagnostics: identity.diagnostics.clone(),
                    warnings: identity.warnings.clone(),
                    executable: identity.executable.clone().unwrap(),
                }
            },
            work: StructuralWork::default(),
            retention: gauges(sample_index),
            oracle: OracleComparison::Matched {
                warm: identity.clone(),
                fresh: identity,
            },
        }
    }

    fn report(manifest: &EditManifest) -> EditReport {
        let mut rows = Vec::new();
        for workload in &manifest.workloads {
            for (scenario_index, scenario) in manifest.scenarios.iter().enumerate() {
                for worker in &manifest.workers {
                    rows.push(EditRow {
                        workload: workload.id.clone(),
                        source: workload.source.clone(),
                        shape: SourceShape {
                            files: 10,
                            modules: 10,
                            bytes: 1_000,
                            lines: 100,
                            tokens: 500,
                            functions: 20,
                        },
                        scenario: scenario.scenario,
                        worker_mode: worker.mode,
                        samples: (0..manifest.samples_per_row)
                            .map(|index| {
                                sample(
                                    scenario.scenario,
                                    scenario_index as u32,
                                    worker.mode,
                                    index,
                                    &workload.id,
                                )
                            })
                            .collect(),
                    });
                }
            }
        }
        let retention_identity = success_identity();
        EditReport {
            schema_version: EDIT_REPORT_SCHEMA_VERSION,
            identity: EditReportIdentity {
                fixture_revision: manifest.fixture_revision,
                commit: std::iter::repeat_n('c', 40).collect(),
                started_at: "2026-08-07T00:00:00Z".to_string(),
                finished_at: "2026-08-07T01:00:00Z".to_string(),
                target: manifest.target.clone(),
                environment: environment(),
            },
            regime: EditReportRegime {
                compiler_state: "retained_session".to_string(),
                os_page_cache: "uncontrolled".to_string(),
                samples_per_row: manifest.samples_per_row,
                retention_revisions: manifest.retention_revisions,
                rotation: manifest.rotation,
                optimization: manifest.optimization,
                compiler_args: manifest.compiler_args.clone(),
            },
            rows,
            retention: RetentionSequence {
                workload: "mosaic".to_string(),
                worker_mode: WorkerMode::Automatic,
                resolved_workers: 4,
                revisions: (0..manifest.retention_revisions)
                    .map(|index| RetentionStep {
                        revision_index: index,
                        state_id: format!("state-{}", index % 7),
                        outcome: RetentionStepOutcome::Success {
                            identity: retention_identity.clone(),
                        },
                        oracle: Some(OracleComparison::Matched {
                            warm: retention_identity.clone(),
                            fresh: retention_identity.clone(),
                        }),
                        retention: gauges(index),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn parses_the_versioned_adr_0068_manifest() {
        let manifest = EditManifest::parse(CHECKED_IN_MANIFEST).unwrap();
        assert_eq!(manifest.scenarios.len(), 8);
        assert_eq!(manifest.workers.len(), 2);
        assert_eq!(manifest.samples_per_row, 5);
        assert_eq!(manifest.retention_revisions, 1_000);
    }

    #[test]
    fn manifest_rejects_unknown_fields_and_incomplete_matrixes() {
        let unknown = MANIFEST.replacen(
            "schema_version = 1",
            "schema_version = 1\nsurprise = true",
            1,
        );
        assert!(
            EditManifest::parse(&unknown)
                .unwrap_err()
                .detail
                .contains("unknown field")
        );
        let missing = MANIFEST.replacen(
            "[[scenarios]]\nscenario = \"unreachable_body\"\nexpected_outcome = \"success\"\nstructural_claim = \"rooted terminals remain green\"\n",
            "",
            1,
        );
        assert!(
            EditManifest::parse(&missing)
                .unwrap_err()
                .detail
                .contains("scenario matrix")
        );
    }

    #[test]
    fn complete_matrix_validates_and_derives_deterministically() {
        let manifest = EditManifest::parse(MANIFEST).unwrap();
        let report = report(&manifest);
        let validation = validate_edit_report(&manifest, &report);
        assert!(validation.is_success(), "{validation:?}");
        let first = derive_edit_report(&manifest, &report).unwrap();
        let second = derive_edit_report(&manifest, &report).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.rows.len(), 32);
        assert!(first.reference_host_eligible);
        assert_eq!(
            render_edit_report_markdown(&first),
            render_edit_report_markdown(&second)
        );
        let json = serde_json::to_string(&report).unwrap();
        let decoded: EditReport = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, report);
    }

    #[test]
    fn missing_rows_bad_endpoints_workers_and_unknown_fields_fail_loudly() {
        let manifest = EditManifest::parse(MANIFEST).unwrap();
        let mut missing = report(&manifest);
        missing.rows.pop();
        assert!(!validate_edit_report(&manifest, &missing).is_structurally_valid());

        let mut malformed = report(&manifest);
        let sample = &mut malformed.rows[0].samples[0];
        sample.resolved_workers = 2;
        if let EditOutcome::Success { endpoints, .. } = &mut sample.outcome {
            endpoints.objects_ready_ns = endpoints.codegen_ready_ns - 1;
        }
        let validation = validate_edit_report(&manifest, &malformed);
        assert!(
            validation
                .errors
                .iter()
                .any(|error| error.detail.contains("one-worker"))
        );
        assert!(
            validation
                .errors
                .iter()
                .any(|error| error.detail.contains("monotonic"))
        );

        let mut unpinned = report(&manifest);
        unpinned
            .rows
            .iter_mut()
            .find(|row| row.worker_mode == WorkerMode::Automatic)
            .unwrap()
            .samples[0]
            .resolved_workers = 3;
        let validation = validate_edit_report(&manifest, &unpinned);
        assert!(
            validation
                .errors
                .iter()
                .any(|error| error.detail.contains("detected parallelism"))
        );

        let mut overflowed = report(&manifest);
        let work = &mut overflowed.rows[0].samples[0].work;
        work.source_observation.computed = u64::MAX;
        work.import_discovery.computed = 1;
        let validation = validate_edit_report(&manifest, &overflowed);
        assert!(
            validation
                .errors
                .iter()
                .any(|error| error.detail.contains("work totals overflowed"))
        );

        let json = serde_json::to_string(&report(&manifest)).unwrap();
        let unknown = json.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"surprise\":true",
            1,
        );
        assert!(
            serde_json::from_str::<EditReport>(&unknown)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }

    #[test]
    fn divergence_is_failing_evidence_not_an_infrastructure_invalidity() {
        let manifest = EditManifest::parse(MANIFEST).unwrap();
        let mut report = report(&manifest);
        let sample = &mut report.rows[0].samples[0];
        let warm = sample.outcome.identity();
        let mut fresh = warm.clone();
        fresh.executable = Some(hash('d'));
        sample.oracle = OracleComparison::Diverged {
            warm,
            fresh,
            first_difference: "executable fingerprint".to_string(),
        };
        let validation = validate_edit_report(&manifest, &report);
        assert!(validation.is_structurally_valid());
        assert!(!validation.is_success());
        let derived = derive_edit_report(&manifest, &report).unwrap();
        assert!(derived.rows.is_empty());
        assert_eq!(derived.divergences.len(), 1);
        assert!(render_edit_report_markdown(&derived).contains("Compiler divergence"));
    }
}
