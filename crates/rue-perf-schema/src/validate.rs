//! Validation: which observations may enter a series, and which samples count.
//!
//! Two kinds of wrongness are deliberately kept apart.
//!
//! A [`ValidationError`] is an *appendability* failure. The run does not
//! describe the configuration it claims to, so it cannot enter a series at all.
//! This is the guarantee that replaces after-the-fact comparability
//! classification: rather than deciding whether two stored points may be
//! compared, the system refuses to store a point that would not be comparable.
//! Fixing one requires a maintainer to declare the next suite revision or
//! epoch deliberately.
//!
//! An [`InvalidSample`] is a *measurement* failure. The run is well-formed and
//! belongs in its series; one sample within it is not trustworthy. The sample
//! stays on disk as evidence and is excluded from medians, dispersion, and
//! every derived publication.
//!
//! [`Completeness`] is neither. A run that measured fewer workloads than the
//! suite declares is a partial run: its valid workloads still publish
//! per-workload observations, and only the headline point is withheld.

use std::collections::BTreeMap;

use crate::encoding::{
    FULL_EVIDENCE_SCHEMA_VERSION, LEGACY_FULL_EVIDENCE_SCHEMA_VERSION, identity_digest,
    reassemble_witness, work_digest, work_digest_v2,
};
use crate::manifest::Manifest;
use crate::run::{FailureRecord, Phase, RunObject, Sample};
use crate::sanity::{is_commit, is_sha256_digest, is_utc_timestamp, samples_beyond_policy};
use crate::stats::median;
use crate::{LEGACY_RUN_SCHEMA_VERSION, RUN_SCHEMA_VERSION};

/// Every run-object schema this reader accepts, in historical-to-current
/// order. Encoding dispatches on these values rather than record shape.
pub const SUPPORTED_SCHEMA_VERSIONS: [u32; 4] = [
    LEGACY_FULL_EVIDENCE_SCHEMA_VERSION,
    LEGACY_RUN_SCHEMA_VERSION,
    RUN_SCHEMA_VERSION,
    FULL_EVIDENCE_SCHEMA_VERSION,
];

/// A reason a run may not enter a series at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// The run was written under a schema version ahead of this crate.
    ///
    /// Readers implement the four schema shapes that can still be in the
    /// store: historical full v1, historical stored v2, current stored v3,
    /// and current full v4. Refusal applies only to versions ahead of this
    /// reader, never to a supported older shape.
    UnsupportedSchemaVersion {
        /// The version the run object declares.
        found: u32,
        /// Every schema version this crate implements.
        expected: [u32; 4],
    },
    /// The run names a suite revision the manifest does not declare.
    UnknownSuiteRevision {
        /// The undeclared revision.
        revision: u32,
    },
    /// The run names an epoch the manifest does not declare for its platform.
    UnknownEpoch {
        /// The platform named by the run.
        platform: String,
        /// The undeclared epoch.
        epoch: u32,
    },
    /// The run's suite revision is not the one its epoch implements.
    EpochSuiteMismatch {
        /// The revision the epoch implements.
        epoch_revision: u32,
        /// The revision the run claims.
        run_revision: u32,
    },
    /// The run measured a workload the suite revision does not declare.
    ///
    /// Measuring *fewer* workloads is a partial run, not an error; measuring
    /// something else is a different suite wearing this one's name.
    UndeclaredWorkload {
        /// The workload that is not in the suite.
        workload: String,
    },
    /// The run recorded the same workload more than once.
    DuplicateWorkloadObservation {
        /// The repeated workload.
        workload: String,
    },
    /// Workload observations are not in sorted order.
    ///
    /// Ordering is part of the canonical form, so an unsorted run object would
    /// hash differently from the identical measurement written correctly.
    WorkloadsNotSorted,
    /// A pinned component of the run does not match its epoch's declaration.
    PinMismatch {
        /// Which pin disagrees, for example `toolchain_hash` or
        /// `workload_source_hashes/caldera`.
        field: String,
        /// What the epoch declares.
        expected: String,
        /// What the run reports.
        actual: String,
    },
    /// The run's environment does not satisfy the epoch's environment policy.
    ///
    /// Only the declared class and image are enforced. Drift in image version,
    /// CPU model, or core count is recorded and annotated, never rejected.
    EnvironmentPolicyViolated {
        /// The runner label and image the epoch requires.
        expected: (String, String),
        /// The runner label and image the run found.
        actual: (String, String),
    },
    /// A workload has more samples than its sampling policy allows.
    TooManySamples {
        /// The over-sampled workload.
        workload: String,
        /// How many the policy allows.
        allowed: u32,
        /// How many the run recorded.
        actual: u32,
    },
    /// A sample batches a different number of compilations than the policy
    /// pins, so it does not measure the unit the series is made of.
    BatchSizeMismatch {
        /// The workload whose sample is mis-batched.
        workload: String,
        /// Which sample.
        sample_index: u32,
        /// The batch size the policy pins.
        expected: u32,
        /// The batch size the sample reports.
        actual: u32,
    },
    /// A sample violates the phase-sum invariant with no failure record.
    ///
    /// The violation is still detected — validation recomputes the invariant
    /// rather than trusting the record — but a producer that hides its own
    /// instrumentation bug is itself the defect.
    UnrecordedInvariantFailure {
        /// The workload holding the sample.
        workload: String,
        /// Which sample.
        sample_index: u32,
    },
    /// A failure record claims an invariant violation that did not occur.
    SpuriousInvariantRecord {
        /// The workload named by the record.
        workload: String,
        /// The sample named by the record.
        sample_index: u32,
    },
    /// A failure record names a workload the suite does not declare.
    FailureForUndeclaredWorkload {
        /// The workload named by the record.
        workload: String,
    },
    /// A timestamp is not `YYYY-MM-DDTHH:MM:SSZ`.
    MalformedTimestamp {
        /// Which field, `started_at` or `finished_at`.
        field: String,
        /// The value found.
        value: String,
    },
    /// The measured compiler revision is not a 40-character hexadecimal hash.
    MalformedCommit {
        /// The value found.
        value: String,
    },
    /// Protocol-v2 process evidence is absent or disagrees with its epoch.
    BoundaryEvidenceMismatch {
        workload: String,
        sample_index: u32,
        detail: String,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::UnsupportedSchemaVersion { found, expected } => {
                write!(
                    f,
                    "run schema version {found} is unsupported; supported versions are {expected:?}"
                )
            }
            ValidationError::UnknownSuiteRevision { revision } => {
                write!(f, "suite revision {revision} is not declared")
            }
            ValidationError::UnknownEpoch { platform, epoch } => {
                write!(f, "epoch {epoch} is not declared for platform {platform}")
            }
            ValidationError::EpochSuiteMismatch {
                epoch_revision,
                run_revision,
            } => write!(
                f,
                "run claims suite revision {run_revision} but its epoch implements {epoch_revision}"
            ),
            ValidationError::UndeclaredWorkload { workload } => {
                write!(
                    f,
                    "workload {workload:?} is not declared by the suite revision"
                )
            }
            ValidationError::DuplicateWorkloadObservation { workload } => {
                write!(f, "workload {workload:?} is observed more than once")
            }
            ValidationError::WorkloadsNotSorted => {
                write!(f, "workload observations are not sorted by identifier")
            }
            ValidationError::PinMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "pin {field} is {actual:?} but the epoch declares {expected:?}"
            ),
            ValidationError::EnvironmentPolicyViolated { expected, actual } => write!(
                f,
                "environment {}/{} does not satisfy the epoch policy {}/{}",
                actual.0, actual.1, expected.0, expected.1
            ),
            ValidationError::TooManySamples {
                workload,
                allowed,
                actual,
            } => write!(
                f,
                "workload {workload:?} has {actual} samples but its policy allows {allowed}"
            ),
            ValidationError::BatchSizeMismatch {
                workload,
                sample_index,
                expected,
                actual,
            } => write!(
                f,
                "workload {workload:?} sample {sample_index} batches {actual} compilations but \
                 its policy pins {expected}"
            ),
            ValidationError::UnrecordedInvariantFailure {
                workload,
                sample_index,
            } => write!(
                f,
                "workload {workload:?} sample {sample_index} violates the phase-sum invariant \
                 with no failure record"
            ),
            ValidationError::SpuriousInvariantRecord {
                workload,
                sample_index,
            } => write!(
                f,
                "a failure record claims workload {workload:?} sample {sample_index} violates \
                 the phase-sum invariant, but it holds"
            ),
            ValidationError::FailureForUndeclaredWorkload { workload } => {
                write!(f, "a failure record names undeclared workload {workload:?}")
            }
            ValidationError::MalformedTimestamp { field, value } => {
                write!(f, "{field} {value:?} is not YYYY-MM-DDTHH:MM:SSZ")
            }
            ValidationError::MalformedCommit { value } => {
                write!(f, "commit {value:?} is not a 40-character hexadecimal hash")
            }
            ValidationError::BoundaryEvidenceMismatch {
                workload,
                sample_index,
                detail,
            } => write!(
                f,
                "workload {workload:?} sample {sample_index} has invalid build-boundary evidence: {detail}"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Why a stored sample may not contribute to a statistic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidSampleReason {
    /// The bands did not sum to compiler-root time.
    PhaseInvariantViolated {
        /// The total the bands should have summed to.
        compiler_root_ns: u64,
        /// What they actually summed to.
        attributed_ns: u64,
    },
    /// The accounting omitted published phases entirely.
    ///
    /// A phase that took no time records zero. An absent phase means the
    /// producer and this schema disagree about the taxonomy.
    MissingPhases {
        /// The absent phases, by wire name.
        phases: Vec<String>,
    },
    /// The process was measured as shorter than the compiler root inside it.
    ProcessShorterThanCompilerRoot {
        /// Externally measured process wall time.
        process_elapsed_ns: u64,
        /// Compiler-root elapsed time.
        compiler_root_ns: u64,
    },
    /// Compiler root measured zero nanoseconds.
    ZeroCompilerRoot,
}

impl std::fmt::Display for InvalidSampleReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidSampleReason::PhaseInvariantViolated {
                compiler_root_ns,
                attributed_ns,
            } => write!(
                f,
                "bands sum to {attributed_ns} ns but compiler root is {compiler_root_ns} ns"
            ),
            InvalidSampleReason::MissingPhases { phases } => {
                write!(f, "phase accounting omits {phases:?}")
            }
            InvalidSampleReason::ProcessShorterThanCompilerRoot {
                process_elapsed_ns,
                compiler_root_ns,
            } => write!(
                f,
                "process elapsed {process_elapsed_ns} ns is shorter than compiler root \
                 {compiler_root_ns} ns"
            ),
            InvalidSampleReason::ZeroCompilerRoot => write!(f, "compiler root measured zero ns"),
        }
    }
}

