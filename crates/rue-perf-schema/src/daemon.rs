//! The explicit daemon client performance boundary (RUE-2130).
//!
//! This record is deliberately separate from [`crate::BenchmarkReport`].  A
//! fresh report measures one compiler process and rejects retained state; a
//! daemon observation measures the caller-visible operation through a local
//! service.  Sharing fields between the two would make it possible to publish
//! a daemon result as fresh evidence by accident.

use serde::{Deserialize, Serialize};

/// Wire kind for daemon client performance records.
pub const DAEMON_PERFORMANCE_RECORD_KIND: &str = "daemon_client_to_publication_v1";
/// Version of the daemon client performance record.
pub const DAEMON_PERFORMANCE_SCHEMA_VERSION: u32 = 1;

/// The operation whose external wall clock is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonArtifact {
    /// A linked executable, including publication to the requested path.
    Executable,
    /// The analysis presentation (`--emit air`).
    Analysis,
    /// A test image before the test runner starts.
    TestImage,
}

/// The source revision transition being exercised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonScenario {
    FirstClientEmpty,
    PreparedNoEdit,
    MixedAnalysis,
    MixedTest,
    MixedBuild,
    BodyEdit,
    ApiEdit,
    ImportEdit,
    Error,
    Repair,
    Revert,
    Eviction,
    Restart,
    Contention,
}

/// Whether an endpoint used a fresh process or the explicitly requested
/// daemon.  This is a field, rather than an inferred label, so validation can
/// reject a producer that records the wrong boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    DirectFresh,
    Daemon,
}

/// Identity of one exact daemon request and its captured inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonIdentity {
    /// Filled by the external paired-run observer. A compiler-side sidecar
    /// leaves this absent so hashing the running executable cannot dominate
    /// the invocation it is measuring.
    pub compiler_image_sha256: Option<String>,
    pub compiler_version: String,
    pub compiler_build_profile: crate::CompilerBuildProfile,
    pub protocol_version: u32,
    pub request_ticket: Option<u64>,
    pub daemon_generation: String,
    pub session_generation: String,
    /// Absent when the request failed before a source/read closure was
    /// accepted. A producer must not hash option labels as a substitute.
    pub input_sha256: Option<String>,
    pub target: String,
    pub requested_workers: u32,
    pub workers: u32,
    pub optimization: String,
    pub preview_features: Vec<String>,
    pub test_jobs: u32,
}

/// Raw phase clocks from one compiler endpoint.  All are external observations
/// except the optional compiler-owned observation/link fields; they must never
/// be summed as if nested compiler spans were disjoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonTiming {
    /// External monotonic start, in the runner's single clock domain.
    pub client_started_ns: Option<u64>,
    /// External client wall clock through publication, or through test
    /// execution for a test invocation.  Internal phase clocks are nested
    /// observations and must not be added to this value.
    pub client_spawn_to_exit_ns: Option<u64>,
    pub executor_ns: Option<u64>,
    /// Compiler-owned phase clocks are optional because the daemon protocol
    /// does not promise them for every artifact. Missing is different from a
    /// measured zero and must not be filled in by a producer.
    pub observation_ns: Option<u64>,
    pub link_ns: Option<u64>,
    pub transfer_ns: Option<u64>,
    pub test_preparation_ns: Option<u64>,
    pub test_execution_ns: Option<u64>,
}

/// One direct or daemon endpoint measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonEndpoint {
    pub path: ExecutionPath,
    pub identity: DaemonIdentity,
    pub timing: DaemonTiming,
    pub output_sha256: Option<String>,
    pub diagnostics_sha256: Option<String>,
    pub exit_code: Option<i32>,
    /// Stable behavior projection for a successful executable run. The
    /// external runner owns this because it executes the published image.
    pub behavior_sha256: Option<String>,
    /// Digest of the prepared test image after client-side publication. This
    /// is distinct from the canonical event projection and raw execution
    /// proof, which describe what the runner did with the image.
    pub prepared_image_sha256: Option<String>,
    /// True only when a test invocation actually dispatched and reaped test
    /// processes. Image preparation alone is not test execution evidence.
    pub tests_executed: bool,
    /// Number of test child processes actually reaped by the runner.
    pub tests_executed_count: Option<u64>,
    /// A digest of the actual test event stream or execution output. This
    /// proves a repeated invocation ran; a preparation-only success cannot
    /// claim execution by setting `tests_executed`.
    pub execution_proof_sha256: Option<String>,
}

