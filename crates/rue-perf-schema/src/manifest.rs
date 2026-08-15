//! Suite revisions and platform epochs: the two-layer versioning model.
//!
//! Logical facts are declared once, and everything that can vary by platform
//! lives with the platform. A [`SuiteRevision`] says what the suite *is*; a
//! [`PlatformEpoch`] says how one platform measures it.
//!
//! The division has a practical consequence. Changing what a workload is
//! creates a new suite revision and therefore new epochs on every participating
//! platform. Raising only the macOS sample count creates a new epoch only
//! there. Either way the change is a declared, reviewable event rather than a
//! silent shift under a continuing headline.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::boundary::{BuildBoundary, BuildBoundaryPolicy};
use crate::run::EnvironmentFingerprint;

/// One workload in the suite.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    /// The workload's logical identifier, stable across suite revisions that
    /// do not change what the workload is.
    pub id: String,
    /// Path to the workload's root source, relative to the repository root.
    pub source: String,
    /// The question this workload exists to answer.
    ///
    /// Required, and not decoration: a probe that cannot name its question is
    /// corpus mass rather than a measurement, and growing the corpus is a new
    /// suite revision precisely so this stays deliberate.
    pub question: String,
    /// The single axis a scaling probe varies, and where this size sits on it.
    ///
    /// Present only on scaling probes (ADR-0067 §10, RUE-1264). Two workloads
    /// sharing an `axis` are sizes of one probe, and the ratio between their
    /// medians is the probe's signal — a sustained drift in that ratio is a
    /// complexity-class change, reportable independently of constant-factor
    /// movement. Declared here so the size is recoverable from the published
    /// record rather than only by parsing the workload id.
    #[serde(default)]
    pub scaling: Option<ScalingAxis>,
}

/// A scaling probe's declared axis position (RUE-1264).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingAxis {
    /// The one axis this probe varies, e.g. `module_count`.
    pub axis: String,
    /// This workload's size on that axis, in the axis's own unit.
    pub size: u64,
}

/// The platform-independent logical contract.
///
/// A suite revision pins what the suite means, not how any platform runs it:
/// which workloads exist and what program each one is, the published phase
/// taxonomy and timing schema version, and the runner protocol semantics —
/// what a sample is, how batching is defined, what a run object contains.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteRevision {
    /// The revision number. Monotonic; never reused.
    pub revision: u32,
    /// The timing schema version this revision's phase taxonomy belongs to.
    pub timing_schema_version: u32,
    /// The runner protocol version this revision's run objects conform to.
    pub protocol_version: u32,
    /// Exhaustive product boundary required by this protocol revision.
    /// Historical protocol-v1 suites predate machine-checkable evidence.
    #[serde(default)]
    pub boundary: Option<BuildBoundary>,
    /// The suite's workloads.
    pub workloads: Vec<Workload>,
}

impl SuiteRevision {
    /// Workload identifiers, sorted.
    pub fn workload_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self
            .workloads
            .iter()
            .map(|workload| workload.id.as_str())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Whether the suite declares a workload with this identifier.
    pub fn declares(&self, workload: &str) -> bool {
        self.workloads.iter().any(|entry| entry.id == workload)
    }
}

/// The environment class an epoch requires, as a policy rather than a machine.
///
/// Hosted runners change underneath a policy. The epoch declares what class of
/// environment is acceptable; each run records the exact
/// [`EnvironmentFingerprint`] it found, and drift within the policy is
/// annotated rather than prevented.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentPolicy {
    /// Required runner environment class, for example `github-hosted`.
    pub runner_label: String,
    /// Required runner image label, for example `ubuntu-24.04`.
    pub runner_image: String,
}

impl EnvironmentPolicy {
    /// Whether a fingerprint satisfies this policy.
    ///
    /// Only the declared class and image are required. The image *version*,
    /// CPU model, and core count are recorded but unconstrained — pinning them
    /// is not available on hosted hardware, and pretending otherwise would make
    /// collection fail rather than make measurements comparable.
    pub fn admits(&self, fingerprint: &EnvironmentFingerprint) -> bool {
        fingerprint.runner_label == self.runner_label
            && fingerprint.runner_image == self.runner_image
    }
}

/// How many samples to take for one workload, and how to batch them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingPolicy {
    /// How many samples to collect.
    pub samples: u32,
    /// How many compilations make up one sample.
    ///
    /// `1` for ordinary workloads. Very short workloads use a larger factor so
    /// timer resolution and per-process jitter do not dominate.
    pub batch_size: u32,
}

