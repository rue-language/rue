//! Runtime performance of compiled Rue programs (ADR-0072, Phase 1).
//!
//! ADR-0067 answers "how fast does Rue compile". This module answers the other
//! half: how fast the program the compiler produced actually runs. It reuses
//! that ADR's suite/epoch versioning, its raw-storage rule, and ADR-0071's
//! fresh-process boundary discipline, and departs from it in exactly three
//! places, each deliberate.
//!
//! **A third input category.** The compile-time suites pin every input and
//! refuse a run whose pinned components moved. A runtime workload also consumes
//! *data*, and the anchor workload's data is expected to move — gazette builds
//! the live rue-lang.dev corpus. So the schema names a second category
//! explicitly: an [`InputCategory::Recorded`] input's identity is captured with
//! every observation ([`RecordedInput`]) instead of failing validation when it
//! changes. Nothing here changes ADR-0067's rules for the compile-time suites.
//! `wordfreq`'s text fixture is generated deterministically from a pinned seed
//! and generator revision, and is *still* a recorded input: the record carries
//! the digest of the bytes the program actually read, so no future generator
//! change can move a series invisibly.
//!
//! **The oracle is not optional.** A run whose program printed the wrong answer
//! is not a slow run or a fast run; it is not a measurement. [`OracleOutcome`]
//! rides every observation and a mismatch makes the report unappendable
//! regardless of any timing it contains. The comparison happens outside the
//! timed window, as does fixture preparation — [`RuntimeRegime`] records both
//! facts rather than leaving them to a reader's assumption.
//!
//! **Flags are advisory until the workload is calibrated.** Dispersion is a
//! property of a workload on a platform, so runtime calibration is never
//! inherited from the compiler suites; an epoch that has not yet calibrated a
//! workload reports [`FlagPosture::Advisory`] for it.
//!
//! Storage stays raw: integer nanoseconds and integer bytes, no medians, no
//! ratios. Everything derived is recomputed by consumers from these records.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::boundary::OptimizationLevel;
use crate::manifest::EnvironmentPolicy;
use crate::run::EnvironmentFingerprint;
use crate::sanity::{
    is_commit, is_measurable_duration, is_ordered_interval, is_sha256_digest, is_utc_timestamp,
    samples_beyond_policy,
};

/// Wire version of [`RuntimeReport`].
///
/// Readers refuse versions they do not implement rather than guessing, exactly
/// as [`crate::RUN_SCHEMA_VERSION`] does for compile-time records.
pub const RUNTIME_REPORT_SCHEMA_VERSION: u32 = 1;

/// The discriminator every runtime record carries.
///
/// The durable store holds more than one kind of record and a reader must be
/// able to tell them apart from the bytes alone, without inferring a kind from
/// which fields happen to parse.
pub const RUNTIME_RECORD_KIND: &str = "runtime_v1";

/// Manifest syntax version understood by [`RuntimeManifest::parse`].
pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// What the measured window contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeBoundary {
    /// One fresh process, timed from immediately before spawn to exit.
    ///
    /// Fixture preparation, compilation, and oracle comparison are all outside
    /// it. In-process iteration timing is deliberately not offered in v1.
    SpawnToExitV1,
}

/// How an input's identity is treated by validation.
///
/// The distinction is the whole of ADR-0072 Decision 2, and it is spelled in
/// the schema rather than in prose so that no record can be ambiguous about
/// which discipline governed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCategory {
    /// Declared in the manifest and required to match; a change is a failure
    /// until a maintainer declares the next suite revision. ADR-0067's rule for
    /// every compile-time input.
    Pinned,
    /// Expected to move. Its identity is captured with every observation, and a
    /// change is a discontinuity in the series rather than an invalid run.
    Recorded,
}

/// Thread policy under which a runtime observation was taken.
///
/// Part of the epoch, not the suite: it is a property of how a platform runs
/// the workload. Rue has no concurrency support yet, so v1 measures
/// single-threaded and says so, rather than leaving a future reader to assume
/// it from the absence of a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadPolicy {
    /// One worker thread.
    SingleThreaded,
}

/// Whether hardware performance counters were collected.
///
/// GitHub-hosted runners expose no PMU, so v1 records their absence as a fact
/// of the regime. Counters are gated on a future controlled-hardware epoch
/// (ADR-0072 Decision 5) rather than silently missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareCounterPolicy {
    /// No PMU is available in this regime; only wall time, RSS, and size.
    UnavailableOnHostedRunner,
}

/// The correctness oracle a workload is judged against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleKind {
    /// The program's stdout must equal a committed golden file, byte for byte.
    GoldenStdout,
}

/// What the oracle said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleVerdict {
    /// Every sample produced exactly the expected output.
    Match,
    /// Output was produced and is wrong.
    Mismatch,
    /// The comparison could not be performed — the golden was unreadable, or
    /// no sample produced output. Not the same as a mismatch, and not an
    /// excuse to publish either.
    Indeterminate,
}

/// How a workload's regression flags should be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagPosture {
    /// The epoch has calibrated this workload's dispersion from its own
    /// repeated samples on this platform.
    Calibrated,
    /// It has not. Movement is worth a look and is never a gate.
    Advisory,
}

/// Metrics a runtime series publishes.
///
/// Deliberately not [`crate::Metric`]: that enum's `Latency` means
/// compiler-root time, which has no meaning for a program the compiler is not
/// running. Sharing the type would invite a consumer to plot one as the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMetric {
    /// Whole-process wall time, spawn to exit.
    WallClock,
    /// Peak resident set size of the measured process.
    PeakMemory,
    /// Size of the release-quality executable that was run.
    BinarySize,
}

impl RuntimeMetric {
    /// Every published runtime metric.
    pub const ALL: [RuntimeMetric; 3] = [
        RuntimeMetric::WallClock,
        RuntimeMetric::PeakMemory,
        RuntimeMetric::BinarySize,
    ];

    /// The stable wire name used in derived chart data.
    pub const fn wire_name(self) -> &'static str {
        match self {
            RuntimeMetric::WallClock => "wall_clock",
            RuntimeMetric::PeakMemory => "peak_memory",
            RuntimeMetric::BinarySize => "binary_size",
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// How one workload's data fixture is produced.
///
/// The declaration is pinned by the suite revision; the bytes it produces are
/// recorded per observation. Both halves matter: the pin makes the fixture
/// reproducible from the repository, and the recording makes any drift between
/// the pin and the bytes visible in the data rather than only in review.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureDeclaration {
    /// Which discipline governs this input's identity. Runtime fixtures are
    /// recorded; the field exists so a future pinned fixture cannot be added
    /// without saying so.
    pub category: InputCategory,
    /// Name of the checked-in generator that produces the fixture.
    pub generator: String,
    /// Revision of that generator. Bumped whenever its output changes, which
    /// invalidates any committed golden derived from it.
    pub generator_revision: u32,
    /// Seed handed to the generator.
    pub seed: u64,
    /// Exact size of the produced fixture, in bytes.
    pub bytes: u64,
    /// Number of distinct tokens the generator's vocabulary contains.
    pub vocabulary_size: u32,
    /// File name the fixture is written under inside the work directory.
    pub file_name: String,
    /// What this fixture is, for a reader of the manifest.
    pub description: String,
}

/// The committed expected result a workload's output is judged against.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleDeclaration {
    /// How the comparison is performed.
    pub kind: OracleKind,
    /// Repository-relative path to the expected output.
    pub path: String,
}

/// One measured program.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeWorkload {
    /// Stable short name.
    pub id: String,
    /// Root source, relative to the repository.
    pub source: String,
    /// The question this workload exists to answer. Required, for the same
    /// reason ADR-0067 requires it: a probe that cannot name its question is
    /// corpus mass rather than a measurement.
    pub question: String,
    /// Arguments passed to the compiled program, in order.
    ///
    /// [`FIXTURE_ARGUMENT`] stands for the prepared fixture's path. The
    /// placeholder rather than the resolved path is what the record stores: the
    /// resolved path names a temporary directory and would make two identical
    /// measurements differ.
    pub program_args: Vec<String>,
    /// How the workload's data is produced.
    pub fixture: FixtureDeclaration,
    /// How its output is judged.
    pub oracle: OracleDeclaration,
}

/// The token in `program_args` replaced by the prepared fixture's path.
pub const FIXTURE_ARGUMENT: &str = "{fixture}";

/// The platform-independent contract: which programs are measured, with which
/// arguments, over which fixtures, judged how.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSuiteRevision {
    /// The revision number. Monotonic; never reused.
    pub revision: u32,
    /// The runner protocol version this revision's records conform to.
    pub protocol_version: u32,
    /// What the measured window contains.
    pub measured_boundary: RuntimeBoundary,
    /// The suite's workloads.
    pub workloads: Vec<RuntimeWorkload>,
}

impl RuntimeSuiteRevision {
    /// The workload with this identifier, if the suite declares one.
    pub fn workload(&self, id: &str) -> Option<&RuntimeWorkload> {
        self.workloads.iter().find(|workload| workload.id == id)
    }

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
}

/// How many independent fresh processes to launch for one workload.
///
/// No batching factor, unlike ADR-0067's sampling policy. These workloads run
/// for seconds; timer resolution is not a threat, and batching would hide the
/// per-run dispersion the calibration this epoch lacks is meant to measure.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSamplingPolicy {
    /// How many samples to collect.
    pub samples: u32,
}

/// A workload's calibrated flagging rule on one platform.
///
/// Present only once the workload's dispersion has been measured *here*, from
/// its own repeated samples. Compiler-workload calibration is never a
/// substitute: dispersion is a property of the workload.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCalibration {
    /// Multiplier applied to pooled uncertainty.
    pub k: f64,
    /// How many prior observations form the trailing window.
    pub window: u32,
    /// The reviewed analysis that established these constants.
    pub reference: String,
}