/// Resource and structural-work observations returned by the daemon status
/// surface. These are gauges/counters, not lifecycle logging.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonWork {
    pub query_claims: Option<u64>,
    pub query_reuses: Option<u64>,
    pub source_bytes: u64,
    pub retained_charge_bytes: u64,
    pub dependency_pins: u64,
    /// Response lease charge is transport-owned and may be unavailable to a
    /// compiler-side sidecar.  It must not be represented as a measured zero.
    pub response_bytes: Option<u64>,
}

/// One paired direct/daemon observation in caller order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonObservation {
    pub sequence: u32,
    pub scenario: DaemonScenario,
    pub artifact: DaemonArtifact,
    pub direct: DaemonEndpoint,
    pub daemon: DaemonEndpoint,
    pub work: DaemonWork,
    /// The runner's independently computed parity verdict. Validation checks
    /// the underlying hashes too; this field prevents presentation-only claims
    /// from being mistaken for a byte comparison.
    pub direct_equivalent: bool,
    pub contention: Option<DaemonContention>,
}

/// External admission evidence collected while concurrent clients are alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonContention {
    pub group: u32,
    pub clients: u32,
    pub observed_queued_requests: u32,
}

/// A complete, raw daemon performance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonPerformanceReport {
    pub record_kind: String,
    pub schema_version: u32,
    pub compiler_revision: String,
    pub compiler_image_sha256: String,
    pub started_at: String,
    pub finished_at: String,
    /// The scenarios this run promised to cover. Validation keeps a partial
    /// collection honest instead of silently treating it as a full regime.
    pub required_scenarios: Vec<DaemonScenario>,
    /// Partial diagnostic runs are allowed, but they cannot be presented as
    /// the complete qualification matrix without this explicit assertion.
    pub qualification_complete: bool,
    pub observations: Vec<DaemonObservation>,
}

/// One compiler invocation sidecar. The external runner pairs direct and
/// daemon sidecars into a [`DaemonPerformanceReport`]. Keeping this on a
/// separate file preserves ordinary stdout and stderr exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonInvocationRecord {
    pub artifact: DaemonArtifact,
    pub record_kind: String,
    pub schema_version: u32,
    pub endpoint: DaemonEndpoint,
    pub work: Option<DaemonWork>,
}

pub fn validate_daemon_invocation_record(record: &DaemonInvocationRecord) -> Vec<String> {
    let mut errors = Vec::new();
    if record.record_kind != DAEMON_PERFORMANCE_RECORD_KIND {
        errors.push("record_kind is not the daemon client boundary".to_string());
    }
    if record.schema_version != DAEMON_PERFORMANCE_SCHEMA_VERSION {
        errors.push("unsupported daemon performance schema version".to_string());
    }
    // Sidecars are emitted by both halves of the paired run.  Validate the
    // endpoint's declared path rather than assuming every input to this
    // command came from the daemon client.
    validate_endpoint(
        "endpoint",
        &record.endpoint,
        record.endpoint.path == ExecutionPath::Daemon,
        false,
        &mut errors,
    );
    errors
}

impl DaemonPerformanceReport {
    pub fn content_address(&self) -> Result<String, crate::CanonicalError> {
        crate::content_address(self)
    }
}