/// An externally observed fresh-process latency target for one workload.
///
/// Targets live on platform epochs rather than suite workloads because an
/// absolute duration is meaningful only in its declared runner regime. The
/// dashboard may show the same workload on other platforms, but it must not
/// imply that their clocks adjudicate this target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessElapsedTarget {
    /// Maximum process-spawn-through-exit duration, in nanoseconds.
    pub process_elapsed_ns: u64,
    /// The reviewed decision that established this target.
    pub reference: String,
}

/// A noise-aware non-regression ratchet for fresh-process latency.
///
/// The center and dispersion are reviewed evidence derived from the immutable
/// baseline run named by the epoch. The limit is fixed policy chosen from that
/// evidence: it absorbs calibrated hosted-runner noise without allowing a
/// later noisy observation to widen its own gate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessElapsedRatchet {
    /// The current best accepted process-spawn-through-exit median.
    pub baseline_process_elapsed_ns: u64,
    /// Calibrated dispersion of the baseline regime, in nanoseconds.
    pub baseline_mad_ns: u64,
    /// Fixed gate derived from the baseline and its calibrated dispersion.
    /// It changes only in a reviewed manifest update and never expands because
    /// a later run happened to be noisy.
    pub process_elapsed_limit_ns: u64,
    /// Content address of the immutable run from which the ratchet was set.
    pub reference_run: String,
}

/// When a workload's movement counts as movement.
///
/// A workload is flagged only when the difference between its current median
/// and the trailing-window median exceeds `k` times the pooled uncertainty of
/// the two. Differences inside that bound are rendered as uncertain, not as
/// movement.
///
/// Both constants come from the unpublished calibration phase and are pinned
/// here, so changing either is a visible epoch event rather than a quiet
/// retuning of what counts as a regression.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlaggingPolicy {
    /// The multiplier applied to pooled uncertainty.
    ///
    /// This is configuration, reviewed in ordinary pull requests. It is never
    /// serialized into a content-addressed run object, which is why a
    /// floating-point value is admissible here and nowhere in raw records.
    pub k: f64,
    /// How many prior runs form the trailing window.
    pub window: u32,
}

/// The run whose per-workload medians define ratio 1.0 for an epoch.
///
/// Only the first *complete, valid* run at a declared trunk revision may serve.
/// An attempted or partial run is never a baseline.
///
/// The medians themselves are not stored: they are derived from the referenced
/// run object, in keeping with storing raw observations and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// The compiler revision the baseline was measured at.
    pub commit: String,
    /// The content address of the baseline run object.
    pub run: String,
}

/// Everything that can vary by platform.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformEpoch {
    /// The epoch number, unique within its platform. Monotonic; never reused.
    pub id: u32,
    /// The platform this epoch measures, for example `x86_64-linux`.
    pub platform: String,
    /// The suite revision this epoch implements.
    pub suite_revision: u32,
    /// The compilation target triple.
    pub target: String,
    /// Every behavior-affecting compiler argument, in order.
    pub args: Vec<String>,
    /// Complete intended policy for protocol-v2 samples.
    #[serde(default)]
    pub boundary: Option<BuildBoundaryPolicy>,
    /// Content hash of the Rust toolchain the compiler is built with.
    ///
    /// Build environment, not product: changing it changes generated code and
    /// therefore measurements, while measuring nothing anyone ships. The Rue
    /// compiler, `std`, and the first-party toolchain are the *subject* of
    /// measurement and are deliberately not pinned here.
    pub toolchain_hash: String,
    /// Content hash of each workload's own source closure, keyed by workload.
    ///
    /// The workload's own sources only. `std` is part of the product under
    /// measurement, so std files a workload reads are excluded — otherwise a
    /// std edit would invalidate every series that reads it.
    pub workload_source_hashes: BTreeMap<String, String>,
    /// The environment class this epoch requires.
    pub environment: EnvironmentPolicy,
    /// Sampling policy per workload.
    pub sampling: BTreeMap<String, SamplingPolicy>,
    /// Absolute process-time targets adjudicated by this platform epoch.
    ///
    /// A workload omitted here has no absolute target in this regime. This is
    /// intentionally sparse: copying a reference-platform target onto every
    /// directional platform would turn unlike clocks into an apparent gate.
    #[serde(default)]
    pub process_elapsed_targets: BTreeMap<String, ProcessElapsedTarget>,
    /// Noise-aware fresh-process non-regression ratchets.
    ///
    /// Ratchets are separate from absolute targets: the former prevents Rue
    /// from losing accepted progress, while the latter says where the product
    /// is going. Only the reference platform normally carries one.
    #[serde(default)]
    pub process_elapsed_ratchets: BTreeMap<String, ProcessElapsedRatchet>,
    /// The regression-flagging rule.
    pub flagging: FlaggingPolicy,
    /// The headline baseline, once a complete valid run has established one.
    ///
    /// Absent until then: an epoch may exist and collect observations before it
    /// can express them as an index.
    #[serde(default)]
    pub baseline: Option<Baseline>,
    /// Whether scheduled collection measures this epoch.
    ///
    /// The manifest is the single place this is declared, so the collector, the
    /// CI gate, and a maintainer resolving a stall all read the same answer. A
    /// workflow that restated it would eventually disagree, and declaring the
    /// next epoch would mean editing two files under time pressure.
    ///
    /// False for the `local` epoch and for calibration epochs, whose pins are
    /// deliberate placeholders — checking those would fail permanently.
    #[serde(default)]
    pub collection: bool,
}