/// Everything that can vary by platform.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEpoch {
    /// The epoch number, unique within its platform. Monotonic; never reused.
    pub id: u32,
    /// The platform this epoch measures, for example `x86_64-linux`.
    pub platform: String,
    /// The suite revision this epoch implements.
    pub suite_revision: u32,
    /// The compilation target triple the workload is built for.
    pub target: String,
    /// Every behavior-affecting compiler argument used to build the workload.
    pub compiler_args: Vec<String>,
    /// Optimization contract of the program under measurement.
    ///
    /// ADR-0071 defines the product as release-quality, so a runtime series
    /// measuring anything else would not be measuring the product.
    pub optimization: OptimizationLevel,
    /// Thread policy in force for every sample.
    pub thread_policy: ThreadPolicy,
    /// Whether hardware counters are collected in this regime.
    pub hardware_counters: HardwareCounterPolicy,
    /// The environment class this epoch requires.
    pub environment: EnvironmentPolicy,
    /// Sampling policy per workload.
    pub sampling: BTreeMap<String, RuntimeSamplingPolicy>,
    /// Per-workload calibrated flagging rules, where they exist yet.
    #[serde(default)]
    pub calibration: BTreeMap<String, RuntimeCalibration>,
    /// Whether scheduled collection measures this epoch.
    #[serde(default)]
    pub collection: bool,
}

impl RuntimeEpoch {
    /// How a workload's movement should be read in this epoch.
    pub fn flag_posture(&self, workload: &str) -> FlagPosture {
        if self.calibration.contains_key(workload) {
            FlagPosture::Calibrated
        } else {
            FlagPosture::Advisory
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimeManifest {
    schema_version: u32,
    #[serde(default)]
    suite: Vec<RuntimeSuiteRevision>,
    #[serde(default)]
    epoch: Vec<RuntimeEpoch>,
}

/// The runtime suite declaration: `performance/runtime.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeManifest {
    suites: BTreeMap<u32, RuntimeSuiteRevision>,
    epochs: Vec<RuntimeEpoch>,
}

impl RuntimeManifest {
    /// Parse and check a runtime manifest.
    ///
    /// Every structural problem is a parse failure rather than a silently
    /// tolerated oddity: a manifest is the thing that makes an invalid
    /// observation unappendable, so it may not itself be ambiguous.
    pub fn parse(text: &str) -> Result<RuntimeManifest, String> {
        let raw: RawRuntimeManifest =
            toml::from_str(text).map_err(|error| format!("malformed runtime manifest: {error}"))?;
        if raw.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "unsupported runtime manifest schema version {}",
                raw.schema_version
            ));
        }

        let mut suites: BTreeMap<u32, RuntimeSuiteRevision> = BTreeMap::new();
        for suite in raw.suite {
            if suite.workloads.is_empty() {
                return Err(format!(
                    "runtime suite revision {} declares no workloads",
                    suite.revision
                ));
            }
            let mut seen = BTreeSet::new();
            for workload in &suite.workloads {
                if workload.id.trim().is_empty() {
                    return Err(format!(
                        "runtime suite revision {} declares a workload with an empty id",
                        suite.revision
                    ));
                }
                if !seen.insert(workload.id.clone()) {
                    return Err(format!(
                        "runtime suite revision {} declares workload {:?} more than once",
                        suite.revision, workload.id
                    ));
                }
                if workload.question.trim().is_empty() {
                    return Err(format!(
                        "runtime workload {:?} names no question it answers",
                        workload.id
                    ));
                }
                if workload.source.is_empty() || workload.source.starts_with('/') {
                    return Err(format!(
                        "runtime workload {:?} must use a repository-relative source",
                        workload.id
                    ));
                }
                if workload.oracle.path.is_empty() || workload.oracle.path.starts_with('/') {
                    return Err(format!(
                        "runtime workload {:?} must use a repository-relative oracle path",
                        workload.id
                    ));
                }
                if workload.fixture.bytes == 0 {
                    return Err(format!(
                        "runtime workload {:?} declares an empty fixture",
                        workload.id
                    ));
                }
                if workload.fixture.vocabulary_size == 0 {
                    return Err(format!(
                        "runtime workload {:?} declares an empty fixture vocabulary",
                        workload.id
                    ));
                }
                if workload.fixture.file_name.contains('/') {
                    return Err(format!(
                        "runtime workload {:?} fixture file name must be a bare name",
                        workload.id
                    ));
                }
                // Wrong output must fail regardless of speed, so a workload
                // without a resolvable oracle is not measurable at all.
                if !workload
                    .program_args
                    .iter()
                    .any(|argument| argument == FIXTURE_ARGUMENT)
                {
                    return Err(format!(
                        "runtime workload {:?} never passes its fixture to the program; \
                         one argument must be {FIXTURE_ARGUMENT:?}",
                        workload.id
                    ));
                }
            }
            if suites.insert(suite.revision, suite.clone()).is_some() {
                return Err(format!(
                    "runtime suite revision {} is declared more than once",
                    suite.revision
                ));
            }
        }

        let manifest = RuntimeManifest {
            suites,
            epochs: raw.epoch,
        };
        manifest.check_epochs()?;
        Ok(manifest)
    }

    fn check_epochs(&self) -> Result<(), String> {
        let mut seen: BTreeSet<(&str, u32)> = BTreeSet::new();
        let mut collecting: BTreeMap<&str, u32> = BTreeMap::new();
        for epoch in &self.epochs {
            if !seen.insert((epoch.platform.as_str(), epoch.id)) {
                return Err(format!(
                    "runtime epoch {} on {} is declared more than once",
                    epoch.id, epoch.platform
                ));
            }
            if epoch.collection
                && let Some(first) = collecting.insert(epoch.platform.as_str(), epoch.id)
            {
                return Err(format!(
                    "platform {} marks both runtime epoch {first} and {} for collection",
                    epoch.platform, epoch.id
                ));
            }
            let Some(suite) = self.suites.get(&epoch.suite_revision) else {
                return Err(format!(
                    "runtime epoch {} on {} implements undeclared suite revision {}",
                    epoch.id, epoch.platform, epoch.suite_revision
                ));
            };
            if epoch.optimization != OptimizationLevel::O3 {
                return Err(format!(
                    "runtime epoch {} on {} must measure the release-quality product (-O3)",
                    epoch.id, epoch.platform
                ));
            }

            let declared: BTreeSet<&str> = suite.workload_ids().into_iter().collect();
            let sampled: BTreeSet<&str> = epoch.sampling.keys().map(|key| key.as_str()).collect();
            if declared != sampled {
                let missing: Vec<&&str> = declared.difference(&sampled).collect();
                let unexpected: Vec<&&str> = sampled.difference(&declared).collect();
                return Err(format!(
                    "runtime epoch {} on {} declares sampling policies that do not match its \
                     suite; missing {missing:?}, unexpected {unexpected:?}",
                    epoch.id, epoch.platform
                ));
            }
            for (workload, policy) in &epoch.sampling {
                // Spread is the whole point of the report; one sample has none.
                if policy.samples < 2 {
                    return Err(format!(
                        "runtime epoch {} on {} asks for {} sample(s) of {workload:?}; \
                         a median and spread need at least two",
                        epoch.id, epoch.platform, policy.samples
                    ));
                }
            }
            for workload in epoch.calibration.keys() {
                if !declared.contains(workload.as_str()) {
                    return Err(format!(
                        "runtime epoch {} on {} calibrates undeclared workload {workload:?}",
                        epoch.id, epoch.platform
                    ));
                }
            }
        }
        Ok(())
    }

    /// The suite revision with this number, if declared.
    pub fn suite(&self, revision: u32) -> Option<&RuntimeSuiteRevision> {
        self.suites.get(&revision)
    }

    /// Every declared suite revision, ascending.
    pub fn suites(&self) -> impl Iterator<Item = &RuntimeSuiteRevision> {
        self.suites.values()
    }

    /// The epoch with this identifier on this platform, if declared.
    pub fn epoch(&self, platform: &str, id: u32) -> Option<&RuntimeEpoch> {
        self.epochs
            .iter()
            .find(|epoch| epoch.platform == platform && epoch.id == id)
    }

    /// Every declared epoch, in manifest order.
    pub fn epochs(&self) -> &[RuntimeEpoch] {
        &self.epochs
    }

    /// The epoch scheduled collection measures on this platform.
    pub fn collection_epoch(&self, platform: &str) -> Option<&RuntimeEpoch> {
        self.epochs
            .iter()
            .find(|epoch| epoch.platform == platform && epoch.collection)
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// How a generated recorded input was produced.
///
/// Absent for inputs that are collected rather than generated — a content tree,
/// for instance — which is why it is optional rather than part of
/// [`RecordedInput`] itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedProvenance {
    /// Name of the checked-in generator.
    pub generator: String,
    /// Its revision at the time of generation.
    pub generator_revision: u32,
    /// The seed it was given.
    pub seed: u64,
    /// The vocabulary size it was given.
    ///
    /// Recorded alongside the seed because it is equally an input to the bytes,
    /// and therefore to the golden output judged against them. Without it a
    /// record could not describe the fixture it read, and two series segments
    /// generated under different vocabularies would look identical in the data.
    pub vocabulary_size: u32,
}

/// The identity of one input the measured program consumed.
///
/// This is ADR-0072's recorded-input category made concrete. The digest is over
/// the bytes the program actually read, not over the declaration that asked for
/// them, so a generator whose output drifted from its declaration is visible in
/// the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedInput {
    /// Short name of the input within its workload, for example `fixture`.
    pub name: String,
    /// Which discipline governs this input's identity.
    pub category: InputCategory,
    /// What the input is.
    pub description: String,
    /// Digest over the input's contents.
    pub identity_sha256: String,
    /// How many files it comprises.
    pub files: u64,
    /// Its total size in bytes.
    pub bytes: u64,
    /// How it was produced, when it was generated rather than collected.
    #[serde(default)]
    pub provenance: Option<GeneratedProvenance>,
}

/// The executable that was measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramIdentity {
    /// Size of the release-quality executable, in bytes.
    pub binary_bytes: u64,
    /// Digest of that executable.
    pub sha256: String,
}

/// What the correctness oracle said about one workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleOutcome {
    /// How the comparison was performed.
    pub kind: OracleKind,
    /// Repository-relative path of the expected output.
    pub reference: String,
    /// Digest of the expected output.
    pub reference_sha256: String,
    /// Digest of the output the program produced.
    pub observed_sha256: String,
    /// The verdict.
    pub verdict: OracleVerdict,
    /// Whether every sample produced byte-identical output.
    ///
    /// Separate from the verdict: a program that agrees with the golden on some
    /// runs and not others is wrong in a different and more alarming way than
    /// one that disagrees consistently.
    pub deterministic_across_samples: bool,
    /// Human-readable evidence, empty on a match.
    pub detail: String,
}