/// A stored sample excluded from every derived statistic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSample {
    /// The workload holding the sample.
    pub workload: String,
    /// Which sample, by position in the workload's sample list.
    pub sample_index: u32,
    /// Why it is excluded.
    pub reason: InvalidSampleReason,
}

/// Whether a run measured the whole suite validly.
///
/// Publication is tiered on this. A per-workload observation publishes for
/// every workload that completed validly; a headline point publishes only when
/// every suite workload did. A partial run therefore keeps its evidence and its
/// per-workload signal without letting the index's cohort change underneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    /// Every suite workload produced a full set of valid samples.
    Complete,
    /// Some suite workloads did not.
    Partial {
        /// The workloads that did not complete validly, sorted.
        missing: Vec<String>,
    },
}

impl Completeness {
    /// Whether a headline point may be published from this run.
    pub fn publishes_headline(&self) -> bool {
        matches!(self, Completeness::Complete)
    }
}

/// The full verdict on one run object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationOutcome {
    /// Appendability failures. A non-empty list means the run may not be
    /// stored in a series at all.
    pub errors: Vec<ValidationError>,
    /// Samples that are stored but excluded from statistics.
    pub invalid_samples: Vec<InvalidSample>,
    /// Whether the run covers the suite validly.
    pub completeness: Completeness,
}

impl ValidationOutcome {
    /// Whether this run may enter its series.
    pub fn is_appendable(&self) -> bool {
        self.errors.is_empty()
    }

    /// Whether a per-workload observation publishes for this workload.
    ///
    /// Requires the run to be appendable, the workload to have completed its
    /// full sample count, and every one of those samples to be valid.
    pub fn publishes_workload(&self, workload: &str) -> bool {
        if !self.is_appendable() {
            return false;
        }
        match &self.completeness {
            Completeness::Complete => true,
            Completeness::Partial { missing } => !missing.iter().any(|entry| entry == workload),
        }
    }

    /// Whether a headline point publishes from this run.
    pub fn publishes_headline(&self) -> bool {
        self.is_appendable() && self.completeness.publishes_headline()
    }
}

/// Check a run object against the manifest that governs it.
///
/// Every problem is reported rather than the first: a run with three mismatched
/// pins should say so once, not across three collection attempts.
pub fn validate_run(manifest: &Manifest, run: &RunObject) -> ValidationOutcome {
    let mut errors = Vec::new();

    if run.schema_version != LEGACY_FULL_EVIDENCE_SCHEMA_VERSION
        && run.schema_version != FULL_EVIDENCE_SCHEMA_VERSION
        && run.schema_version != LEGACY_RUN_SCHEMA_VERSION
        && run.schema_version != RUN_SCHEMA_VERSION
    {
        // Nothing below can be trusted to mean what it appears to mean, so this
        // is the one failure that stops validation short.
        return ValidationOutcome {
            errors: vec![ValidationError::UnsupportedSchemaVersion {
                found: run.schema_version,
                expected: SUPPORTED_SCHEMA_VERSIONS,
            }],
            invalid_samples: Vec::new(),
            completeness: Completeness::Partial {
                missing: Vec::new(),
            },
        };
    }

    check_identity_shape(run, &mut errors);

    let epoch = manifest.epoch(&run.identity.platform, run.identity.epoch);
    let Some(epoch) = epoch else {
        errors.push(ValidationError::UnknownEpoch {
            platform: run.identity.platform.clone(),
            epoch: run.identity.epoch,
        });
        return ValidationOutcome {
            errors,
            invalid_samples: Vec::new(),
            completeness: Completeness::Partial {
                missing: Vec::new(),
            },
        };
    };

    if epoch.suite_revision != run.identity.suite_revision {
        errors.push(ValidationError::EpochSuiteMismatch {
            epoch_revision: epoch.suite_revision,
            run_revision: run.identity.suite_revision,
        });
    }

    let Some(suite) = manifest.suite(epoch.suite_revision) else {
        // Unreachable through Manifest::parse, which rejects an epoch naming an
        // undeclared revision. Reported rather than asserted so a manifest
        // constructed some other way still fails loudly.
        errors.push(ValidationError::UnknownSuiteRevision {
            revision: epoch.suite_revision,
        });
        return ValidationOutcome {
            errors,
            invalid_samples: Vec::new(),
            completeness: Completeness::Partial {
                missing: Vec::new(),
            },
        };
    };

    check_pins(run, epoch, &mut errors);
    check_environment(run, epoch, &mut errors);
    check_membership(run, suite, &mut errors);
    check_boundary_evidence(run, suite, epoch, &mut errors);

    let invalid_samples = collect_invalid_samples(run, epoch, &mut errors);
    check_failure_records(run, suite, &invalid_samples, &mut errors);

    let completeness = assess_completeness(run, suite, epoch, &invalid_samples);

    ValidationOutcome {
        errors,
        invalid_samples,
        completeness,
    }
}

/// A comparable, publishable run exceeded a reviewed non-regression ratchet.
///
/// This is deliberately not a [`ValidationError`]: the raw run belongs in its
/// series and is the evidence of the regression. Collection publishes it, then
/// fails its gate so the regression is visible instead of freezing the chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessElapsedRegression {
    pub workload: String,
    pub current_median_ns: u64,
    pub limit_ns: u64,
}

/// Evaluate fixed fresh-process ratchets for an otherwise validated run.
pub fn process_elapsed_regressions(
    manifest: &Manifest,
    run: &RunObject,
    outcome: &ValidationOutcome,
) -> Vec<ProcessElapsedRegression> {
    if !outcome.is_appendable() {
        return Vec::new();
    }
    let Some(epoch) = manifest.epoch(&run.identity.platform, run.identity.epoch) else {
        return Vec::new();
    };
    let mut regressions = Vec::new();
    for (workload, ratchet) in &epoch.process_elapsed_ratchets {
        if !outcome.publishes_workload(workload) {
            continue;
        }
        let Some(observation) = run.observation(workload) else {
            continue;
        };
        let values: Vec<u64> = observation
            .samples
            .iter()
            .map(|sample| sample.process_elapsed_ns / u64::from(sample.batch_size).max(1))
            .collect();
        let Some(current_median_ns) = median(&values) else {
            continue;
        };
        if current_median_ns > ratchet.process_elapsed_limit_ns {
            regressions.push(ProcessElapsedRegression {
                workload: workload.clone(),
                current_median_ns,
                limit_ns: ratchet.process_elapsed_limit_ns,
            });
        }
    }
    regressions
}