/// Structural validation for a daemon report.  This intentionally returns all
/// findings so a malformed report is useful evidence without being publishable.
pub fn validate_daemon_performance_report(report: &DaemonPerformanceReport) -> Vec<String> {
    let mut errors = Vec::new();
    if report.record_kind != DAEMON_PERFORMANCE_RECORD_KIND {
        errors.push("record_kind is not the daemon client boundary".to_string());
    }
    if report.schema_version != DAEMON_PERFORMANCE_SCHEMA_VERSION {
        errors.push("unsupported daemon performance schema version".to_string());
    }
    if report.compiler_revision.len() != 40
        || !report
            .compiler_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        errors.push("compiler_revision is not a lowercase 40-character revision".to_string());
    }
    if report.compiler_image_sha256.len() != 64
        || !report
            .compiler_image_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        errors.push("compiler_image_sha256 is not a lowercase SHA-256".to_string());
    }
    if report.started_at.is_empty()
        || report.finished_at.is_empty()
        || report.started_at >= report.finished_at
    {
        errors.push("report timestamps are empty or not strictly ordered".to_string());
    }
    if report.required_scenarios.is_empty() {
        errors.push("required_scenarios is empty".to_string());
    }
    if report.observations.is_empty() {
        errors.push("daemon performance report has no observations".to_string());
    }
    for required in &report.required_scenarios {
        if !report
            .observations
            .iter()
            .any(|observation| observation.scenario == *required)
        {
            errors.push(format!("required scenario {required:?} has no observation"));
        }
    }
    if report.qualification_complete {
        let complete = [
            DaemonScenario::FirstClientEmpty,
            DaemonScenario::PreparedNoEdit,
            DaemonScenario::MixedAnalysis,
            DaemonScenario::MixedTest,
            DaemonScenario::MixedBuild,
            DaemonScenario::BodyEdit,
            DaemonScenario::ApiEdit,
            DaemonScenario::ImportEdit,
            DaemonScenario::Error,
            DaemonScenario::Repair,
            DaemonScenario::Revert,
            DaemonScenario::Eviction,
            DaemonScenario::Restart,
            DaemonScenario::Contention,
        ];
        for scenario in complete {
            if !report
                .observations
                .iter()
                .any(|observation| observation.scenario == scenario)
            {
                errors.push(format!("complete qualification lacks {scenario:?}"));
            }
        }
    }
    let mut expected_sequence = 0;
    for (index, observation) in report.observations.iter().enumerate() {
        if observation.sequence != expected_sequence {
            errors.push(format!(
                "observations[{index}].sequence is not caller order"
            ));
        }
        expected_sequence = expected_sequence.saturating_add(1);
        validate_endpoint(
            &format!("observations[{index}].direct"),
            &observation.direct,
            false,
            true,
            &mut errors,
        );
        validate_endpoint(
            &format!("observations[{index}].daemon"),
            &observation.daemon,
            true,
            true,
            &mut errors,
        );
        for (name, endpoint) in [
            ("direct", &observation.direct),
            ("daemon", &observation.daemon),
        ] {
            let expected_exit = if observation.scenario == DaemonScenario::Error {
                1
            } else {
                0
            };
            if endpoint.exit_code != Some(expected_exit) {
                errors.push(format!("observations[{index}].{name} did not reach the scenario's expected compiler outcome"));
            }
            if endpoint.identity.compiler_build_profile
                != observation.direct.identity.compiler_build_profile
                || (report.qualification_complete
                    && endpoint.identity.compiler_build_profile
                        != crate::CompilerBuildProfile::ReleaseThinLto)
            {
                errors.push(format!(
                    "observations[{index}].{name} compiler build profile does not qualify"
                ));
            }
            if endpoint.identity.compiler_image_sha256.as_deref()
                != Some(report.compiler_image_sha256.as_str())
            {
                errors.push(format!(
                    "observations[{index}].{name}.identity.compiler_image_sha256 disagrees with report"
                ));
            }
            if endpoint.identity.target != observation.direct.identity.target {
                errors.push(format!(
                    "observations[{index}].{name}.identity.target disagrees with direct endpoint"
                ));
            }
            if endpoint.identity.optimization != observation.direct.identity.optimization {
                errors.push(format!(
                    "observations[{index}].{name}.identity.optimization disagrees with direct endpoint"
                ));
            }
            if endpoint.identity.preview_features != observation.direct.identity.preview_features {
                errors.push(format!(
                    "observations[{index}].{name}.identity.preview_features disagrees with direct endpoint"
                ));
            }
        }
        let hashes_agree = observation
            .direct
            .output_sha256
            .as_ref()
            .zip(observation.daemon.output_sha256.as_ref())
            .is_some_and(|(direct, daemon)| direct == daemon)
            && observation
                .direct
                .diagnostics_sha256
                .as_ref()
                .zip(observation.daemon.diagnostics_sha256.as_ref())
                .is_some_and(|(direct, daemon)| direct == daemon)
            && observation
                .direct
                .exit_code
                .zip(observation.daemon.exit_code)
                .is_some_and(|(direct, daemon)| direct == daemon);
        if !hashes_agree {
            errors.push(format!(
                "observations[{index}] direct and daemon behavior differs"
            ));
        }
        if observation.direct_equivalent != hashes_agree {
            errors.push(format!(
                "observations[{index}].direct_equivalent disagrees with endpoint evidence"
            ));
        }
        if observation.direct.identity.input_sha256 != observation.daemon.identity.input_sha256 {
            errors.push(format!(
                "observations[{index}] endpoints use different accepted input identities"
            ));
        }
        if matches!(observation.artifact, DaemonArtifact::Executable)
            && observation.direct.exit_code == Some(0)
            && observation.daemon.exit_code == Some(0)
        {
            match (
                observation.direct.behavior_sha256.as_ref(),
                observation.daemon.behavior_sha256.as_ref(),
            ) {
                (Some(direct), Some(daemon)) if direct == daemon => {}
                (None, _) | (_, None) => errors.push(format!(
                    "observations[{index}] successful executable lacks behavior evidence"
                )),
                (Some(_), Some(_)) => {
                    errors.push(format!("observations[{index}] executable behavior differs"))
                }
            }
        }
        if matches!(observation.artifact, DaemonArtifact::TestImage)
            && (observation.daemon.timing.test_preparation_ns.is_none()
                || observation.daemon.timing.test_execution_ns.is_none()
                || observation.direct.timing.test_preparation_ns.is_none()
                || observation.direct.timing.test_execution_ns.is_none()
                || !observation.direct.tests_executed
                || !observation.daemon.tests_executed
                || observation.direct.tests_executed_count.is_none()
                || observation.daemon.tests_executed_count.is_none()
                || observation.direct.execution_proof_sha256.is_none()
                || observation.daemon.execution_proof_sha256.is_none()
                || observation.daemon.prepared_image_sha256.is_none()
                || observation.direct.prepared_image_sha256.is_none())
        {
            errors.push(format!(
                "observations[{index}] test image lacks preparation/execution evidence"
            ));
        }
        if matches!(observation.artifact, DaemonArtifact::TestImage)
            && observation.direct.prepared_image_sha256 != observation.daemon.prepared_image_sha256
        {
            errors.push(format!(
                "observations[{index}] direct and daemon prepared test images differ"
            ));
        }
        if matches!(observation.artifact, DaemonArtifact::TestImage) {
            if observation.direct.behavior_sha256.is_none()
                || observation.direct.behavior_sha256 != observation.daemon.behavior_sha256
                || observation.direct.tests_executed_count
                    != observation.daemon.tests_executed_count
                || observation.direct.identity.test_jobs == 0
                || observation.direct.identity.test_jobs != observation.daemon.identity.test_jobs
            {
                errors.push(format!(
                    "observations[{index}] test behavior or execution policy differs"
                ));
            }
            for endpoint in [&observation.direct, &observation.daemon] {
                if endpoint.output_sha256 != endpoint.prepared_image_sha256 {
                    errors.push(format!(
                        "observations[{index}] test output is not the prepared image"
                    ));
                }
            }
        }
        if observation.work.query_claims.is_none()
            || observation.work.query_reuses.is_none()
            || observation.work.source_bytes == 0
            || observation.daemon.timing.executor_ns.is_none()
            || observation.daemon.timing.observation_ns.is_none()
            || observation.daemon.timing.transfer_ns.is_none()
        {
            errors.push(format!(
                "observations[{index}] lacks request-owned compiler work or phase evidence"
            ));
        }
        if observation.scenario != DaemonScenario::Error
            && matches!(
                observation.artifact,
                DaemonArtifact::Executable | DaemonArtifact::TestImage
            )
            && observation.daemon.timing.link_ns.is_none()
        {
            errors.push(format!(
                "observations[{index}] linked image lacks link phase evidence"
            ));
        }
    }
    validate_transitions(report, &mut errors);
    errors
}

