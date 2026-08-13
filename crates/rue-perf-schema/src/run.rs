//! Raw observation records: what a run object contains on disk.
//!
//! Everything here is raw. Durations are integer nanoseconds, sizes are integer
//! bytes, and no field holds a derived statistic. Milliseconds, percentages,
//! medians, and dispersion are presentation-time computations that consumers
//! rebuild from these records.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{BuildBoundaryEvidence, RUN_SCHEMA_VERSION};

/// A published wall-clock phase.
///
/// These are the mutually exclusive, explicitly instrumented top-level phases
/// whose durations sum — with the two structural buckets of [`Band`] — to
/// compiler-root elapsed time. They are the only bands that may appear in an
/// additive visualization.
///
/// The phase set is part of the logical contract a [`crate::SuiteRevision`]
/// pins. Adding, removing, or redrawing a phase is a timing-schema change and
/// therefore requires a new suite revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Locating source files and lexing/parsing them into module syntax.
    SourceDiscoveryAndParsing,
    /// Assembling parsed modules into the canonical merged program.
    ProgramConstruction,
    /// Type checking, inference, ownership, and the rest of semantic analysis.
    SemanticAnalysis,
    /// CFG construction and the optimization pipeline over it.
    CfgAndOptimization,
    /// Instruction selection, register allocation, scheduling, and emission.
    Backend,
    /// Producing object files from emitted machine code.
    ObjectGeneration,
    /// Linking objects and runtime archives into the output binary.
    Linking,
}

impl Phase {
    /// Every published phase, in the order the compiler reaches them.
    ///
    /// This order is presentational only; correctness never depends on it.
    pub const ALL: [Phase; 7] = [
        Phase::SourceDiscoveryAndParsing,
        Phase::ProgramConstruction,
        Phase::SemanticAnalysis,
        Phase::CfgAndOptimization,
        Phase::Backend,
        Phase::ObjectGeneration,
        Phase::Linking,
    ];

    /// The stable wire name used in run objects and chart data.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Phase::SourceDiscoveryAndParsing => "source_discovery_and_parsing",
            Phase::ProgramConstruction => "program_construction",
            Phase::SemanticAnalysis => "semantic_analysis",
            Phase::CfgAndOptimization => "cfg_and_optimization",
            Phase::Backend => "backend",
            Phase::ObjectGeneration => "object_generation",
            Phase::Linking => "linking",
        }
    }

    /// The phase with this wire name, if the taxonomy declares one.
    ///
    /// The compiler's instrumentation names its phases with these strings, so
    /// parsing them here rather than in the collector keeps the producer and
    /// this schema from drifting apart silently.
    pub fn from_wire_name(name: &str) -> Option<Phase> {
        Phase::ALL
            .into_iter()
            .find(|phase| phase.wire_name() == name)
    }

    /// This phase's position in [`Phase::ALL`].
    ///
    /// Collectors keep per-phase state in fixed-size arrays; this is the index
    /// into them.
    pub fn index(self) -> usize {
        Phase::ALL
            .into_iter()
            .position(|phase| phase == self)
            .expect("Phase::ALL covers every phase")
    }
}

/// A band of the additive stack: a published phase or a structural bucket.
///
/// `MixedParallel` and `Unattributed` are published bands, not artifacts to be
/// hidden. Sustained growth in `MixedParallel` means the phase boundaries no
/// longer describe the compiler's parallel structure and should be redrawn;
/// growth in `Unattributed` means compiler time is moving somewhere the
/// instrumentation does not describe. Fractional attribution of parallel work
/// across phases is deliberately not offered: if the partition is unsatisfying,
/// the fix is better phase boundaries, not invented weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Band {
    /// Exactly one published phase was active over the interval.
    Phase(Phase),
    /// More than one distinct phase was active over the interval.
    MixedParallel,
    /// Compiler root was active with no phase active over the interval.
    Unattributed,
}

