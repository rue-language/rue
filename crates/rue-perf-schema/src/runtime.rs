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
    /// The program's emitted output TREE is judged, page by page, by the
    /// checked-in comparison the oracle declaration names.
    ///
    /// Gazette's oracle (ADR-0072 Decision 4). A site build's answer is not a
    /// line of stdout: it is 96 pages, a feed, and a redirect stub, and the
    /// thing that makes it right is that every page's normalized semantics —
    /// heading tree, visible text, link targets, shortcode expansions, section
    /// membership, feed order — agree with an independent authority. A byte
    /// golden over live corpus output would fail on every content change and be
    /// re-blessed rather than read, which is the failure a golden exists to
    /// prevent (Decision 2), so the reference here is the JUDGE rather than an
    /// expected output, its own identity is recorded with every observation,
    /// and the digest compared across samples is the emitted tree's.
    SemanticSitePages,
}

impl OracleKind {
    /// Which per-sample digest this oracle's verdict is a claim about.
    ///
    /// The whole of the producer/validator separation rests on there being a
    /// stored per-sample digest of the thing that was judged. A stdout oracle
    /// judges stdout; a site oracle judges the emitted tree, and reading
    /// stdout for it would check the emptiness of an empty stream while the
    /// output tree went unexamined.
    pub fn judged_digest(self, sample: &RuntimeSample) -> Option<&str> {
        match self {
            OracleKind::GoldenStdout => Some(sample.stdout_sha256.as_str()),
            OracleKind::SemanticSitePages => sample.artifact_sha256.as_deref(),
        }
    }

    /// The name of the digest field this oracle judges, for diagnostics.
    pub const fn judged_field(self) -> &'static str {
        match self {
            OracleKind::GoldenStdout => "stdout_sha256",
            OracleKind::SemanticSitePages => "artifact_sha256",
        }
    }
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
/// Tagged, because the suite now has two genuinely different kinds of input and
/// a single struct could only describe both by making most of its fields
/// optional — which is the shape that lets a wordfreq record omit its seed and
/// still validate. Each variant carries exactly the fields its own shape
/// requires, and validation holds a record to the variant its workload
/// declares.
///
/// The declaration is pinned by the suite revision; the bytes it produces are
/// recorded per observation. Both halves matter: the pin makes the fixture
/// reproducible from the repository, and the recording makes any drift between
/// the pin and the bytes visible in the data rather than only in review.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureDeclaration {
    /// One file, generated deterministically from a pinned seed.
    SeededGenerator {
        /// Which discipline governs this input's identity. Runtime fixtures are
        /// recorded; the field exists so a future pinned fixture cannot be added
        /// without saying so.
        category: InputCategory,
        /// Name of the checked-in generator that produces the fixture.
        generator: String,
        /// Revision of that generator. Bumped whenever its output changes,
        /// which invalidates any committed golden derived from it.
        generator_revision: u32,
        /// Seed handed to the generator.
        seed: u64,
        /// Exact size of the produced fixture, in bytes.
        bytes: u64,
        /// Number of distinct tokens the generator's vocabulary contains.
        vocabulary_size: u32,
        /// File name the fixture is written under inside the work directory.
        file_name: String,
        /// What this fixture is, for a reader of the manifest.
        description: String,
    },
    /// A tree of files assembled from the repository at preparation time, at a
    /// declared scale factor and with a declared set of exclusions.
    ///
    /// Gazette's corpus (ADR-0072 Decision 2). Its bytes are expected to move —
    /// it is the live rue-lang.dev content — so the *bytes* are recorded rather
    /// than pinned, exactly as the seeded fixture's are. What is pinned is
    /// everything about how the tree is assembled: which preparer, at which
    /// revision, at which scale, with which pages excluded. A record whose
    /// scale or exclusion set differs from the declaration is measuring a
    /// different workload, and validation refuses it for the same reason a
    /// wordfreq record generated from another seed is refused.
    CorpusTree {
        /// Which discipline governs this input's identity.
        category: InputCategory,
        /// Repository-relative path of the checked-in preparer that assembles
        /// the tree. ADR-0072 names `scripts/gazette-corpus-diff.py` as the
        /// reference implementation the harness adopts rather than reinvents,
        /// so the harness names it here instead of carrying a second copy of
        /// the corpus-assembly, routing, and validation rules.
        preparer: String,
        /// Revision of that preparer. Bumped when its assembly changes.
        preparer_revision: u32,
        /// The scale variant this workload measures: 1x is the corpus as it
        /// stands, 10x and 100x are the deterministic path-prefixed
        /// duplications. Published curves over this axis are PAGE-COUNT curves
        /// and must say so (Decision 2).
        scale: u32,
        /// Content paths removed from the corpus before any tool sees it, each
        /// of which must be justified where the preparer declares it. Part of
        /// the declaration because a page joining or leaving the set changes
        /// the measured job.
        excluded: Vec<String>,
        /// Directory name the assembled fixture is written under inside the
        /// work directory.
        root_name: String,
        /// What this fixture is, for a reader of the manifest.
        description: String,
    },
}

impl FixtureDeclaration {
    /// Which identity discipline governs this fixture.
    pub fn category(&self) -> InputCategory {
        match self {
            FixtureDeclaration::SeededGenerator { category, .. }
            | FixtureDeclaration::CorpusTree { category, .. } => *category,
        }
    }

    /// What this fixture is, for a reader of the manifest.
    pub fn description(&self) -> &str {
        match self {
            FixtureDeclaration::SeededGenerator { description, .. }
            | FixtureDeclaration::CorpusTree { description, .. } => description,
        }
    }

    /// The name this fixture occupies in the work directory — a file for the
    /// seeded shape, a directory for the corpus tree.
    pub fn work_name(&self) -> &str {
        match self {
            FixtureDeclaration::SeededGenerator { file_name, .. } => file_name,
            FixtureDeclaration::CorpusTree { root_name, .. } => root_name,
        }
    }
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
    /// [`FIXTURE_ARGUMENT`] stands for the prepared fixture's path and
    /// [`OUTPUT_ARGUMENT`] for the directory a workload that emits a tree
    /// writes into. The placeholders rather than the resolved paths are what
    /// the record stores: a resolved path names a temporary directory and
    /// would make two identical measurements differ.
    pub program_args: Vec<String>,
    /// How the workload's data is produced.
    pub fixture: FixtureDeclaration,
    /// How its output is judged.
    pub oracle: OracleDeclaration,
}

/// The token in `program_args` replaced by the prepared fixture's path.
pub const FIXTURE_ARGUMENT: &str = "{fixture}";

/// The token in `program_args` replaced by the directory the program writes to.
///
/// Required of a workload whose oracle judges an emitted tree: a site build
/// that was never told where to write produced no artifact to judge, and the
/// manifest refuses it rather than leaving the runner to discover it.
pub const OUTPUT_ARGUMENT: &str = "{output}";

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

/// A provisional flagging bound a maintainer declared before calibrating one.
///
/// The runtime counterpart of `manifest.toml`'s `[epoch.flagging]`, and
/// deliberately a separate table from [`RuntimeCalibration`]: this one is a
/// judgement call, that one is a measurement. Declaring it makes a workload's
/// movement visible for triage while [`RuntimeEpoch::flag_posture`] keeps
/// reporting [`FlagPosture::Advisory`], which is exactly the state ADR-0072
/// Decision 9 describes.
///
/// There is no default. What `k` and `window` should be on a five-sample
/// runtime workload is ADR-0072's open question 4, and a constant compiled in
/// here would answer it — quietly, in the wrong place, and in a way a
/// maintainer would have to change code to correct. Until an epoch declares
/// one of these or a calibration, a runtime workload is judged by no rule and
/// publishes no flag.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFlaggingPolicy {
    /// Multiplier applied to pooled uncertainty.
    pub k: f64,
    /// How many prior in-segment observations form the trailing window.
    pub window: u32,
}

/// The rule one workload's movement is judged against on one platform.
///
/// Resolved through [`RuntimeEpoch::flagging`] rather than assembled by each
/// consumer, so the dashboard and any future calibrator cannot disagree about
/// which bound was applied — the same reason [`crate::flags_movement`] has one
/// implementation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeFlagging<'a> {
    /// Multiplier applied to the pooled uncertainty of the two summaries.
    pub k: f64,
    /// How many prior observations, within one segment, form the window.
    pub window: u32,
    /// How the resulting flag must be read.
    pub posture: FlagPosture,
    /// The reviewed analysis behind the constants, when they are calibrated.
    pub reference: Option<&'a str>,
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
    /// Per-workload provisional flagging bounds, where a maintainer declared
    /// one ahead of calibrating it. Superseded by `calibration` per workload.
    #[serde(default)]
    pub flagging: BTreeMap<String, RuntimeFlaggingPolicy>,
    /// Per-workload calibrated flagging rules, where they exist yet.
    #[serde(default)]
    pub calibration: BTreeMap<String, RuntimeCalibration>,
    /// Whether scheduled collection measures this epoch.
    #[serde(default)]
    pub collection: bool,
    /// The cross-tool comparison this epoch runs, per workload that has one.
    ///
    /// Thread policy is part of the epoch rather than the suite because it is a
    /// property of how a platform runs the workload (ADR-0072 Decision 5) — the
    /// same reason `thread_policy` above is here. An epoch that declares no
    /// peer policy for a workload must carry no peer observations for it, and
    /// one that declares a canary must carry it in every observation.
    #[serde(default)]
    pub peers: BTreeMap<String, PeerPolicy>,
}