fn check_boundary_evidence(
    run: &RunObject,
    suite: &crate::manifest::SuiteRevision,
    epoch: &crate::manifest::PlatformEpoch,
    errors: &mut Vec<ValidationError>,
) {
    // Encoding shape dispatches on the record's schema version; what must be
    // proven dispatches on the suite's protocol version. The two cross here
    // and nowhere else.
    if run.schema_version == RUN_SCHEMA_VERSION || run.schema_version == LEGACY_RUN_SCHEMA_VERSION {
        check_boundary_evidence_encoded(run, suite, epoch, errors);
        return;
    }
    // The full-evidence encoding must not smuggle in stored shapes: a full
    // carrying digests or blocks is malformed, not partially upgraded.
    if run.full_evidence.is_some() {
        if let Some(observation) = run.workloads.first() {
            errors.push(ValidationError::BoundaryEvidenceMismatch {
                workload: observation.workload.clone(),
                sample_index: 0,
                detail: "a full-evidence record must not name a full-evidence form".to_string(),
            });
        }
    }
    if run.boundary.is_some() {
        if let Some(observation) = run.workloads.first() {
            errors.push(ValidationError::BoundaryEvidenceMismatch {
                workload: observation.workload.clone(),
                sample_index: 0,
                detail: "a full-evidence record must not carry a run boundary block".to_string(),
            });
        }
    }
    for observation in &run.workloads {
        if observation.boundary.is_some() {
            errors.push(ValidationError::BoundaryEvidenceMismatch {
                workload: observation.workload.clone(),
                sample_index: 0,
                detail: "a full-evidence record must not carry a workload boundary block"
                    .to_string(),
            });
        }
        for (sample_index, sample) in observation.samples.iter().enumerate() {
            if !sample.boundary_processes.is_empty() || !sample.boundary_work_processes.is_empty() {
                errors.push(ValidationError::BoundaryEvidenceMismatch {
                    workload: observation.workload.clone(),
                    sample_index: sample_index as u32,
                    detail: "a full-evidence record must not carry per-process digests".to_string(),
                });
            }
        }
    }
    for observation in &run.workloads {
        let mut expected_output = None;
        let mut expected_work = None;
        for (sample_index, sample) in observation.samples.iter().enumerate() {
            let mut detail = match (suite.protocol_version, &epoch.boundary) {
                (1, None) if sample.boundary_evidence.is_empty() => None,
                (1, None) => {
                    Some("historical protocol v1 must not carry boundary-v2 evidence".to_string())
                }
                (2, Some(policy))
                    if sample.boundary_evidence.len() == sample.batch_size as usize =>
                {
                    sample
                        .boundary_evidence
                        .iter()
                        .enumerate()
                        .find_map(|(process, evidence)| {
                            if evidence.runner.output_size_bytes != sample.output_binary_bytes {
                                return Some(format!(
                                    "process {process}: proof output size {} disagrees with sample size {}",
                                    evidence.runner.output_size_bytes,
                                    sample.output_binary_bytes
                                ));
                            }
                            evidence
                                .validate_against(policy, &epoch.target)
                                .err()
                                .map(|detail| format!("process {process}: {detail}"))
                        })
                }
                (2, Some(_)) => Some(format!(
                    "expected {} process proofs, found {}",
                    sample.batch_size,
                    sample.boundary_evidence.len()
                )),
                (protocol, boundary) => Some(format!(
                    "unsupported protocol/boundary pairing: protocol {protocol}, policy {boundary:?}"
                )),
            };
            if detail.is_none() && suite.protocol_version == 2 {
                for evidence in &sample.boundary_evidence {
                    match &expected_output {
                        None => expected_output = Some(evidence.runner.output_sha256.clone()),
                        Some(expected) if expected == &evidence.runner.output_sha256 => {}
                        Some(_) => {
                            detail = Some(
                                "fresh processes produced nondeterministic native output"
                                    .to_string(),
                            );
                            break;
                        }
                    }
                    // At one worker the complete work record is deterministic
                    // and protects the architecture row from silent
                    // amplification. Parallel rows deliberately include
                    // schedule-dependent joins, reuses, and validation paths;
                    // those are distribution evidence, not output identity.
                    if epoch
                        .boundary
                        .as_ref()
                        .is_some_and(|policy| policy.worker_setting == crate::WorkerSetting::One)
                    {
                        match expected_work {
                            None => expected_work = Some(evidence.compiler_work),
                            Some(expected) if expected == evidence.compiler_work => {}
                            Some(_) => {
                                detail = Some(
                                    "one-worker fresh processes reported nondeterministic compiler work"
                                        .to_string(),
                                );
                                break;
                            }
                        }
                    }
                }
            }
            if let Some(detail) = detail {
                errors.push(ValidationError::BoundaryEvidenceMismatch {
                    workload: observation.workload.clone(),
                    sample_index: sample_index as u32,
                    detail,
                });
            }
            for (process_index, evidence) in sample.boundary_evidence.iter().enumerate() {
                let work_shape = if run.schema_version == crate::LEGACY_FULL_EVIDENCE_SCHEMA_VERSION
                {
                    validate_legacy_v2_work(&evidence.compiler_work, &evidence.critical_path)
                } else {
                    validate_v3_work(&evidence.compiler_work)
                };
                if let Err(detail) = work_shape {
                    errors.push(ValidationError::BoundaryEvidenceMismatch {
                        workload: observation.workload.clone(),
                        sample_index: sample_index as u32,
                        detail: format!("process {process_index}: {detail}"),
                    });
                }
            }
        }
    }
}

/// The stored (schema v3) half of the boundary check: witness plus digests.
///
/// Every guarantee the full-evidence path checks per process is re-derived
/// here from the retained witness and the per-process digests: semantic
/// validity of the witness against the epoch policy, output-size agreement
/// per sample, and cross-process identity — a process's digest equal to the
/// witness digest is the digest-level statement of the byte-equality the
/// full path asserted.
fn validate_legacy_v2_work(
    work: &crate::CompilerWork,
    critical: &crate::CompilerCriticalPathEvidence,
) -> Result<(), String> {
    let Some(structure) = work.legacy_v2_semantic_body_structure.as_ref() else {
        return Err(
            "schema v2 compiler_work must preserve semantic_body_structure for work.1 validation"
                .to_string(),
        );
    };
    let successful_lowerings = critical.semantic_body_input_attributed_total.count;
    if structure.body_lowerings != successful_lowerings
        || structure.index_builds != structure.body_lowerings
        || structure.rir_instructions != structure.index_rir_instructions_visited
        || structure.precompute_bodies != critical.semantic_inference_precompute.count
        || structure.precompute_alias_eval_attempts != structure.precompute_alias_filter_accepts
        || structure
            .precompute_alias_filter_accepts
            .saturating_add(structure.precompute_alias_filter_skips)
            > structure.precompute_alias_allocations_examined
        || structure.precompute_alias_type_successes > structure.precompute_alias_eval_attempts
        || structure.precompute_inline_scan_pops
            != structure
                .precompute_inline_scan_child_edges
                .saturating_add(structure.precompute_inline_scan_bodies)
        || structure.precompute_inline_scan_bodies > structure.precompute_bodies
        || structure.precompute_inline_final_candidates > structure.precompute_inline_raw_candidates
        || structure.precompute_inline_eval_attempts != structure.precompute_inline_final_candidates
        || structure.precompute_inline_type_successes > structure.precompute_inline_eval_attempts
        || structure.staged_canonical_evaluations > structure.staged_fact_nodes
    {
        return Err("schema v2 semantic_body_structure attribution is inconsistent".to_string());
    }
    Ok(())
}

fn validate_v3_work(work: &crate::CompilerWork) -> Result<(), String> {
    if work.legacy_v2_semantic_body_structure.is_some() {
        return Err(
            "schema v3 compiler_work must not carry retired semantic_body_structure".to_string(),
        );
    }
    let structure = work.semantic_analysis_structure;
    let precompute_subgroup = [
        structure.precompute_alias_nodes_visited,
        structure.precompute_alias_block_statements,
        structure.precompute_alias_allocations_examined,
        structure.precompute_alias_filter_accepts,
        structure.precompute_alias_filter_skips,
        structure.precompute_alias_eval_attempts,
        structure.precompute_alias_type_successes,
        structure.precompute_inline_scan_pops,
        structure.precompute_inline_scan_child_edges,
        structure.precompute_inline_scan_bodies,
        structure.precompute_inline_raw_candidates,
        structure.precompute_inline_final_candidates,
        structure.precompute_inline_eval_attempts,
        structure.precompute_inline_type_successes,
    ];
    if structure.precompute_bodies == 0 && precompute_subgroup.iter().any(|value| *value != 0)
        || (structure.precompute_bodies != 0
            && (structure.precompute_alias_eval_attempts
                != structure.precompute_alias_filter_accepts
                || structure
                    .precompute_alias_filter_accepts
                    .saturating_add(structure.precompute_alias_filter_skips)
                    > structure.precompute_alias_allocations_examined
                || structure.precompute_alias_type_successes
                    > structure.precompute_alias_eval_attempts
                || structure.precompute_inline_scan_pops
                    != structure
                        .precompute_inline_scan_child_edges
                        .saturating_add(structure.precompute_inline_scan_bodies)
                || structure.precompute_inline_scan_bodies > structure.precompute_bodies
                || structure.precompute_inline_final_candidates
                    > structure.precompute_inline_raw_candidates
                || structure.precompute_inline_eval_attempts
                    != structure.precompute_inline_final_candidates
                || structure.precompute_inline_type_successes
                    > structure.precompute_inline_eval_attempts))
        || structure.staged_canonical_evaluations > structure.staged_fact_nodes
    {
        return Err(
            "schema v3 semantic_analysis_structure attribution is inconsistent".to_string(),
        );
    }
    Ok(())
}