impl Band {
    /// Every band of the additive stack, phases first.
    pub fn all() -> Vec<Band> {
        let mut bands: Vec<Band> = Phase::ALL.into_iter().map(Band::Phase).collect();
        bands.push(Band::MixedParallel);
        bands.push(Band::Unattributed);
        bands
    }

    /// The stable wire name used in run objects and chart data.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Band::Phase(phase) => phase.wire_name(),
            Band::MixedParallel => "mixed_parallel",
            Band::Unattributed => "unattributed",
        }
    }
}

/// The wall-clock partition of one compilation's compiler-root time.
///
/// The compiler produces this directly. It is never reconstructed from span
/// names downstream, because inclusive spans nest and overlap and summing them
/// double-counts.
///
/// The defining property, in exact integer nanoseconds:
///
/// ```text
/// sum(phase_ns) + mixed_parallel_ns + unattributed_ns == compiler_root_ns
/// ```
///
/// [`PhaseAccounting::holds`] checks it. A sample that violates it is evidence
/// of an instrumentation bug: it is excluded from every derived statistic and
/// kept on disk with a [`FailureRecord::PhaseInvariant`] record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseAccounting {
    /// Nanoseconds attributed to each published phase.
    ///
    /// Every phase in [`Phase::ALL`] is present, including those that measured
    /// zero. A missing or unknown key is a schema violation, not a default.
    pub phase_ns: BTreeMap<Phase, u64>,
    /// Nanoseconds during which more than one distinct phase was active.
    pub mixed_parallel_ns: u64,
    /// Nanoseconds during which compiler root was active but no phase was.
    pub unattributed_ns: u64,
    /// Compiler-root elapsed nanoseconds: the total the stack must sum to.
    ///
    /// This is the union of active root intervals, not a sum of spans.
    pub compiler_root_ns: u64,
}

impl PhaseAccounting {
    /// Nanoseconds in a single band.
    ///
    /// A phase absent from `phase_ns` reads as zero here; use
    /// [`PhaseAccounting::missing_phases`] to detect that absence, which is a
    /// schema violation rather than a measurement of zero.
    pub fn band_ns(&self, band: Band) -> u64 {
        match band {
            Band::Phase(phase) => self.phase_ns.get(&phase).copied().unwrap_or(0),
            Band::MixedParallel => self.mixed_parallel_ns,
            Band::Unattributed => self.unattributed_ns,
        }
    }

    /// Total nanoseconds attributed across every band.
    ///
    /// Saturating arithmetic keeps a corrupt record from panicking a reader;
    /// the resulting mismatch with `compiler_root_ns` is reported as an
    /// invariant violation instead.
    pub fn attributed_ns(&self) -> u64 {
        Band::all()
            .into_iter()
            .fold(0u64, |total, band| total.saturating_add(self.band_ns(band)))
    }

    /// Whether the exact phase-sum invariant holds for this sample.
    pub fn holds(&self) -> bool {
        self.attributed_ns() == self.compiler_root_ns
    }

    /// Published phases with no entry in `phase_ns`.
    ///
    /// A phase that genuinely took no time records `0`. An absent key means the
    /// producer and this schema disagree about the taxonomy.
    pub fn missing_phases(&self) -> Vec<Phase> {
        Phase::ALL
            .into_iter()
            .filter(|phase| !self.phase_ns.contains_key(phase))
            .collect()
    }
}