/// One independent fresh-process measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSample {
    /// Whole-process wall time from immediately before spawn through exit.
    pub process_elapsed_ns: u64,
    /// Peak resident set size of the measured process, in bytes.
    pub peak_memory_bytes: u64,
    /// The program's exit code.
    pub exit_code: i32,
    /// How many bytes it wrote to stdout.
    pub stdout_bytes: u64,
    /// Digest of those bytes, so a nondeterministic run is provable from the
    /// record rather than only from the runner's summary.
    pub stdout_sha256: String,
}

/// Raw measurements of one program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeObservation {
    /// Stable workload id.
    pub workload: String,
    /// Root source, recorded for human inspection.
    pub source: String,
    /// Why this program is measured.
    pub question: String,
    /// Declared program arguments, with [`FIXTURE_ARGUMENT`] unresolved.
    pub program_args: Vec<String>,
    /// Identity of every input the program consumed.
    pub recorded_inputs: Vec<RecordedInput>,
    /// The executable that was run.
    pub program: ProgramIdentity,
    /// What the oracle said, computed outside the timed window.
    pub oracle: OracleOutcome,
    /// Independent raw fresh-process measurements.
    pub samples: Vec<RuntimeSample>,
}

/// Identity of one runtime report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    /// Runtime suite revision implemented.
    pub suite_revision: u32,
    /// Platform epoch measured.
    pub epoch: u32,
    /// The platform, for example `x86_64-linux`.
    pub platform: String,
    /// Compiler source revision that built the measured programs.
    ///
    /// Recorded from day one: without it a runtime series is a collection of
    /// numbers with no way to attribute a movement to a compiler change.
    pub commit: String,
    /// The compiler's own version string.
    pub compiler_version: String,
    /// UTC start timestamp.
    pub started_at: String,
    /// UTC completion timestamp.
    pub finished_at: String,
    /// Content hash of the Rust toolchain the compiler was built with.
    pub toolchain_hash: String,
    /// Content hash of the standard library, which is part of the product.
    pub stdlib_hash: String,
    /// Content hash of each workload's own source closure.
    ///
    /// Recorded, not pinned — the same choice `performance/scaling.toml` makes
    /// for the maintained examples it measures. That suite's curve runs over
    /// `examples/ruelex`, `examples/mosaic`, `examples/harbor`, and
    /// `examples/lattice` and pins no per-workload source hashes, because a
    /// maintained example is a program the repository keeps improving rather
    /// than a frozen probe; pinning would stall the series on every ordinary
    /// edit to it. ADR-0072 asks for the same treatment explicitly, saying that
    /// observations *record* the workload's source identity, and requires
    /// gazette to be an ordinary maintained example rather than a
    /// benchmark-only artifact.
    ///
    /// `performance/workloads/lattice/main.rue` is the other pattern and not
    /// the one that applies: ADR-0067 pins that copy precisely because it is a
    /// frozen probe with no life outside measurement.
    ///
    /// Recording is only half a bargain, though. It is defensible because
    /// consumers can see when the identity moved and segment the series on it,
    /// which is why the derived data surfaces this alongside the fixture
    /// digest; a recorded identity nobody can read would be a pin quietly
    /// omitted.
    pub workload_source_hashes: BTreeMap<String, String>,
    /// Machine and runner fingerprint.
    pub environment: EnvironmentFingerprint,
}

/// What every sample in a runtime report means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRegime {
    /// What the measured window contains.
    pub measured_boundary: RuntimeBoundary,
    /// Always `fresh_process`: every sample launches the program anew.
    pub program_state: String,
    /// Always `uncontrolled`: page-cache state is observed, not reset.
    pub os_page_cache: String,
    /// Always false. Generating a fixture is setup, not the workload.
    pub fixture_preparation_measured: bool,
    /// Always false. Judging output is not part of running the program.
    pub oracle_comparison_measured: bool,
    /// Optimization contract of the measured executables.
    pub optimization: OptimizationLevel,
    /// Compiler arguments used to build them.
    pub compiler_args: Vec<String>,
    /// Compilation target.
    pub target: String,
    /// Thread policy in force.
    pub thread_policy: ThreadPolicy,
    /// Whether hardware counters were collected.
    pub hardware_counters: HardwareCounterPolicy,
}

/// Structured evidence of something that went wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeFailure {
    /// The workload could not be built.
    CompileFailed {
        /// The workload.
        workload: String,
        /// What the compiler said.
        detail: String,
    },
    /// The fixture could not be prepared.
    FixturePreparationFailed {
        /// The workload.
        workload: String,
        /// What failed.
        detail: String,
    },
    /// A measured process did not exit successfully.
    ProgramCrashed {
        /// The workload.
        workload: String,
        /// Which sample.
        sample_index: u32,
        /// What happened.
        detail: String,
    },
    /// The program ran and produced the wrong answer.
    WrongOutput {
        /// The workload.
        workload: String,
        /// The evidence.
        detail: String,
    },
    /// The runner rejected something before validation saw it.
    ValidationRejected {
        /// The workload.
        workload: String,
        /// Why.
        detail: String,
    },
}

impl RuntimeFailure {
    /// The workload this failure belongs to.
    pub fn workload(&self) -> &str {
        match self {
            RuntimeFailure::CompileFailed { workload, .. }
            | RuntimeFailure::FixturePreparationFailed { workload, .. }
            | RuntimeFailure::ProgramCrashed { workload, .. }
            | RuntimeFailure::WrongOutput { workload, .. }
            | RuntimeFailure::ValidationRejected { workload, .. } => workload,
        }
    }
}

/// One immutable, raw runtime report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReport {
    /// Always [`RUNTIME_RECORD_KIND`], so a store holding several record kinds
    /// can route on the bytes rather than on which fields happen to parse.
    pub record_kind: String,
    /// Wire version of this record.
    pub schema_version: u32,
    /// Identity and environment of the measurement.
    pub identity: RuntimeIdentity,
    /// Structural measurement regime.
    pub regime: RuntimeRegime,
    /// Raw measurements, sorted by workload id.
    pub workloads: Vec<RuntimeObservation>,
    /// Everything that went wrong, whether or not it stopped the run.
    pub failures: Vec<RuntimeFailure>,
}

impl RuntimeReport {
    /// The content address of this report's canonical form.
    pub fn content_address(&self) -> Result<String, crate::CanonicalError> {
        crate::content_address(self)
    }

    /// The observation for one workload, if the report has one.
    pub fn observation(&self, workload: &str) -> Option<&RuntimeObservation> {
        self.workloads
            .iter()
            .find(|observation| observation.workload == workload)
    }
}

// Runtime reports are named exactly as run objects are; `crate::Stored` owns
// that rule for every record kind in the store.

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Why a runtime report may not enter its series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeValidationError {
    /// The record is not a runtime record, or not a version this build reads.
    UnsupportedRecord {
        /// The kind found.
        record_kind: String,
        /// The schema version found.
        schema_version: u32,
    },
    /// The manifest declares no such epoch.
    UnknownEpoch {
        /// The platform claimed.
        platform: String,
        /// The epoch claimed.
        epoch: u32,
    },
    /// The report's suite revision is not the one its epoch implements.
    SuiteRevisionMismatch {
        /// What the report claims.
        found: u32,
        /// What the epoch declares.
        expected: u32,
    },
    /// A regime field disagrees with the epoch that governs it.
    RegimeMismatch {
        /// The field.
        field: String,
        /// What the report recorded.
        found: String,
        /// What the epoch requires.
        expected: String,
    },
    /// The environment does not satisfy the epoch's policy.
    EnvironmentPolicy {
        /// The policy's required class.
        expected: String,
        /// The fingerprint's class.
        found: String,
    },
    /// A required identity field is missing or malformed.
    MalformedIdentity {
        /// The field.
        field: String,
        /// Why it is malformed.
        detail: String,
    },
    /// The report contains an observation the suite does not declare.
    UndeclaredWorkload {
        /// The offending workload.
        workload: String,
    },
    /// A workload's declared fixture and its recorded input disagree.
    ///
    /// The fixture is a *recorded* input, so its digest is never required to
    /// match a declaration — but the provenance that produced it is pinned, and
    /// a report generated from a different seed or generator revision is
    /// measuring a different workload than the suite declares.
    FixtureProvenanceMismatch {
        /// The workload.
        workload: String,
        /// The disagreeing field.
        field: String,
        /// What the record carries.
        found: String,
        /// What the suite declares.
        expected: String,
    },
    /// A workload has no recorded identity for the input it consumed.
    MissingRecordedInput {
        /// The workload.
        workload: String,
        /// The input that should have been recorded.
        name: String,
    },
    /// The program produced the wrong answer, or the oracle could not judge it.
    ///
    /// Unappendable regardless of any timing in the report: a measurement of a
    /// program computing the wrong result measures nothing anyone wants.
    OracleFailed {
        /// The workload.
        workload: String,
        /// The verdict.
        verdict: OracleVerdict,
        /// The evidence.
        detail: String,
    },
    /// The workload's samples did not all produce the same output.
    NondeterministicOutput {
        /// The workload.
        workload: String,
    },
    /// The oracle names a reference other than the one the suite declares.
    OracleReferenceMismatch {
        /// The workload.
        workload: String,
        /// What the record names.
        found: String,
        /// What the suite declares.
        expected: String,
    },
    /// A workload's program arguments are not the ones it declares.
    ProgramArgumentsMismatch {
        /// The workload.
        workload: String,
        /// What the record carries.
        found: Vec<String>,
        /// What the suite declares.
        expected: Vec<String>,
    },
    /// The producer's summary of its own run disagrees with the raw samples
    /// stored beside it.
    ///
    /// This is the variant that makes the producer/validator separation real
    /// rather than aspirational. `verdict` and `deterministic_across_samples`
    /// are written by the runner; `samples[].stdout_sha256` is the evidence.
    /// A validator that read only the summary would accept exactly the records
    /// a broken or dishonest producer emits, in a store that cannot delete
    /// them.
    OracleContradictsSamples {
        /// The workload.
        workload: String,
        /// The summary field the evidence refutes.
        field: String,
        /// What the evidence shows.
        detail: String,
    },
    /// A recorded value is not in the one spelling records use.
    MalformedDigest {
        /// The workload.
        workload: String,
        /// Which digest.
        field: String,
        /// What was found.
        found: String,
    },
    /// The observation carries more samples than its epoch permits.
    ///
    /// A protocol violation rather than a measurement problem, exactly as in
    /// ADR-0067: a run that took more samples was not taken under the policy
    /// the series compares against, whatever the extra samples say.
    TooManySamples {
        /// The workload.
        workload: String,
        /// What the epoch permits.
        allowed: u32,
        /// What the record carries.
        actual: u32,
    },
    /// The measured executable cannot have been the one that ran.
    ImpossibleProgramIdentity {
        /// The workload.
        workload: String,
        /// Why.
        detail: String,
    },
}