/// The peer legs one workload runs in one epoch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPolicy {
    /// The pinned peer tools, by the name of their repository-root shim.
    pub tools: Vec<String>,
    /// The thread configuration the primary published ratio is taken under.
    pub primary_thread_policy: PeerThreadPolicy,
    /// The configuration published beside it as a labelled secondary row.
    /// `None` publishes no secondary row at all rather than a hidden one.
    #[serde(default)]
    pub secondary_thread_policy: Option<PeerThreadPolicy>,
    /// The peer whose build rides every observation as the same-run ratio
    /// denominator (Decision 9).
    pub canary_tool: String,
    /// The corpus scale the canary builds. 1x, because the canary is meant to
    /// be cheap enough to ride a per-push job.
    pub canary_scale: u32,
    /// Whether observations under this epoch must record the
    /// comparison-configuration identity (ADR-0072 Decision 2, RUE-1493).
    ///
    /// Declared per epoch rather than required outright BECAUSE THE STORE IS
    /// APPEND-ONLY. Records written before that identity existed carry no such
    /// field, and a validator that demanded it of them would invalidate
    /// evidence nobody can rewrite. This is the same mechanism that retired
    /// epoch 1 while keeping it declared: a new requirement arrives with a new
    /// epoch, and the epoch a record names goes on describing the run it
    /// describes.
    ///
    /// One direction only. An epoch that sets this may never unset it, because
    /// the derive step's join reads the identity from both sides and an epoch
    /// that stopped recording it would silently fall back to joining on the
    /// workload identity alone — which is precisely the hole this closes.
    #[serde(default)]
    pub records_comparison_identity: bool,
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

    /// The bound this epoch judges a workload's movement against, if any.
    ///
    /// `None` when the epoch declares neither a calibration nor a provisional
    /// bound, and that is a real state rather than a gap to be papered over: no
    /// rule means no flag, which is honest, where a bound invented here would
    /// be a threshold nobody chose applied to a workload nobody measured.
    ///
    /// A calibration wins over a declared bound. Both may exist during the run
    /// that replaces one with the other.
    pub fn flagging(&self, workload: &str) -> Option<RuntimeFlagging<'_>> {
        if let Some(calibration) = self.calibration.get(workload) {
            return Some(RuntimeFlagging {
                k: calibration.k,
                window: calibration.window,
                posture: FlagPosture::Calibrated,
                reference: Some(calibration.reference.as_str()),
            });
        }
        self.flagging.get(workload).map(|policy| RuntimeFlagging {
            k: policy.k,
            window: policy.window,
            posture: FlagPosture::Advisory,
            reference: None,
        })
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
                // Every rule below is per shape. Generalizing the declaration
                // must not relax the seeded shape by one field: RUE-1046's
                // review found nine of ten fabricated wordfreq records being
                // accepted, and the checks that closed that are these.
                match &workload.fixture {
                    FixtureDeclaration::SeededGenerator {
                        generator,
                        bytes,
                        vocabulary_size,
                        file_name,
                        ..
                    } => {
                        if generator.trim().is_empty() {
                            return Err(format!(
                                "runtime workload {:?} names no fixture generator",
                                workload.id
                            ));
                        }
                        if *bytes == 0 {
                            return Err(format!(
                                "runtime workload {:?} declares an empty fixture",
                                workload.id
                            ));
                        }
                        if *vocabulary_size == 0 {
                            return Err(format!(
                                "runtime workload {:?} declares an empty fixture vocabulary",
                                workload.id
                            ));
                        }
                        if file_name.contains('/') {
                            return Err(format!(
                                "runtime workload {:?} fixture file name must be a bare name",
                                workload.id
                            ));
                        }
                    }
                    FixtureDeclaration::CorpusTree {
                        preparer,
                        scale,
                        excluded,
                        root_name,
                        ..
                    } => {
                        if preparer.is_empty() || preparer.starts_with('/') {
                            return Err(format!(
                                "runtime workload {:?} must name a repository-relative \
                                 fixture preparer",
                                workload.id
                            ));
                        }
                        // 1x is the corpus as it stands; anything smaller is
                        // not a scale variant of it.
                        if *scale == 0 {
                            return Err(format!(
                                "runtime workload {:?} declares corpus scale 0",
                                workload.id
                            ));
                        }
                        if root_name.contains('/') {
                            return Err(format!(
                                "runtime workload {:?} fixture root name must be a bare name",
                                workload.id
                            ));
                        }
                        for path in excluded {
                            if path.is_empty() || path.starts_with('/') {
                                return Err(format!(
                                    "runtime workload {:?} excludes {path:?}, which is not a \
                                     corpus-relative path",
                                    workload.id
                                ));
                            }
                        }
                        // A tree-emitting workload that is never told where to
                        // write produces nothing its oracle can judge.
                        if !workload
                            .program_args
                            .iter()
                            .any(|argument| argument == OUTPUT_ARGUMENT)
                        {
                            return Err(format!(
                                "runtime workload {:?} emits an output tree but never receives \
                                 a destination; one argument must be {OUTPUT_ARGUMENT:?}",
                                workload.id
                            ));
                        }
                    }
                }
                // The oracle has to be able to judge what the workload
                // produces. A stdout golden over a program whose answer is a
                // directory would compare two empty streams and pass.
                let tree_shaped = matches!(workload.fixture, FixtureDeclaration::CorpusTree { .. });
                let tree_oracle = workload.oracle.kind == OracleKind::SemanticSitePages;
                if tree_shaped != tree_oracle {
                    return Err(format!(
                        "runtime workload {:?} pairs a {:?} fixture with a {:?} oracle; a \
                         corpus-tree workload is judged by its emitted tree and a seeded \
                         one by its stdout",
                        workload.id,
                        if tree_shaped {
                            "corpus_tree"
                        } else {
                            "seeded_generator"
                        },
                        workload.oracle.kind
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
            for (workload, policy) in &epoch.flagging {
                if !declared.contains(workload.as_str()) {
                    return Err(format!(
                        "runtime epoch {} on {} declares a flagging bound for undeclared \
                         workload {workload:?}",
                        epoch.id, epoch.platform
                    ));
                }
                // A rule that cannot flag is worse than no rule: it renders as
                // a published bound and never speaks. NaN is spelled out rather
                // than left to `!(k > 0.0)`, which catches it only as a side
                // effect of partial ordering.
                if policy.k.is_nan() || policy.k <= 0.0 || policy.window == 0 {
                    return Err(format!(
                        "runtime epoch {} on {} declares a flagging bound for {workload:?} that \
                         can never flag; k must be positive and window at least one",
                        epoch.id, epoch.platform
                    ));
                }
            }
            for (workload, policy) in &epoch.peers {
                if !declared.contains(workload.as_str()) {
                    return Err(format!(
                        "runtime epoch {} on {} declares a peer policy for undeclared \
                         workload {workload:?}",
                        epoch.id, epoch.platform
                    ));
                }
                if policy.tools.is_empty() {
                    return Err(format!(
                        "runtime epoch {} on {} declares a peer policy for {workload:?} with \
                         no peer tools",
                        epoch.id, epoch.platform
                    ));
                }
                // A canary that is not one of the declared peers would be an
                // unpinned denominator, and the ratio it anchors would compare
                // against a tool the record does not describe.
                if !policy.tools.contains(&policy.canary_tool) {
                    return Err(format!(
                        "runtime epoch {} on {} names {:?} as the peer canary for {workload:?}, \
                         which is not one of its peer tools {:?}",
                        epoch.id, epoch.platform, policy.canary_tool, policy.tools
                    ));
                }
                if policy.canary_scale == 0 {
                    return Err(format!(
                        "runtime epoch {} on {} declares a canary at scale 0 for {workload:?}",
                        epoch.id, epoch.platform
                    ));
                }
                // The peers build the workload's OWN fixture, so a canary
                // at another scale would be a denominator for a corpus nobody
                // built. Declaring the scale here anyway, and checking it,
                // keeps the manifest legible without letting it lie.
                match suite.workload(workload).map(|declared| &declared.fixture) {
                    Some(FixtureDeclaration::CorpusTree { scale, .. }) => {
                        if policy.canary_scale != *scale {
                            return Err(format!(
                                "runtime epoch {} on {} declares a {}x canary for {workload:?}, \
                                 whose fixture is the {scale}x corpus",
                                epoch.id, epoch.platform, policy.canary_scale
                            ));
                        }
                    }
                    _ => {
                        return Err(format!(
                            "runtime epoch {} on {} declares peers for {workload:?}, which does \
                             not build a corpus tree; there is nothing for a peer to build",
                            epoch.id, epoch.platform
                        ));
                    }
                }
                if policy.secondary_thread_policy == Some(policy.primary_thread_policy) {
                    return Err(format!(
                        "runtime epoch {} on {} publishes the same thread policy for \
                         {workload:?} as both the primary ratio and the secondary row; a \
                         secondary row that repeats the primary informs nobody",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<GeneratedProvenance>,
    /// How it was assembled, when it is a collected tree rather than generated
    /// bytes.
    ///
    /// Exactly one of the two is present, and which one is decided by the
    /// workload's declared fixture shape rather than by whichever happens to
    /// parse: a record carrying neither, or carrying the other shape's, is
    /// describing an input its suite did not declare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<CorpusTreeProvenance>,
}

/// How a collected recorded input was assembled.
///
/// "Recorded" is not "unpinned". The corpus's BYTES are expected to move, and
/// the digest beside these fields is what makes a movement visible; everything
/// about how the tree was built is still a contract, and a report that changed
/// scale or quietly dropped another page is measuring a different workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusTreeProvenance {
    /// Repository-relative path of the preparer that assembled the tree.
    pub preparer: String,
    /// Its revision at the time of assembly.
    pub preparer_revision: u32,
    /// The scale variant assembled.
    pub scale: u32,
    /// The content paths excluded from the corpus, sorted.
    pub excluded: Vec<String>,
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
    /// Digest of the output TREE this sample emitted, for a workload whose
    /// answer is an artifact rather than a stream.
    ///
    /// Absent for a stdout workload, and required for a tree one: it is what
    /// makes determinism and the oracle's verdict provable from the stored
    /// samples for a program whose stdout is empty by design. Omitted from the
    /// serialized form when absent, so a stdout record's content address is
    /// unchanged by this field existing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
}

/// Which of the two published thread configurations a peer ran under.
///
/// ADR-0072 Decision 5. Rue has no concurrency and hosted runners have four
/// vCPUs, so a default-configuration comparison would publish thread count as
/// though it were per-unit work. The primary ratio pins the peers to one
/// worker thread; their default parallel configuration is measured too and
/// published as a clearly labelled secondary row, so the number a user would
/// actually experience is never hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerThreadPolicy {
    /// One worker thread: `RAYON_NUM_THREADS=1` for Zola, `GOMAXPROCS=1` for
    /// Hugo. The primary published ratio's denominator.
    PinnedSingleThread,
    /// The peer's own default. Published as a secondary row, never as the
    /// primary ratio.
    ToolDefaultParallel,
}

/// Why a peer measurement was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerRole {
    /// The per-run canary (Decision 9): one single-threaded build of the 1x
    /// corpus, riding every gazette observation so the ratio published for it
    /// has a SAME-RUN denominator rather than a stale or singleton one.
    Canary,
    /// A leg of the full peer matrix, which runs only on an event: the recorded
    /// fixture identity moved, a peer version moved, or the epoch changed.
    Full,
}

/// One peer tool's measurement of the same corpus, in the same run.
///
/// Peer samples live inside the gazette observation they are the denominator
/// for. The alternative — a separate record kind joined after the fact — is
/// what Decision 9's canary exists to avoid: a ratio whose two sides were
/// measured on different days on a hosted runner is a ratio of two different
/// machines' moods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerObservation {
    /// The peer, for example `zola`.
    pub tool: String,
    /// Its pinned version, recorded with every observation so a bump is an
    /// annotated event rather than a silent shift.
    pub version: String,
    /// Why this measurement was taken.
    pub role: PeerRole,
    /// The thread configuration in force.
    pub thread_policy: PeerThreadPolicy,
    /// The corpus scale variant built.
    pub scale: u32,
    /// Digest of the tree this peer emitted.
    pub output_sha256: String,
    /// How many files it emitted.
    pub emitted_files: u64,
    /// Independent raw fresh-process measurements, taken by the same harness
    /// and the same clock as the Rue program's.
    pub samples: Vec<RuntimeSample>,
}

/// The configuration a cross-tool comparison was taken under.
///
/// ADR-0072 Decision 2 records TWO identities per observation, and the split is
/// the whole of RUE-1493. The workload identity — the fixture's
/// `identity_sha256` — is what gazette consumed: corpus, static assets,
/// assembly rules, and gazette's own template port. It segments Rue's
/// longitudinal series. This is the other one: the workload identity plus every
/// peer template port, every peer version, and the epoch. It is what a joined
/// peer row must match, and nothing else.
///
/// One identity could not do both jobs, and did neither correctly. Composed as
/// it was, editing a Hugo template moved the digest that segments GAZETTE's own
/// wall-clock series, for a change that cannot alter what gazette does — while
/// bumping the Hugo shim moved no recorded identity at all, so a failed full
/// peer leg left the next canary-only run joining a Hugo row taken under a
/// version the project no longer pins. The row self-labelled its version, so no
/// number was misattributed, but the table went on publishing against a pin
/// that had never successfully run, and nothing in the data said so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonIdentity {
    /// Digest over the workload identity, the peer ports, the peer PREPARER's
    /// own digest and revision, the peer versions, and the epoch. The join
    /// condition, and nothing a consumer has to reassemble from parts.
    ///
    /// The preparer's digest is in there because the peer half of the harness
    /// is its own file: the respelling, the invocations, the thread parity, and
    /// the cross-tool oracle all decide what a peer measurement means, and none
    /// of them can move the WORKLOAD identity without segmenting Rue's series
    /// for a change no gazette build can observe.
    pub identity_sha256: String,
    /// The peer-port revision this comparison was configured by: the
    /// human-legible label beside the digest, as the workload identity's port
    /// revision is beside its own.
    pub peer_port_revision: u32,
    /// EVERY pinned peer's version, on every observation — including a
    /// canary-only one, which measures a single peer and must still record
    /// what the others were pinned at. Without that, a bumped peer whose full
    /// leg never succeeded is invisible: the canary run says nothing about the
    /// tool it did not run, and the joined row names the version it was
    /// measured under rather than the version now pinned.
    pub peer_versions: BTreeMap<String, String>,
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
    /// Peer tools' measurements of the same corpus in the same run.
    ///
    /// Empty for a workload with no peers, and empty is the honest state:
    /// nothing here is ever synthesized from an earlier run. Omitted from the
    /// serialized form when empty, so a wordfreq record's content address is
    /// unchanged by this field existing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<PeerObservation>,
    /// The comparison configuration this observation was taken under.
    ///
    /// Present exactly when the epoch's peer policy declares
    /// `records_comparison_identity`. Optional in the wire form for the reason
    /// that field exists: the store is append-only, and records written before
    /// this identity existed must go on validating unchanged. Omitted when
    /// absent, so no existing record's content address moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<ComparisonIdentity>,
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
    /// The oracle that judged the run is not the one the suite declares.
    ///
    /// Separate from a reference mismatch: this is the wrong KIND of judgement,
    /// and a stdout comparison over a program whose answer is a directory
    /// compares two empty streams and reports a match.
    OracleKindMismatch {
        /// The workload.
        workload: String,
        /// What the record carries.
        found: OracleKind,
        /// What the suite declares.
        expected: OracleKind,
    },
    /// A peer observation does not match the epoch's declared peer policy.
    PeerPolicyViolation {
        /// The workload.
        workload: String,
        /// What is wrong.
        detail: String,
    },
    /// The comparison-configuration identity is missing, malformed, or
    /// contradicted by the peer observations stored beside it.
    ///
    /// Separate from a peer-policy violation because it is a claim about the
    /// CONFIGURATION rather than about a measurement: the peers may each be
    /// impeccable and the record still unable to say which peer pins they were
    /// taken under, which is what a joined row is published against.
    ComparisonIdentityViolation {
        /// The workload.
        workload: String,
        /// What is wrong.
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
            RuntimeValidationError::OracleKindMismatch {
                workload,
                found,
                expected,
            } => write!(
                f,
                "workload {workload:?} was judged by a {found:?} oracle, \
                 but the suite declares {expected:?}"
            ),
            RuntimeValidationError::PeerPolicyViolation { workload, detail } => write!(
                f,
                "workload {workload:?} carries a peer observation its epoch does not \
                 admit: {detail}"
            ),
            RuntimeValidationError::ComparisonIdentityViolation { workload, detail } => write!(
                f,
                "workload {workload:?} does not record the comparison configuration its \
                 epoch pins: {detail}"
            ),
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
        check_observation(observation, workload, epoch, &mut errors);

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
        // The canary joins that tiering rather than blocking the report
        // (ADR-0072 Decision 9). A run whose peer build failed still holds a
        // valid Rue measurement worth storing, and the ratio it was meant to
        // anchor is exactly what cannot be computed — so the evidence is kept,
        // no point publishes, and collection health shows the hole.
        let complete = observation.samples.len() as u32 == expected_samples
            && invalid_samples.len() == before
            && has_required_canary(observation, workload, epoch);
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
    epoch: &RuntimeEpoch,
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
                format!("{:?}", workload.fixture.category()),
            );
            match &workload.fixture {
                FixtureDeclaration::SeededGenerator {
                    generator,
                    generator_revision,
                    seed,
                    bytes,
                    vocabulary_size,
                    ..
                } => {
                    mismatch("bytes", input.bytes.to_string(), bytes.to_string());
                    // A seeded `FixtureDeclaration` names exactly one
                    // `file_name`, so a single-file fixture claiming any other
                    // count is describing an input the suite did not declare.
                    // The corpus-tree shape below relaxes this and NOTHING
                    // else: the seeded rules are the ones RUE-1046's review
                    // tightened after nine of ten fabricated records were
                    // accepted, and generalizing the declaration must not
                    // loosen a single one of them.
                    mismatch("files", input.files.to_string(), 1.to_string());
                    if input.tree.is_some() {
                        mismatch(
                            "tree",
                            "a corpus-tree provenance".to_string(),
                            "none, for a seeded fixture".to_string(),
                        );
                    }
                    match &input.provenance {
                        None => mismatch(
                            "provenance",
                            "none".to_string(),
                            format!("{generator} revision {generator_revision} seed {seed}"),
                        ),
                        Some(provenance) => {
                            mismatch("generator", provenance.generator.clone(), generator.clone());
                            mismatch(
                                "generator_revision",
                                provenance.generator_revision.to_string(),
                                generator_revision.to_string(),
                            );
                            mismatch("seed", provenance.seed.to_string(), seed.to_string());
                            mismatch(
                                "vocabulary_size",
                                provenance.vocabulary_size.to_string(),
                                vocabulary_size.to_string(),
                            );
                        }
                    }
                }
                FixtureDeclaration::CorpusTree {
                    preparer,
                    preparer_revision,
                    scale,
                    excluded,
                    ..
                } => {
                    // The tree's SIZE is recorded rather than declared — that
                    // is the whole of Decision 2 — but a fixture of no files
                    // or no bytes is not a corpus that moved, it is a
                    // preparation that failed.
                    if input.files == 0 {
                        mismatch("files", "0".to_string(), "at least one".to_string());
                    }
                    if input.bytes == 0 {
                        mismatch("bytes", "0".to_string(), "more than zero".to_string());
                    }
                    if input.provenance.is_some() {
                        mismatch(
                            "provenance",
                            "a generated provenance".to_string(),
                            "none, for a collected tree".to_string(),
                        );
                    }
                    match &input.tree {
                        None => mismatch(
                            "tree",
                            "none".to_string(),
                            format!("{preparer} revision {preparer_revision} at scale {scale}x"),
                        ),
                        Some(tree) => {
                            mismatch("preparer", tree.preparer.clone(), preparer.clone());
                            mismatch(
                                "preparer_revision",
                                tree.preparer_revision.to_string(),
                                preparer_revision.to_string(),
                            );
                            mismatch("scale", tree.scale.to_string(), scale.to_string());
                            // Sorted on both sides: an exclusion set that
                            // differs only in order is the same set, and one
                            // that differs in membership is a different job.
                            let mut found = tree.excluded.clone();
                            let mut expected = excluded.clone();
                            found.sort();
                            expected.sort();
                            mismatch("excluded", format!("{found:?}"), format!("{expected:?}"));
                        }
                    }
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

    check_peers(observation, workload, epoch, errors);

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

    if observation.oracle.kind != workload.oracle.kind {
        errors.push(RuntimeValidationError::OracleKindMismatch {
            workload: workload.id.clone(),
            found: observation.oracle.kind,
            expected: workload.oracle.kind,
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

/// Hold peer observations to the epoch's declared peer policy.
///
/// Peers are the denominator of a published ratio, so every property the ratio
/// depends on is checked from the record rather than trusted from the runner:
/// which tool, which version, which thread configuration, which scale, how many
/// samples, whether each of those samples is a measurement at all, and whether
/// the peer's own repeated builds agreed with each other.
///
/// THE PEER SAMPLES ANSWER TO THE SAME SANITY RULES AS THE RUE SAMPLES, and
/// RUE-1493 is why that sentence is here rather than assumed. This function
/// once checked only that every peer sample's artifact digest matched the
/// recorded output digest, which a one-sample, zero-duration, non-zero-exit
/// canary satisfies trivially — and `latest_comparison` would have published
/// its median as the denominator of the headline ratio. A peer sample is judged
/// harder than a Rue sample rather than more leniently: an invalid Rue sample
/// is a measurement failure that keeps its evidence and publishes nothing
/// (see [`collect_invalid_samples`]), while a peer observation carrying one
/// cannot have come from this runner at all — `measure_one_peer` aborts the leg
/// on a non-zero exit and never records a partial one — so it is a broken or
/// dishonest producer, which is exactly what appendability exists to refuse.
///
/// The asymmetry is deliberate and it does cost something: one runner defect in
/// a peer leg sinks the whole report, including the Rue observations of every
/// other workload in it. Two things make that the right side. A peer row is
/// judged as a RECORD rather than as a measurement — it is the configuration a
/// published ratio divides by, and every other property of it (its tool, its
/// version, its thread policy, its digests) is already appendability-checked
/// here, so tiering the sample rules alone would leave the blast radius
/// unchanged for most defects while making the loudest ones quiet. And a Rue
/// sample is the evidence being collected, which is why a truncated one is kept
/// and unpublished; a peer sample the runner would never have written is
/// evidence of the producer, not of the workload, and the store cannot delete
/// what it accepts.
///
/// A peer whose build FAILED is not represented here at all — the runner
/// records a [`RuntimeFailure`] instead — so an observation missing its canary
/// is handled as incompleteness, where the evidence is kept and no ratio
/// publishes, rather than as a malformed record.
fn check_peers(
    observation: &RuntimeObservation,
    workload: &RuntimeWorkload,
    epoch: &RuntimeEpoch,
    errors: &mut Vec<RuntimeValidationError>,
) {
    // Collected rather than pushed directly, so the digest checks below can
    // borrow `errors` in the same scope.
    let mut details: Vec<String> = Vec::new();
    let Some(policy) = epoch.peers.get(&workload.id) else {
        if !observation.peers.is_empty() {
            details.push(format!(
                "{} peer observation(s) are stored, but this epoch declares no peer policy \
                 for the workload",
                observation.peers.len()
            ));
        }
        if observation.comparison.is_some() {
            errors.push(RuntimeValidationError::ComparisonIdentityViolation {
                workload: workload.id.clone(),
                detail: "a comparison configuration is recorded, but this epoch declares no \
                         peer policy for the workload; there is no comparison to configure"
                    .to_string(),
            });
        }
        flush(errors, workload, details);
        return;
    };

    // The scale every non-canary row must have built. The peers build the
    // WORKLOAD's own fixture — a 1x denominator under a 10x numerator would not
    // be a ratio of anything — so this is the workload's declared scale rather
    // than a free field. The canary's is checked against the policy below,
    // which the manifest already holds to the same number.
    let declared_scale = match &workload.fixture {
        FixtureDeclaration::CorpusTree { scale, .. } => Some(*scale),
        FixtureDeclaration::SeededGenerator { .. } => None,
    };
    let expected_samples = epoch
        .sampling
        .get(&workload.id)
        .map(|sampling| sampling.samples);

    let mut canaries = 0;
    // The configuration key, checked as a UNIT rather than field by field:
    // tool, scale, thread policy. ROLE IS DELIBERATELY NOT PART OF IT. The
    // derive step keys a published row on (tool, thread policy) and keeps
    // whichever it reaches first, so a canary and a full row of the same tool
    // and policy are two denominators for one cell of the table with array
    // order deciding between them — the thing this check exists to refuse,
    // not an exception to it.
    //
    // The key here carries one field the table's does not, and the two are
    // equivalent only because SCALE CANNOT VARY WITHIN AN OBSERVATION: the
    // manifest holds a canary's scale to the workload's own (`check_epochs`),
    // the canary's scale is checked against the policy below, and every other
    // row's is checked against the workload's declared fixture scale. Those
    // three are why keeping scale here is free rather than a mismatch. Do not
    // "reconcile" the two keys by adding scale to the derive key or dropping
    // it here without first checking that those pins still hold — the safety
    // is in the pins, not in the spelling.
    let mut configurations: BTreeSet<(&str, u32, String)> = BTreeSet::new();
    // One emitted tree per (tool, scale), across thread policies: see the
    // work-equivalence check after the loop.
    let mut primary_trees: BTreeMap<(&str, u32), &str> = BTreeMap::new();
    for peer in &observation.peers {
        if !policy.tools.contains(&peer.tool) {
            details.push(format!(
                "{:?} is not one of the epoch's peer tools {:?}",
                peer.tool, policy.tools
            ));
        }
        if peer.version.trim().is_empty() {
            details.push(format!(
                "the {:?} observation records no version; a peer whose version is unknown \
                 cannot anchor a comparable ratio",
                peer.tool
            ));
        }
        let admitted = peer.thread_policy == policy.primary_thread_policy
            || Some(peer.thread_policy) == policy.secondary_thread_policy;
        if !admitted {
            details.push(format!(
                "the {:?} observation ran under {:?}, which is neither this epoch's primary \
                 nor its published secondary thread policy",
                peer.tool, peer.thread_policy
            ));
        }
        if !configurations.insert((
            peer.tool.as_str(),
            peer.scale,
            format!("{:?}", peer.thread_policy),
        )) {
            details.push(format!(
                "two observations claim the same configuration — {:?} at {}x under {:?} — so \
                 one cell of the published table has two denominators and no rule says which; \
                 a differing role does not separate them, because the table does not key on \
                 role",
                peer.tool, peer.scale, peer.thread_policy
            ));
        }
        if peer.thread_policy == policy.primary_thread_policy {
            primary_trees.insert(
                (peer.tool.as_str(), peer.scale),
                peer.output_sha256.as_str(),
            );
        }
        if peer.emitted_files == 0 {
            details.push(format!(
                "the {:?} observation emitted no files; a tool that built nothing is not a \
                 denominator, however quickly it did it",
                peer.tool
            ));
        }
        // The epoch's sample count, exactly — the same protocol rule the Rue
        // observation answers to. Fewer is a leg this runner would never have
        // recorded, since it aborts rather than storing a short one, and more
        // was not taken under the policy the series compares against.
        if let Some(wanted) = expected_samples
            && peer.samples.len() as u32 != wanted
        {
            details.push(format!(
                "the {:?} observation stores {} sample(s) where this epoch's policy for the \
                 workload is {wanted}; a denominator taken under another sampling policy is \
                 not comparable with the numerator beside it",
                peer.tool,
                peer.samples.len()
            ));
        }
        for (index, sample) in peer.samples.iter().enumerate() {
            if !is_measurable_duration(sample.process_elapsed_ns) {
                details.push(format!(
                    "the {:?} observation's sample {index} elapsed zero ns; a monotonic clock \
                     read either side of a process that ran cannot produce that, and it is \
                     exactly the value that would make a peer look infinitely fast",
                    peer.tool
                ));
            }
            if sample.exit_code != 0 {
                details.push(format!(
                    "the {:?} observation's sample {index} exited with code {}; a build that \
                     failed measured nothing",
                    peer.tool, sample.exit_code
                ));
            }
            if !is_sha256_digest(&sample.stdout_sha256) {
                errors.push(RuntimeValidationError::MalformedDigest {
                    workload: workload.id.clone(),
                    field: format!("peers/{}/samples/{index}/stdout_sha256", peer.tool),
                    found: sample.stdout_sha256.clone(),
                });
            }
        }
        if peer.samples.is_empty() {
            details.push(format!(
                "the {:?} observation stores no sample to have measured",
                peer.tool
            ));
        }
        if !is_sha256_digest(&peer.output_sha256) {
            errors.push(RuntimeValidationError::MalformedDigest {
                workload: workload.id.clone(),
                field: format!("peers/{}/output_sha256", peer.tool),
                found: peer.output_sha256.clone(),
            });
        }
        // Decided from the stored per-sample digests, never from a flag: a peer
        // whose repeated builds disagree is a denominator nobody should divide
        // by, and the ports strip build-time-varying output precisely so this
        // holds byte-exactly.
        if let Some(index) = peer
            .samples
            .iter()
            .position(|sample| sample.artifact_sha256.as_deref() != Some(&peer.output_sha256))
        {
            details.push(format!(
                "the {:?} observation's sample {index} emitted {:?} rather than the recorded \
                 {:?}; a peer that does not build the same tree twice cannot anchor a ratio",
                peer.tool,
                peer.samples[index]
                    .artifact_sha256
                    .as_deref()
                    .unwrap_or("nothing"),
                peer.output_sha256
            ));
        }
        if peer.role == PeerRole::Canary {
            canaries += 1;
            if peer.tool != policy.canary_tool {
                details.push(format!(
                    "the canary is {:?}, but this epoch's canary is {:?}",
                    peer.tool, policy.canary_tool
                ));
            }
            if peer.scale != policy.canary_scale {
                details.push(format!(
                    "the canary built the {}x corpus, but this epoch's canary builds {}x",
                    peer.scale, policy.canary_scale
                ));
            }
            if peer.thread_policy != policy.primary_thread_policy {
                details.push(format!(
                    "the canary ran under {:?}, but the primary ratio it anchors is taken \
                     under {:?}",
                    peer.thread_policy, policy.primary_thread_policy
                ));
            }
        } else if let Some(scale) = declared_scale
            && peer.scale != scale
        {
            details.push(format!(
                "the {:?} observation built the {}x corpus, but this workload's fixture is \
                 the {scale}x one; the peers build the workload's own corpus or the ratio \
                 divides two different jobs",
                peer.tool, peer.scale
            ));
        }
    }
    if canaries > 1 {
        details.push(format!(
            "{canaries} observations claim to be the per-run canary; the ratio's denominator \
             must be one measurement, not a choice between several"
        ));
    }

    // WORK EQUIVALENCE FOR THE DEFAULT-PARALLEL ROW (RUE-1493). The semantic
    // oracle reads one emitted tree per tool — the primary-policy one — so a
    // secondary row was previously proved only to be deterministic WITH
    // ITSELF: a parallel build that reproducibly emitted a different site
    // published as the number a user would actually experience. It is the same
    // tool on the same corpus, so the check that closes it needs no second
    // oracle pass, only the digest the oracle already judged.
    for peer in &observation.peers {
        if peer.thread_policy == policy.primary_thread_policy {
            continue;
        }
        match primary_trees.get(&(peer.tool.as_str(), peer.scale)) {
            Some(primary) if *primary == peer.output_sha256 => {}
            Some(primary) => details.push(format!(
                "the {:?} observation under {:?} emitted {} where its own {:?} build — the \
                 one the semantic oracle judged — emitted {}; the same tool on the same \
                 corpus did different work, so the secondary row is not the same job \
                 measured with more threads",
                peer.tool,
                peer.thread_policy,
                peer.output_sha256,
                policy.primary_thread_policy,
                primary
            )),
            None => details.push(format!(
                "the {:?} observation under {:?} has no {:?} build at {}x beside it, so the \
                 tree it emitted was never judged against anything",
                peer.tool, peer.thread_policy, policy.primary_thread_policy, peer.scale
            )),
        }
    }

    errors.extend(check_comparison_identity(observation, workload, policy));
    flush(errors, workload, details);
}

/// Hold the comparison-configuration identity to the epoch that pins it.
///
/// The identity is what a joined peer row is published against (ADR-0072
/// Decision 9), so the record has to carry it, carry it well-formed, and carry
/// a peer-version table that agrees with the peer rows stored beside it. A row
/// self-labelling a version the run did not pin is the exact shape of the
/// staleness RUE-1493 closes.
fn check_comparison_identity(
    observation: &RuntimeObservation,
    workload: &RuntimeWorkload,
    policy: &PeerPolicy,
) -> Vec<RuntimeValidationError> {
    let mut errors: Vec<RuntimeValidationError> = Vec::new();
    let violation = |detail: String| RuntimeValidationError::ComparisonIdentityViolation {
        workload: workload.id.clone(),
        detail,
    };
    let Some(comparison) = &observation.comparison else {
        if policy.records_comparison_identity {
            errors.push(violation(
                "this epoch pins the comparison configuration, and the observation records \
                 none; a peer row joined to it would be published against peer ports and \
                 peer versions nobody can name"
                    .to_string(),
            ));
        }
        // An epoch that does not pin it is one whose records predate it. They
        // stay valid, and the derive step joins them exactly as it did while
        // they were being collected.
        return errors;
    };
    if !is_sha256_digest(&comparison.identity_sha256) {
        errors.push(RuntimeValidationError::MalformedDigest {
            workload: workload.id.clone(),
            field: "comparison/identity_sha256".to_string(),
            found: comparison.identity_sha256.clone(),
        });
    }
    // EVERY declared peer, not only the ones this run measured. A canary-only
    // observation measures one tool and must still say what the others were
    // pinned at, or a bump whose full leg never succeeded leaves no trace in
    // the data at all.
    let declared: BTreeSet<&str> = policy.tools.iter().map(String::as_str).collect();
    let recorded: BTreeSet<&str> = comparison
        .peer_versions
        .keys()
        .map(String::as_str)
        .collect();
    if declared != recorded {
        errors.push(violation(format!(
            "the recorded peer versions cover {recorded:?}, but this epoch pins {declared:?}; \
             every declared peer's version rides every observation, including a canary-only \
             one that measured none of them"
        )));
    }
    for (tool, version) in &comparison.peer_versions {
        if version.trim().is_empty() {
            errors.push(violation(format!(
                "the recorded version of {tool:?} is empty; an unnamed pin is not a pin"
            )));
        }
    }
    for peer in &observation.peers {
        if let Some(pinned) = comparison.peer_versions.get(&peer.tool)
            && pinned != &peer.version
        {
            errors.push(violation(format!(
                "the {:?} observation reports version {:?}, but this run pinned {pinned:?}; a \
                 measurement and the configuration it was taken under cannot disagree",
                peer.tool, peer.version
            )));
        }
    }
    errors
}

/// Turn collected peer complaints into validation errors.
fn flush(
    errors: &mut Vec<RuntimeValidationError>,
    workload: &RuntimeWorkload,
    details: Vec<String>,
) {
    for detail in details {
        errors.push(RuntimeValidationError::PeerPolicyViolation {
            workload: workload.id.clone(),
            detail,
        });
    }
}

/// Whether an observation carries the same-run peer denominator its epoch asks
/// for (ADR-0072 Decision 9).
fn has_required_canary(
    observation: &RuntimeObservation,
    workload: &RuntimeWorkload,
    epoch: &RuntimeEpoch,
) -> bool {
    let Some(policy) = epoch.peers.get(&workload.id) else {
        return true;
    };
    observation.peers.iter().any(|peer| {
        peer.role == PeerRole::Canary
            && peer.tool == policy.canary_tool
            && peer.scale == policy.canary_scale
            && !peer.samples.is_empty()
    })
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

    // Which digest a verdict is a claim about depends on what the oracle
    // judges: stdout for a stdout golden, the emitted tree for a site oracle.
    // Reading stdout for gazette would compare two empty streams and report a
    // match while the 96 pages went unexamined.
    let kind = workload.oracle.kind;
    for (index, sample) in observation.samples.iter().enumerate() {
        if !is_sha256_digest(&sample.stdout_sha256) {
            errors.push(RuntimeValidationError::MalformedDigest {
                workload: workload.id.clone(),
                field: format!("samples/{index}/stdout_sha256"),
                found: sample.stdout_sha256.clone(),
            });
        }
        match kind.judged_digest(sample) {
            Some(digest) if is_sha256_digest(digest) => {}
            other => errors.push(RuntimeValidationError::MalformedDigest {
                workload: workload.id.clone(),
                field: format!("samples/{index}/{}", kind.judged_field()),
                found: other.unwrap_or("absent").to_string(),
            }),
        }
    }

    // Determinism is decided by the digests, never by the flag.
    let distinct: BTreeSet<&str> = observation
        .samples
        .iter()
        .map(|sample| kind.judged_digest(sample).unwrap_or_default())
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
    // Only a golden comparison claims that the two digests are equal. A site
    // oracle's `reference_sha256` is the identity of the JUDGE — the checked-in
    // comparison that read every emitted page — so requiring it to equal the
    // digest of the tree it judged would be requiring a program to equal its
    // own output. What the site oracle's verdict claims instead is checked
    // below: that every sample produced the tree that was judged.
    if kind == OracleKind::GoldenStdout && oracle.observed_sha256 != oracle.reference_sha256 {
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
        .position(|sample| kind.judged_digest(sample) != Some(oracle.observed_sha256.as_str()))
    {
        contradiction(
            errors,
            &workload.id,
            "verdict",
            format!(
                "a match was declared, but sample {index} produced {} rather than the judged {}",
                kind.judged_digest(&observation.samples[index])
                    .unwrap_or("nothing"),
                oracle.observed_sha256
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
kind = "seeded_generator"
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
        assert_eq!(workload.fixture.category(), InputCategory::Recorded);
        assert_eq!(workload.oracle.kind, OracleKind::GoldenStdout);
        let FixtureDeclaration::SeededGenerator { bytes, .. } = &workload.fixture else {
            panic!("wordfreq's fixture is one file from a seeded generator");
        };
        assert!(
            *bytes >= 16 * 1024 * 1024,
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
        assert_eq!(epoch.suite_revision, 3);
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
        assert_eq!(epoch.suite_revision, 3);
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

    #[test]
    fn every_collected_platform_has_a_local_reproduction_epoch() {
        // RUE-1498. A `local` epoch builds for its own `target` and then
        // EXECUTES what it built, so a matrix row whose target no local epoch
        // names can only be reproduced by pushing and waiting for CI. That was
        // the state this test closes: both `local` epochs targeted x86-64
        // Linux, which is not what this project's maintainers develop on.
        //
        // Stated as an invariant over whatever is collecting rather than as a
        // list of ids, because the failure it guards against is drift. Suite
        // revisions supersede one another while every earlier revision stays
        // declared forever, so a local epoch pinned to a retired revision goes
        // on parsing, goes on running, and quietly reproduces a contract the
        // matrix no longer measures. Matching the revision as well as the
        // target makes that a build break at the moment the new revision
        // arrives, which is when someone can still act on it.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        let collecting: Vec<&RuntimeEpoch> = manifest
            .epochs()
            .iter()
            .filter(|epoch| epoch.collection)
            .collect();
        assert!(
            !collecting.is_empty(),
            "the matrix collects nothing; this invariant would hold vacuously"
        );
        for reference in collecting {
            let local = manifest
                .epochs()
                .iter()
                .find(|epoch| {
                    epoch.platform == "local"
                        && epoch.target == reference.target
                        && epoch.suite_revision == reference.suite_revision
                })
                .unwrap_or_else(|| {
                    panic!(
                        "platform {} collects epoch {} at suite revision {} for target {}, but no \
                         `local` epoch reproduces that target at that revision; declare one with \
                         its own id, `collection = false`, and the `local` environment policy",
                        reference.platform,
                        reference.id,
                        reference.suite_revision,
                        reference.target
                    )
                });
            // The two fields that keep a laptop's numbers out of a reference
            // series. Asserted here rather than left to the manifest's prose:
            // a `local` epoch that collected, or one whose environment policy
            // admitted a hosted runner, would be a reproduction path that had
            // quietly become a collection row.
            assert!(
                !local.collection,
                "local epoch {} collects; a development epoch must not",
                local.id
            );
            assert_eq!(local.environment.runner_label, "local");
            assert_eq!(local.environment.runner_image, "local");
        }
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
                    tree: None,
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
                        artifact_sha256: None,
                    })
                    .collect(),
                peers: Vec::new(),
                comparison: None,
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
        assert_eq!(workload.fixture.category(), InputCategory::Recorded);
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
    fn a_workload_with_no_declared_bound_is_judged_by_no_rule() {
        // The alternative — a constant compiled into this crate — would answer
        // ADR-0072's open question 4 in the one place a maintainer cannot edit,
        // and would apply a threshold nobody chose to a workload nobody has
        // measured. No rule, no flag, and the dashboard says which.
        let manifest = manifest();
        let epoch = manifest.epoch("x86_64-linux", 1).unwrap();
        assert_eq!(epoch.flagging("wordfreq"), None);
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Advisory);
    }

    #[test]
    fn a_declared_bound_flags_while_staying_advisory() {
        // A maintainer may want movement visible before dispersion is measured.
        // That is a judgement, not a calibration, and the posture keeps saying
        // so.
        let text = format!("{MANIFEST}\n[epoch.flagging.wordfreq]\nk = 8.0\nwindow = 5\n");
        let manifest = RuntimeManifest::parse(&text).expect("manifest with a declared bound");
        let epoch = manifest.epoch("x86_64-linux", 1).unwrap();
        let rule = epoch.flagging("wordfreq").expect("a declared bound");
        assert_eq!(rule.k, 8.0);
        assert_eq!(rule.window, 5);
        assert_eq!(rule.posture, FlagPosture::Advisory);
        assert_eq!(rule.reference, None);
        assert_eq!(epoch.flag_posture("wordfreq"), FlagPosture::Advisory);
    }

    #[test]
    fn a_calibration_supersedes_a_declared_bound() {
        let text = format!(
            "{MANIFEST}\n[epoch.flagging.wordfreq]\nk = 8.0\nwindow = 5\n\
             [epoch.calibration.wordfreq]\nk = 4.5\nwindow = 12\n\
             reference = \"RUE-9999 runtime sweep\"\n"
        );
        let manifest = RuntimeManifest::parse(&text).expect("calibrated manifest");
        let rule = manifest
            .epoch("x86_64-linux", 1)
            .unwrap()
            .flagging("wordfreq")
            .expect("a calibrated rule");
        assert_eq!(rule.k, 4.5);
        assert_eq!(rule.window, 12);
        assert_eq!(rule.posture, FlagPosture::Calibrated);
        assert_eq!(rule.reference, Some("RUE-9999 runtime sweep"));
    }

    #[test]
    fn rejects_a_flagging_bound_that_can_never_flag() {
        let text = format!("{MANIFEST}\n[epoch.flagging.wordfreq]\nk = 3.0\nwindow = 0\n");
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("can never flag"), "{error}");
    }

    #[test]
    fn rejects_a_flagging_bound_for_a_workload_the_suite_does_not_declare() {
        let text = format!("{MANIFEST}\n[epoch.flagging.jsonfmt]\nk = 3.0\nwindow = 5\n");
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("undeclared workload"), "{error}");
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

    // -----------------------------------------------------------------------
    // The corpus-tree fixture shape, gazette's oracle, and the peer legs
    // (ADR-0072 Phase 5, RUE-1485).
    //
    // The first three tests below are the ones that matter most: generalizing a
    // declaration is exactly how a validator gets quietly weakened, so each
    // asserts that the new shape's relaxation applies to the new shape ALONE.
    // -----------------------------------------------------------------------

    const GAZETTE_MANIFEST: &str = r#"
schema_version = 1

[[suite]]
revision = 1
protocol_version = 1
measured_boundary = "spawn_to_exit_v1"

[[suite.workloads]]
id = "gazette"
source = "examples/gazette/main.rue"
question = "How fast does compiled Rue build the real site?"
program_args = ["build", "{fixture}", "-o", "{output}"]

[suite.workloads.fixture]
kind = "corpus_tree"
category = "recorded"
preparer = "scripts/gazette-corpus-diff.py"
preparer_revision = 1
scale = 1
excluded = ["performance.md", "runtime.md"]
root_name = "gazette-site"
description = "the live corpus"

[suite.workloads.oracle]
kind = "semantic_site_pages"
path = "scripts/gazette-corpus-diff.py"

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

[epoch.sampling.gazette]
samples = 3

[epoch.peers.gazette]
tools = ["zola", "hugo"]
primary_thread_policy = "pinned_single_thread"
secondary_thread_policy = "tool_default_parallel"
canary_tool = "zola"
canary_scale = 1
records_comparison_identity = true
"#;

    fn gazette_manifest() -> RuntimeManifest {
        RuntimeManifest::parse(GAZETTE_MANIFEST).expect("gazette manifest")
    }

    fn site_sample(elapsed: u64) -> RuntimeSample {
        RuntimeSample {
            process_elapsed_ns: elapsed,
            peak_memory_bytes: 8 * 1024 * 1024,
            exit_code: 0,
            stdout_bytes: 0,
            stdout_sha256: "e".repeat(64),
            artifact_sha256: Some("7".repeat(64)),
        }
    }

    /// A peer sample, taken by the same harness and the same clock as the Rue
    /// program's — and answering to the same sanity rules (RUE-1493).
    fn peer_sample(elapsed: u64, tree: &str) -> RuntimeSample {
        RuntimeSample {
            process_elapsed_ns: elapsed,
            peak_memory_bytes: 60 * 1024 * 1024,
            exit_code: 0,
            stdout_bytes: 64,
            stdout_sha256: "a".repeat(64),
            artifact_sha256: Some(tree.repeat(64)),
        }
    }

    /// The per-run canary: the epoch's own sample count, because a denominator
    /// taken under another sampling policy is not comparable with the numerator
    /// beside it.
    fn canary() -> PeerObservation {
        PeerObservation {
            tool: "zola".to_string(),
            version: "0.21.0".to_string(),
            role: PeerRole::Canary,
            thread_policy: PeerThreadPolicy::PinnedSingleThread,
            scale: 1,
            output_sha256: "8".repeat(64),
            emitted_files: 131,
            samples: (0..3)
                .map(|index| peer_sample(200_000_000 + index, "8"))
                .collect(),
        }
    }

    /// The comparison configuration a gazette observation is taken under.
    fn comparison() -> ComparisonIdentity {
        ComparisonIdentity {
            identity_sha256: "a".repeat(64),
            peer_port_revision: 3,
            peer_versions: BTreeMap::from([
                ("zola".to_string(), "0.21.0".to_string()),
                ("hugo".to_string(), "0.152.2".to_string()),
            ]),
        }
    }

    fn gazette_report() -> RuntimeReport {
        let mut report = report();
        report.identity.workload_source_hashes =
            BTreeMap::from([("gazette".to_string(), "3".repeat(64))]);
        report.workloads = vec![RuntimeObservation {
            workload: "gazette".to_string(),
            source: "examples/gazette/main.rue".to_string(),
            question: "How fast does compiled Rue build the real site?".to_string(),
            program_args: vec![
                "build".to_string(),
                FIXTURE_ARGUMENT.to_string(),
                "-o".to_string(),
                OUTPUT_ARGUMENT.to_string(),
            ],
            recorded_inputs: vec![RecordedInput {
                name: FIXTURE_INPUT_NAME.to_string(),
                category: InputCategory::Recorded,
                description: "the live corpus".to_string(),
                identity_sha256: "f".repeat(64),
                files: 130,
                bytes: 1_800_000,
                provenance: None,
                tree: Some(CorpusTreeProvenance {
                    preparer: "scripts/gazette-corpus-diff.py".to_string(),
                    preparer_revision: 1,
                    scale: 1,
                    excluded: vec!["performance.md".to_string(), "runtime.md".to_string()],
                }),
            }],
            program: ProgramIdentity {
                binary_bytes: 1_048_576,
                sha256: "b".repeat(64),
            },
            oracle: OracleOutcome {
                kind: OracleKind::SemanticSitePages,
                reference: "scripts/gazette-corpus-diff.py".to_string(),
                reference_sha256: "d".repeat(64),
                observed_sha256: "7".repeat(64),
                verdict: OracleVerdict::Match,
                deterministic_across_samples: true,
                detail: String::new(),
            },
            samples: (0..3)
                .map(|index| site_sample(2_000_000_000 + index))
                .collect(),
            peers: vec![canary()],
            comparison: Some(comparison()),
        }];
        report
    }

    #[test]
    fn a_corpus_tree_workload_is_appendable() {
        let outcome = validate_runtime_report(&gazette_manifest(), &gazette_report());
        assert_eq!(outcome.errors, Vec::new());
        assert_eq!(outcome.completeness, RuntimeCompleteness::Complete);
        assert!(outcome.publishes_workload("gazette"));
    }

    #[test]
    fn the_multi_file_relaxation_applies_to_the_corpus_tree_alone() {
        // The whole risk of this generalization in one test. A corpus tree may
        // record 130 files; wordfreq may not record 2, and RUE-1046's review —
        // which found nine of ten fabricated records being accepted — is the
        // reason. Both halves are asserted, because a change that relaxed the
        // seeded shape would still pass the first half alone.
        let mut tree = gazette_report();
        tree.workloads[0].recorded_inputs[0].files = 130;
        assert!(
            validate_runtime_report(&gazette_manifest(), &tree).is_appendable(),
            "a corpus tree is many files by definition"
        );

        let mut seeded = report();
        seeded.workloads[0].recorded_inputs[0].files = 130;
        assert!(
            validate_runtime_report(&manifest(), &seeded).errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::FixtureProvenanceMismatch { field, .. } if field == "files"
            )),
            "a seeded fixture is exactly one file, and still is"
        );
    }

    #[test]
    fn a_seeded_record_may_not_borrow_the_corpus_tree_s_shape() {
        // The other direction of the same risk: a wordfreq record that dropped
        // its generated provenance and claimed a tree provenance instead would,
        // under a declaration with every field optional, describe an input no
        // seed produced. Both substitutions are refused.
        let mut report = report();
        report.workloads[0].recorded_inputs[0].provenance = None;
        report.workloads[0].recorded_inputs[0].tree = Some(CorpusTreeProvenance {
            preparer: "scripts/gazette-corpus-diff.py".to_string(),
            preparer_revision: 1,
            scale: 1,
            excluded: Vec::new(),
        });
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::FixtureProvenanceMismatch { field, .. }
                    if field == "provenance"
            )),
            "{:?}",
            outcome.errors
        );
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::FixtureProvenanceMismatch { field, .. } if field == "tree"
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_corpus_tree_at_another_scale_is_a_different_workload() {
        // Recorded is not unpinned. The corpus's BYTES may move; the scale it
        // was duplicated to may not, or a 10x observation would land in the 1x
        // series and read as a tenfold regression.
        let mut report = gazette_report();
        report.workloads[0].recorded_inputs[0]
            .tree
            .as_mut()
            .unwrap()
            .scale = 10;
        assert!(validate_runtime_report(&gazette_manifest(), &report).errors.iter().any(
            |error| matches!(
                error,
                RuntimeValidationError::FixtureProvenanceMismatch { field, .. } if field == "scale"
            )
        ));
    }

    #[test]
    fn a_corpus_tree_that_excluded_another_page_is_a_different_workload() {
        let mut report = gazette_report();
        report.workloads[0].recorded_inputs[0]
            .tree
            .as_mut()
            .unwrap()
            .excluded
            .push("tutorial/_index.md".to_string());
        assert!(
            validate_runtime_report(&gazette_manifest(), &report)
                .errors
                .iter()
                .any(|error| matches!(
                    error,
                    RuntimeValidationError::FixtureProvenanceMismatch { field, .. }
                        if field == "excluded"
                ))
        );
    }

    #[test]
    fn a_site_oracle_judges_the_emitted_tree_rather_than_empty_stdout() {
        // Gazette writes nothing to stdout, so every sample's stdout digest is
        // the digest of nothing and agrees with every other. A verdict read
        // from stdout would therefore report determinism and a match for a run
        // whose 96 pages were never looked at. This is the case that proves the
        // verdict follows the ORACLE KIND: the trees differ, the stdouts do
        // not, and the record is refused.
        let mut report = gazette_report();
        report.workloads[0].samples[1].artifact_sha256 = Some("9".repeat(64));
        let distinct_stdout: BTreeSet<&str> = report.workloads[0]
            .samples
            .iter()
            .map(|sample| sample.stdout_sha256.as_str())
            .collect();
        assert_eq!(distinct_stdout.len(), 1, "the stdouts agree, as they will");
        let outcome = validate_runtime_report(&gazette_manifest(), &report);
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

    #[test]
    fn a_site_workload_with_no_artifact_digest_is_refused() {
        let mut report = gazette_report();
        report.workloads[0].samples[0].artifact_sha256 = None;
        let outcome = validate_runtime_report(&gazette_manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::MalformedDigest { field, .. }
                    if field.ends_with("artifact_sha256")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn an_oracle_of_the_wrong_kind_is_refused() {
        let mut report = gazette_report();
        report.workloads[0].oracle.kind = OracleKind::GoldenStdout;
        assert!(
            validate_runtime_report(&gazette_manifest(), &report)
                .errors
                .iter()
                .any(|error| matches!(error, RuntimeValidationError::OracleKindMismatch { .. }))
        );
    }

    #[test]
    fn an_observation_without_its_canary_publishes_nothing_but_is_kept() {
        // ADR-0072 Decision 9: the canary is the same-run ratio denominator. A
        // run that lost it holds a valid Rue measurement worth storing and a
        // ratio that cannot be computed, so the evidence is kept and no point
        // publishes — the same tiering a truncated observation gets.
        let mut report = gazette_report();
        report.workloads[0].peers.clear();
        let outcome = validate_runtime_report(&gazette_manifest(), &report);
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
        assert!(!outcome.publishes_workload("gazette"));
    }

    #[test]
    fn a_peer_measured_under_an_undeclared_thread_policy_is_refused() {
        // The fairness property the record has to carry. A canary taken with
        // Zola's default rayon pool on a four-vCPU runner would publish thread
        // count as though it were per-unit work.
        let mut report = gazette_report();
        report.workloads[0].peers[0].thread_policy = PeerThreadPolicy::ToolDefaultParallel;
        let outcome = validate_runtime_report(&gazette_manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::PeerPolicyViolation { detail, .. }
                    if detail.contains("primary ratio it anchors")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_peer_whose_samples_disagree_cannot_anchor_a_ratio() {
        // The determinism trap ADR-0072 Decision 4 names: Hugo's built-in feed
        // emits `lastBuildDate` and `generator`, so a port that kept it would
        // produce a different tree on every sample. Decided from the stored
        // digests rather than from any flag.
        let mut report = gazette_report();
        let second = RuntimeSample {
            artifact_sha256: Some("1".repeat(64)),
            ..report.workloads[0].peers[0].samples[0].clone()
        };
        report.workloads[0].peers[0].samples.push(second);
        let outcome = validate_runtime_report(&gazette_manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome.errors.iter().any(|error| matches!(
                error,
                RuntimeValidationError::PeerPolicyViolation { detail, .. }
                    if detail.contains("same tree twice")
            )),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn peer_observations_without_a_declared_policy_are_refused() {
        // wordfreq has no peers. A record carrying some would publish a ratio
        // against a tool this epoch never declared it would run.
        let mut report = report();
        report.workloads[0].peers.push(canary());
        let outcome = validate_runtime_report(&manifest(), &report);
        assert!(!outcome.is_appendable());
        assert!(
            outcome
                .errors
                .iter()
                .any(|error| matches!(error, RuntimeValidationError::PeerPolicyViolation { .. }))
        );
    }

    // -----------------------------------------------------------------------
    // RUE-1493: the peer evidence a published ratio rests on.
    //
    // Every test below constructs a record that WAS appendable and must not be.
    // The common shape is a peer observation that satisfies the one rule this
    // module used to check — every sample's artifact digest equals the recorded
    // output digest — while failing to be a measurement at all.
    // -----------------------------------------------------------------------

    /// A peer complaint mentioning `needle`, or a panic naming what was found.
    fn refused_because(outcome: &RuntimeValidationOutcome, needle: &str) {
        assert!(!outcome.is_appendable(), "the record was accepted");
        assert!(
            outcome
                .errors
                .iter()
                .any(|error| error.to_string().contains(needle)),
            "no error mentions {needle:?}: {:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_zero_length_peer_sample_cannot_anchor_a_ratio() {
        // The headline case. A one-sample canary whose process elapsed zero ns
        // used to be appendable — its artifact digest matched — and the derive
        // step would have published its median as the denominator of the
        // ratio, where a zero divides into anything.
        let mut report = gazette_report();
        report.workloads[0].peers[0].samples[0].process_elapsed_ns = 0;
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "elapsed zero ns",
        );
    }

    #[test]
    fn a_peer_build_that_failed_cannot_anchor_a_ratio() {
        // This runner aborts a peer leg on a non-zero exit and records a
        // failure rather than a short observation, so a record carrying one did
        // not come from it — which is exactly the producer appendability exists
        // to refuse.
        let mut report = gazette_report();
        report.workloads[0].peers[0].samples[1].exit_code = 2;
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "exited with code 2",
        );
    }

    #[test]
    fn a_peer_taken_under_another_sampling_policy_is_refused() {
        // The epoch declares three samples of this workload, and the peer legs
        // take the same count. A single-sample denominator has no spread and
        // was not taken under the policy the numerator beside it was.
        let mut report = gazette_report();
        report.workloads[0].peers[0].samples.truncate(1);
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "stores 1 sample(s) where this epoch's policy",
        );
    }

    #[test]
    fn a_peer_that_emitted_nothing_is_refused() {
        let mut report = gazette_report();
        report.workloads[0].peers[0].emitted_files = 0;
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "emitted no files",
        );
    }

    #[test]
    fn a_full_peer_row_at_another_scale_is_refused() {
        // The canary's scale was checked and a full row's was not, so a Hugo
        // row could claim to have built a corpus this workload never assembled
        // — a ratio dividing two different jobs.
        let mut report = gazette_report();
        report.workloads[0].peers.push(PeerObservation {
            tool: "hugo".to_string(),
            version: "0.152.2".to_string(),
            role: PeerRole::Full,
            thread_policy: PeerThreadPolicy::PinnedSingleThread,
            scale: 10,
            output_sha256: "9".repeat(64),
            emitted_files: 131,
            samples: (0..3)
                .map(|index| peer_sample(300_000_000 + index, "9"))
                .collect(),
        });
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "this workload's fixture is the 1x one",
        );
    }

    #[test]
    fn two_rows_claiming_one_configuration_are_refused() {
        // The configuration key is (tool, scale, thread policy), checked as a
        // unit: two rows sharing it are two denominators for one cell of the
        // published table, and the derive step's deduplication would silently
        // keep whichever it reached first.
        //
        // A DIFFERING ROLE DOES NOT SEPARATE THEM, and that is the case this
        // test is really about. The table keys a row on tool and thread policy
        // and nothing else, so a canary and a full row of the same tool at the
        // same policy collapse into one cell — array order deciding which
        // median publishes and which is silently discarded. Cloning the canary
        // as a full row with a ten-times-slower measurement was accepted before
        // role left the key.
        let mut report = gazette_report();
        let slow = PeerObservation {
            role: PeerRole::Full,
            samples: (0..3)
                .map(|index| peer_sample(2_000_000_000 + index, "8"))
                .collect(),
            ..report.workloads[0].peers[0].clone()
        };
        report.workloads[0].peers.push(slow);
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "two observations claim the same configuration",
        );
    }

    #[test]
    fn a_parallel_row_that_built_a_different_site_is_refused() {
        // ADR-0072 Decision 4 for the SECONDARY row (RUE-1493). Only the
        // primary-policy tree reaches the semantic oracle, so a deterministic
        // parallel build emitting a different site satisfied every check there
        // was and published as the number a user would actually experience. It
        // is the same tool on the same corpus, so its digest must equal the one
        // the oracle judged.
        let mut report = gazette_report();
        report.workloads[0].peers.push(PeerObservation {
            tool: "zola".to_string(),
            version: "0.21.0".to_string(),
            role: PeerRole::Full,
            thread_policy: PeerThreadPolicy::ToolDefaultParallel,
            scale: 1,
            output_sha256: "9".repeat(64),
            emitted_files: 131,
            samples: (0..3)
                .map(|index| peer_sample(100_000_000 + index, "9"))
                .collect(),
        });
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "did different work",
        );

        // And the same row agreeing with its judged primary build is fine,
        // which is what makes the check about work equivalence rather than
        // about there being a secondary row at all.
        let mut agreeing = gazette_report();
        agreeing.workloads[0].peers.push(PeerObservation {
            tool: "zola".to_string(),
            version: "0.21.0".to_string(),
            role: PeerRole::Full,
            thread_policy: PeerThreadPolicy::ToolDefaultParallel,
            scale: 1,
            output_sha256: "8".repeat(64),
            emitted_files: 131,
            samples: (0..3)
                .map(|index| peer_sample(100_000_000 + index, "8"))
                .collect(),
        });
        let outcome = validate_runtime_report(&gazette_manifest(), &agreeing);
        assert_eq!(outcome.errors, Vec::new());
    }

    #[test]
    fn a_parallel_row_with_no_judged_primary_build_is_refused() {
        // The tree of a secondary row is never handed to the oracle, so
        // without its own tool's primary-policy build beside it there is
        // nothing its digest could have been judged against.
        let mut report = gazette_report();
        report.workloads[0].peers = vec![
            canary(),
            PeerObservation {
                tool: "hugo".to_string(),
                version: "0.152.2".to_string(),
                role: PeerRole::Full,
                thread_policy: PeerThreadPolicy::ToolDefaultParallel,
                scale: 1,
                output_sha256: "9".repeat(64),
                emitted_files: 131,
                samples: (0..3)
                    .map(|index| peer_sample(100_000_000 + index, "9"))
                    .collect(),
            },
        ];
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "never judged against anything",
        );
    }

    #[test]
    fn an_observation_that_records_no_comparison_configuration_is_refused() {
        // ADR-0072 Decision 2 as amended: the epoch pins the identity a joined
        // peer row is published against, so an observation that records none
        // cannot be joined to and is not appendable under that epoch.
        let mut report = gazette_report();
        report.workloads[0].comparison = None;
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "the observation records none",
        );
    }

    #[test]
    fn a_canary_only_observation_records_every_peer_version() {
        // THE DATA GAP RUE-1493 NAMES. A canary-only run measures one tool, and
        // recording only that tool's version is how a Hugo bump whose full leg
        // failed became invisible: nothing in the store said which Hugo the run
        // pinned, so nothing could say the joined row's Hugo was no longer it.
        let mut report = gazette_report();
        report.workloads[0]
            .comparison
            .as_mut()
            .unwrap()
            .peer_versions
            .remove("hugo");
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "every declared peer's version rides every observation",
        );
    }

    #[test]
    fn a_peer_row_may_not_disagree_with_the_pin_it_ran_under() {
        let mut report = gazette_report();
        report.workloads[0].peers[0].version = "0.20.0".to_string();
        refused_because(
            &validate_runtime_report(&gazette_manifest(), &report),
            "this run pinned",
        );
    }

    #[test]
    fn a_comparison_identity_without_a_peer_policy_is_refused() {
        // The symmetric rule to peer observations without a policy: wordfreq
        // has no comparison to configure, so a record describing one is
        // describing a leg its epoch never declared.
        let mut report = report();
        report.workloads[0].comparison = Some(comparison());
        refused_because(
            &validate_runtime_report(&manifest(), &report),
            "there is no comparison to configure",
        );
    }

    #[test]
    fn a_record_from_an_epoch_that_predates_the_comparison_identity_still_validates() {
        // THE APPEND-ONLY RULE. Records taken before the comparison identity
        // existed carry none and never could, and the store cannot delete them.
        // An epoch that does not pin the identity therefore keeps accepting
        // records without one — the same mechanism that retired epoch 1 in
        // RUE-1485 while keeping it declared, so its records stayed valid.
        let text = GAZETTE_MANIFEST.replace("records_comparison_identity = true\n", "");
        let earlier = RuntimeManifest::parse(&text).expect("an epoch that pins no identity");
        let mut report = gazette_report();
        report.workloads[0].comparison = None;
        let outcome = validate_runtime_report(&earlier, &report);
        assert_eq!(outcome.errors, Vec::new());
        assert!(outcome.publishes_workload("gazette"));
    }

    #[test]
    fn a_fixture_table_still_refuses_unknown_fields_after_being_tagged() {
        // The tagging must not have bought its flexibility by accepting
        // anything. A key nobody reads in a fixture declaration is how a
        // maintainer comes to believe a pin is in force that is not: a
        // misspelled `vocabulary_sizes` would sit in the manifest looking
        // authoritative while the generator ran on a default.
        let text = MANIFEST.replace(
            "file_name = \"input.txt\"",
            "file_name = \"input.txt\"\nvocabulary_sizes = 512",
        );
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");

        // And a field belonging to the OTHER shape is exactly as unknown, so
        // neither declaration can quietly grow the other's fields.
        let text = GAZETTE_MANIFEST.replace(
            "preparer_revision = 1",
            "preparer_revision = 1\nseed = 20260813",
        );
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn a_manifest_pairing_a_site_fixture_with_a_stdout_oracle_is_refused() {
        let text =
            GAZETTE_MANIFEST.replace("kind = \"semantic_site_pages\"", "kind = \"golden_stdout\"");
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("corpus-tree workload is judged"), "{error}");
    }

    #[test]
    fn a_site_workload_that_is_never_told_where_to_write_is_refused() {
        let text = GAZETTE_MANIFEST.replace(
            r#"program_args = ["build", "{fixture}", "-o", "{output}"]"#,
            r#"program_args = ["build", "{fixture}"]"#,
        );
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("never receives a destination"), "{error}");
    }

    #[test]
    fn a_canary_outside_the_declared_peer_tools_is_refused_by_the_manifest() {
        let text =
            GAZETTE_MANIFEST.replace(r#"canary_tool = "zola""#, r#"canary_tool = "eleventy""#);
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("not one of its peer tools"), "{error}");
    }

    #[test]
    fn a_secondary_row_that_repeats_the_primary_is_refused() {
        let text = GAZETTE_MANIFEST.replace(
            r#"secondary_thread_policy = "tool_default_parallel""#,
            r#"secondary_thread_policy = "pinned_single_thread""#,
        );
        let error = RuntimeManifest::parse(&text).unwrap_err();
        assert!(error.contains("informs nobody"), "{error}");
    }

    #[test]
    fn the_checked_in_manifest_measures_gazette_against_pinned_peers() {
        // The Phase 5 entry itself: gazette in the collected suite, with the
        // thread parity and the per-run canary ADR-0072 Decisions 5 and 9
        // require, and advisory flags because no platform inherits calibration.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        for platform in ["x86_64-linux", "aarch64-linux", "aarch64-macos"] {
            let epoch = manifest
                .collection_epoch(platform)
                .unwrap_or_else(|| panic!("{platform} collects"));
            let suite = manifest.suite(epoch.suite_revision).expect("suite");
            let workload = suite.workload("gazette").expect("gazette is measured");
            let FixtureDeclaration::CorpusTree {
                scale, excluded, ..
            } = &workload.fixture
            else {
                panic!("gazette's fixture is a corpus tree");
            };
            assert_eq!(*scale, 1);
            assert_eq!(excluded, &["performance.md", "runtime.md"]);
            assert_eq!(workload.oracle.kind, OracleKind::SemanticSitePages);

            let peers = epoch.peers.get("gazette").expect("a peer policy");
            assert_eq!(peers.tools, vec!["zola", "hugo"]);
            assert_eq!(
                peers.primary_thread_policy,
                PeerThreadPolicy::PinnedSingleThread,
                "the primary published ratio pins the peers to one worker thread"
            );
            assert_eq!(
                peers.secondary_thread_policy,
                Some(PeerThreadPolicy::ToolDefaultParallel),
                "the peers' default parallel configuration is published, not hidden"
            );
            assert_eq!(peers.canary_tool, "zola");
            assert_eq!(peers.canary_scale, 1);
            // Each rung's peers build that rung's own corpus, so the 10x
            // workload's canary is a 10x peer build rather than a 1x one
            // borrowed from the row above.
            assert_eq!(epoch.peers["gazette_10x"].canary_scale, 10);
            assert_eq!(epoch.flag_posture("gazette"), FlagPosture::Advisory);
            // The 10x rung is measured for Rue; the 100x one is the safety
            // valve of Decision 9 and is deliberately not collected yet.
            assert!(suite.workload("gazette_10x").is_some());
            assert!(suite.workload("gazette_100x").is_none());
        }
    }

    /// A complete report against the CHECKED-IN manifest, as the runner writes
    /// one: every declared workload, the canary each peer policy requires, and
    /// the fixture provenance the suite revision pins.
    ///
    /// Parameterized by epoch because that is the whole point of it. The store
    /// is append-only, so a record written under a retired epoch has to go on
    /// validating against the manifest as it stands today.
    fn shipped_report(epoch: u32, suite_revision: u32, preparer_revision: u32) -> RuntimeReport {
        let comparison = (epoch >= 3).then(comparison);
        let corpus = |scale: u32, samples: u32| RuntimeObservation {
            workload: if scale == 1 {
                "gazette".to_string()
            } else {
                format!("gazette_{scale}x")
            },
            source: "examples/gazette/main.rue".to_string(),
            question: "How fast does a compiled Rue program build the real rue-lang.dev site, \
                       against the production tools that build sites for a living?"
                .to_string(),
            program_args: vec![
                "build".to_string(),
                FIXTURE_ARGUMENT.to_string(),
                "-o".to_string(),
                OUTPUT_ARGUMENT.to_string(),
            ],
            recorded_inputs: vec![RecordedInput {
                name: FIXTURE_INPUT_NAME.to_string(),
                category: InputCategory::Recorded,
                description: "the live corpus".to_string(),
                identity_sha256: "f".repeat(64),
                files: 130 * u64::from(scale),
                bytes: 1_800_000 * u64::from(scale),
                provenance: None,
                tree: Some(CorpusTreeProvenance {
                    preparer: "scripts/gazette-corpus-diff.py".to_string(),
                    preparer_revision,
                    scale,
                    excluded: vec!["performance.md".to_string(), "runtime.md".to_string()],
                }),
            }],
            program: ProgramIdentity {
                binary_bytes: 1_048_576,
                sha256: "b".repeat(64),
            },
            oracle: OracleOutcome {
                kind: OracleKind::SemanticSitePages,
                reference: "scripts/gazette-corpus-diff.py".to_string(),
                reference_sha256: "d".repeat(64),
                observed_sha256: "7".repeat(64),
                verdict: OracleVerdict::Match,
                deterministic_across_samples: true,
                detail: String::new(),
            },
            samples: (0..u64::from(samples))
                .map(|index| site_sample(2_000_000_000 + index))
                .collect(),
            peers: vec![PeerObservation {
                tool: "zola".to_string(),
                version: "0.21.0".to_string(),
                role: PeerRole::Canary,
                thread_policy: PeerThreadPolicy::PinnedSingleThread,
                scale,
                output_sha256: "8".repeat(64),
                emitted_files: 131,
                samples: (0..u64::from(samples))
                    .map(|index| peer_sample(200_000_000 + index, "8"))
                    .collect(),
            }],
            comparison: comparison.clone(),
        };

        let mut report = report();
        report.identity.suite_revision = suite_revision;
        report.identity.epoch = epoch;
        report.identity.workload_source_hashes = BTreeMap::from([
            ("wordfreq".to_string(), "3".repeat(64)),
            ("gazette".to_string(), "4".repeat(64)),
            ("gazette_10x".to_string(), "4".repeat(64)),
        ]);
        report.regime.target = "x86-64-linux".to_string();
        report.workloads[0].recorded_inputs[0].bytes = 33_554_432;
        report.workloads[0]
            .recorded_inputs
            .iter_mut()
            .for_each(|input| {
                if let Some(provenance) = input.provenance.as_mut() {
                    provenance.vocabulary_size = 65_536;
                }
            });
        report.workloads.push(corpus(1, 5));
        report.workloads.push(corpus(10, 3));
        report
            .workloads
            .sort_by(|left, right| left.workload.cmp(&right.workload));
        report
    }

    #[test]
    fn a_record_from_the_retired_epoch_validates_beside_one_from_the_new_epoch() {
        // THE APPEND-ONLY EVIDENCE, against the manifest the repository ships
        // rather than a fixture written to agree with it. RUE-1485 proved this
        // shape by deriving an epoch-1 record beside two epoch-2 records with
        // none rejected; RUE-1493 changes what an observation RECORDS, so the
        // same proof is owed again.
        //
        // Epoch 2's records carry preparer revision 1 and no comparison
        // identity, because neither existed when they were written. Epoch 3's
        // carry revision 2 and the identity, because its peer policy pins it.
        // Both must be appendable, and both must publish, or this change would
        // have invalidated evidence nobody can rewrite.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        for (epoch, suite_revision, preparer_revision) in [(2, 2, 1), (3, 3, 2)] {
            let report = shipped_report(epoch, suite_revision, preparer_revision);
            let outcome = validate_runtime_report(&manifest, &report);
            assert_eq!(
                outcome.errors,
                Vec::new(),
                "the epoch-{epoch} record was refused"
            );
            assert_eq!(outcome.completeness, RuntimeCompleteness::Complete);
            for workload in ["wordfreq", "gazette", "gazette_10x"] {
                assert!(
                    outcome.publishes_workload(workload),
                    "epoch {epoch} publishes {workload}"
                );
            }
        }
    }

    #[test]
    fn the_new_epoch_refuses_a_record_written_without_the_comparison_identity() {
        // The other half of the same rule: epoch 3 pins the identity, so a
        // runner that skipped it is refused HERE rather than at the point
        // someone reads a joined row and cannot say which peer pins it names.
        let manifest = RuntimeManifest::parse(CHECKED_IN_MANIFEST).expect("checked-in manifest");
        let mut report = shipped_report(3, 3, 2);
        for observation in &mut report.workloads {
            observation.comparison = None;
        }
        refused_because(
            &validate_runtime_report(&manifest, &report),
            "the observation records none",
        );
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