fn check_boundary_evidence_encoded(
    run: &RunObject,
    suite: &crate::manifest::SuiteRevision,
    epoch: &crate::manifest::PlatformEpoch,
    errors: &mut Vec<ValidationError>,
) {
    let mut push = |workload: &str, sample_index: u32, detail: String| {
        errors.push(ValidationError::BoundaryEvidenceMismatch {
            workload: workload.to_string(),
            sample_index,
            detail,
        });
    };
    // Every stored (v3) record commits to its full-evidence form by content
    // address, whatever its protocol: the name of the retained artifact for a
    // fresh collection, of the pre-compaction original for a re-encoded one.
    match &run.full_evidence {
        Some(address) if is_sha256_digest(address) => {}
        Some(address) => {
            if let Some(observation) = run.workloads.first() {
                push(
                    &observation.workload,
                    0,
                    format!("full_evidence {address:?} is not a content address"),
                );
            }
        }
        None => {
            if let Some(observation) = run.workloads.first() {
                push(
                    &observation.workload,
                    0,
                    "a stored record must name its full-evidence form".to_string(),
                );
            }
        }
    }
    match (suite.protocol_version, &epoch.boundary) {
        (1, None) => {
            // A protocol-1 suite carries no evidence under either encoding.
            if run.boundary.is_some() {
                if let Some(observation) = run.workloads.first() {
                    push(
                        &observation.workload,
                        0,
                        "historical protocol v1 must not carry boundary evidence".to_string(),
                    );
                }
            }
            for observation in &run.workloads {
                if observation.boundary.is_some() {
                    push(
                        &observation.workload,
                        0,
                        "historical protocol v1 must not carry boundary evidence".to_string(),
                    );
                }
                for (sample_index, sample) in observation.samples.iter().enumerate() {
                    if !sample.boundary_evidence.is_empty()
                        || !sample.boundary_processes.is_empty()
                        || !sample.boundary_work_processes.is_empty()
                    {
                        push(
                            &observation.workload,
                            sample_index as u32,
                            "historical protocol v1 must not carry boundary evidence".to_string(),
                        );
                    }
                }
            }
        }
        (2, Some(policy)) => {
            let Some(run_boundary) = run.boundary.as_ref() else {
                for observation in &run.workloads {
                    push(
                        &observation.workload,
                        0,
                        "schema v3 protocol-2 record carries no run boundary block".to_string(),
                    );
                }
                return;
            };
            let one_worker = policy.worker_setting == crate::WorkerSetting::One;
            for observation in &run.workloads {
                let Some(workload_boundary) = observation.boundary.as_ref() else {
                    // A workload with no samples carried no evidence under
                    // either encoding; the full path's per-sample loop was
                    // vacuous over it, and this path must not be stricter.
                    if !observation.samples.is_empty() {
                        push(
                            &observation.workload,
                            0,
                            "schema v3 protocol-2 record carries no workload boundary block"
                                .to_string(),
                        );
                    }
                    continue;
                };
                let (runner, compiler) = reassemble_witness(run_boundary, workload_boundary);
                let witness_sample = workload_boundary.critical_path_source.sample_index;
                let witness_evidence = crate::BuildBoundaryEvidence {
                    runner,
                    compiler,
                    critical_path: workload_boundary.critical_path.clone(),
                    compiler_work: workload_boundary.compiler_work,
                };
                if let Err(detail) = witness_evidence.validate_against(policy, &epoch.target) {
                    push(
                        &observation.workload,
                        witness_sample,
                        format!("witness: {detail}"),
                    );
                }
                let work_shape = if run.schema_version == LEGACY_RUN_SCHEMA_VERSION {
                    validate_legacy_v2_work(
                        &workload_boundary.compiler_work,
                        &workload_boundary.critical_path,
                    )
                } else {
                    validate_v3_work(&workload_boundary.compiler_work)
                };
                if let Err(detail) = work_shape {
                    push(&observation.workload, witness_sample, detail);
                }
                let witness_digest =
                    match identity_digest(&witness_evidence.runner, &witness_evidence.compiler) {
                        Ok(digest) => digest,
                        Err(error) => {
                            push(
                                &observation.workload,
                                witness_sample,
                                format!("witness cannot be digested: {error}"),
                            );
                            continue;
                        }
                    };
                let witness_work_digest = if one_worker {
                    let digest = if run.schema_version == LEGACY_RUN_SCHEMA_VERSION {
                        work_digest_v2(&workload_boundary.compiler_work)
                    } else {
                        work_digest(&workload_boundary.compiler_work)
                    };
                    match digest {
                        Ok(digest) => Some(digest),
                        Err(error) => {
                            push(
                                &observation.workload,
                                witness_sample,
                                format!("witness work cannot be digested: {error}"),
                            );
                            continue;
                        }
                    }
                } else {
                    None
                };
                let sample_count = observation.samples.len() as u32;
                for (member, source) in [
                    (
                        "critical_path_source",
                        workload_boundary.critical_path_source,
                    ),
                    (
                        "compiler_work_source",
                        workload_boundary.compiler_work_source,
                    ),
                ] {
                    if source.sample_index >= sample_count {
                        push(
                            &observation.workload,
                            0,
                            format!(
                                "{member} names sample {} of {sample_count}",
                                source.sample_index
                            ),
                        );
                        continue;
                    }
                    let batch = observation.samples[source.sample_index as usize].batch_size;
                    if source.process_index >= batch {
                        push(
                            &observation.workload,
                            source.sample_index,
                            format!(
                                "{member} names process {} of a batch of {batch}",
                                source.process_index
                            ),
                        );
                    }
                }
                for (sample_index, sample) in observation.samples.iter().enumerate() {
                    let sample_index = sample_index as u32;
                    if !sample.boundary_evidence.is_empty() {
                        push(
                            &observation.workload,
                            sample_index,
                            "schema v3 record must not carry inline boundary evidence".to_string(),
                        );
                        continue;
                    }
                    if sample.boundary_processes.len() != sample.batch_size as usize {
                        push(
                            &observation.workload,
                            sample_index,
                            format!(
                                "expected {} process proofs, found {}",
                                sample.batch_size,
                                sample.boundary_processes.len()
                            ),
                        );
                        continue;
                    }
                    if workload_boundary.runner.output_size_bytes != sample.output_binary_bytes {
                        push(
                            &observation.workload,
                            sample_index,
                            format!(
                                "proof output size {} disagrees with sample size {}",
                                workload_boundary.runner.output_size_bytes,
                                sample.output_binary_bytes
                            ),
                        );
                    }
                    if let Some(process) = sample
                        .boundary_processes
                        .iter()
                        .position(|digest| digest != &witness_digest)
                    {
                        push(
                            &observation.workload,
                            sample_index,
                            format!(
                                "process {process}: identity digest disagrees with the workload witness"
                            ),
                        );
                    }
                    match &witness_work_digest {
                        Some(expected) => {
                            if sample.boundary_work_processes.len() != sample.batch_size as usize {
                                push(
                                    &observation.workload,
                                    sample_index,
                                    format!(
                                        "expected {} work digests, found {}",
                                        sample.batch_size,
                                        sample.boundary_work_processes.len()
                                    ),
                                );
                            } else if sample
                                .boundary_work_processes
                                .iter()
                                .any(|digest| digest != expected)
                            {
                                push(
                                    &observation.workload,
                                    sample_index,
                                    "one-worker fresh processes reported nondeterministic compiler work"
                                        .to_string(),
                                );
                            }
                        }
                        None => {
                            if !sample.boundary_work_processes.is_empty() {
                                push(
                                    &observation.workload,
                                    sample_index,
                                    "a parallel-epoch record must not carry work digests"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
            }
        }
        (protocol, boundary) => {
            for observation in &run.workloads {
                push(
                    &observation.workload,
                    0,
                    format!(
                        "unsupported protocol/boundary pairing: protocol {protocol},                          policy {boundary:?}"
                    ),
                );
            }
        }
    }
}

fn check_identity_shape(run: &RunObject, errors: &mut Vec<ValidationError>) {
    let commit = &run.identity.commit;
    if !is_commit(commit) {
        errors.push(ValidationError::MalformedCommit {
            value: commit.clone(),
        });
    }
    for (field, value) in [
        ("started_at", &run.identity.started_at),
        ("finished_at", &run.identity.finished_at),
    ] {
        if !is_utc_timestamp(value) {
            errors.push(ValidationError::MalformedTimestamp {
                field: field.to_string(),
                value: value.clone(),
            });
        }
    }
}

fn check_pins(
    run: &RunObject,
    epoch: &crate::manifest::PlatformEpoch,
    errors: &mut Vec<ValidationError>,
) {
    let pins = &run.identity.pins;
    let mut compare = |field: &str, expected: &str, actual: &str| {
        if expected != actual {
            errors.push(ValidationError::PinMismatch {
                field: field.to_string(),
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
    };
    compare(
        "toolchain_hash",
        &epoch.toolchain_hash,
        &pins.toolchain_hash,
    );
    // `stdlib_hash` is deliberately not compared. `std` is part of the product
    // under measurement, not an input to it, so a std change moves the series
    // exactly as a compiler change does. The run still records the hash it
    // resolved, and the dashboard annotates the point where it changed.
    compare("invocation/target", &epoch.target, &pins.invocation.target);

    if epoch.args != pins.invocation.args {
        errors.push(ValidationError::PinMismatch {
            field: "invocation/args".to_string(),
            expected: epoch.args.join(" "),
            actual: pins.invocation.args.join(" "),
        });
    }

    // Source hashes describe the tree that was measured, so they must match
    // exactly even for workloads this run failed to complete.
    for (workload, expected) in &epoch.workload_source_hashes {
        let actual = pins
            .workload_source_hashes
            .get(workload)
            .map(String::as_str)
            .unwrap_or("");
        if actual != expected {
            errors.push(ValidationError::PinMismatch {
                field: format!("workload_source_hashes/{workload}"),
                expected: expected.clone(),
                actual: actual.to_string(),
            });
        }
    }
    for workload in pins.workload_source_hashes.keys() {
        if !epoch.workload_source_hashes.contains_key(workload) {
            errors.push(ValidationError::PinMismatch {
                field: format!("workload_source_hashes/{workload}"),
                expected: String::new(),
                actual: pins.workload_source_hashes[workload].clone(),
            });
        }
    }
}

fn check_environment(
    run: &RunObject,
    epoch: &crate::manifest::PlatformEpoch,
    errors: &mut Vec<ValidationError>,
) {
    if !epoch.environment.admits(&run.identity.environment) {
        errors.push(ValidationError::EnvironmentPolicyViolated {
            expected: (
                epoch.environment.runner_label.clone(),
                epoch.environment.runner_image.clone(),
            ),
            actual: (
                run.identity.environment.runner_label.clone(),
                run.identity.environment.runner_image.clone(),
            ),
        });
    }
}

fn check_membership(
    run: &RunObject,
    suite: &crate::manifest::SuiteRevision,
    errors: &mut Vec<ValidationError>,
) {
    let mut seen: Vec<&str> = Vec::new();
    for observation in &run.workloads {
        if !suite.declares(&observation.workload) {
            errors.push(ValidationError::UndeclaredWorkload {
                workload: observation.workload.clone(),
            });
        }
        if seen.contains(&observation.workload.as_str()) {
            errors.push(ValidationError::DuplicateWorkloadObservation {
                workload: observation.workload.clone(),
            });
        }
        seen.push(&observation.workload);
    }
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    if seen != sorted {
        errors.push(ValidationError::WorkloadsNotSorted);
    }
}

fn collect_invalid_samples(
    run: &RunObject,
    epoch: &crate::manifest::PlatformEpoch,
    errors: &mut Vec<ValidationError>,
) -> Vec<InvalidSample> {
    let mut invalid = Vec::new();
    for observation in &run.workloads {
        let policy = epoch.sampling.get(&observation.workload);
        if let Some(policy) = policy
            && let Some(actual) = samples_beyond_policy(observation.samples.len(), policy.samples)
        {
            errors.push(ValidationError::TooManySamples {
                workload: observation.workload.clone(),
                allowed: policy.samples,
                actual,
            });
        }

        for (index, sample) in observation.samples.iter().enumerate() {
            let index = index as u32;
            if let Some(policy) = policy
                && sample.batch_size != policy.batch_size
            {
                errors.push(ValidationError::BatchSizeMismatch {
                    workload: observation.workload.clone(),
                    sample_index: index,
                    expected: policy.batch_size,
                    actual: sample.batch_size,
                });
            }
            if let Some(reason) = sample_invalidity(sample) {
                invalid.push(InvalidSample {
                    workload: observation.workload.clone(),
                    sample_index: index,
                    reason,
                });
            }
        }
    }
    invalid
}

fn sample_invalidity(sample: &Sample) -> Option<InvalidSampleReason> {
    let missing = sample.phases.missing_phases();
    if !missing.is_empty() {
        return Some(InvalidSampleReason::MissingPhases {
            phases: missing
                .into_iter()
                .map(Phase::wire_name)
                .map(String::from)
                .collect(),
        });
    }
    if !sample.phases.holds() {
        return Some(InvalidSampleReason::PhaseInvariantViolated {
            compiler_root_ns: sample.phases.compiler_root_ns,
            attributed_ns: sample.phases.attributed_ns(),
        });
    }
    if sample.phases.compiler_root_ns == 0 {
        return Some(InvalidSampleReason::ZeroCompilerRoot);
    }
    if sample.driver_overhead_ns().is_none() {
        return Some(InvalidSampleReason::ProcessShorterThanCompilerRoot {
            process_elapsed_ns: sample.process_elapsed_ns,
            compiler_root_ns: sample.phases.compiler_root_ns,
        });
    }
    None
}

fn check_failure_records(
    run: &RunObject,
    suite: &crate::manifest::SuiteRevision,
    invalid_samples: &[InvalidSample],
    errors: &mut Vec<ValidationError>,
) {
    let violated: Vec<(&str, u32)> = invalid_samples
        .iter()
        .filter(|sample| {
            matches!(
                sample.reason,
                InvalidSampleReason::PhaseInvariantViolated { .. }
            )
        })
        .map(|sample| (sample.workload.as_str(), sample.sample_index))
        .collect();

    let mut recorded: Vec<(&str, u32)> = Vec::new();
    for failure in &run.failures {
        if let Some(workload) = failure.workload()
            && !suite.declares(workload)
        {
            errors.push(ValidationError::FailureForUndeclaredWorkload {
                workload: workload.to_string(),
            });
        }
        if let FailureRecord::PhaseInvariant {
            workload,
            sample_index,
            ..
        } = failure
        {
            recorded.push((workload.as_str(), *sample_index));
            if !violated.contains(&(workload.as_str(), *sample_index)) {
                errors.push(ValidationError::SpuriousInvariantRecord {
                    workload: workload.clone(),
                    sample_index: *sample_index,
                });
            }
        }
    }

    for (workload, sample_index) in violated {
        if !recorded.contains(&(workload, sample_index)) {
            errors.push(ValidationError::UnrecordedInvariantFailure {
                workload: workload.to_string(),
                sample_index,
            });
        }
    }
}

fn assess_completeness(
    run: &RunObject,
    suite: &crate::manifest::SuiteRevision,
    epoch: &crate::manifest::PlatformEpoch,
    invalid_samples: &[InvalidSample],
) -> Completeness {
    let mut invalid_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for sample in invalid_samples {
        *invalid_counts.entry(sample.workload.as_str()).or_default() += 1;
    }

    let mut missing = Vec::new();
    for workload in suite.workload_ids() {
        let required = epoch.sampling.get(workload).map(|policy| policy.samples);
        let observed = run.observation(workload).map(|entry| entry.samples.len());
        let complete = match (required, observed) {
            (Some(required), Some(observed)) => {
                observed as u64 == u64::from(required)
                    && invalid_counts.get(workload).copied().unwrap_or(0) == 0
            }
            _ => false,
        };
        if !complete {
            missing.push(workload.to_string());
        }
    }

    if missing.is_empty() {
        Completeness::Complete
    } else {
        Completeness::Partial { missing }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::run::{
        EnvironmentFingerprint, Invocation, PhaseAccounting, ResolvedPins, RunIdentity,
        WorkloadObservation,
    };

    pub(crate) const MANIFEST: &str = r#"
[[suite]]
revision = 1
timing_schema_version = 1
protocol_version = 1

[[suite.workloads]]
id = "caldera"
source = "examples/caldera/main.rue"
question = "How does a large single-program compilation behave?"

[[suite.workloads]]
id = "startup"
source = "performance/workloads/startup/main.rue"
question = "What does a minimal compilation cost end to end?"

[[epoch]]
id = 1
platform = "x86_64-linux"
suite_revision = 1
target = "x86_64-unknown-linux-gnu"
args = ["-O2"]
toolchain_hash = "toolchain-aaa"

[epoch.workload_source_hashes]
caldera = "caldera-ccc"
startup = "startup-ddd"

[epoch.environment]
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"

[epoch.sampling.caldera]
samples = 2
batch_size = 1

[epoch.sampling.startup]
samples = 2
batch_size = 40

[epoch.flagging]
k = 3.0
window = 10
"#;

    pub(crate) fn manifest() -> Manifest {
        Manifest::parse(MANIFEST).expect("fixture manifest is valid")
    }

    pub(crate) fn accounting(root_ns: u64) -> PhaseAccounting {
        let mut phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        phase_ns.insert(Phase::SemanticAnalysis, root_ns);
        PhaseAccounting {
            phase_ns,
            mixed_parallel_ns: 0,
            unattributed_ns: 0,
            compiler_root_ns: root_ns,
        }
    }

    pub(crate) fn sample(batch_size: u32, root_ns: u64) -> Sample {
        Sample {
            batch_size,
            process_elapsed_ns: root_ns + 1_000,
            peak_memory_bytes: 64 * 1024 * 1024,
            output_binary_bytes: 131_072,
            phases: accounting(root_ns),
            boundary_evidence: Vec::new(),
            boundary_processes: Vec::new(),
            boundary_work_processes: Vec::new(),
        }
    }

    pub(crate) fn sample_run() -> RunObject {
        RunObject {
            // The deep per-process checks below exercise the full-evidence
            // encoding; the stored encoding's checks live in the encoded
            // tests and in crate::encoding.
            schema_version: FULL_EVIDENCE_SCHEMA_VERSION,
            identity: RunIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "x86_64-linux".to_string(),
                commit: "a".repeat(40),
                started_at: "2026-07-28T04:00:00Z".to_string(),
                finished_at: "2026-07-28T04:12:00Z".to_string(),
                pins: ResolvedPins {
                    toolchain_hash: "toolchain-aaa".to_string(),
                    stdlib_hash: "stdlib-bbb".to_string(),
                    workload_source_hashes: BTreeMap::from([
                        ("caldera".to_string(), "caldera-ccc".to_string()),
                        ("startup".to_string(), "startup-ddd".to_string()),
                    ]),
                    invocation: Invocation {
                        target: "x86_64-unknown-linux-gnu".to_string(),
                        args: vec!["-O2".to_string()],
                    },
                },
                environment: EnvironmentFingerprint {
                    runner_label: "github-hosted".to_string(),
                    runner_image: "ubuntu-24.04".to_string(),
                    runner_image_version: "ubuntu24/20250720.1.0".to_string(),
                    cpu_model: "AMD EPYC 7763".to_string(),
                    core_count: 4,
                    memory_bytes: 16 * 1024 * 1024 * 1024,
                    kernel_version: "6.8.0-1014-azure".to_string(),
                    os_version: "Ubuntu 24.04.2 LTS".to_string(),
                    architecture: "x86_64".to_string(),
                },
            },
            boundary: None,
            full_evidence: None,
            workloads: vec![
                WorkloadObservation {
                    workload: "caldera".to_string(),
                    boundary: None,
                    samples: vec![sample(1, 900_000_000), sample(1, 910_000_000)],
                },
                WorkloadObservation {
                    workload: "startup".to_string(),
                    boundary: None,
                    samples: vec![sample(40, 400_000_000), sample(40, 404_000_000)],
                },
            ],
            failures: Vec::new(),
        }
    }

    #[test]
    fn a_complete_well_formed_run_is_appendable_and_publishes_a_headline() {
        let outcome = validate_run(&manifest(), &sample_run());
        assert_eq!(outcome.errors, Vec::new());
        assert_eq!(outcome.invalid_samples, Vec::new());
        assert_eq!(outcome.completeness, Completeness::Complete);
        assert!(outcome.is_appendable());
        assert!(outcome.publishes_headline());
        assert!(outcome.publishes_workload("caldera"));
    }

    fn manifest_with_caldera_ratchet(center: u64, mad: u64) -> Manifest {
        Manifest::parse(&format!(
            "{MANIFEST}\n\
             [epoch.baseline]\n\
             commit = \"{}\"\n\
             run = \"baseline-run\"\n\n\
             [epoch.process_elapsed_ratchets.caldera]\n\
             baseline_process_elapsed_ns = {center}\n\
             baseline_mad_ns = {mad}\n\
             process_elapsed_limit_ns = {}\n\
             reference_run = \"baseline-run\"\n",
            "a".repeat(40),
            center.saturating_add(mad.saturating_mul(6))
        ))
        .expect("ratcheted manifest")
    }

    #[test]
    fn a_material_fresh_process_regression_is_publishable_but_fails_the_ratchet() {
        let manifest = manifest_with_caldera_ratchet(800_000_000, 1_000_000);
        let run = sample_run();
        let outcome = validate_run(&manifest, &run);
        assert!(outcome.is_appendable());
        assert!(matches!(
            process_elapsed_regressions(&manifest, &run, &outcome).as_slice(),
            [ProcessElapsedRegression {
                workload,
                current_median_ns: 905_001_000,
                limit_ns: 806_000_000,
            }] if workload == "caldera"
        ));
    }

    #[test]
    fn observations_below_the_fixed_limit_pass_the_ratchet() {
        for manifest in [
            manifest_with_caldera_ratchet(1_000_000_000, 1_000_000),
            manifest_with_caldera_ratchet(900_000_000, 100_000_000),
        ] {
            let run = sample_run();
            let outcome = validate_run(&manifest, &run);
            assert!(outcome.is_appendable());
            assert!(process_elapsed_regressions(&manifest, &run, &outcome).is_empty());
        }
    }

    #[test]
    fn an_unsupported_schema_version_stops_validation_immediately() {
        let mut run = sample_run();
        run.schema_version = FULL_EVIDENCE_SCHEMA_VERSION + 1;
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(
            outcome.errors,
            vec![ValidationError::UnsupportedSchemaVersion {
                found: FULL_EVIDENCE_SCHEMA_VERSION + 1,
                expected: SUPPORTED_SCHEMA_VERSIONS,
            }]
        );
        assert!(!outcome.is_appendable());
    }

    #[test]
    fn a_run_naming_an_undeclared_epoch_cannot_be_appended() {
        let mut run = sample_run();
        run.identity.epoch = 9;
        let outcome = validate_run(&manifest(), &run);
        assert!(outcome.errors.contains(&ValidationError::UnknownEpoch {
            platform: "x86_64-linux".to_string(),
            epoch: 9,
        }));
    }

    #[test]
    fn a_run_naming_another_platform_cannot_borrow_this_epoch() {
        let mut run = sample_run();
        run.identity.platform = "aarch64-macos".to_string();
        let outcome = validate_run(&manifest(), &run);
        assert!(!outcome.is_appendable());
    }

    #[test]
    fn a_run_claiming_the_wrong_suite_revision_is_rejected() {
        let mut run = sample_run();
        run.identity.suite_revision = 2;
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::EpochSuiteMismatch {
                    epoch_revision: 1,
                    run_revision: 2,
                })
        );
    }

    #[test]
    fn an_edited_workload_cannot_silently_reset_its_own_baseline() {
        // This is the property the whole pin mechanism exists for: changing what
        // a workload *is* must fail validation rather than continue the series.
        let mut run = sample_run();
        run.identity
            .pins
            .workload_source_hashes
            .insert("caldera".to_string(), "caldera-edited".to_string());
        let outcome = validate_run(&manifest(), &run);
        assert!(outcome.errors.contains(&ValidationError::PinMismatch {
            field: "workload_source_hashes/caldera".to_string(),
            expected: "caldera-ccc".to_string(),
            actual: "caldera-edited".to_string(),
        }));
        assert!(!outcome.is_appendable());
    }

    #[test]
    fn each_pinned_component_is_checked() {
        let cases: Vec<(&str, Box<dyn Fn(&mut RunObject)>)> = vec![
            (
                "toolchain_hash",
                Box::new(|run: &mut RunObject| {
                    run.identity.pins.toolchain_hash = "other".to_string()
                }),
            ),
            (
                "invocation/target",
                Box::new(|run: &mut RunObject| {
                    run.identity.pins.invocation.target = "other".to_string()
                }),
            ),
            (
                "invocation/args",
                Box::new(|run: &mut RunObject| {
                    run.identity.pins.invocation.args = vec!["-O0".to_string()]
                }),
            ),
        ];
        for (field, mutate) in cases {
            let mut run = sample_run();
            mutate(&mut run);
            let outcome = validate_run(&manifest(), &run);
            assert!(
                outcome
                    .errors
                    .iter()
                    .any(|error| matches!(error, ValidationError::PinMismatch { field: f, .. } if f == field)),
                "{field} was not checked"
            );
        }
    }

    #[test]
    fn a_pin_for_a_workload_the_epoch_does_not_declare_is_rejected() {
        let mut run = sample_run();
        run.identity
            .pins
            .workload_source_hashes
            .insert("meridian".to_string(), "meridian-eee".to_string());
        let outcome = validate_run(&manifest(), &run);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            ValidationError::PinMismatch { field, .. } if field == "workload_source_hashes/meridian"
        )));
    }

    #[test]
    fn an_environment_outside_the_policy_is_rejected() {
        let mut run = sample_run();
        run.identity.environment.runner_image = "ubuntu-22.04".to_string();
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::EnvironmentPolicyViolated {
                    expected: ("github-hosted".to_string(), "ubuntu-24.04".to_string()),
                    actual: ("github-hosted".to_string(), "ubuntu-22.04".to_string()),
                })
        );
    }

    #[test]
    fn environment_drift_within_the_policy_stays_appendable() {
        // Hosted hardware changes underneath an epoch. The system records that
        // rather than refusing the measurement; the dashboard renders crossings
        // as advisory.
        let mut run = sample_run();
        run.identity.environment.runner_image_version = "ubuntu24/20990101.9.0".to_string();
        run.identity.environment.cpu_model = "Intel Xeon Platinum 8370C".to_string();
        run.identity.environment.core_count = 8;
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(outcome.errors, Vec::new());
        assert!(outcome.publishes_headline());
    }

    #[test]
    fn measuring_a_workload_the_suite_does_not_declare_is_rejected() {
        let mut run = sample_run();
        run.workloads.push(WorkloadObservation {
            workload: "surprise".to_string(),
            boundary: None,
            samples: vec![sample(1, 5)],
        });
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::UndeclaredWorkload {
                    workload: "surprise".to_string(),
                })
        );
    }

    #[test]
    fn duplicate_and_unsorted_observations_are_rejected() {
        let mut run = sample_run();
        run.workloads.swap(0, 1);
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::WorkloadsNotSorted)
        );

        let mut run = sample_run();
        let repeated = run.workloads[0].clone();
        run.workloads.insert(1, repeated);
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::DuplicateWorkloadObservation {
                    workload: "caldera".to_string(),
                })
        );
    }

    #[test]
    fn more_samples_than_the_policy_allows_is_a_protocol_violation() {
        let mut run = sample_run();
        run.workloads[0].samples.push(sample(1, 905_000_000));
        let outcome = validate_run(&manifest(), &run);
        assert!(outcome.errors.contains(&ValidationError::TooManySamples {
            workload: "caldera".to_string(),
            allowed: 2,
            actual: 3,
        }));
    }

    #[test]
    fn a_mis_batched_sample_is_a_protocol_violation() {
        let mut run = sample_run();
        run.workloads[1].samples[0].batch_size = 1;
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::BatchSizeMismatch {
                    workload: "startup".to_string(),
                    sample_index: 0,
                    expected: 40,
                    actual: 1,
                })
        );
    }

    #[test]
    fn an_invariant_violation_invalidates_its_sample_without_rejecting_the_run() {
        let mut run = sample_run();
        run.workloads[0].samples[0].phases.unattributed_ns = 5;
        run.failures.push(FailureRecord::PhaseInvariant {
            workload: "caldera".to_string(),
            sample_index: 0,
            compiler_root_ns: 900_000_000,
            attributed_ns: 900_000_005,
        });
        let outcome = validate_run(&manifest(), &run);

        assert_eq!(outcome.errors, Vec::new(), "the run itself is well formed");
        assert_eq!(
            outcome.invalid_samples,
            vec![InvalidSample {
                workload: "caldera".to_string(),
                sample_index: 0,
                reason: InvalidSampleReason::PhaseInvariantViolated {
                    compiler_root_ns: 900_000_000,
                    attributed_ns: 900_000_005,
                },
            }]
        );
        // The sample is still on disk; only its contribution is withdrawn.
        assert_eq!(run.workloads[0].samples.len(), 2);
        assert!(outcome.is_appendable());
        assert!(!outcome.publishes_headline());
        assert!(!outcome.publishes_workload("caldera"));
        assert!(outcome.publishes_workload("startup"));
    }

    #[test]
    fn a_producer_cannot_hide_its_own_invariant_failure() {
        let mut run = sample_run();
        run.workloads[0].samples[0].phases.unattributed_ns = 5;
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::UnrecordedInvariantFailure {
                    workload: "caldera".to_string(),
                    sample_index: 0,
                })
        );
    }

    #[test]
    fn a_failure_record_cannot_invent_a_violation_that_did_not_happen() {
        let mut run = sample_run();
        run.failures.push(FailureRecord::PhaseInvariant {
            workload: "caldera".to_string(),
            sample_index: 1,
            compiler_root_ns: 1,
            attributed_ns: 2,
        });
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::SpuriousInvariantRecord {
                    workload: "caldera".to_string(),
                    sample_index: 1,
                })
        );
    }

    #[test]
    fn a_failure_record_for_an_undeclared_workload_is_rejected() {
        let mut run = sample_run();
        run.failures.push(FailureRecord::Timeout {
            workload: "phantom".to_string(),
            sample_index: 0,
            limit_ns: 1,
        });
        let outcome = validate_run(&manifest(), &run);
        assert!(
            outcome
                .errors
                .contains(&ValidationError::FailureForUndeclaredWorkload {
                    workload: "phantom".to_string(),
                })
        );
    }

    #[test]
    fn a_run_level_build_failure_names_no_workload_and_is_accepted() {
        let mut run = sample_run();
        run.failures.push(FailureRecord::Build {
            detail: "linker unavailable".to_string(),
        });
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(outcome.errors, Vec::new());
    }

    #[test]
    fn missing_phases_invalidate_a_sample_even_when_the_total_happens_to_match() {
        let mut run = sample_run();
        run.workloads[0].samples[0]
            .phases
            .phase_ns
            .remove(&Phase::Linking);
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(
            outcome.invalid_samples,
            vec![InvalidSample {
                workload: "caldera".to_string(),
                sample_index: 0,
                reason: InvalidSampleReason::MissingPhases {
                    phases: vec!["linking".to_string()],
                },
            }]
        );
    }

    #[test]
    fn a_process_shorter_than_its_compiler_root_invalidates_the_sample() {
        let mut run = sample_run();
        run.workloads[1].samples[1].process_elapsed_ns = 1;
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(
            outcome.invalid_samples,
            vec![InvalidSample {
                workload: "startup".to_string(),
                sample_index: 1,
                reason: InvalidSampleReason::ProcessShorterThanCompilerRoot {
                    process_elapsed_ns: 1,
                    compiler_root_ns: 404_000_000,
                },
            }]
        );
    }

    #[test]
    fn a_zero_length_compilation_invalidates_the_sample() {
        let mut run = sample_run();
        run.workloads[0].samples[0].phases = accounting(0);
        run.workloads[0].samples[0].phases.compiler_root_ns = 0;
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(
            outcome.invalid_samples,
            vec![InvalidSample {
                workload: "caldera".to_string(),
                sample_index: 0,
                reason: InvalidSampleReason::ZeroCompilerRoot,
            }]
        );
    }

    #[test]
    fn a_crashed_workload_makes_the_run_partial_but_keeps_the_others_publishing() {
        let mut run = sample_run();
        run.workloads[0].samples.truncate(1);
        run.failures.push(FailureRecord::WorkloadCrashed {
            workload: "caldera".to_string(),
            sample_index: 1,
            detail: "signal 11".to_string(),
        });
        let outcome = validate_run(&manifest(), &run);

        assert_eq!(outcome.errors, Vec::new(), "a partial run is still stored");
        assert_eq!(
            outcome.completeness,
            Completeness::Partial {
                missing: vec!["caldera".to_string()],
            }
        );
        assert!(outcome.is_appendable());
        assert!(!outcome.publishes_headline());
        assert!(!outcome.publishes_workload("caldera"));
        assert!(outcome.publishes_workload("startup"));
    }

    #[test]
    fn a_workload_absent_entirely_is_reported_as_missing() {
        let mut run = sample_run();
        run.workloads.remove(0);
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(
            outcome.completeness,
            Completeness::Partial {
                missing: vec!["caldera".to_string()],
            }
        );
        assert_eq!(outcome.errors, Vec::new());
    }

    #[test]
    fn nothing_publishes_from_a_run_that_cannot_be_appended() {
        let mut run = sample_run();
        run.identity.pins.toolchain_hash = "other".to_string();
        let outcome = validate_run(&manifest(), &run);
        assert!(!outcome.is_appendable());
        assert!(!outcome.publishes_headline());
        assert!(!outcome.publishes_workload("startup"));
    }

    #[test]
    fn a_malformed_commit_or_timestamp_is_rejected() {
        let mut run = sample_run();
        run.identity.commit = "abc".to_string();
        run.identity.started_at = "2026-07-28 04:00:00".to_string();
        run.identity.finished_at = "2026-07-28T04:12:00.500Z".to_string();
        let outcome = validate_run(&manifest(), &run);
        assert!(outcome.errors.contains(&ValidationError::MalformedCommit {
            value: "abc".to_string(),
        }));
        assert_eq!(
            outcome
                .errors
                .iter()
                .filter(|error| matches!(error, ValidationError::MalformedTimestamp { .. }))
                .count(),
            2,
            "one spelling of an instant keeps content addresses stable"
        );
    }

    #[test]
    fn every_problem_is_reported_rather_than_the_first() {
        let mut run = sample_run();
        run.identity.pins.toolchain_hash = "other".to_string();
        run.identity.pins.invocation.target = "other".to_string();
        run.identity.environment.runner_image = "ubuntu-22.04".to_string();
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(outcome.errors.len(), 3, "{:?}", outcome.errors);
    }

    #[test]
    fn a_std_change_does_not_prevent_a_run_from_entering_its_series() {
        // `std` is part of the product being measured, not an input pinned
        // against it: a std edit moves the series exactly as a compiler change
        // does. Rejecting the run instead would stop the series entirely.
        let mut run = sample_run();
        run.identity.pins.stdlib_hash = "a-completely-different-standard-library".to_string();
        let outcome = validate_run(&manifest(), &run);

        assert!(
            outcome.is_appendable(),
            "a std change must not reject a run: {:?}",
            outcome.errors
        );
        assert!(
            !outcome.errors.iter().any(
                |error| matches!(error, ValidationError::PinMismatch { field, .. }
                    if field == "stdlib_hash")
            ),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_workloads_own_source_change_still_rejects_the_run() {
        // The counterpart to the test above. Relaxing the std pin must not
        // relax the pin that gives a series its meaning: changing what a
        // workload *is* still requires declaring the next revision.
        let mut run = sample_run();
        run.identity
            .pins
            .workload_source_hashes
            .insert("startup".to_string(), "edited".to_string());
        let outcome = validate_run(&manifest(), &run);

        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(
                |error| matches!(error, ValidationError::PinMismatch { field, .. }
                    if field == "workload_source_hashes/startup")
            ),
            "{:?}",
            outcome.errors
        );
    }

    // ---- The stored (schema v3) encoding, and the dual-version reader ----

    const BOUNDARY_MANIFEST: &str = r#"
[[suite]]
revision = 2
timing_schema_version = 1
protocol_version = 2
boundary = "fresh_source_to_native_v1"

[[suite.workloads]]
id = "startup"
source = "performance/workloads/startup/main.rue"
question = "What does a minimal fresh compilation cost end to end?"

[[epoch]]
id = 3
collection = true
platform = "x86_64-linux"
suite_revision = 2
target = "x86-64-linux"
args = ["-O3", "-j1"]
toolchain_hash = "toolchain-aaa"

[epoch.boundary]
boundary = "fresh_source_to_native_v1"
pipeline = "canonical_rooted_query_graph_v1"
compiler_build_profile = "release_thin_lto"
optimization = "o3"
linker = "internal"
output_kind = "native_executable"
worker_setting = "one"
allowed_input_classes = ["workload_source", "trusted_standard_library_source"]
allowed_embedded_asset_classes = ["bundled_runtime_archive"]
required_stages = ["source_discovery_and_parsing", "program_construction", "semantic_analysis", "cfg_and_optimization", "backend", "object_generation", "linking", "output_publication"]

[epoch.workload_source_hashes]
startup = "startup-ddd"

[epoch.environment]
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"

[epoch.sampling.startup]
samples = 2
batch_size = 2

[epoch.flagging]
k = 3.0
window = 10
"#;

    pub(crate) fn boundary_manifest() -> Manifest {
        Manifest::parse(BOUNDARY_MANIFEST).expect("fixture boundary manifest is valid")
    }

    /// A protocol-2 run in the current full-evidence encoding, consistent with
    /// [`boundary_manifest`] and appendable under it.
    pub(crate) fn boundary_run() -> RunObject {
        let evidence = crate::boundary::tests::evidence();
        let mut run = sample_run();
        run.identity.suite_revision = 2;
        run.identity.epoch = 3;
        run.identity.pins.invocation.target = "x86-64-linux".to_string();
        run.identity.pins.invocation.args = vec!["-O3".to_string(), "-j1".to_string()];
        run.identity.pins.workload_source_hashes =
            BTreeMap::from([("startup".to_string(), "startup-ddd".to_string())]);
        let mut sample = sample(2, 900_000_000);
        sample.output_binary_bytes = evidence.runner.output_size_bytes;
        sample.boundary_evidence = vec![evidence.clone(), evidence];
        run.workloads = vec![WorkloadObservation {
            workload: "startup".to_string(),
            boundary: None,
            samples: vec![sample.clone(), sample],
        }];
        run
    }

    #[test]
    fn a_full_evidence_protocol_two_run_is_appendable() {
        let outcome = validate_run(&boundary_manifest(), &boundary_run());
        assert_eq!(outcome.errors, Vec::new());
        assert!(outcome.publishes_headline());
    }

    #[test]
    fn the_encoded_form_validates_exactly_like_the_full_form() {
        let full = boundary_run();
        let encoded = crate::encode_stored_v3(&full).expect("fixture encodes");
        let full_outcome = validate_run(&boundary_manifest(), &full);
        let encoded_outcome = validate_run(&boundary_manifest(), &encoded);
        assert_eq!(encoded_outcome.errors, full_outcome.errors);
        assert_eq!(encoded_outcome.errors, Vec::new());
        assert!(encoded_outcome.publishes_headline());
    }

    #[test]
    fn a_tampered_process_digest_fails_the_witness_comparison() {
        let mut encoded = crate::encode_stored_v3(&boundary_run()).unwrap();
        encoded.workloads[0].samples[1].boundary_processes[1] = "0".repeat(64);
        let outcome = validate_run(&boundary_manifest(), &encoded);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { sample_index: 1, detail, .. }
                    if detail.contains("identity digest disagrees")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_stored_record_must_not_carry_inline_evidence() {
        let full = boundary_run();
        let mut encoded = crate::encode_stored_v3(&full).unwrap();
        encoded.workloads[0].samples[0].boundary_evidence =
            full.workloads[0].samples[0].boundary_evidence.clone();
        let outcome = validate_run(&boundary_manifest(), &encoded);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("must not carry inline boundary evidence")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_one_worker_record_without_work_digests_is_rejected() {
        let mut encoded = crate::encode_stored_v3(&boundary_run()).unwrap();
        encoded.workloads[0].samples[0].boundary_work_processes = Vec::new();
        let outcome = validate_run(&boundary_manifest(), &encoded);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("expected 2 work digests, found 0")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_stored_record_missing_its_workload_block_is_rejected() {
        let mut encoded = crate::encode_stored_v3(&boundary_run()).unwrap();
        encoded.workloads[0].boundary = None;
        let outcome = validate_run(&boundary_manifest(), &encoded);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("no workload boundary block")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_full_evidence_record_must_not_carry_digests() {
        let mut full = boundary_run();
        full.workloads[0].samples[0].boundary_processes = vec!["0".repeat(64); 2];
        let outcome = validate_run(&boundary_manifest(), &full);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("must not carry per-process digests")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn both_store_versions_read_and_versions_ahead_refuse() {
        // Readers implement every version still in the store; refusal applies
        // forward, never backward.
        let full = boundary_run();
        assert_eq!(full.schema_version, FULL_EVIDENCE_SCHEMA_VERSION);
        assert!(validate_run(&boundary_manifest(), &full).is_appendable());
        let encoded = crate::encode_stored_v3(&full).unwrap();
        assert_eq!(encoded.schema_version, RUN_SCHEMA_VERSION);
        assert!(validate_run(&boundary_manifest(), &encoded).is_appendable());
        let mut ahead = encoded;
        ahead.schema_version = FULL_EVIDENCE_SCHEMA_VERSION + 1;
        let outcome = validate_run(&boundary_manifest(), &ahead);
        assert_eq!(
            outcome.errors,
            vec![ValidationError::UnsupportedSchemaVersion {
                found: FULL_EVIDENCE_SCHEMA_VERSION + 1,
                expected: SUPPORTED_SCHEMA_VERSIONS,
            }]
        );
    }

    #[test]
    fn historical_full_evidence_v1_validates_and_invalidates_by_its_evidence() {
        let mut historical = boundary_run();
        historical.schema_version = crate::LEGACY_FULL_EVIDENCE_SCHEMA_VERSION;
        for observation in &mut historical.workloads {
            for sample in &mut observation.samples {
                for evidence in &mut sample.boundary_evidence {
                    let mut work = evidence.compiler_work;
                    work.legacy_v2_semantic_body_structure =
                        Some(crate::scaling::LegacySemanticBodyStructureWork {
                            body_lowerings: 1,
                            index_builds: 1,
                            precompute_bodies: 1,
                            ..Default::default()
                        });
                    evidence.compiler_work = work;
                }
            }
        }
        assert!(validate_run(&boundary_manifest(), &historical).is_appendable());

        historical.workloads[0].samples[0].boundary_evidence[1]
            .compiler_work
            .legacy_v2_semantic_body_structure
            .as_mut()
            .unwrap()
            .body_lowerings = 2;
        let outcome = validate_run(&boundary_manifest(), &historical);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("process 1:")
                        && detail.contains("schema v2 semantic_body_structure attribution")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );

        // Keep the fixture valid for the independent digest-shape check below.
        historical.workloads[0].samples[0].boundary_evidence[1]
            .compiler_work
            .legacy_v2_semantic_body_structure
            .as_mut()
            .unwrap()
            .body_lowerings = 1;

        historical.workloads[0].samples[0].boundary_processes = vec!["0".repeat(64); 2];
        let outcome = validate_run(&boundary_manifest(), &historical);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            ValidationError::BoundaryEvidenceMismatch { detail, .. }
                if detail.contains("must not carry per-process digests")
        )));
    }

    #[test]
    fn current_full_evidence_validates_work_shape_for_every_process() {
        let mut current = boundary_run();
        assert!(validate_run(&boundary_manifest(), &current).is_appendable());
        current.workloads[0].samples[0].boundary_evidence[1]
            .compiler_work
            .legacy_v2_semantic_body_structure =
            Some(crate::scaling::LegacySemanticBodyStructureWork::default());
        let outcome = validate_run(&boundary_manifest(), &current);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("process 1:")
                        && detail.contains("schema v3 compiler_work must not carry retired")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn historical_v2_run_reconstructs_its_work_digest_before_v3_reencoding() {
        // Build a genuine stored-shaped fixture, then replace the work block
        // with the historical wire shape. This exercises schema dispatch and
        // the old preimage rather than merely accepting missing fields.
        let full = boundary_run();
        let mut value = serde_json::to_value(crate::encode_stored_v3(&full).unwrap()).unwrap();
        value["schema_version"] = LEGACY_RUN_SCHEMA_VERSION.into();
        for observation in value["workloads"].as_array_mut().unwrap() {
            let boundary = observation["boundary"].as_object_mut().unwrap();
            let mut historical =
                serde_json::to_value(crate::scaling::LegacySemanticBodyStructureWork::default())
                    .unwrap();
            historical["body_lowerings"] = 1.into();
            historical["source_bytes"] = 17.into();
            historical["rir_instructions"] = 19.into();
            historical["index_builds"] = 1.into();
            historical["index_rir_instructions_visited"] = 19.into();
            historical["precompute_bodies"] = 1.into();
            historical["precompute_alias_allocations_examined"] = 1.into();
            historical["precompute_alias_filter_accepts"] = 1.into();
            historical["precompute_alias_eval_attempts"] = 1.into();
            boundary.insert("compiler_work".to_string(), {
                let mut work = boundary["compiler_work"].clone();
                let object = work.as_object_mut().unwrap();
                object.remove("candidate_body_plan_construction");
                object.remove("candidate_body_plan_materialization");
                object.remove("canonical_rir_presentation");
                object.remove("semantic_analysis_structure");
                object.insert("semantic_body_structure".to_string(), historical);
                work
            });
        }
        let mut historical: RunObject = serde_json::from_value(value).unwrap();
        const HISTORICAL_WORK_DIGEST: &str =
            "e837cde685fc85e75a1889bb8e29d0233618c8a4b836c941aeab7b8011ba5375";
        for observation in &mut historical.workloads {
            let boundary = observation.boundary.as_ref().unwrap();
            let digest = work_digest_v2(&boundary.compiler_work).unwrap();
            assert_eq!(digest, HISTORICAL_WORK_DIGEST);
            for sample in &mut observation.samples {
                sample.boundary_work_processes =
                    vec![HISTORICAL_WORK_DIGEST.to_owned(); sample.batch_size as usize];
            }
        }
        let outcome = validate_run(&boundary_manifest(), &historical);
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);

        let mut invalid = historical.clone();
        invalid.workloads[0]
            .boundary
            .as_mut()
            .unwrap()
            .compiler_work
            .legacy_v2_semantic_body_structure
            .as_mut()
            .unwrap()
            .body_lowerings = 2;
        let invalid_outcome = validate_run(&boundary_manifest(), &invalid);
        assert!(
            invalid_outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("schema v2 semantic_body_structure attribution")
            )),
            "unexpected errors: {:?}",
            invalid_outcome.errors
        );

        let v3 = crate::encode_stored_v3(&full).unwrap();
        assert_eq!(v3.schema_version, RUN_SCHEMA_VERSION);
        let encoded = serde_json::to_value(&v3).unwrap();
        assert!(
            encoded["workloads"][0]["boundary"]["compiler_work"]
                .get("semantic_body_structure")
                .is_none()
        );
        assert!(
            encoded["workloads"][0]["boundary"]["compiler_work"]
                .get("semantic_analysis_structure")
                .is_some()
        );

        let mut invalid_v3 = v3;
        invalid_v3.workloads[0]
            .boundary
            .as_mut()
            .unwrap()
            .compiler_work
            .legacy_v2_semantic_body_structure =
            Some(crate::scaling::LegacySemanticBodyStructureWork::default());
        let invalid_v3_outcome = validate_run(&boundary_manifest(), &invalid_v3);
        assert!(
            invalid_v3_outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("schema v3 compiler_work must not carry retired")
            )),
            "unexpected errors: {:?}",
            invalid_v3_outcome.errors
        );
    }

    #[test]
    fn provenance_naming_a_nonexistent_process_is_rejected() {
        let mut encoded = crate::encode_stored_v3(&boundary_run()).unwrap();
        encoded.workloads[0]
            .boundary
            .as_mut()
            .unwrap()
            .critical_path_source
            .process_index = 999;
        let outcome = validate_run(&boundary_manifest(), &encoded);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("names process 999 of a batch of 2")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_stored_record_must_name_its_full_evidence_form() {
        let mut encoded = crate::encode_stored_v3(&boundary_run()).unwrap();
        encoded.full_evidence = None;
        let outcome = validate_run(&boundary_manifest(), &encoded);
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                ValidationError::BoundaryEvidenceMismatch { detail, .. }
                    if detail.contains("must name its full-evidence form")
            )),
            "unexpected errors: {:?}",
            outcome.errors
        );
        // And the commitment is the input's own address.
        let full = boundary_run();
        let encoded = crate::encode_stored_v3(&full).unwrap();
        assert_eq!(
            encoded.full_evidence.as_deref(),
            Some(crate::content_address(&full).unwrap().as_str())
        );
    }
}