/// Why a stored runtime sample may not contribute to a statistic.
///
/// The runtime counterpart of [`crate::InvalidSampleReason`], and tiered the
/// same way: an invalid sample is a *measurement* failure, so the report stays
/// appendable and keeps its evidence while the sample is excluded and its
/// workload publishes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeInvalidSampleReason {
    /// The process was measured as taking zero nanoseconds.
    ///
    /// A monotonic clock read either side of a process that genuinely ran
    /// cannot produce this, so it is evidence the measurement did not happen —
    /// and it is exactly the value that would drag a median down while looking
    /// like a spectacular result.
    ZeroElapsed,
    /// The process did not exit successfully.
    NonZeroExit {
        /// The exit code, or the negated signal that killed it.
        exit_code: i32,
    },
}

impl std::fmt::Display for RuntimeInvalidSampleReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeInvalidSampleReason::ZeroElapsed => {
                write!(f, "the measured process elapsed zero ns")
            }
            RuntimeInvalidSampleReason::NonZeroExit { exit_code } => {
                write!(f, "the program exited with code {exit_code}")
            }
        }
    }
}

/// A stored runtime sample excluded from every derived statistic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInvalidSample {
    /// The workload holding the sample.
    pub workload: String,
    /// Which sample, by position in the workload's sample list.
    pub sample_index: u32,
    /// Why it is excluded.
    pub reason: RuntimeInvalidSampleReason,
}

impl std::fmt::Display for RuntimeValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeValidationError::UnsupportedRecord {
                record_kind,
                schema_version,
            } => write!(
                f,
                "record {record_kind:?} version {schema_version} is not a \
                 {RUNTIME_RECORD_KIND} version {RUNTIME_REPORT_SCHEMA_VERSION} record"
            ),
            RuntimeValidationError::UnknownEpoch { platform, epoch } => {
                write!(f, "the manifest declares no epoch {epoch} for {platform}")
            }
            RuntimeValidationError::SuiteRevisionMismatch { found, expected } => write!(
                f,
                "the report claims suite revision {found} but its epoch implements {expected}"
            ),
            RuntimeValidationError::RegimeMismatch {
                field,
                found,
                expected,
            } => write!(
                f,
                "regime field {field} is {found}, but the epoch requires {expected}"
            ),
            RuntimeValidationError::EnvironmentPolicy { expected, found } => write!(
                f,
                "the run was taken on {found}, which the epoch's {expected} policy does not admit"
            ),
            RuntimeValidationError::MalformedIdentity { field, detail } => {
                write!(f, "identity field {field} is malformed: {detail}")
            }
            RuntimeValidationError::UndeclaredWorkload { workload } => write!(
                f,
                "the report observes workload {workload:?}, which its suite does not declare"
            ),
            RuntimeValidationError::FixtureProvenanceMismatch {
                workload,
                field,
                found,
                expected,
            } => write!(
                f,
                "workload {workload:?} recorded fixture {field} {found}, \
                 but the suite pins {expected}"
            ),
            RuntimeValidationError::MissingRecordedInput { workload, name } => write!(
                f,
                "workload {workload:?} records no identity for its {name:?} input"
            ),
            RuntimeValidationError::OracleFailed {
                workload,
                verdict,
                detail,
            } => write!(
                f,
                "workload {workload:?} did not satisfy its correctness oracle ({verdict:?}): \
                 {detail}"
            ),
            RuntimeValidationError::NondeterministicOutput { workload } => write!(
                f,
                "workload {workload:?} produced different output across samples"
            ),
            RuntimeValidationError::OracleReferenceMismatch {
                workload,
                found,
                expected,
            } => write!(
                f,
                "workload {workload:?} was judged against {found:?}, \
                 but the suite declares {expected:?}"
            ),
            RuntimeValidationError::ProgramArgumentsMismatch {
                workload,
                found,
                expected,
            } => write!(
                f,
                "workload {workload:?} ran with {found:?}, but the suite declares {expected:?}"
            ),
            RuntimeValidationError::OracleContradictsSamples {
                workload,
                field,
                detail,
            } => write!(
                f,
                "workload {workload:?} reports {field} but its stored samples say otherwise: \
                 {detail}"
            ),
            RuntimeValidationError::MalformedDigest {
                workload,
                field,
                found,
            } => write!(
                f,
                "workload {workload:?} field {field} is {found:?}, not a lowercase SHA-256 digest"
            ),
            RuntimeValidationError::TooManySamples {
                workload,
                allowed,
                actual,
            } => write!(
                f,
                "workload {workload:?} carries {actual} samples but its epoch permits {allowed}"
            ),
            RuntimeValidationError::ImpossibleProgramIdentity { workload, detail } => {
                write!(
                    f,
                    "workload {workload:?} program identity is impossible: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for RuntimeValidationError {}

/// Whether a runtime report covers its suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCompleteness {
    /// Every declared workload produced its full sample count.
    Complete,
    /// Some did not.
    Partial {
        /// Workloads that did not complete, sorted.
        missing: Vec<String>,
    },
}

/// The full verdict on one runtime report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeValidationOutcome {
    /// Appendability failures. Non-empty means the report may not be stored in
    /// a series at all.
    pub errors: Vec<RuntimeValidationError>,
    /// Samples that are stored but excluded from statistics.
    pub invalid_samples: Vec<RuntimeInvalidSample>,
    /// Whether the report covers its suite.
    pub completeness: RuntimeCompleteness,
}

impl RuntimeValidationOutcome {
    /// Whether this report may enter its series.
    pub fn is_appendable(&self) -> bool {
        self.errors.is_empty()
    }

    /// Whether a point publishes for one workload.
    pub fn publishes_workload(&self, workload: &str) -> bool {
        if !self.is_appendable() {
            return false;
        }
        match &self.completeness {
            RuntimeCompleteness::Complete => true,
            RuntimeCompleteness::Partial { missing } => {
                !missing.iter().any(|entry| entry == workload)
            }
        }
    }
}

/// Check a runtime report against the manifest that governs it.
///
/// Every problem is reported rather than the first. The producer records what
/// happened and never decides whether it was good; this function is where that
/// judgement lives, so a runner that was itself wrong is caught here rather
/// than believed.
pub fn validate_runtime_report(
    manifest: &RuntimeManifest,
    report: &RuntimeReport,
) -> RuntimeValidationOutcome {
    let mut errors = Vec::new();

    if report.record_kind != RUNTIME_RECORD_KIND
        || report.schema_version != RUNTIME_REPORT_SCHEMA_VERSION
    {
        // Nothing below can be trusted to mean what it appears to mean.
        return RuntimeValidationOutcome {
            errors: vec![RuntimeValidationError::UnsupportedRecord {
                record_kind: report.record_kind.clone(),
                schema_version: report.schema_version,
            }],
            invalid_samples: Vec::new(),
            completeness: RuntimeCompleteness::Partial {
                missing: Vec::new(),
            },
        };
    }

    check_identity_shape(report, &mut errors);

    let Some(epoch) = manifest.epoch(&report.identity.platform, report.identity.epoch) else {
        errors.push(RuntimeValidationError::UnknownEpoch {
            platform: report.identity.platform.clone(),
            epoch: report.identity.epoch,
        });
        return RuntimeValidationOutcome {
            errors,
            invalid_samples: Vec::new(),
            completeness: RuntimeCompleteness::Partial {
                missing: Vec::new(),
            },
        };
    };

    if report.identity.suite_revision != epoch.suite_revision {
        errors.push(RuntimeValidationError::SuiteRevisionMismatch {
            found: report.identity.suite_revision,
            expected: epoch.suite_revision,
        });
    }
    let Some(suite) = manifest.suite(epoch.suite_revision) else {
        // Manifest parsing rejects this, so reaching it means the manifest was
        // constructed some other way.
        errors.push(RuntimeValidationError::SuiteRevisionMismatch {
            found: report.identity.suite_revision,
            expected: epoch.suite_revision,
        });
        return RuntimeValidationOutcome {
            errors,
            invalid_samples: Vec::new(),
            completeness: RuntimeCompleteness::Partial {
                missing: Vec::new(),
            },
        };
    };

    check_regime(report, epoch, suite, &mut errors);

    if !epoch.environment.admits(&report.identity.environment) {
        errors.push(RuntimeValidationError::EnvironmentPolicy {
            expected: format!(
                "{}/{}",
                epoch.environment.runner_label, epoch.environment.runner_image
            ),
            found: format!(
                "{}/{}",
                report.identity.environment.runner_label, report.identity.environment.runner_image
            ),
        });
    }

    let mut missing: Vec<String> = Vec::new();
    let mut invalid_samples: Vec<RuntimeInvalidSample> = Vec::new();
    for workload in &suite.workloads {
        let policy = epoch.sampling.get(&workload.id);
        let Some(observation) = report.observation(&workload.id) else {
            missing.push(workload.id.clone());
            continue;
        };
        check_observation(observation, workload, &mut errors);

        if let Some(policy) = policy
            && let Some(actual) = samples_beyond_policy(observation.samples.len(), policy.samples)
        {
            errors.push(RuntimeValidationError::TooManySamples {
                workload: workload.id.clone(),
                allowed: policy.samples,
                actual,
            });
        }
        let before = invalid_samples.len();
        collect_invalid_samples(observation, &mut invalid_samples);

        let expected_samples = policy.map(|policy| policy.samples).unwrap_or(0);
        // A workload publishes only when it produced its full sample count and
        // every one of those samples is valid — the same tiering ADR-0067 uses,
        // so a truncated or partly broken observation keeps its evidence and
        // publishes no median.
        let complete =
            observation.samples.len() as u32 == expected_samples && invalid_samples.len() == before;
        if !complete {
            missing.push(workload.id.clone());
        }
    }
    for observation in &report.workloads {
        if suite.workload(&observation.workload).is_none() {
            errors.push(RuntimeValidationError::UndeclaredWorkload {
                workload: observation.workload.clone(),
            });
        }
    }

    missing.sort();
    missing.dedup();
    let completeness = if missing.is_empty() {
        RuntimeCompleteness::Complete
    } else {
        RuntimeCompleteness::Partial { missing }
    };

    RuntimeValidationOutcome {
        errors,
        invalid_samples,
        completeness,
    }
}