fn same_session(left: &DaemonObservation, right: &DaemonObservation) -> bool {
    left.daemon.identity.daemon_generation == right.daemon.identity.daemon_generation
        && left.daemon.identity.session_generation == right.daemon.identity.session_generation
}

fn same_input(left: &DaemonObservation, right: &DaemonObservation) -> bool {
    left.daemon.identity.input_sha256.is_some()
        && left.daemon.identity.input_sha256 == right.daemon.identity.input_sha256
}

fn overlaps(left: &DaemonEndpoint, right: &DaemonEndpoint) -> bool {
    let (Some(left_start), Some(left_duration), Some(right_start), Some(right_duration)) = (
        left.timing.client_started_ns,
        left.timing.client_spawn_to_exit_ns,
        right.timing.client_started_ns,
        right.timing.client_spawn_to_exit_ns,
    ) else {
        return false;
    };
    let (Some(left_end), Some(right_end)) = (
        left_start.checked_add(left_duration),
        right_start.checked_add(right_duration),
    ) else {
        return false;
    };
    left_start.max(right_start) < left_end.min(right_end)
}

fn validate_transitions(report: &DaemonPerformanceReport, errors: &mut Vec<String>) {
    let rows = &report.observations;
    let mut requests = std::collections::BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        if let Some(ticket) = row.daemon.identity.request_ticket
            && !requests.insert((&row.daemon.identity.daemon_generation, ticket))
        {
            errors.push(format!(
                "observations[{index}] repeats a daemon request ticket"
            ));
        }
        let previous = index.checked_sub(1).map(|index| &rows[index]);
        match row.scenario {
            DaemonScenario::PreparedNoEdit => {
                if !previous.is_some_and(|previous| {
                    same_session(previous, row) && same_input(previous, row)
                }) || row.work.query_reuses.is_none_or(|count| count == 0)
                {
                    errors.push(format!("observations[{index}] prepared request lacks unchanged inputs, retained session, or shared query reuse"));
                }
            }
            DaemonScenario::BodyEdit
            | DaemonScenario::ApiEdit
            | DaemonScenario::ImportEdit
            | DaemonScenario::Error
            | DaemonScenario::Repair
            | DaemonScenario::Revert => {
                if !previous.is_some_and(|previous| {
                    same_session(previous, row) && !same_input(previous, row)
                }) {
                    errors.push(format!("observations[{index}] edit transition lacks changed inputs in the retained session"));
                }
                if row.scenario == DaemonScenario::Repair
                    && !previous.is_some_and(|previous| previous.scenario == DaemonScenario::Error)
                {
                    errors.push(format!(
                        "observations[{index}] repair has no preceding error"
                    ));
                }
                if row.scenario == DaemonScenario::Revert
                    && !rows[..index].iter().any(|earlier| {
                        same_session(earlier, row)
                            && same_input(earlier, row)
                            && earlier.direct.exit_code == Some(0)
                    })
                {
                    errors.push(format!(
                        "observations[{index}] revert does not restore earlier accepted inputs"
                    ));
                }
            }
            DaemonScenario::Restart => {
                if !previous.is_some_and(|previous| {
                    same_input(previous, row)
                        && previous.daemon.identity.daemon_generation
                            != row.daemon.identity.daemon_generation
                }) {
                    errors.push(format!(
                        "observations[{index}] restart lacks a new daemon for the same inputs"
                    ));
                }
            }
            DaemonScenario::Contention => {
                let Some(contention) = &row.contention else {
                    errors.push(format!(
                        "observations[{index}] contention lacks admission evidence"
                    ));
                    continue;
                };
                let peers = rows
                    .iter()
                    .filter(|other| {
                        other.sequence != row.sequence
                            && other
                                .contention
                                .as_ref()
                                .is_some_and(|other| other.group == contention.group)
                    })
                    .collect::<Vec<_>>();
                if contention.clients < 2
                    || peers.len() + 1 != contention.clients as usize
                    || contention.observed_queued_requests == 0
                    || !peers.iter().any(|other| {
                        same_session(row, other)
                            && same_input(row, other)
                            && overlaps(&row.direct, &other.direct)
                            && overlaps(&row.daemon, &other.daemon)
                    })
                {
                    errors.push(format!("observations[{index}] contention lacks overlapping admitted clients and a queued request"));
                }
            }
            _ => {}
        }
        if row.scenario != DaemonScenario::Contention && row.contention.is_some() {
            errors.push(format!(
                "observations[{index}] has contention evidence on another scenario"
            ));
        }
    }
    if !report.qualification_complete {
        return;
    }
    if rows.first().map(|row| row.scenario) != Some(DaemonScenario::FirstClientEmpty) {
        errors.push("complete qualification must begin with the empty daemon".into());
    }
    for order in [
        [
            DaemonArtifact::Analysis,
            DaemonArtifact::TestImage,
            DaemonArtifact::Executable,
        ],
        [
            DaemonArtifact::Executable,
            DaemonArtifact::TestImage,
            DaemonArtifact::Analysis,
        ],
    ] {
        if !rows.windows(3).any(|window| {
            window.iter().map(|row| row.artifact).eq(order)
                && same_session(&window[0], &window[1])
                && same_session(&window[1], &window[2])
                && same_input(&window[0], &window[1])
                && same_input(&window[1], &window[2])
                && window[1..]
                    .iter()
                    .all(|row| row.work.query_reuses.is_some_and(|count| count > 0))
        }) {
            errors.push(format!(
                "complete qualification lacks shared computations across mixed root order {order:?}"
            ));
        }
    }
    if !rows.windows(2).any(|window| {
        window
            .iter()
            .all(|row| row.scenario == DaemonScenario::Eviction)
            && same_input(&window[0], &window[1])
            && window[0].daemon.identity.daemon_generation
                == window[1].daemon.identity.daemon_generation
            && window[0].daemon.identity.session_generation
                != window[1].daemon.identity.session_generation
    }) {
        errors.push(
            "complete qualification lacks a recreated session after eviction of the same input"
                .into(),
        );
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_endpoint(
    path: &str,
    endpoint: &DaemonEndpoint,
    daemon: bool,
    require_external: bool,
    errors: &mut Vec<String>,
) {
    let expected_path = if daemon {
        ExecutionPath::Daemon
    } else {
        ExecutionPath::DirectFresh
    };
    if endpoint.path != expected_path {
        errors.push(format!("{path}.path does not match its endpoint"));
    }
    let identity = &endpoint.identity;
    if identity.compiler_version.is_empty()
        || identity.daemon_generation.is_empty()
        || identity.session_generation.is_empty()
        || identity.target.is_empty()
        || identity.optimization.is_empty()
        || (require_external && identity.workers == 0)
    {
        errors.push(format!("{path}.identity is incomplete"));
    }
    for (name, value, required) in [
        ("output_sha256", &endpoint.output_sha256, require_external),
        (
            "diagnostics_sha256",
            &endpoint.diagnostics_sha256,
            require_external,
        ),
        (
            "identity.compiler_image_sha256",
            &identity.compiler_image_sha256,
            require_external,
        ),
        (
            "identity.input_sha256",
            &identity.input_sha256,
            require_external,
        ),
        ("behavior_sha256", &endpoint.behavior_sha256, false),
        (
            "prepared_image_sha256",
            &endpoint.prepared_image_sha256,
            false,
        ),
        (
            "execution_proof_sha256",
            &endpoint.execution_proof_sha256,
            require_external && endpoint.tests_executed,
        ),
    ] {
        match value {
            None if required => errors.push(format!("{path}.{name} is unavailable")),
            Some(value) if !valid_sha256(value) => {
                errors.push(format!("{path}.{name} is not a lowercase SHA-256"))
            }
            _ => {}
        }
    }
    if require_external && endpoint.exit_code.is_none() {
        errors.push(format!("{path}.exit_code is unavailable"));
    }
    if require_external
        && daemon
        && (identity.protocol_version == 0 || identity.request_ticket.is_none())
    {
        errors.push(format!("{path} lacks an admitted daemon request identity"));
    }
    if !daemon && (identity.protocol_version != 0 || identity.request_ticket.is_some()) {
        errors.push(format!(
            "{path} fresh endpoint claims a daemon request identity"
        ));
    }
    if endpoint.tests_executed && endpoint.tests_executed_count.is_none_or(|count| count == 0) {
        errors.push(format!("{path}.tests_executed lacks actual reaped tests"));
    }
    if !endpoint.tests_executed && endpoint.tests_executed_count.is_some() {
        errors.push(format!(
            "{path}.tests_executed_count is present without execution"
        ));
    }
    let timing = &endpoint.timing;
    if require_external
        && (timing.client_started_ns.is_none()
            || timing
                .client_spawn_to_exit_ns
                .is_none_or(|duration| duration == 0))
    {
        errors.push(format!(
            "{path} external client interval is unavailable or empty"
        ));
    }
    if let (Some(start), Some(duration)) =
        (timing.client_started_ns, timing.client_spawn_to_exit_ns)
        && start.checked_add(duration).is_none()
    {
        errors.push(format!("{path} external client interval overflows"));
    }
    for (name, value) in [
        ("executor_ns", timing.executor_ns),
        ("observation_ns", timing.observation_ns),
        ("link_ns", timing.link_ns),
        ("transfer_ns", timing.transfer_ns),
        ("test_preparation_ns", timing.test_preparation_ns),
        ("test_execution_ns", timing.test_execution_ns),
    ] {
        if let (Some(phase), Some(total)) = (value, timing.client_spawn_to_exit_ns)
            && phase > total
        {
            errors.push(format!(
                "{path}.{name} exceeds the external client interval"
            ));
        }
    }
    for (name, phase) in [
        ("observation", timing.observation_ns),
        ("link", timing.link_ns),
    ] {
        if let (Some(phase), Some(executor)) = (phase, timing.executor_ns)
            && phase > executor
        {
            errors.push(format!("{path} {name} phase exceeds executor time"));
        }
    }
    if let (Some(preparation), Some(execution), Some(total)) = (
        timing.test_preparation_ns,
        timing.test_execution_ns,
        timing.client_spawn_to_exit_ns,
    ) && preparation
        .checked_add(execution)
        .is_none_or(|phases| phases > total)
    {
        errors.push(format!(
            "{path} sequential test phases exceed the external client interval"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_fresh_records_disguised_as_daemon_measurements() {
        let mut report = valid_report();
        report.observations[0].daemon.path = ExecutionPath::DirectFresh;
        assert!(
            validate_daemon_performance_report(&report)
                .iter()
                .any(|error| error.contains("path"))
        );
    }

    #[test]
    fn requires_real_test_execution_and_direct_parity() {
        let mut report = valid_report();
        report.observations[0].daemon.tests_executed = false;
        report.observations[0].direct_equivalent = false;
        assert!(
            validate_daemon_performance_report(&report)
                .iter()
                .any(|error| error.contains("test image"))
        );
        assert!(validate_daemon_performance_report(&report).is_empty() == false);
    }

    #[test]
    fn accepts_a_complete_positive_report_before_negative_mutations() {
        let report = valid_report();
        assert!(validate_daemon_performance_report(&report).is_empty());
    }

    #[test]
    fn accepts_a_direct_invocation_sidecar_for_pairing() {
        let report = valid_report();
        let endpoint = report.observations[0].direct.clone();
        let record = DaemonInvocationRecord {
            artifact: report.observations[0].artifact,
            record_kind: DAEMON_PERFORMANCE_RECORD_KIND.into(),
            schema_version: DAEMON_PERFORMANCE_SCHEMA_VERSION,
            endpoint,
            work: Some(DaemonWork::default()),
        };
        assert!(validate_daemon_invocation_record(&record).is_empty());
    }

    fn valid_report() -> DaemonPerformanceReport {
        let endpoint = |path| DaemonEndpoint {
            path,
            identity: DaemonIdentity {
                compiler_image_sha256: Some("b".repeat(64)),
                compiler_version: "rue-test".into(),
                compiler_build_profile: crate::CompilerBuildProfile::ReleaseThinLto,
                protocol_version: if path == ExecutionPath::Daemon { 2 } else { 0 },
                request_ticket: (path == ExecutionPath::Daemon).then_some(1),
                daemon_generation: "daemon-1".into(),
                session_generation: "session-1".into(),
                input_sha256: Some("b".repeat(64)),
                target: "x86-64-linux".into(),
                requested_workers: 1,
                workers: 1,
                optimization: "O0".into(),
                preview_features: Vec::new(),
                test_jobs: 1,
            },
            timing: DaemonTiming {
                client_started_ns: Some(1),
                client_spawn_to_exit_ns: Some(3),
                executor_ns: Some(1),
                observation_ns: Some(1),
                link_ns: Some(1),
                transfer_ns: Some(1),
                test_preparation_ns: Some(1),
                test_execution_ns: Some(1),
            },
            output_sha256: Some("c".repeat(64)),
            diagnostics_sha256: Some("d".repeat(64)),
            exit_code: Some(0),
            behavior_sha256: Some("f".repeat(64)),
            prepared_image_sha256: Some("c".repeat(64)),
            tests_executed: true,
            tests_executed_count: Some(1),
            execution_proof_sha256: Some("e".repeat(64)),
        };
        DaemonPerformanceReport {
            record_kind: DAEMON_PERFORMANCE_RECORD_KIND.into(),
            schema_version: DAEMON_PERFORMANCE_SCHEMA_VERSION,
            compiler_revision: "a".repeat(40),
            compiler_image_sha256: "b".repeat(64),
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: "2026-01-01T00:00:01Z".into(),
            required_scenarios: vec![DaemonScenario::FirstClientEmpty],
            qualification_complete: false,
            observations: vec![DaemonObservation {
                sequence: 0,
                scenario: DaemonScenario::FirstClientEmpty,
                artifact: DaemonArtifact::TestImage,
                direct: endpoint(ExecutionPath::DirectFresh),
                daemon: endpoint(ExecutionPath::Daemon),
                work: DaemonWork {
                    query_claims: Some(1),
                    query_reuses: Some(1),
                    source_bytes: 10,
                    retained_charge_bytes: 10,
                    dependency_pins: 1,
                    response_bytes: Some(1),
                },
                direct_equivalent: true,
                contention: None,
            }],
        }
    }

    fn complete_report() -> DaemonPerformanceReport {
        use DaemonArtifact::{Analysis, Executable, TestImage};
        use DaemonScenario::*;
        let mut report = valid_report();
        let template = report.observations.pop().unwrap();
        let plan = [
            (FirstClientEmpty, Executable, '1', 1, 1),
            (PreparedNoEdit, Executable, '1', 1, 1),
            (MixedAnalysis, Analysis, '2', 2, 1),
            (MixedTest, TestImage, '2', 2, 1),
            (MixedBuild, Executable, '2', 2, 1),
            (MixedTest, TestImage, '2', 2, 1),
            (MixedAnalysis, Analysis, '2', 2, 1),
            (MixedBuild, Executable, '2', 2, 1),
            (BodyEdit, Executable, '3', 2, 1),
            (ApiEdit, Executable, '4', 2, 1),
            (ImportEdit, Executable, '5', 2, 1),
            (Error, Executable, '6', 2, 1),
            (Repair, Executable, '3', 2, 1),
            (Revert, Executable, '2', 2, 1),
            (Eviction, Executable, '7', 3, 1),
            (Eviction, Executable, '7', 4, 1),
            (MixedBuild, Executable, '2', 5, 1),
            (Restart, Executable, '2', 1, 2),
            (Contention, Executable, '2', 1, 2),
            (Contention, Executable, '2', 1, 2),
        ];
        for (sequence, (scenario, artifact, input, session, daemon)) in plan.into_iter().enumerate()
        {
            let mut row = template.clone();
            row.sequence = sequence as u32;
            row.scenario = scenario;
            row.artifact = artifact;
            row.daemon.identity.daemon_generation = format!("daemon-{daemon}");
            row.daemon.identity.session_generation = session.to_string();
            row.daemon.identity.request_ticket = Some(sequence as u64 + 1);
            for endpoint in [&mut row.direct, &mut row.daemon] {
                endpoint.identity.input_sha256 = Some(input.to_string().repeat(64));
                endpoint.timing.client_started_ns = Some(sequence as u64 * 100);
                endpoint.timing.client_spawn_to_exit_ns = Some(200);
                endpoint.exit_code = Some(if scenario == Error { 1 } else { 0 });
                if artifact != TestImage {
                    endpoint.tests_executed = false;
                    endpoint.tests_executed_count = None;
                    endpoint.execution_proof_sha256 = None;
                    endpoint.prepared_image_sha256 = None;
                    endpoint.timing.test_preparation_ns = None;
                    endpoint.timing.test_execution_ns = None;
                    endpoint.identity.test_jobs = 0;
                }
            }
            if scenario == Contention {
                row.contention = Some(DaemonContention {
                    group: 1,
                    clients: 2,
                    observed_queued_requests: 1,
                });
            }
            if !report.required_scenarios.contains(&scenario) {
                report.required_scenarios.push(scenario);
            }
            report.observations.push(row);
        }
        report.qualification_complete = true;
        report
    }

    #[test]
    fn complete_qualification_validates_transitions_before_mutations() {
        let report = complete_report();
        assert_eq!(
            validate_daemon_performance_report(&report),
            Vec::<String>::new()
        );
        for index in [1, 3, 5] {
            let mut invalid = report.clone();
            invalid.observations[index].work.query_reuses = Some(0);
            assert!(!validate_daemon_performance_report(&invalid).is_empty());
        }
        for index in [15, 17] {
            let mut invalid = report.clone();
            invalid.observations[index]
                .daemon
                .identity
                .session_generation = invalid.observations[index - 1]
                .daemon
                .identity
                .session_generation
                .clone();
            invalid.observations[index]
                .daemon
                .identity
                .daemon_generation = invalid.observations[index - 1]
                .daemon
                .identity
                .daemon_generation
                .clone();
            assert!(!validate_daemon_performance_report(&invalid).is_empty());
        }
        let mut invalid = report.clone();
        invalid.observations[19].daemon.timing.client_started_ns = Some(10_000);
        assert!(!validate_daemon_performance_report(&invalid).is_empty());
        let mut invalid = report;
        invalid.observations[18]
            .contention
            .as_mut()
            .unwrap()
            .observed_queued_requests = 0;
        assert!(!validate_daemon_performance_report(&invalid).is_empty());
    }

    #[test]
    fn matching_crashes_missing_proofs_and_impossible_clocks_do_not_qualify() {
        let report = valid_report();
        let mut invalid = report.clone();
        invalid.observations[0].direct.exit_code = Some(101);
        invalid.observations[0].daemon.exit_code = Some(101);
        assert!(!validate_daemon_performance_report(&invalid).is_empty());
        let mut invalid = report.clone();
        invalid.observations[0].direct.tests_executed = false;
        assert!(!validate_daemon_performance_report(&invalid).is_empty());
        let mut invalid = report.clone();
        invalid.observations[0].daemon.behavior_sha256 = Some("0".repeat(64));
        assert!(!validate_daemon_performance_report(&invalid).is_empty());
        let mut invalid = report;
        invalid.observations[0].daemon.timing.link_ns = Some(2);
        assert!(!validate_daemon_performance_report(&invalid).is_empty());
    }

    #[test]
    fn sequential_test_phases_must_fit_together_without_overflow() {
        let report = valid_report();
        for (preparation, execution, total) in [(2, 2, 3), (u64::MAX, 1, u64::MAX)] {
            let mut invalid = report.clone();
            let timing = &mut invalid.observations[0].daemon.timing;
            timing.test_preparation_ns = Some(preparation);
            timing.test_execution_ns = Some(execution);
            timing.client_spawn_to_exit_ns = Some(total);
            assert!(
                validate_daemon_performance_report(&invalid)
                    .iter()
                    .any(|error| error.contains("sequential test phases"))
            );
        }
    }
}