/// One measured compilation, or one measured batch of them.
///
/// A **fresh build**: a newly launched compiler process with no retained
/// compiler-session state. Operating-system state (filesystem and page cache)
/// is not controlled, so these are not "cold" measurements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    /// How many compilations this sample measures as one unit.
    ///
    /// `1` for ordinary workloads. Short workloads such as the startup probe
    /// use a larger factor so that timer resolution and per-process jitter do
    /// not dominate. The factor is pinned by the epoch's
    /// [`crate::SamplingPolicy`], so changing it is a visible epoch event.
    pub batch_size: u32,
    /// Externally measured process wall time, in nanoseconds.
    ///
    /// The difference from `phases.compiler_root_ns` — process startup, output
    /// publication, other driver overhead — is real time the user experiences.
    /// It is reported, but it is outside the phase stack.
    pub process_elapsed_ns: u64,
    /// Externally measured peak resident memory, in bytes.
    pub peak_memory_bytes: u64,
    /// Size of the produced binary, in bytes.
    pub output_binary_bytes: u64,
    /// The compiler's own wall-clock partition for this sample.
    pub phases: PhaseAccounting,
    /// One independent runner/compiler proof for each process in this sample.
    ///
    /// Empty only for historical protocol-v1 suites. Protocol v2 requires
    /// exactly `batch_size` entries and validates each one against its epoch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundary_evidence: Vec<BuildBoundaryEvidence>,
}

impl Sample {
    /// Driver overhead: process wall time outside compiler root.
    ///
    /// Returns `None` when the process was measured as shorter than the
    /// compiler root it contains, which cannot happen physically and marks the
    /// sample as invalid.
    pub fn driver_overhead_ns(&self) -> Option<u64> {
        self.process_elapsed_ns
            .checked_sub(self.phases.compiler_root_ns)
    }
}

/// Every sample collected for one workload in one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadObservation {
    /// The workload's logical identifier, as declared by the suite revision.
    pub workload: String,
    /// Raw samples in collection order.
    ///
    /// All samples are stored, including invalid ones. Consumers exclude
    /// invalid samples from statistics; nothing deletes them.
    pub samples: Vec<Sample>,
}

/// Structured evidence about something that went wrong during a run.
///
/// Failures are recorded, never silently dropped. A run with failures is still
/// stored: a persistently broken workload should be visible rather than
/// appearing as an unexplained hole in the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FailureRecord {
    /// The compiler under measurement could not be built. No workload ran.
    Build {
        /// Human-readable description of the build failure.
        detail: String,
    },
    /// A measured compilation exited abnormally.
    WorkloadCrashed {
        /// The workload whose compilation crashed.
        workload: String,
        /// Which sample of that workload crashed.
        sample_index: u32,
        /// Human-readable description, typically exit status and stderr tail.
        detail: String,
    },
    /// The runner refused to record a compilation that ran.
    ValidationRejected {
        /// The workload that was rejected.
        workload: String,
        /// Why the runner refused it.
        detail: String,
    },
    /// A sample's phase accounting did not sum to compiler-root time.
    ///
    /// This is an instrumentation bug, not a discarded measurement. The sample
    /// stays in the run object; only its contribution to statistics is removed.
    PhaseInvariant {
        /// The workload whose sample violated the invariant.
        workload: String,
        /// Which sample violated it.
        sample_index: u32,
        /// The compiler-root total the bands should have summed to.
        compiler_root_ns: u64,
        /// What the bands actually summed to.
        attributed_ns: u64,
    },
    /// A measured compilation exceeded its time limit and was stopped.
    Timeout {
        /// The workload that timed out.
        workload: String,
        /// Which sample timed out.
        sample_index: u32,
        /// The limit that was exceeded, in nanoseconds.
        limit_ns: u64,
    },
}

impl FailureRecord {
    /// The workload this failure concerns, if it concerns one.
    ///
    /// A [`FailureRecord::Build`] failure is run-level and names no workload.
    pub fn workload(&self) -> Option<&str> {
        match self {
            FailureRecord::Build { .. } => None,
            FailureRecord::WorkloadCrashed { workload, .. }
            | FailureRecord::ValidationRejected { workload, .. }
            | FailureRecord::PhaseInvariant { workload, .. }
            | FailureRecord::Timeout { workload, .. } => Some(workload),
        }
    }
}

/// The complete compiler invocation a platform epoch pins.
///
/// "Complete" is the point: optimization level, linker mode, feature flags, and
/// every other behavior-affecting argument. Two runs whose invocations differ
/// are not measuring the same thing, and validation refuses to put them in one
/// series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    /// The compilation target triple.
    pub target: String,
    /// Every behavior-affecting compiler argument, in order.
    pub args: Vec<String>,
}