/// Why a manifest could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The manifest is not well-formed TOML, or does not match the schema.
    Malformed {
        /// The underlying parse error, rendered.
        detail: String,
    },
    /// Two suite revisions share a revision number.
    DuplicateSuiteRevision {
        /// The repeated revision number.
        revision: u32,
    },
    /// Two epochs on one platform share an identifier.
    DuplicateEpoch {
        /// The platform carrying the collision.
        platform: String,
        /// The repeated epoch identifier.
        id: u32,
    },
    /// A suite revision declares the same workload identifier twice.
    DuplicateWorkload {
        /// The suite revision containing the duplicate.
        revision: u32,
        /// The repeated workload identifier.
        workload: String,
    },
    /// A workload declares no question it answers.
    WorkloadWithoutQuestion {
        /// The suite revision containing the workload.
        revision: u32,
        /// The workload with an empty question.
        workload: String,
    },
    /// An epoch implements a suite revision the manifest does not declare.
    UnknownSuiteRevision {
        /// The platform of the offending epoch.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// The revision it claims to implement.
        revision: u32,
    },
    /// An epoch's per-workload declarations do not match its suite revision.
    ///
    /// Every workload needs a source hash and a sampling policy, and neither
    /// may name a workload the suite does not declare. An epoch that is
    /// incomplete or over-specified cannot pin what a run must match.
    EpochWorkloadMismatch {
        /// The platform of the offending epoch.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// What is being declared: `source hashes` or `sampling policies`.
        field: String,
        /// Suite workloads with no declaration.
        missing: Vec<String>,
        /// Declarations naming no suite workload.
        unexpected: Vec<String>,
    },
    /// A sampling policy asks for zero samples or a zero batch size.
    ///
    /// Both are protocol nonsense rather than an aggressive setting: a series
    /// cannot have a median of nothing, and a sample cannot measure zero
    /// compilations.
    EmptySamplingPolicy {
        /// The platform of the offending epoch.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// The workload whose policy is empty.
        workload: String,
    },
    /// An epoch declares an absolute target for a workload outside its suite.
    UnknownTargetWorkload {
        /// The platform carrying the target.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// The undeclared workload.
        workload: String,
    },
    /// An absolute target has no positive duration or reviewed reference.
    InvalidProcessElapsedTarget {
        /// The platform carrying the target.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// The workload whose target is invalid.
        workload: String,
    },
    /// An epoch declares a ratchet for a workload outside its suite.
    UnknownRatchetWorkload {
        /// The platform carrying the ratchet.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// The undeclared workload.
        workload: String,
    },
    /// A ratchet has no positive center, no reference, or names a run other
    /// than the epoch's baseline.
    InvalidProcessElapsedRatchet {
        /// The platform carrying the ratchet.
        platform: String,
        /// The offending epoch.
        epoch: u32,
        /// The workload whose ratchet is invalid.
        workload: String,
    },
    /// A platform declares more than one collection epoch.
    ///
    /// Collection measures one epoch per platform. Two would make "the epoch
    /// being collected" ambiguous for the collector and the CI gate alike, and
    /// they could resolve it differently.
    DuplicateCollectionEpoch {
        /// The platform carrying the ambiguity.
        platform: String,
        /// The epoch already marked for collection.
        first: u32,
        /// The epoch that also claims it.
        second: u32,
    },
    /// Suite protocol, epoch policy, and canonical invocation disagree.
    BoundaryContract {
        platform: String,
        epoch: u32,
        detail: String,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Malformed { detail } => write!(f, "malformed manifest: {detail}"),
            ManifestError::DuplicateSuiteRevision { revision } => {
                write!(f, "suite revision {revision} is declared more than once")
            }
            ManifestError::DuplicateEpoch { platform, id } => {
                write!(f, "epoch {id} on {platform} is declared more than once")
            }
            ManifestError::DuplicateWorkload { revision, workload } => write!(
                f,
                "suite revision {revision} declares workload {workload:?} more than once"
            ),
            ManifestError::WorkloadWithoutQuestion { revision, workload } => write!(
                f,
                "workload {workload:?} in suite revision {revision} names no question it answers"
            ),
            ManifestError::UnknownSuiteRevision {
                platform,
                epoch,
                revision,
            } => write!(
                f,
                "epoch {epoch} on {platform} implements undeclared suite revision {revision}"
            ),
            ManifestError::EpochWorkloadMismatch {
                platform,
                epoch,
                field,
                missing,
                unexpected,
            } => write!(
                f,
                "epoch {epoch} on {platform} has {field} that do not match its suite revision \
                 (missing: {missing:?}, unexpected: {unexpected:?})"
            ),
            ManifestError::EmptySamplingPolicy {
                platform,
                epoch,
                workload,
            } => write!(
                f,
                "epoch {epoch} on {platform} samples workload {workload:?} zero times or in \
                zero-sized batches"
            ),
            ManifestError::UnknownTargetWorkload {
                platform,
                epoch,
                workload,
            } => write!(
                f,
                "epoch {epoch} on {platform} declares a process target for undeclared workload \
                 {workload:?}"
            ),
            ManifestError::InvalidProcessElapsedTarget {
                platform,
                epoch,
                workload,
            } => write!(
                f,
                "epoch {epoch} on {platform} declares an invalid process target for workload \
                 {workload:?}"
            ),
            ManifestError::UnknownRatchetWorkload {
                platform,
                epoch,
                workload,
            } => write!(
                f,
                "epoch {epoch} on {platform} declares a process ratchet for undeclared workload \
                 {workload:?}"
            ),
            ManifestError::InvalidProcessElapsedRatchet {
                platform,
                epoch,
                workload,
            } => write!(
                f,
                "epoch {epoch} on {platform} declares an invalid process ratchet for workload \
                 {workload:?}"
            ),
            ManifestError::DuplicateCollectionEpoch {
                platform,
                first,
                second,
            } => write!(
                f,
                "platform {platform} marks epochs {first} and {second} for collection; \
                exactly one epoch per platform may be collected"
            ),
            ManifestError::BoundaryContract {
                platform,
                epoch,
                detail,
            } => write!(
                f,
                "epoch {epoch} on {platform} violates its build-boundary contract: {detail}"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default)]
    suite: Vec<SuiteRevision>,
    #[serde(default)]
    epoch: Vec<PlatformEpoch>,
}

