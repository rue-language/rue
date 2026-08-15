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

use crate::RUN_SCHEMA_VERSION;
use crate::manifest::Manifest;
use crate::run::{FailureRecord, Phase, RunObject, Sample};
use crate::sanity::{is_commit, is_utc_timestamp, samples_beyond_policy};
use crate::stats::median;

/// A reason a run may not enter a series at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// The run was written under a schema version this crate does not
    /// implement. There is no compatibility path, by design.
    UnsupportedSchemaVersion {
        /// The version the run object declares.
        found: u32,
        /// The version this crate implements.
        expected: u32,
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
                    "run schema version {found} is not the supported version {expected}"
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

    if run.schema_version != RUN_SCHEMA_VERSION {
        // Nothing below can be trusted to mean what it appears to mean, so this
        // is the one failure that stops validation short.
        return ValidationOutcome {
            errors: vec![ValidationError::UnsupportedSchemaVersion {
                found: run.schema_version,
                expected: RUN_SCHEMA_VERSION,
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
        }
    }

    pub(crate) fn sample_run() -> RunObject {
        RunObject {
            schema_version: RUN_SCHEMA_VERSION,
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
            workloads: vec![
                WorkloadObservation {
                    workload: "caldera".to_string(),
                    samples: vec![sample(1, 900_000_000), sample(1, 910_000_000)],
                },
                WorkloadObservation {
                    workload: "startup".to_string(),
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
        run.schema_version = RUN_SCHEMA_VERSION + 1;
        let outcome = validate_run(&manifest(), &run);
        assert_eq!(
            outcome.errors,
            vec![ValidationError::UnsupportedSchemaVersion {
                found: RUN_SCHEMA_VERSION + 1,
                expected: RUN_SCHEMA_VERSION,
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
}