/// The content identity a run claims to have measured.
///
/// The runner resolves these from the tree it actually measured; validation
/// compares them against the epoch's declaration. A workload edit changes its
/// source hash, fails that comparison, and therefore requires a new suite
/// revision — a workload can never silently reset its own baseline while the
/// headline continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPins {
    /// Content hash of the toolchain as built for this target.
    pub toolchain_hash: String,
    /// Content hash of the standard library at the measured revision.
    ///
    /// Recorded, not pinned. `std` is part of the product under measurement, so
    /// a change here moves the series rather than invalidating it; validation
    /// does not compare this against the epoch, and the dashboard annotates the
    /// point where it changed. It stays in the run object because the
    /// observation is worth keeping even though nothing gates on it.
    pub stdlib_hash: String,
    /// Content hash of each workload's full source closure, keyed by workload.
    pub workload_source_hashes: BTreeMap<String, String>,
    /// The compiler invocation used for every workload in this run.
    pub invocation: Invocation,
}

/// What the measurement environment actually was.
///
/// An epoch pins the environment *policy*, not the exact machine. Hosted
/// runners change underneath a policy, so every run records what it found. A
/// fingerprint change within an epoch produces a structural environment
/// annotation, and comparisons crossing it are rendered as advisory rather than
/// used to declare a regression.
///
/// This is the honest statement for hosted hardware: points in a series satisfy
/// the epoch's declared environment policy, and exact environment changes are
/// recorded rather than prevented. Moving to controlled hardware would allow
/// tightening a per-epoch policy to exact-fingerprint pinning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentFingerprint {
    /// Runner environment class, for example `github-hosted`.
    pub runner_label: String,
    /// Runner image label, for example `ubuntu-24.04`.
    pub runner_image: String,
    /// Runner image version as the hosted runner exposes it, for example
    /// `ubuntu24/20250720.1.0`.
    pub runner_image_version: String,
    /// CPU model string as reported by the host.
    pub cpu_model: String,
    /// Number of logical cores visible to the runner.
    pub core_count: u32,
    /// Total system memory in bytes.
    pub memory_bytes: u64,
    /// Kernel version string.
    pub kernel_version: String,
    /// Operating-system version string.
    pub os_version: String,
    /// Machine architecture, for example `x86_64`.
    pub architecture: String,
}

impl EnvironmentFingerprint {
    /// A stable identifier for this exact environment.
    ///
    /// Two runs with equal fingerprint identifiers ran somewhere
    /// indistinguishable at this resolution. An inequality within an epoch is
    /// what triggers a derived environment annotation.
    pub fn fingerprint_id(&self) -> Result<String, crate::CanonicalError> {
        crate::content_address(self)
    }
}

/// Who, what, where, and when — everything but the measurements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunIdentity {
    /// The suite revision whose logical contract this run implements.
    pub suite_revision: u32,
    /// The platform epoch this run belongs to.
    pub epoch: u32,
    /// The platform the epoch describes, for example `x86_64-linux`.
    pub platform: String,
    /// The compiler revision measured.
    ///
    /// The compiler revision is a *point within* a series, never part of the
    /// series' identity.
    pub commit: String,
    /// When collection started, RFC 3339 in UTC.
    pub started_at: String,
    /// When collection finished, RFC 3339 in UTC.
    pub finished_at: String,
    /// The content identity this run claims to have measured.
    pub pins: ResolvedPins,
    /// The environment this run actually found.
    pub environment: EnvironmentFingerprint,
}

/// One immutable raw observation record.
///
/// Run objects are content-addressed by [`crate::content_address`] and never
/// rewritten. Everything derived from them — indexes, dispersion, chart data,
/// summaries — is rebuilt by consumers and never stored alongside them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunObject {
    /// The schema version this object was written under.
    pub schema_version: u32,
    /// Identity and configuration of the run.
    pub identity: RunIdentity,
    /// Per-workload raw samples, sorted by workload identifier.
    pub workloads: Vec<WorkloadObservation>,
    /// Structured evidence of everything that failed.
    pub failures: Vec<FailureRecord>,
}