/// The declared suite revisions and platform epochs.
///
/// Loaded from `performance/manifest.toml`. Parsing is not enough: [`Manifest::parse`]
/// also enforces that the two layers agree, so a later validation failure
/// against an epoch always means the *run* is wrong rather than the manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    suites: BTreeMap<u32, SuiteRevision>,
    epochs: Vec<PlatformEpoch>,
}

impl Manifest {
    /// Parse and check a manifest.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let raw: RawManifest = toml::from_str(text).map_err(|error| ManifestError::Malformed {
            detail: error.to_string(),
        })?;

        let mut suites: BTreeMap<u32, SuiteRevision> = BTreeMap::new();
        for suite in raw.suite {
            let mut seen: Vec<&str> = Vec::new();
            for workload in &suite.workloads {
                if seen.contains(&workload.id.as_str()) {
                    return Err(ManifestError::DuplicateWorkload {
                        revision: suite.revision,
                        workload: workload.id.clone(),
                    });
                }
                if workload.question.trim().is_empty() {
                    return Err(ManifestError::WorkloadWithoutQuestion {
                        revision: suite.revision,
                        workload: workload.id.clone(),
                    });
                }
                seen.push(&workload.id);
            }
            if suites.contains_key(&suite.revision) {
                return Err(ManifestError::DuplicateSuiteRevision {
                    revision: suite.revision,
                });
            }
            suites.insert(suite.revision, suite);
        }

        let manifest = Manifest {
            suites,
            epochs: raw.epoch,
        };
        manifest.check_epochs()?;
        Ok(manifest)
    }

    fn check_epochs(&self) -> Result<(), ManifestError> {
        let mut seen: Vec<(&str, u32)> = Vec::new();
        let mut collected: Vec<(&str, u32)> = Vec::new();
        for epoch in &self.epochs {
            if epoch.collection {
                if let Some((_, first)) = collected
                    .iter()
                    .find(|(platform, _)| *platform == epoch.platform)
                {
                    return Err(ManifestError::DuplicateCollectionEpoch {
                        platform: epoch.platform.clone(),
                        first: *first,
                        second: epoch.id,
                    });
                }
                collected.push((epoch.platform.as_str(), epoch.id));
            }
            let key = (epoch.platform.as_str(), epoch.id);
            if seen.contains(&key) {
                return Err(ManifestError::DuplicateEpoch {
                    platform: epoch.platform.clone(),
                    id: epoch.id,
                });
            }
            seen.push(key);

            let Some(suite) = self.suites.get(&epoch.suite_revision) else {
                return Err(ManifestError::UnknownSuiteRevision {
                    platform: epoch.platform.clone(),
                    epoch: epoch.id,
                    revision: epoch.suite_revision,
                });
            };

            match (suite.protocol_version, suite.boundary, &epoch.boundary) {
                (1, None, None) => {}
                (2, Some(boundary), Some(policy)) if boundary == policy.boundary => {
                    policy
                        .validate()
                        .map_err(|detail| ManifestError::BoundaryContract {
                            platform: epoch.platform.clone(),
                            epoch: epoch.id,
                            detail,
                        })?;
                    let expected_args = policy.canonical_compiler_args();
                    if epoch.args != expected_args {
                        return Err(ManifestError::BoundaryContract {
                            platform: epoch.platform.clone(),
                            epoch: epoch.id,
                            detail: format!(
                                "arguments {:?} do not match canonical boundary arguments {:?}",
                                epoch.args, expected_args
                            ),
                        });
                    }
                }
                (protocol, suite_boundary, epoch_boundary) => {
                    return Err(ManifestError::BoundaryContract {
                        platform: epoch.platform.clone(),
                        epoch: epoch.id,
                        detail: format!(
                            "protocol {protocol} declares suite boundary {suite_boundary:?} and epoch policy {epoch_boundary:?}"
                        ),
                    });
                }
            }

            let declared = suite.workload_ids();
            check_coverage(
                epoch,
                "source hashes",
                &declared,
                epoch.workload_source_hashes.keys(),
            )?;
            check_coverage(epoch, "sampling policies", &declared, epoch.sampling.keys())?;

            for (workload, policy) in &epoch.sampling {
                if policy.samples == 0 || policy.batch_size == 0 {
                    return Err(ManifestError::EmptySamplingPolicy {
                        platform: epoch.platform.clone(),
                        epoch: epoch.id,
                        workload: workload.clone(),
                    });
                }
            }
            for (workload, target) in &epoch.process_elapsed_targets {
                if !declared.contains(&workload.as_str()) {
                    return Err(ManifestError::UnknownTargetWorkload {
                        platform: epoch.platform.clone(),
                        epoch: epoch.id,
                        workload: workload.clone(),
                    });
                }
                if target.process_elapsed_ns == 0 || target.reference.trim().is_empty() {
                    return Err(ManifestError::InvalidProcessElapsedTarget {
                        platform: epoch.platform.clone(),
                        epoch: epoch.id,
                        workload: workload.clone(),
                    });
                }
            }
            for (workload, ratchet) in &epoch.process_elapsed_ratchets {
                if !declared.contains(&workload.as_str()) {
                    return Err(ManifestError::UnknownRatchetWorkload {
                        platform: epoch.platform.clone(),
                        epoch: epoch.id,
                        workload: workload.clone(),
                    });
                }
                if ratchet.baseline_process_elapsed_ns == 0
                    || ratchet.process_elapsed_limit_ns < ratchet.baseline_process_elapsed_ns
                    || ratchet.reference_run.trim().is_empty()
                    || epoch
                        .baseline
                        .as_ref()
                        .is_none_or(|baseline| baseline.run != ratchet.reference_run)
                {
                    return Err(ManifestError::InvalidProcessElapsedRatchet {
                        platform: epoch.platform.clone(),
                        epoch: epoch.id,
                        workload: workload.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// The suite revision with this number, if declared.
    pub fn suite(&self, revision: u32) -> Option<&SuiteRevision> {
        self.suites.get(&revision)
    }

    /// The epoch with this identifier on this platform, if declared.
    pub fn epoch(&self, platform: &str, id: u32) -> Option<&PlatformEpoch> {
        self.epochs
            .iter()
            .find(|epoch| epoch.platform == platform && epoch.id == id)
    }

    /// Every declared suite revision, in ascending revision order.
    pub fn suites(&self) -> impl Iterator<Item = &SuiteRevision> {
        self.suites.values()
    }

    /// Every declared epoch, in declaration order.
    pub fn epochs(&self) -> impl Iterator<Item = &PlatformEpoch> {
        self.epochs.iter()
    }

    /// Every epoch scheduled collection measures, in declaration order.
    pub fn collection_epochs(&self) -> impl Iterator<Item = &PlatformEpoch> {
        self.epochs.iter().filter(|epoch| epoch.collection)
    }

    /// The epoch scheduled collection measures on this platform, if any.
    pub fn collection_epoch(&self, platform: &str) -> Option<&PlatformEpoch> {
        self.epochs
            .iter()
            .find(|epoch| epoch.collection && epoch.platform == platform)
    }
}

fn check_coverage<'a>(
    epoch: &PlatformEpoch,
    field: &str,
    declared: &[&str],
    provided: impl Iterator<Item = &'a String>,
) -> Result<(), ManifestError> {
    let provided: Vec<&str> = provided.map(String::as_str).collect();
    let missing: Vec<String> = declared
        .iter()
        .filter(|id| !provided.contains(*id))
        .map(|id| (*id).to_string())
        .collect();
    let unexpected: Vec<String> = provided
        .iter()
        .filter(|id| !declared.contains(*id))
        .map(|id| (*id).to_string())
        .collect();
    if missing.is_empty() && unexpected.is_empty() {
        return Ok(());
    }
    Err(ManifestError::EpochWorkloadMismatch {
        platform: epoch.platform.clone(),
        epoch: epoch.id,
        field: field.to_string(),
        missing,
        unexpected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUITE: &str = r#"
[[suite]]
revision = 1
timing_schema_version = 1
protocol_version = 1

[[suite.workloads]]
id = "startup"
source = "performance/workloads/startup/main.rue"
question = "What does a minimal compilation cost end to end?"

[[suite.workloads]]
id = "caldera"
source = "examples/caldera/main.rue"
question = "How does a large single-program compilation behave?"
"#;

    const EPOCH: &str = r#"
[[epoch]]
id = 1
platform = "x86_64-linux"
suite_revision = 1
target = "x86_64-unknown-linux-gnu"
args = ["-O2"]
toolchain_hash = "toolchain-aaa"

[epoch.workload_source_hashes]
startup = "startup-ccc"
caldera = "caldera-ddd"

[epoch.environment]
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"

[epoch.sampling.startup]
samples = 25
batch_size = 40

[epoch.sampling.caldera]
samples = 15
batch_size = 1

[epoch.flagging]
k = 3.0
window = 10
"#;

    fn manifest() -> Manifest {
        Manifest::parse(&format!("{SUITE}{EPOCH}")).expect("fixture manifest is valid")
    }

    #[test]
    fn a_well_formed_manifest_parses_both_layers() {
        let manifest = manifest();
        let suite = manifest.suite(1).expect("suite 1 is declared");
        assert_eq!(suite.workload_ids(), vec!["caldera", "startup"]);
        assert!(suite.declares("startup"));
        assert!(!suite.declares("meridian"));

        let epoch = manifest
            .epoch("x86_64-linux", 1)
            .expect("epoch 1 is declared");
        assert_eq!(epoch.args, vec!["-O2".to_string()]);
        assert_eq!(epoch.sampling["startup"].batch_size, 40);
        assert!(
            epoch.baseline.is_none(),
            "an epoch may exist before its baseline"
        );
    }

    #[test]
    fn an_epoch_may_carry_a_baseline() {
        let text =
            format!("{SUITE}{EPOCH}\n[epoch.baseline]\ncommit = \"abc123\"\nrun = \"deadbeef\"\n");
        let manifest = Manifest::parse(&text).expect("baseline is well formed");
        let baseline = manifest
            .epoch("x86_64-linux", 1)
            .and_then(|epoch| epoch.baseline.clone())
            .expect("baseline is present");
        assert_eq!(baseline.commit, "abc123");
        assert_eq!(baseline.run, "deadbeef");
    }

    #[test]
    fn a_platform_epoch_may_publish_an_external_latency_target() {
        let text = format!(
            "{SUITE}{}",
            EPOCH.replace(
                "[epoch.flagging]",
                "[epoch.process_elapsed_targets.caldera]\n\
                 process_elapsed_ns = 250000000\n\
                 reference = \"ADR-0071\"\n\n\
                 [epoch.flagging]"
            )
        );
        let manifest = Manifest::parse(&text).expect("target belongs to the platform epoch");
        let target = &manifest
            .epoch("x86_64-linux", 1)
            .expect("epoch")
            .process_elapsed_targets["caldera"];
        assert_eq!(target.process_elapsed_ns, 250_000_000);
        assert_eq!(target.reference, "ADR-0071");
    }

    #[test]
    fn a_ratchet_must_name_the_epochs_immutable_baseline_run() {
        let text = format!(
            "{SUITE}{EPOCH}\n\
             [epoch.baseline]\n\
             commit = \"abc123\"\n\
             run = \"deadbeef\"\n\n\
             [epoch.process_elapsed_ratchets.caldera]\n\
             baseline_process_elapsed_ns = 250000000\n\
             baseline_mad_ns = 1000000\n\
             process_elapsed_limit_ns = 256000000\n\
             reference_run = \"deadbeef\"\n"
        );
        let manifest = Manifest::parse(&text).expect("ratchet names the baseline run");
        let ratchet = &manifest
            .epoch("x86_64-linux", 1)
            .unwrap()
            .process_elapsed_ratchets["caldera"];
        assert_eq!(ratchet.baseline_process_elapsed_ns, 250_000_000);
        assert_eq!(ratchet.reference_run, "deadbeef");

        let mismatched = text.replace("reference_run = \"deadbeef\"", "reference_run = \"other\"");
        assert!(matches!(
            Manifest::parse(&mismatched),
            Err(ManifestError::InvalidProcessElapsedRatchet { .. })
        ));
    }

    #[test]
    fn a_target_for_an_undeclared_workload_is_rejected() {
        let text = format!(
            "{SUITE}{}",
            EPOCH.replace(
                "[epoch.flagging]",
                "[epoch.process_elapsed_targets.unknown]\n\
                 process_elapsed_ns = 250000000\n\
                 reference = \"ADR-0071\"\n\n\
                 [epoch.flagging]"
            )
        );
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::UnknownTargetWorkload {
                platform: "x86_64-linux".to_string(),
                epoch: 1,
                workload: "unknown".to_string(),
            }
        );
    }

    #[test]
    fn an_empty_or_zero_target_is_rejected() {
        for declaration in [
            "process_elapsed_ns = 0\nreference = \"ADR-0071\"",
            "process_elapsed_ns = 250000000\nreference = \"   \"",
        ] {
            let text = format!(
                "{SUITE}{}",
                EPOCH.replace(
                    "[epoch.flagging]",
                    &format!(
                        "[epoch.process_elapsed_targets.caldera]\n{declaration}\n\n[epoch.flagging]"
                    )
                )
            );
            assert_eq!(
                Manifest::parse(&text).unwrap_err(),
                ManifestError::InvalidProcessElapsedTarget {
                    platform: "x86_64-linux".to_string(),
                    epoch: 1,
                    workload: "caldera".to_string(),
                }
            );
        }
    }

    #[test]
    fn a_workload_must_name_the_question_it_answers() {
        let text = SUITE.replace(
            r#"question = "What does a minimal compilation cost end to end?""#,
            r#"question = "  ""#,
        );
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::WorkloadWithoutQuestion {
                revision: 1,
                workload: "startup".to_string(),
            }
        );
    }

    #[test]
    fn duplicate_workload_identifiers_are_rejected() {
        let text = format!(
            "{SUITE}\n[[suite.workloads]]\nid = \"startup\"\nsource = \"x\"\nquestion = \"q\"\n"
        );
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::DuplicateWorkload {
                revision: 1,
                workload: "startup".to_string(),
            }
        );
    }

    #[test]
    fn duplicate_suite_revisions_are_rejected() {
        assert_eq!(
            Manifest::parse(&format!("{SUITE}{SUITE}")).unwrap_err(),
            ManifestError::DuplicateSuiteRevision { revision: 1 }
        );
    }

    #[test]
    fn duplicate_epochs_on_one_platform_are_rejected() {
        assert_eq!(
            Manifest::parse(&format!("{SUITE}{EPOCH}{EPOCH}")).unwrap_err(),
            ManifestError::DuplicateEpoch {
                platform: "x86_64-linux".to_string(),
                id: 1,
            }
        );
    }

    #[test]
    fn the_same_epoch_number_on_another_platform_is_fine() {
        let other = EPOCH
            .replace(
                r#"platform = "x86_64-linux""#,
                r#"platform = "aarch64-macos""#,
            )
            .replace(
                r#"runner_image = "ubuntu-24.04""#,
                r#"runner_image = "macos-15""#,
            );
        let manifest = Manifest::parse(&format!("{SUITE}{EPOCH}{other}"))
            .expect("epoch numbering is per platform");
        assert!(manifest.epoch("aarch64-macos", 1).is_some());
        assert_eq!(manifest.epochs().count(), 2);
    }

    #[test]
    fn an_epoch_implementing_an_undeclared_suite_revision_is_rejected() {
        let text = format!(
            "{SUITE}{}",
            EPOCH.replace("suite_revision = 1", "suite_revision = 7")
        );
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::UnknownSuiteRevision {
                platform: "x86_64-linux".to_string(),
                epoch: 1,
                revision: 7,
            }
        );
    }

    #[test]
    fn an_epoch_missing_a_workload_source_hash_is_rejected() {
        let text = format!(
            "{SUITE}{}",
            EPOCH.replace("caldera = \"caldera-ddd\"\n", "")
        );
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::EpochWorkloadMismatch {
                platform: "x86_64-linux".to_string(),
                epoch: 1,
                field: "source hashes".to_string(),
                missing: vec!["caldera".to_string()],
                unexpected: Vec::new(),
            }
        );
    }

    #[test]
    fn an_epoch_sampling_an_undeclared_workload_is_rejected() {
        let text =
            format!("{SUITE}{EPOCH}\n[epoch.sampling.meridian]\nsamples = 5\nbatch_size = 1\n");
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::EpochWorkloadMismatch {
                platform: "x86_64-linux".to_string(),
                epoch: 1,
                field: "sampling policies".to_string(),
                missing: Vec::new(),
                unexpected: vec!["meridian".to_string()],
            }
        );
    }

    #[test]
    fn a_zero_sample_policy_is_rejected() {
        let text = format!("{SUITE}{}", EPOCH.replace("samples = 15", "samples = 0"));
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::EmptySamplingPolicy {
                platform: "x86_64-linux".to_string(),
                epoch: 1,
                workload: "caldera".to_string(),
            }
        );
    }

    #[test]
    fn a_zero_batch_size_is_rejected() {
        let text = format!(
            "{SUITE}{}",
            EPOCH.replace("batch_size = 40", "batch_size = 0")
        );
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::EmptySamplingPolicy {
                platform: "x86_64-linux".to_string(),
                epoch: 1,
                workload: "startup".to_string(),
            }
        );
    }

    #[test]
    fn unknown_manifest_keys_are_rejected_rather_than_ignored() {
        let text = format!("{SUITE}{EPOCH}\n[[suite]]\nrevision = 2\ntimings = 1\n");
        assert!(matches!(
            Manifest::parse(&text).unwrap_err(),
            ManifestError::Malformed { .. }
        ));
    }

    #[test]
    fn environment_policy_admits_only_its_declared_class_and_image() {
        let policy = EnvironmentPolicy {
            runner_label: "github-hosted".to_string(),
            runner_image: "ubuntu-24.04".to_string(),
        };
        let mut fingerprint = crate::EnvironmentFingerprint {
            runner_label: "github-hosted".to_string(),
            runner_image: "ubuntu-24.04".to_string(),
            runner_image_version: "ubuntu24/20250720.1.0".to_string(),
            cpu_model: "probe".to_string(),
            core_count: 4,
            memory_bytes: 1,
            kernel_version: "probe".to_string(),
            os_version: "probe".to_string(),
            architecture: "x86_64".to_string(),
        };
        assert!(policy.admits(&fingerprint));

        // Image version drift is recorded, never rejected: it is the drift the
        // policy deliberately cannot prevent on hosted hardware.
        fingerprint.runner_image_version = "ubuntu24/20250901.2.0".to_string();
        assert!(policy.admits(&fingerprint));

        fingerprint.runner_image = "ubuntu-22.04".to_string();
        assert!(!policy.admits(&fingerprint));
    }
}