/// Exclude samples that cannot have measured what they claim to.
///
/// The runtime half of ADR-0067's `collect_invalid_samples`, using the shared
/// [`crate::sanity`] rules so the two record kinds cannot drift on what counts
/// as a measurement.
fn collect_invalid_samples(
    observation: &RuntimeObservation,
    invalid: &mut Vec<RuntimeInvalidSample>,
) {
    for (index, sample) in observation.samples.iter().enumerate() {
        let reason = if !is_measurable_duration(sample.process_elapsed_ns) {
            Some(RuntimeInvalidSampleReason::ZeroElapsed)
        } else if sample.exit_code != 0 {
            Some(RuntimeInvalidSampleReason::NonZeroExit {
                exit_code: sample.exit_code,
            })
        } else {
            None
        };
        if let Some(reason) = reason {
            invalid.push(RuntimeInvalidSample {
                workload: observation.workload.clone(),
                sample_index: index as u32,
                reason,
            });
        }
    }
}

fn check_identity_shape(report: &RuntimeReport, errors: &mut Vec<RuntimeValidationError>) {
    let identity = &report.identity;
    let mut malformed = |field: &str, detail: &str| {
        errors.push(RuntimeValidationError::MalformedIdentity {
            field: field.to_string(),
            detail: detail.to_string(),
        });
    };
    if !is_commit(&identity.commit) {
        malformed("commit", "expected a 40-character hexadecimal revision");
    }
    for (field, value) in [
        ("started_at", &identity.started_at),
        ("finished_at", &identity.finished_at),
    ] {
        if !is_utc_timestamp(value) {
            malformed(field, "expected YYYY-MM-DDTHH:MM:SSZ");
        }
    }
    // Consumers order points by completion time, so a record whose interval
    // runs backwards would place itself arbitrarily within its own series.
    if is_utc_timestamp(&identity.started_at)
        && is_utc_timestamp(&identity.finished_at)
        && !is_ordered_interval(&identity.started_at, &identity.finished_at)
    {
        malformed("finished_at", "the measurement interval runs backwards");
    }
    if identity.compiler_version.trim().is_empty() {
        malformed(
            "compiler_version",
            "a runtime observation must name the compiler that produced the program",
        );
    }
    // `std` is part of the product under measurement, so a runtime series that
    // cannot say which standard library it ran against cannot attribute a
    // movement. An empty hash means the runner never resolved one.
    for (field, value) in [
        ("toolchain_hash", &identity.toolchain_hash),
        ("stdlib_hash", &identity.stdlib_hash),
    ] {
        if !is_sha256_digest(value) {
            malformed(field, "expected a lowercase SHA-256 digest");
        }
    }
    for observation in &report.workloads {
        match identity.workload_source_hashes.get(&observation.workload) {
            None => malformed(
                &format!("workload_source_hashes/{}", observation.workload),
                "an observed workload must record the source it was built from",
            ),
            Some(hash) if !is_sha256_digest(hash) => malformed(
                &format!("workload_source_hashes/{}", observation.workload),
                "expected a lowercase SHA-256 digest",
            ),
            Some(_) => {}
        }
    }
}

fn check_regime(
    report: &RuntimeReport,
    epoch: &RuntimeEpoch,
    suite: &RuntimeSuiteRevision,
    errors: &mut Vec<RuntimeValidationError>,
) {
    let regime = &report.regime;
    let mut mismatch = |field: &str, found: String, expected: String| {
        if found != expected {
            errors.push(RuntimeValidationError::RegimeMismatch {
                field: field.to_string(),
                found,
                expected,
            });
        }
    };
    mismatch(
        "measured_boundary",
        format!("{:?}", regime.measured_boundary),
        format!("{:?}", suite.measured_boundary),
    );
    mismatch(
        "optimization",
        format!("{:?}", regime.optimization),
        format!("{:?}", epoch.optimization),
    );
    mismatch(
        "thread_policy",
        format!("{:?}", regime.thread_policy),
        format!("{:?}", epoch.thread_policy),
    );
    mismatch(
        "hardware_counters",
        format!("{:?}", regime.hardware_counters),
        format!("{:?}", epoch.hardware_counters),
    );
    mismatch("target", regime.target.clone(), epoch.target.clone());
    mismatch(
        "compiler_args",
        format!("{:?}", regime.compiler_args),
        format!("{:?}", epoch.compiler_args),
    );
    mismatch(
        "program_state",
        regime.program_state.clone(),
        "fresh_process".to_string(),
    );
    mismatch(
        "os_page_cache",
        regime.os_page_cache.clone(),
        "uncontrolled".to_string(),
    );
    // The boundary claims these two are outside the timed window. A report
    // asserting otherwise is not describing the boundary its suite declares.
    mismatch(
        "fixture_preparation_measured",
        regime.fixture_preparation_measured.to_string(),
        false.to_string(),
    );
    mismatch(
        "oracle_comparison_measured",
        regime.oracle_comparison_measured.to_string(),
        false.to_string(),
    );
}

fn check_observation(
    observation: &RuntimeObservation,
    workload: &RuntimeWorkload,
    errors: &mut Vec<RuntimeValidationError>,
) {
    if observation.program_args != workload.program_args {
        errors.push(RuntimeValidationError::ProgramArgumentsMismatch {
            workload: workload.id.clone(),
            found: observation.program_args.clone(),
            expected: workload.program_args.clone(),
        });
    }

    match observation
        .recorded_inputs
        .iter()
        .find(|input| input.name == FIXTURE_INPUT_NAME)
    {
        None => errors.push(RuntimeValidationError::MissingRecordedInput {
            workload: workload.id.clone(),
            name: FIXTURE_INPUT_NAME.to_string(),
        }),
        Some(input) => {
            let mut mismatch = |field: &str, found: String, expected: String| {
                if found != expected {
                    errors.push(RuntimeValidationError::FixtureProvenanceMismatch {
                        workload: workload.id.clone(),
                        field: field.to_string(),
                        found,
                        expected,
                    });
                }
            };
            mismatch(
                "category",
                format!("{:?}", input.category),
                format!("{:?}", workload.fixture.category),
            );
            mismatch(
                "bytes",
                input.bytes.to_string(),
                workload.fixture.bytes.to_string(),
            );
            // A `FixtureDeclaration` names exactly one `file_name`, so a
            // single-file fixture claiming any other count is describing an
            // input the suite did not declare. A multi-file recorded input —
            // gazette's corpus — arrives with its own declaration kind.
            mismatch("files", input.files.to_string(), 1.to_string());
            match &input.provenance {
                None => mismatch(
                    "provenance",
                    "none".to_string(),
                    format!(
                        "{} revision {} seed {}",
                        workload.fixture.generator,
                        workload.fixture.generator_revision,
                        workload.fixture.seed
                    ),
                ),
                Some(provenance) => {
                    mismatch(
                        "generator",
                        provenance.generator.clone(),
                        workload.fixture.generator.clone(),
                    );
                    mismatch(
                        "generator_revision",
                        provenance.generator_revision.to_string(),
                        workload.fixture.generator_revision.to_string(),
                    );
                    mismatch(
                        "seed",
                        provenance.seed.to_string(),
                        workload.fixture.seed.to_string(),
                    );
                    mismatch(
                        "vocabulary_size",
                        provenance.vocabulary_size.to_string(),
                        workload.fixture.vocabulary_size.to_string(),
                    );
                }
            }
            // The digest is the only thing that makes two raw medians
            // comparable, so a placeholder that is merely non-empty is worse
            // than useless: it looks segmentable and segments nothing.
            if !is_sha256_digest(&input.identity_sha256) {
                errors.push(RuntimeValidationError::MalformedDigest {
                    workload: workload.id.clone(),
                    field: format!("recorded_inputs/{FIXTURE_INPUT_NAME}/identity_sha256"),
                    found: input.identity_sha256.clone(),
                });
            }
        }
    }

    if !is_sha256_digest(&observation.program.sha256) {
        errors.push(RuntimeValidationError::MalformedDigest {
            workload: workload.id.clone(),
            field: "program/sha256".to_string(),
            found: observation.program.sha256.clone(),
        });
    }
    if observation.program.binary_bytes == 0 {
        errors.push(RuntimeValidationError::ImpossibleProgramIdentity {
            workload: workload.id.clone(),
            detail: "a zero-byte executable cannot have been run".to_string(),
        });
    }

    if observation.oracle.reference != workload.oracle.path {
        errors.push(RuntimeValidationError::OracleReferenceMismatch {
            workload: workload.id.clone(),
            found: observation.oracle.reference.clone(),
            expected: workload.oracle.path.clone(),
        });
    }
    check_oracle_against_samples(observation, workload, errors);
}