impl RunObject {
    /// Whether this object's schema version is the one this crate implements.
    pub fn is_current_schema(&self) -> bool {
        self.schema_version == RUN_SCHEMA_VERSION
    }

    /// The observation for one workload, if the run recorded it.
    pub fn observation(&self, workload: &str) -> Option<&WorkloadObservation> {
        self.workloads
            .iter()
            .find(|observation| observation.workload == workload)
    }

    /// This run's immutable name: the content address of its canonical form.
    pub fn content_address(&self) -> Result<String, crate::CanonicalError> {
        crate::content_address(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounting(
        root_ns: u64,
        parse_ns: u64,
        mixed_ns: u64,
        unattributed_ns: u64,
    ) -> PhaseAccounting {
        let mut phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        phase_ns.insert(Phase::SourceDiscoveryAndParsing, parse_ns);
        PhaseAccounting {
            phase_ns,
            mixed_parallel_ns: mixed_ns,
            unattributed_ns,
            compiler_root_ns: root_ns,
        }
    }

    #[test]
    fn the_invariant_holds_when_the_bands_partition_root_time() {
        let accounting = accounting(100, 60, 30, 10);
        assert_eq!(accounting.attributed_ns(), 100);
        assert!(accounting.holds());
    }

    #[test]
    fn the_invariant_is_exact_rather_than_approximate() {
        // One nanosecond of slack is a real instrumentation bug, not rounding.
        assert!(!accounting(100, 60, 30, 9).holds());
        assert!(!accounting(100, 60, 30, 11).holds());
    }

    #[test]
    fn every_published_phase_participates_in_the_sum() {
        for phase in Phase::ALL {
            let mut accounting = accounting(0, 0, 0, 0);
            accounting.phase_ns.insert(phase, 42);
            accounting.compiler_root_ns = 42;
            assert!(accounting.holds(), "{} is not counted", phase.wire_name());
        }
    }

    #[test]
    fn a_corrupt_record_reports_a_violation_rather_than_overflowing() {
        let mut accounting = accounting(1, 0, 0, 0);
        accounting.phase_ns.insert(Phase::Backend, u64::MAX);
        accounting.mixed_parallel_ns = u64::MAX;
        assert!(!accounting.holds());
    }

    #[test]
    fn an_absent_phase_is_distinguishable_from_a_phase_that_took_no_time() {
        let mut accounting = accounting(0, 0, 0, 0);
        assert!(accounting.missing_phases().is_empty());
        accounting.phase_ns.remove(&Phase::Linking);
        assert_eq!(accounting.missing_phases(), vec![Phase::Linking]);
        // It still reads as zero when summed, which is exactly why absence
        // needs its own check rather than being inferred from the total.
        assert_eq!(accounting.band_ns(Band::Phase(Phase::Linking)), 0);
    }

    #[test]
    fn bands_cover_the_phases_plus_both_structural_buckets() {
        let bands = Band::all();
        assert_eq!(bands.len(), Phase::ALL.len() + 2);
        assert!(bands.contains(&Band::MixedParallel));
        assert!(bands.contains(&Band::Unattributed));
    }

    #[test]
    fn band_wire_names_are_distinct() {
        let mut names: Vec<&str> = Band::all().into_iter().map(Band::wire_name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Phase::ALL.len() + 2);
    }

    #[test]
    fn wire_names_round_trip_through_from_wire_name() {
        for phase in Phase::ALL {
            assert_eq!(Phase::from_wire_name(phase.wire_name()), Some(phase));
        }
        assert_eq!(Phase::from_wire_name("semantic"), None);
        assert_eq!(Phase::from_wire_name(""), None);
        // The structural buckets are bands, not phases, and must not parse as
        // one: a producer marking a span `mixed_parallel` is a bug, not a
        // declaration.
        assert_eq!(Phase::from_wire_name("mixed_parallel"), None);
        assert_eq!(Phase::from_wire_name("unattributed"), None);
    }

    #[test]
    fn phase_indexes_are_unique_and_dense() {
        let mut indexes: Vec<usize> = Phase::ALL.into_iter().map(Phase::index).collect();
        indexes.sort_unstable();
        assert_eq!(indexes, (0..Phase::ALL.len()).collect::<Vec<_>>());
    }

    #[test]
    fn phase_wire_names_match_their_serialized_form() {
        // Chart data keys and run-object map keys must be the same strings, or
        // the dashboard silently renders empty bands.
        for phase in Phase::ALL {
            let serialized = serde_json::to_string(&phase).unwrap();
            assert_eq!(serialized, format!("\"{}\"", phase.wire_name()));
        }
    }

    #[test]
    fn driver_overhead_is_process_time_outside_compiler_root() {
        let sample = Sample {
            batch_size: 1,
            process_elapsed_ns: 150,
            peak_memory_bytes: 1,
            output_binary_bytes: 1,
            phases: accounting(100, 100, 0, 0),
            boundary_evidence: Vec::new(),
        };
        assert_eq!(sample.driver_overhead_ns(), Some(50));
    }

    #[test]
    fn a_process_shorter_than_its_compiler_root_has_no_overhead_to_report() {
        let sample = Sample {
            batch_size: 1,
            process_elapsed_ns: 50,
            peak_memory_bytes: 1,
            output_binary_bytes: 1,
            phases: accounting(100, 100, 0, 0),
            boundary_evidence: Vec::new(),
        };
        assert_eq!(sample.driver_overhead_ns(), None);
    }

    #[test]
    fn a_run_object_round_trips_through_json() {
        let run = crate::validate::tests::sample_run();
        let text = serde_json::to_string(&run).unwrap();
        let parsed: RunObject = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, run);
    }

    #[test]
    fn a_run_objects_content_address_is_stable_across_serializations() {
        let run = crate::validate::tests::sample_run();
        let first = run.content_address().unwrap();
        let reparsed: RunObject =
            serde_json::from_str(&serde_json::to_string(&run).unwrap()).unwrap();
        assert_eq!(reparsed.content_address().unwrap(), first);
    }

    #[test]
    fn changing_any_measurement_changes_the_content_address() {
        let run = crate::validate::tests::sample_run();
        let mut altered = run.clone();
        altered.workloads[0].samples[0].process_elapsed_ns += 1;
        assert_ne!(
            run.content_address().unwrap(),
            altered.content_address().unwrap()
        );
    }

    #[test]
    fn unknown_fields_are_refused_rather_than_ignored() {
        let mut value = serde_json::to_value(crate::validate::tests::sample_run()).unwrap();
        value["surprise"] = serde_json::json!(1);
        let parsed: Result<RunObject, _> = serde_json::from_value(value);
        assert!(parsed.is_err());
    }

    #[test]
    fn a_failure_record_knows_whether_it_names_a_workload() {
        assert_eq!(
            FailureRecord::Build {
                detail: "no compiler".to_string()
            }
            .workload(),
            None
        );
        assert_eq!(
            FailureRecord::Timeout {
                workload: "caldera".to_string(),
                sample_index: 2,
                limit_ns: 5,
            }
            .workload(),
            Some("caldera")
        );
    }

    #[test]
    fn environment_fingerprints_differ_exactly_when_the_environment_does() {
        let run = crate::validate::tests::sample_run();
        let base = run.identity.environment.fingerprint_id().unwrap();
        let mut drifted = run.identity.environment.clone();
        drifted.runner_image_version = "ubuntu24/20990101.9.0".to_string();
        assert_ne!(drifted.fingerprint_id().unwrap(), base);
        assert_eq!(run.identity.environment.fingerprint_id().unwrap(), base);
    }
}