/// Check the producer's summary of a run against the samples stored beside it.
///
/// The module doc claims that keeping the verdict out of the producer is what
/// lets validation catch a producer that is itself wrong. This function is what
/// makes that claim true. `verdict` and `deterministic_across_samples` are
/// written by the runner; `samples[].stdout_sha256` is the evidence the record
/// carries precisely so the claim is provable, and a validator reading only the
/// summary would accept exactly the records a broken producer emits — into a
/// store that cannot delete them.
///
/// ADR-0072 Decision 4's determinism requirement is enforced here, from the
/// digests, rather than trusted from the flag.
fn check_oracle_against_samples(
    observation: &RuntimeObservation,
    workload: &RuntimeWorkload,
    errors: &mut Vec<RuntimeValidationError>,
) {
    let oracle = &observation.oracle;
    fn contradiction(
        errors: &mut Vec<RuntimeValidationError>,
        workload: &str,
        field: &str,
        detail: String,
    ) {
        errors.push(RuntimeValidationError::OracleContradictsSamples {
            workload: workload.to_string(),
            field: field.to_string(),
            detail,
        });
    }

    for (index, sample) in observation.samples.iter().enumerate() {
        if !is_sha256_digest(&sample.stdout_sha256) {
            errors.push(RuntimeValidationError::MalformedDigest {
                workload: workload.id.clone(),
                field: format!("samples/{index}/stdout_sha256"),
                found: sample.stdout_sha256.clone(),
            });
        }
    }

    // Determinism is decided by the digests, never by the flag.
    let distinct: BTreeSet<&str> = observation
        .samples
        .iter()
        .map(|sample| sample.stdout_sha256.as_str())
        .collect();
    let observed_deterministic = distinct.len() <= 1;
    if oracle.deterministic_across_samples != observed_deterministic {
        contradiction(
            errors,
            &workload.id,
            "deterministic_across_samples",
            format!(
                "the flag says {} but {} distinct stdout digest(s) are stored",
                oracle.deterministic_across_samples,
                distinct.len()
            ),
        );
    }
    if !observed_deterministic {
        errors.push(RuntimeValidationError::NondeterministicOutput {
            workload: workload.id.clone(),
        });
    }

    if oracle.verdict != OracleVerdict::Match {
        errors.push(RuntimeValidationError::OracleFailed {
            workload: workload.id.clone(),
            verdict: oracle.verdict,
            detail: oracle.detail.clone(),
        });
        // A non-matching verdict already blocks the report. Its digests
        // legitimately carry the empty-string placeholders `judge` writes when
        // there was nothing to read or nothing to judge, so holding them to the
        // match-case contract below would only add confusing noise.
        return;
    }

    // A `Match` is a claim about three things agreeing. Each is stored.
    if observation.samples.is_empty() {
        contradiction(
            errors,
            &workload.id,
            "verdict",
            "a match was declared but the report stores no sample to have judged".to_string(),
        );
        return;
    }
    for (field, value) in [
        ("observed_sha256", &oracle.observed_sha256),
        ("reference_sha256", &oracle.reference_sha256),
    ] {
        if !is_sha256_digest(value) {
            errors.push(RuntimeValidationError::MalformedDigest {
                workload: workload.id.clone(),
                field: format!("oracle/{field}"),
                found: value.clone(),
            });
        }
    }
    if oracle.observed_sha256 != oracle.reference_sha256 {
        contradiction(
            errors,
            &workload.id,
            "verdict",
            format!(
                "a match was declared, but the observed output {} is not the expected {}",
                oracle.observed_sha256, oracle.reference_sha256
            ),
        );
    }
    // Every sample, not just the first: a report whose later samples disagree
    // with the golden is a program that was wrong most of the time.
    if let Some(index) = observation
        .samples
        .iter()
        .position(|sample| sample.stdout_sha256 != oracle.observed_sha256)
    {
        contradiction(
            errors,
            &workload.id,
            "verdict",
            format!(
                "a match was declared, but sample {index} produced {} rather than the judged {}",
                observation.samples[index].stdout_sha256, oracle.observed_sha256
            ),
        );
    }
}

/// The name under which a workload's data fixture is recorded.
pub const FIXTURE_INPUT_NAME: &str = "fixture";

// ---------------------------------------------------------------------------
// Derived statistics
// ---------------------------------------------------------------------------

/// The value one sample contributes for a runtime metric.
///
/// Binary size is a property of the observation rather than the sample, so it
/// is taken from the program identity: reporting it per sample would invite a
/// median over a constant.
pub fn runtime_sample_value(
    observation: &RuntimeObservation,
    sample: &RuntimeSample,
    metric: RuntimeMetric,
) -> u64 {
    match metric {
        RuntimeMetric::WallClock => sample.process_elapsed_ns,
        RuntimeMetric::PeakMemory => sample.peak_memory_bytes,
        RuntimeMetric::BinarySize => observation.program.binary_bytes,
    }
}

/// Median and spread of one metric across an observation's samples.
///
/// `None` when the observation has no samples; there is no median of nothing,
/// and inventing zero would silently poison every comparison downstream.
pub fn summarize(
    observation: &RuntimeObservation,
    metric: RuntimeMetric,
) -> Option<crate::Summary> {
    let values: Vec<u64> = observation
        .samples
        .iter()
        .map(|sample| runtime_sample_value(observation, sample, metric))
        .collect();
    crate::Summary::of(&values)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
schema_version = 1

[[suite]]
revision = 1
protocol_version = 1
measured_boundary = "spawn_to_exit_v1"

[[suite.workloads]]
id = "wordfreq"
source = "examples/wordfreq/main.rue"
question = "How fast does compiled Rue count words in a large text?"
program_args = ["{fixture}"]

[suite.workloads.fixture]
category = "recorded"
generator = "zipf_ascii_text"
generator_revision = 1
seed = 20260813
bytes = 4096
vocabulary_size = 256
file_name = "input.txt"
description = "deterministic ASCII prose"

[suite.workloads.oracle]
kind = "golden_stdout"
path = "performance/fixtures/wordfreq/expected-stdout.txt"

[[epoch]]
id = 1
platform = "x86_64-linux"
suite_revision = 1
target = "x86_64-unknown-linux-gnu"
compiler_args = ["-O3"]
optimization = "o3"
thread_policy = "single_threaded"
hardware_counters = "unavailable_on_hosted_runner"
collection = true

[epoch.environment]
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"

[epoch.sampling.wordfreq]
samples = 5
"#;

    fn manifest() -> RuntimeManifest {
        RuntimeManifest::parse(MANIFEST).expect("fixture manifest")
    }

    /// The manifest the repository actually ships, checked against this schema.
    const CHECKED_IN_MANIFEST: &str = include_str!("runtime.toml");

    #[test]
    fn the_checked_in_manifest_declares_the_v1_contract() {
        // Guards the shipped declaration against this schema: a manifest that
        // stopped parsing, or quietly stopped measuring the release-quality
        // product, would otherwise be discovered by a CI collection run.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        let epoch = manifest
            .collection_epoch("x86_64-linux")
            .expect("x86_64-linux is the per-push row of the v1 platform matrix");
        assert_eq!(epoch.optimization, OptimizationLevel::O3);
        assert_eq!(epoch.thread_policy, ThreadPolicy::SingleThreaded);
        assert_eq!(
            epoch.hardware_counters,
            HardwareCounterPolicy::UnavailableOnHostedRunner,
            "hosted runners expose no PMU; counters wait for a controlled-hardware epoch"
        );
        assert_eq!(epoch.environment.runner_image, "ubuntu-24.04");

        let suite = manifest.suite(epoch.suite_revision).expect("suite");
        assert_eq!(suite.measured_boundary, RuntimeBoundary::SpawnToExitV1);
        let workload = suite
            .workload("wordfreq")
            .expect("wordfreq is a permanent member of this suite");
        assert_eq!(workload.fixture.category, InputCategory::Recorded);
        assert_eq!(workload.oracle.kind, OracleKind::GoldenStdout);
        assert!(
            workload.fixture.bytes >= 16 * 1024 * 1024,
            "the fixture must be large enough that the run measures counting \
             rather than process startup"
        );
        // No calibration yet, so every flag on this series is advisory.
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Advisory);
    }

    #[test]
    fn the_checked_in_manifest_measures_arm64_linux_per_push() {
        // RUE-1488. `aarch64-linux` joined on the condition ADR-0072 set for
        // it: the Phase 2 standard-library work CI-verified on that hardware,
        // which RUE-1481 and RUE-1482 satisfied. Same runner class as the
        // x86-64 row, so same cadence and same sampling policy.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        let epoch = manifest
            .collection_epoch("aarch64-linux")
            .expect("aarch64-linux is a row of the v1 platform matrix");
        assert_eq!(epoch.suite_revision, 1);
        assert_eq!(epoch.target, "aarch64-linux");
        assert_eq!(epoch.optimization, OptimizationLevel::O3);
        assert_eq!(epoch.thread_policy, ThreadPolicy::SingleThreaded);
        assert_eq!(
            epoch.hardware_counters,
            HardwareCounterPolicy::UnavailableOnHostedRunner
        );
        // The exact policy, not a relation to another row: a policy that only
        // has to compare equal to its neighbour is satisfied by moving both.
        // The environment is pinned the same way — a row whose label slipped to
        // `local` parses fine and fails at collection time instead.
        assert_eq!(epoch.environment.runner_label, "github-hosted");
        assert_eq!(epoch.environment.runner_image, "ubuntu-24.04");
        assert_eq!(epoch.sampling["wordfreq"].samples, 5);
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Advisory);
    }

    #[test]
    fn the_checked_in_manifest_measures_apple_silicon_without_calibration() {
        // RUE-1488. macOS is measured rather than deferred: the deferral named
        // the `@syscall` carry-flag error-detection gap, and RUE-945 closed
        // that on aarch64-macos. The row rides a daily schedule rather than
        // per-push collection, for cost (ADR-0072 Decision 9).
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        let epoch = manifest
            .collection_epoch("aarch64-macos")
            .expect("aarch64-macos is the scheduled row of the v1 platform matrix");
        assert_eq!(epoch.suite_revision, 1);
        assert_eq!(epoch.target, "aarch64-macos");
        assert_eq!(epoch.optimization, OptimizationLevel::O3);
        assert_eq!(epoch.thread_policy, ThreadPolicy::SingleThreaded);
        assert_eq!(
            epoch.hardware_counters,
            HardwareCounterPolicy::UnavailableOnHostedRunner
        );
        // Pinned exactly, environment included. A row whose label slipped to
        // `local` parses, passes a laxer test, and then refuses every hosted
        // observation at collection time.
        assert_eq!(epoch.environment.runner_label, "github-hosted");
        assert_eq!(epoch.environment.runner_image, "macos-15");
        // Nine is a starting point pending this workload's own calibration
        // here, not a calibrated value; the assertion exists so changing it is
        // a deliberate edit rather than a drift.
        assert_eq!(epoch.sampling["wordfreq"].samples, 9);
        // Never inherited from a row already collecting, however long it has
        // been running: dispersion is a property of a workload on a platform,
        // so this one's flags are advisory until it has calibrated its own
        // repeated samples.
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Advisory);
    }

    #[test]
    fn no_platform_outside_the_v1_matrix_collects() {
        // The matrix is every platform Rue targets and nothing else. A fourth
        // row could only be a target this compiler does not have, so a new
        // entry here should be a deliberate change to that list rather than a
        // manifest edit — `x86-64-macos` in particular is out of scope for the
        // project as a whole.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        let mut collecting: Vec<&str> = manifest
            .epochs()
            .iter()
            .filter(|epoch| epoch.collection)
            .map(|epoch| epoch.platform.as_str())
            .collect();
        collecting.sort_unstable();
        assert_eq!(
            collecting,
            vec!["aarch64-linux", "aarch64-macos", "x86_64-linux"]
        );
    }

    fn fingerprint() -> EnvironmentFingerprint {
        EnvironmentFingerprint {
            runner_label: "github-hosted".to_string(),
            runner_image: "ubuntu-24.04".to_string(),
            runner_image_version: "20260720.1".to_string(),
            cpu_model: "AMD EPYC 7763".to_string(),
            core_count: 4,
            memory_bytes: 16 * 1024 * 1024 * 1024,
            kernel_version: "6.8.0".to_string(),
            os_version: "Ubuntu 24.04".to_string(),
            architecture: "x86_64".to_string(),
        }
    }

    fn report() -> RuntimeReport {
        RuntimeReport {
            record_kind: RUNTIME_RECORD_KIND.to_string(),
            schema_version: RUNTIME_REPORT_SCHEMA_VERSION,
            identity: RuntimeIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "x86_64-linux".to_string(),
                commit: "a".repeat(40),
                compiler_version: "rue 0.1.0".to_string(),
                started_at: "2026-08-13T00:00:00Z".to_string(),
                finished_at: "2026-08-13T00:01:00Z".to_string(),
                toolchain_hash: "1".repeat(64),
                stdlib_hash: "2".repeat(64),
                workload_source_hashes: BTreeMap::from([("wordfreq".to_string(), "3".repeat(64))]),
                environment: fingerprint(),
            },
            regime: RuntimeRegime {
                measured_boundary: RuntimeBoundary::SpawnToExitV1,
                program_state: "fresh_process".to_string(),
                os_page_cache: "uncontrolled".to_string(),
                fixture_preparation_measured: false,
                oracle_comparison_measured: false,
                optimization: OptimizationLevel::O3,
                compiler_args: vec!["-O3".to_string()],
                target: "x86_64-unknown-linux-gnu".to_string(),
                thread_policy: ThreadPolicy::SingleThreaded,
                hardware_counters: HardwareCounterPolicy::UnavailableOnHostedRunner,
            },
            workloads: vec![RuntimeObservation {
                workload: "wordfreq".to_string(),
                source: "examples/wordfreq/main.rue".to_string(),
                question: "How fast does compiled Rue count words in a large text?".to_string(),
                program_args: vec![FIXTURE_ARGUMENT.to_string()],
                recorded_inputs: vec![RecordedInput {
                    name: FIXTURE_INPUT_NAME.to_string(),
                    category: InputCategory::Recorded,
                    description: "deterministic ASCII prose".to_string(),
                    identity_sha256: "f".repeat(64),
                    files: 1,
                    bytes: 4096,
                    provenance: Some(GeneratedProvenance {
                        generator: "zipf_ascii_text".to_string(),
                        generator_revision: 1,
                        seed: 20260813,
                        vocabulary_size: 256,
                    }),
                }],
                program: ProgramIdentity {
                    binary_bytes: 262_144,
                    sha256: "b".repeat(64),
                },
                oracle: OracleOutcome {
                    kind: OracleKind::GoldenStdout,
                    reference: "performance/fixtures/wordfreq/expected-stdout.txt".to_string(),
                    reference_sha256: "c".repeat(64),
                    observed_sha256: "c".repeat(64),
                    verdict: OracleVerdict::Match,
                    deterministic_across_samples: true,
                    detail: String::new(),
                },
                samples: (0..5)
                    .map(|index| RuntimeSample {
                        process_elapsed_ns: 1_000_000_000 + index * 1_000_000,
                        peak_memory_bytes: 100 * 1024 * 1024,
                        exit_code: 0,
                        stdout_bytes: 42,
                        stdout_sha256: "c".repeat(64),
                    })
                    .collect(),
            }],
            failures: Vec::new(),
        }
    }

    #[test]
    fn parses_a_versioned_runtime_manifest() {
        let manifest = manifest();
        let suite = manifest.suite(1).expect("suite 1");
        assert_eq!(suite.workload_ids(), vec!["wordfreq"]);
        assert_eq!(suite.measured_boundary, RuntimeBoundary::SpawnToExitV1);
        let epoch = manifest.collection_epoch("x86_64-linux").expect("epoch");
        assert_eq!(epoch.id, 1);
        assert_eq!(epoch.optimization, OptimizationLevel::O3);
    }

    #[test]
    fn the_fixture_is_declared_as_a_recorded_input_not_a_pinned_one() {
        // ADR-0072 Decision 2's third input category, named in the schema so a
        // record can never be ambiguous about which discipline governed it.
        let manifest = manifest();
        let workload = manifest.suite(1).unwrap().workload("wordfreq").unwrap();
        assert_eq!(workload.fixture.category, InputCategory::Recorded);
    }

    #[test]
    fn an_uncalibrated_workload_reports_an_advisory_posture() {
        // Runtime calibration is never inherited from the compiler suites, so
        // until this workload's own dispersion is measured here, movement is
        // advisory rather than a gate.
        let manifest = manifest();
        let epoch = manifest.epoch("x86_64-linux", 1).unwrap();
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Advisory);
    }

    #[test]
    fn a_calibrated_workload_reports_a_calibrated_posture() {
        let text = format!(
            "{MANIFEST}\n[epoch.calibration.wordfreq]\nk = 3.0\nwindow = 10\n\
             reference = \"RUE-1046 calibration\"\n"
        );
        let manifest = RuntimeManifest::parse(&text).expect("calibrated manifest");
        let epoch = manifest.epoch("x86_64-linux", 1).unwrap();
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Calibrated);
    }

    #[test]
    fn rejects_a_workload_that_never_receives_its_fixture() {
        let text = MANIFEST.replace(r#"program_args = ["{fixture}"]"#, "program_args = []");
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("never passes its fixture"), "{error}");
    }

    #[test]
    fn rejects_single_sample_policies_because_they_have_no_spread() {
        let text = MANIFEST.replace("samples = 5", "samples = 1");
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("at least two"), "{error}");
    }

    #[test]
    fn rejects_an_epoch_measuring_something_other_than_the_release_product() {
        let text = MANIFEST.replace(r#"optimization = "o3""#, r#"optimization = "o0""#);
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("release-quality"), "{error}");
    }

    #[test]
    fn rejects_sampling_policies_that_do_not_cover_the_suite() {
        let text = MANIFEST.replace("[epoch.sampling.wordfreq]", "[epoch.sampling.jsonfmt]");
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("do not match its suite"), "{error}");
    }

    #[test]
    fn rejects_unknown_fields_instead_of_guessing() {
        let error = RuntimeManifest::parse(&format!("{MANIFEST}\nsurprise = true\n")).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn rejects_two_collection_epochs_on_one_platform() {
        let text = format!(
            "{MANIFEST}\n[[epoch]]\nid = 2\nplatform = \"x86_64-linux\"\nsuite_revision = 1\n\
             target = \"x86_64-unknown-linux-gnu\"\ncompiler_args = [\"-O3\"]\n\
             optimization = \"o3\"\nthread_policy = \"single_threaded\"\n\
             hardware_counters = \"unavailable_on_hosted_runner\"\ncollection = true\n\
             [epoch.environment]\nrunner_label = \"github-hosted\"\n\
             runner_image = \"ubuntu-24.04\"\n[epoch.sampling.wordfreq]\nsamples = 5\n"
        );
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("for collection"), "{error}");
    }

    #[test]
    fn a_runner_shaped_report_is_appendable() {
        let outcome = validate_runtime_report(&manifest(), &report());
        assert_eq!(outcome.errors, Vec::new());
        assert_eq!(outcome.completeness, RuntimeCompleteness::Complete);
        assert!(outcome.publishes_workload("wordfreq"));
    }

    #[test]
    fn wrong_output_is_unappendable_however_fast_it_was() {
        // The rule the oracle exists for. This report's samples are the fastest
        // in the suite and it still may not enter a series.
        let mut report = report();
        for sample in &mut report.workloads[0].samples {
            sample.process_elapsed_ns = 1;
        }
        report.workloads[0].oracle.verdict = OracleVerdict::Mismatch;
        report.workloads[0].oracle.observed_sha256 = "d".repeat(64);
        report.workloads[0].oracle.detail = "line 3 differs".to_string();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome
                .errors
                .iter()
                .any(|error| matches!(error, RuntimeValidationError::OracleFailed { .. }))
        );
    }

    #[test]
    fn an_unjudgeable_run_is_refused_rather_than_assumed_correct() {
        let mut report = report();
        report.workloads[0].oracle.verdict = OracleVerdict::Indeterminate;
        report.workloads[0].oracle.detail = "the golden file could not be read".to_string();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
    }

    #[test]
    fn output_that_varies_between_samples_is_refused() {
        // Decided from the stored digests, not from the producer's flag —
        // which here still claims the run was deterministic.
        let mut report = report();
        report.workloads[0].samples[1].stdout_sha256 = "d".repeat(64);
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::NondeterministicOutput { .. }
            )),
            "{:?}",
            outcome.errors
        );
    }

    // -----------------------------------------------------------------------
    // The producer is evidence, not an authority.
    //
    // Every case below was hand-crafted to pass a validator that reads the
    // runner's own summary fields, and every one is a record that could reach a
    // store which cannot delete it. They are the assertion that the separation
    // this module claims is real.
    // -----------------------------------------------------------------------

    #[test]
    fn a_determinism_claim_is_checked_against_the_stored_digests() {
        // `deterministic_across_samples: true` while sample 1's digest differs.
        // The flag is the claim; the digests are the evidence.
        let mut report = report();
        report.workloads[0].samples[1].stdout_sha256 = "d".repeat(64);
        assert!(report.workloads[0].oracle.deterministic_across_samples);
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::OracleContradictsSamples { field, .. }
                    if field == "deterministic_across_samples"
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_determinism_claim_is_checked_in_both_directions() {
        // The mirror case: samples agree, but the runner claims they did not.
        // A producer this confused is not one whose verdict should be believed.
        let mut report = report();
        report.workloads[0].oracle.deterministic_across_samples = false;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::OracleContradictsSamples { .. }
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_match_whose_oracle_digests_disagree_is_refused() {
        // `verdict: match` with `observed_sha256 != reference_sha256`. The
        // verdict is a claim that these two agree; they are both stored.
        let mut report = report();
        report.workloads[0].oracle.observed_sha256 = "d".repeat(64);
        for sample in &mut report.workloads[0].samples {
            sample.stdout_sha256 = "d".repeat(64);
        }
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::OracleContradictsSamples { field, detail, .. }
                    if field == "verdict" && detail.contains("not the expected")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_match_no_sample_actually_produced_is_refused() {
        // Every sample disagrees with both oracle digests, under `match`. The
        // most brazen of the crafted records, and previously the one that
        // published a point like any other.
        let mut report = report();
        for sample in &mut report.workloads[0].samples {
            sample.stdout_sha256 = "d".repeat(64);
        }
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::OracleContradictsSamples { field, detail, .. }
                    if field == "verdict" && detail.contains("rather than the judged")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_match_declared_over_no_samples_at_all_is_refused() {
        let mut report = report();
        report.workloads[0].samples.clear();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::OracleContradictsSamples { detail, .. }
                    if detail.contains("no sample")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_zero_length_sample_invalidates_its_workload() {
        // The runtime counterpart of ADR-0067's
        // `a_zero_length_compilation_invalidates_the_sample`. A monotonic clock
        // either side of a process that ran cannot produce zero, and zero is
        // exactly the value that would look like a spectacular result.
        let mut report = report();
        report.workloads[0].samples[2].process_elapsed_ns = 0;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert_eq!(
            outcome.invalid_samples,
            vec![RuntimeInvalidSample {
                workload: "wordfreq".to_string(),
                sample_index: 2,
                reason: RuntimeInvalidSampleReason::ZeroElapsed,
            }]
        );
        // Kept as evidence, published as nothing.
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
        assert!(!outcome.publishes_workload("wordfreq"));
    }

    #[test]
    fn a_zero_byte_executable_is_refused_outright() {
        // Unlike a zero-length sample, this is not a noisy measurement: no
        // process ran an empty file, so the record is impossible rather than
        // unreliable.
        let mut report = report();
        report.workloads[0].program.binary_bytes = 0;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::ImpossibleProgramIdentity { .. }
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn more_samples_than_the_policy_allows_is_a_protocol_violation() {
        // ADR-0067 makes this a hard error; the shared rule makes it one here
        // too, rather than a report that quietly publishes nothing.
        let mut report = report();
        let extra = report.workloads[0].samples[0].clone();
        report.workloads[0]
            .samples
            .extend(std::iter::repeat_n(extra, 5));
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::TooManySamples {
                    allowed: 5,
                    actual: 10,
                    ..
                }
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_placeholder_fixture_digest_is_refused() {
        // The digest is the only thing making two raw medians comparable, so a
        // value that is merely non-empty looks segmentable and segments nothing.
        let mut report = report();
        report.workloads[0].recorded_inputs[0].identity_sha256 = "not-a-hash".to_string();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::MalformedDigest { field, .. }
                    if field.ends_with("identity_sha256")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_fixture_claiming_the_wrong_file_count_is_refused() {
        let mut report = report();
        report.workloads[0].recorded_inputs[0].files = 4242;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::FixtureProvenanceMismatch { field, .. } if field == "files"
        )));
    }

    #[test]
    fn a_fixture_generated_from_another_vocabulary_is_a_different_workload() {
        // The vocabulary is as much an input to the golden as the seed is, so a
        // record must be able to describe it and validation must check it.
        let mut report = report();
        report.workloads[0].recorded_inputs[0]
            .provenance
            .as_mut()
            .unwrap()
            .vocabulary_size = 512;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::FixtureProvenanceMismatch { field, .. }
                if field == "vocabulary_size"
        )));
    }

    #[test]
    fn a_measurement_interval_may_not_run_backwards() {
        // Derivation orders points by completion time, so a reversed interval
        // would place a point arbitrarily within its own series.
        let mut report = report();
        report.identity.started_at = "2026-08-13T00:02:00Z".to_string();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::MalformedIdentity { field, .. } if field == "finished_at"
        )));
    }

    #[test]
    fn a_report_that_resolved_no_standard_library_is_refused() {
        // `std` is part of the product under measurement. A runtime series that
        // cannot say which one it ran against cannot attribute a movement.
        let mut report = report();
        report.identity.stdlib_hash = String::new();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::MalformedIdentity { field, .. } if field == "stdlib_hash"
        )));
    }

    #[test]
    fn a_fixture_digest_that_moved_is_recorded_rather_than_rejected() {
        // The recorded-input rule. The compile-time suites fail a run whose
        // pinned inputs moved; this one keeps the observation and lets the
        // digest mark the discontinuity.
        let mut report = report();
        report.workloads[0].recorded_inputs[0].identity_sha256 = "9".repeat(64);
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
    }

    #[test]
    fn a_fixture_generated_from_another_seed_is_a_different_workload() {
        // Recorded is not unpinned: the *provenance* the suite declares is
        // still a contract, or the series would silently change subject.
        let mut report = report();
        report.workloads[0].recorded_inputs[0]
            .provenance
            .as_mut()
            .unwrap()
            .seed = 1;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::FixtureProvenanceMismatch { field, .. } if field == "seed"
        )));
    }

    #[test]
    fn an_observation_without_a_recorded_fixture_identity_is_refused() {
        let mut report = report();
        report.workloads[0].recorded_inputs.clear();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(
            outcome
                .errors
                .iter()
                .any(|error| matches!(error, RuntimeValidationError::MissingRecordedInput { .. }))
        );
    }

    #[test]
    fn a_report_measuring_a_debug_build_is_refused() {
        let mut report = report();
        report.regime.optimization = OptimizationLevel::O0;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::RegimeMismatch { field, .. } if field == "optimization"
        )));
    }

    #[test]
    fn a_report_that_timed_fixture_preparation_is_refused() {
        // The boundary is the measurement. A report claiming it measured setup
        // is describing a different experiment than its suite declares.
        let mut report = report();
        report.regime.fixture_preparation_measured = true;
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::RegimeMismatch { field, .. }
                if field == "fixture_preparation_measured"
        )));
    }

    #[test]
    fn a_local_run_cannot_enter_the_hosted_series() {
        let mut report = report();
        report.identity.environment.runner_label = "local".to_string();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(
            outcome
                .errors
                .iter()
                .any(|error| matches!(error, RuntimeValidationError::EnvironmentPolicy { .. }))
        );
    }

    #[test]
    fn a_crashed_workload_leaves_a_partial_but_appendable_report() {
        // Collection health must be visible rather than appearing as a hole.
        let mut report = report();
        report.workloads[0].samples.truncate(2);
        report.failures.push(RuntimeFailure::ProgramCrashed {
            workload: "wordfreq".to_string(),
            sample_index: 2,
            detail: "signal 11".to_string(),
        });
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
        assert!(!outcome.publishes_workload("wordfreq"));
        assert_eq!(
            outcome.completeness,
            RuntimeCompleteness::Partial {
                missing: vec!["wordfreq".to_string()],
            }
        );
    }

    #[test]
    fn a_missing_compiler_version_is_refused() {
        // Longitudinal tracking is the point of the series; a record that
        // cannot say which compiler built the program cannot serve it.
        let mut report = report();
        report.identity.compiler_version = String::new();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(outcome.errors.iter().any(|error| matches!(
            error,
            RuntimeValidationError::MalformedIdentity { field, .. } if field == "compiler_version"
        )));
    }

    #[test]
    fn a_record_of_another_kind_is_refused_before_anything_else_is_read() {
        let mut report = report();
        report.record_kind = "compiler_run".to_string();
        report.identity.commit = "not-a-commit".to_string();
        let outcome = validate_runtime_report(&manifest(), &report);
        assert_eq!(outcome.errors.len(), 1);
        assert!(matches!(
            outcome.errors[0],
            RuntimeValidationError::UnsupportedRecord { .. }
        ));
    }

    #[test]
    fn a_report_is_content_addressable_and_round_trips() {
        let report = report();
        let text = crate::canonical_json(&report).expect("addressable");
        let parsed: RuntimeReport = serde_json::from_str(&text).expect("readable");
        assert_eq!(parsed, report);
        assert_eq!(
            parsed.content_address().unwrap(),
            report.content_address().unwrap()
        );
    }

    #[test]
    fn a_stored_report_is_named_by_the_bytes_it_was_published_as() {
        let report = report();
        let text = crate::canonical_json(&report).expect("addressable");
        let stored = crate::StoredRuntimeReport::read(&text).expect("readable");
        assert_eq!(stored.address(), report.content_address().unwrap());
        assert_eq!(stored.record(), &report);
    }

    #[test]
    fn a_stored_report_keeps_the_name_its_own_bytes_have() {
        // The trap [`crate::StoredRun`] exists to avoid, in its cheapest form.
        // Runtime records will grow fields — peer tool versions, scale variants
        // — and a reader that re-derived a name from today's struct would
        // rename every record written before each addition.
        let report = report();
        let published = crate::canonical_json(&report).expect("addressable");
        let pretty = serde_json::to_string_pretty(&report).unwrap();
        assert_ne!(pretty, published, "the fixture must actually differ");
        assert_eq!(
            crate::StoredRuntimeReport::read(&pretty).unwrap().address(),
            crate::StoredRuntimeReport::read(&published)
                .unwrap()
                .address(),
            "insignificant whitespace is not part of a record's identity"
        );
    }

    #[test]
    fn changing_a_measurement_changes_the_stored_name() {
        let report = report();
        let mut altered = report.clone();
        altered.workloads[0].samples[0].process_elapsed_ns += 1;
        let one =
            crate::StoredRuntimeReport::read(&crate::canonical_json(&report).unwrap()).unwrap();
        let two =
            crate::StoredRuntimeReport::read(&crate::canonical_json(&altered).unwrap()).unwrap();
        assert_ne!(one.address(), two.address());
    }

    #[test]
    fn a_malformed_record_is_reported_rather_than_guessed_at() {
        assert!(crate::StoredRuntimeReport::read("{").is_err());
        assert!(crate::StoredRuntimeReport::read(r#"{"record_kind":"runtime_v1"}"#).is_err());
    }

    #[test]
    fn summaries_are_derived_from_raw_samples() {
        let report = report();
        let observation = report.observation("wordfreq").unwrap();
        let wall = summarize(observation, RuntimeMetric::WallClock).unwrap();
        assert_eq!(wall.median, 1_002_000_000);
        assert_eq!(wall.count, 5);
        // Binary size is a property of the observation, so its spread is zero
        // rather than a median over a constant computed per sample.
        let size = summarize(observation, RuntimeMetric::BinarySize).unwrap();
        assert_eq!(size.median, 262_144);
        assert_eq!(size.mad, 0);
    }

    #[test]
    fn an_observation_without_samples_has_no_summary() {
        let mut report = report();
        report.workloads[0].samples.clear();
        let observation = report.observation("wordfreq").unwrap();
        assert_eq!(summarize(observation, RuntimeMetric::WallClock), None);
    }
}
