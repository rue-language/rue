//! Deriving everything the dashboard shows from the raw records.
//!
//! The data branch stores raw observations and nothing else, so every figure on
//! the page — indexes, medians, dispersion, flags, epoch boundaries — is
//! recomputed here at site build time. A derived value stored alongside the
//! records would eventually disagree with them, and the stored copy is the one
//! that would be believed.
//!
//! This lives in the runner rather than the site build so that it goes through
//! `rue_perf_schema::stats`, the same code the calibrator uses. Two
//! implementations of the flagging rule would drift, and the one that drifted
//! would be the one declaring regressions.
//!
//! Nothing here decides whether a run is trustworthy. Runs are put through
//! `validate_run` exactly as collection does, and anything unappendable is
//! excluded from every series while still being counted in collection health —
//! broken collection should be visible on the page, not absent from it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rue_perf_schema::{
    Band, Completeness, FIXTURE_INPUT_NAME, Manifest, Metric, PeerRole, PeerThreadPolicy,
    RunObject, RuntimeCompleteness, RuntimeManifest, RuntimeMetric, RuntimeValidationOutcome,
    StoredRun, StoredRuntimeReport, Summary, flags_movement, geometric_mean,
    median_absolute_deviation, ratio, sample_value, summarize, validate_run,
    validate_runtime_report,
};
use serde::Serialize;

/// Everything the dashboard renders, for every platform.
#[derive(Debug, Serialize)]
pub struct SiteData {
    /// The bands of the additive stack, in presentation order.
    pub bands: Vec<String>,
    /// Workload identifiers with the question each one answers.
    pub workloads: Vec<WorkloadDescription>,
    /// One entry per platform that has any observation.
    pub platforms: Vec<PlatformData>,
    /// Runs that exist but could not enter any series, with the reason.
    ///
    /// Surfaced rather than dropped: a run rejected for a pin mismatch is the
    /// single most useful thing to see when the page stops updating.
    pub rejected: Vec<RejectedRun>,
    /// The ADR-0072 runtime series, when a runtime manifest was supplied.
    ///
    /// `None` rather than an empty object when the caller asked for no runtime
    /// derivation at all: a page must be able to tell "not requested" from
    /// "requested and nothing has been collected yet".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeData>,
}

/// Everything a runtime view renders.
///
/// Deliberately a sibling of the compile-time series rather than a member of
/// it. The two answer different questions — how fast Rue compiles, and how fast
/// compiled Rue runs — over different record kinds, and merging them would
/// invite a chart that plots one against the other.
#[derive(Debug, Serialize)]
pub struct RuntimeData {
    /// Measured programs with the question each one answers.
    pub workloads: Vec<RuntimeWorkloadDescription>,
    /// Published runtime metrics, in presentation order.
    pub metrics: Vec<String>,
    /// One entry per platform that has any observation.
    pub platforms: Vec<RuntimePlatformData>,
    /// Reports that exist but could not enter any series, with the reason.
    pub rejected: Vec<RejectedRuntimeReport>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeWorkloadDescription {
    pub id: String,
    pub source: String,
    pub question: String,
}

#[derive(Debug, Serialize)]
pub struct RejectedRuntimeReport {
    pub report: String,
    pub platform: String,
    pub epoch: u32,
    pub commit: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RuntimePlatformData {
    pub platform: String,
    /// Epoch stretches, oldest first.
    pub epochs: Vec<RuntimeEpochData>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeEpochData {
    pub epoch: u32,
    pub suite_revision: u32,
    /// What the programs were built as, so a reader can see that the series
    /// measures the release-quality product rather than a debug build.
    pub optimization: String,
    pub thread_policy: String,
    pub hardware_counters: String,
    /// The bound each workload's movement is judged against, keyed by workload.
    ///
    /// Published so the page can state the rule it is reporting against instead
    /// of asserting a verdict, exactly as the compile-time epochs publish
    /// theirs.
    pub flagging: BTreeMap<String, RuntimeFlaggingRule>,
    /// Notable events within this epoch, oldest first.
    ///
    /// Corpus and workload-source changes are the ones that matter most: each is
    /// a discontinuity across which a raw median means something different, and
    /// each opens the next segment.
    pub annotations: Vec<RuntimeAnnotation>,
    /// Points in measurement order.
    pub points: Vec<RuntimePointData>,
    /// The cross-tool comparison from the newest run that measured peers.
    ///
    /// Absent when no peer measurement exists in this epoch, and the page then
    /// renders the honest empty state rather than an estimate. Never a mixture
    /// of runs: a ratio whose two sides came from different commits on a hosted
    /// runner is a ratio of two different machines' moods, which is exactly
    /// what ADR-0072 Decision 9's per-run canary exists to prevent.
    pub comparison: Option<RuntimeComparison>,
}

/// The cross-tool table one run produced.
#[derive(Debug, Serialize)]
pub struct RuntimeComparison {
    /// The commit whose run these rows come from.
    pub commit: String,
    /// The fixture identity every row in the table was measured against.
    pub fixture: String,
    /// Rows, Rue first and then its peers, ascending by corpus scale.
    pub rows: Vec<RuntimeComparisonRow>,
}

/// One row of the cross-tool comparison.
#[derive(Debug, Serialize)]
pub struct RuntimeComparisonRow {
    /// The tool, as a reader knows it.
    pub tool: String,
    /// The corpus scale this row built, `1x` or `10x`.
    pub scale: String,
    /// The thread configuration, spelled for a reader rather than as an enum.
    pub threads: String,
    /// Median whole-process wall time, in nanoseconds.
    pub median_ns: u64,
    /// This row's median over the Rue program's median at the same scale.
    ///
    /// `None` on the Rue rows themselves and wherever the same-scale
    /// denominator is missing — never a 1.0 standing in for "unknown".
    pub ratio: Option<f64>,
    /// The tool's version.
    pub version: String,
    /// Whether this row is the primary published ratio or the labelled
    /// secondary one (ADR-0072 Decision 5).
    pub secondary: bool,
    /// The commit whose run this row was measured in.
    pub commit: String,
    /// Whether this row was joined from an earlier full peer leg rather than
    /// measured in the same run as the Rue figure it is compared against.
    ///
    /// Published per row rather than inferred, because the two kinds of row
    /// carry different weight: a same-run ratio controls for the machine, and a
    /// joined one only controls for the input.
    pub joined: bool,
}

/// The rule one workload's movement is judged against, as published.
#[derive(Debug, Serialize)]
pub struct RuntimeFlaggingRule {
    pub k: f64,
    pub window: u32,
    /// `calibrated` or `advisory`.
    pub posture: String,
    /// The reviewed analysis behind the constants, empty while advisory.
    pub reference: String,
}

/// One notable event on a runtime series.
///
/// ADR-0072 Decision 2 asks for corpus changes to appear as annotated events
/// alongside compiler releases and peer toolchain bumps. The first three kinds
/// are derivable from the records this store already holds; peer bumps arrive
/// with the peer leg (RUE-1485) and have no kind here yet, because inventing an
/// empty one would suggest the page had looked and found none.
#[derive(Debug, Serialize)]
pub struct RuntimeAnnotation {
    /// `corpus_change`, `workload_change`, or `compiler_release`.
    pub kind: String,
    /// The workload the event belongs to, empty when it belongs to the run.
    pub workload: String,
    /// The first commit measured after the change.
    pub commit: String,
    pub finished_at: String,
    /// The segment this event opened, for the two kinds that open one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<u32>,
    /// What changed, in the terms a reader needs.
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct RuntimePointData {
    /// The record's immutable name.
    pub report: String,
    /// The compiler revision that built the measured programs.
    pub commit: String,
    /// The compiler's own version string.
    pub compiler_version: String,
    pub finished_at: String,
    /// Whether every declared workload completed.
    pub complete: bool,
    /// Per-workload figures, keyed by workload id.
    pub workloads: BTreeMap<String, RuntimeWorkloadPoint>,
    /// What went wrong during the run, whether or not it stopped the report
    /// entering the series.
    ///
    /// The record has carried these since Phase 1 and nothing read them. That
    /// was survivable while every failure either sank the report — where the
    /// reason surfaces under `rejected` — or lost the canary, which shows as an
    /// unpublished point. RUE-1493 added a third kind: a peer row refused for
    /// work equivalence is dropped from a report that stays complete and
    /// publishable, so without this the row would simply be absent from the
    /// table with nothing anywhere saying why.
    pub failures: Vec<RuntimeFailureNote>,
}

/// One thing that went wrong during a run, as the page renders it.
#[derive(Debug, Serialize)]
pub struct RuntimeFailureNote {
    /// Which kind of failure, in the record's own vocabulary.
    pub kind: String,
    /// The workload it belongs to.
    pub workload: String,
    /// The evidence the runner recorded.
    pub detail: String,
}

impl RuntimeFailureNote {
    fn of(failure: &rue_perf_schema::RuntimeFailure) -> RuntimeFailureNote {
        use rue_perf_schema::RuntimeFailure;
        let (kind, detail) = match failure {
            RuntimeFailure::CompileFailed { detail, .. } => ("compile_failed", detail.clone()),
            RuntimeFailure::FixturePreparationFailed { detail, .. } => {
                ("fixture_preparation_failed", detail.clone())
            }
            RuntimeFailure::ProgramCrashed {
                sample_index,
                detail,
                ..
            } => (
                "program_crashed",
                format!("sample {sample_index}: {detail}"),
            ),
            RuntimeFailure::WrongOutput { detail, .. } => ("wrong_output", detail.clone()),
            RuntimeFailure::ValidationRejected { detail, .. } => {
                ("validation_rejected", detail.clone())
            }
        };
        RuntimeFailureNote {
            kind: kind.to_string(),
            workload: failure.workload().to_string(),
            detail,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RuntimeWorkloadPoint {
    /// Digest of the input this observation consumed.
    ///
    /// The recorded-input category made visible: a raw median may only be
    /// compared with another point carrying the same identity, and a change
    /// here is a discontinuity in the series rather than a movement in it.
    pub fixture_identity: String,
    /// Size of that input, in bytes.
    pub fixture_bytes: u64,
    /// Digest of the workload's own source closure at this point.
    ///
    /// The other half of the recorded-not-pinned bargain, and the reason it is
    /// a bargain rather than a gap. This suite records the program's identity
    /// instead of pinning it — the same choice `performance/scaling.toml` makes
    /// for the maintained examples it measures — which is only defensible if a
    /// consumer can see when it moved. Both identities segment the series the
    /// same way: raw medians are comparable within a segment and not across
    /// one.
    pub source_identity: String,
    /// `calibrated` or `advisory`. Movement in an advisory workload is a
    /// triage item, never a gate.
    pub flag_posture: String,
    /// Which identity-matched stretch of this workload's series the point is in.
    ///
    /// The whole of ADR-0072 Decision 9 reduced to one number a chart can draw
    /// with. Segments start at 0 and advance whenever `fixture_identity` or
    /// `source_identity` differs from the preceding point's, so two points share
    /// a segment exactly when their raw medians are on the same scale. A line
    /// drawn across a boundary would publish a false trend; a comparison taken
    /// across one would report an input change as a compiler change.
    pub segment: u32,
    /// Whether this point moved beyond its epoch's bound, within its segment.
    ///
    /// `None` when the trailing window is not full — including immediately
    /// after a discontinuity, where the window restarts because the prior
    /// points measure a different input. Too little history is not stability.
    pub flagged: Option<bool>,
    /// The trailing in-segment window's median wall time, which `flagged`
    /// compares against. `None` exactly when `flagged` is.
    pub window_median_ns: Option<u64>,
    /// Median and dispersion per metric, keyed by metric wire name.
    pub metrics: BTreeMap<String, SummaryData>,
}

#[derive(Debug, Serialize)]
pub struct SummaryData {
    pub median: u64,
    pub mad: u64,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct WorkloadDescription {
    pub id: String,
    pub question: String,
    /// The scaling probe's declared axis and size, when the workload is one
    /// (RUE-1264). Published so the ratio between two sizes of a probe is
    /// recoverable from this record rather than by parsing workload ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaling: Option<ScalingDescription>,
}

/// A scaling probe's axis position, as published to the dashboard.
#[derive(Debug, Serialize)]
pub struct ScalingDescription {
    pub axis: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct RejectedRun {
    pub run: String,
    pub platform: String,
    pub epoch: u32,
    pub commit: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PlatformData {
    pub platform: String,
    /// Epoch stretches, oldest first.
    ///
    /// Rendered as separate continuous stretches with labelled boundaries. The
    /// index is normalized against a per-epoch baseline, so joining two epochs
    /// into one line would draw a discontinuity as if it were a change in the
    /// compiler.
    pub epochs: Vec<EpochData>,
}

#[derive(Debug, Serialize)]
pub struct EpochData {
    pub epoch: u32,
    pub suite_revision: u32,
    /// The commit whose medians define ratio 1.0, once one exists.
    pub baseline_commit: Option<String>,
    /// Points in measurement order.
    pub points: Vec<PointData>,
    /// Fingerprint changes within this epoch.
    ///
    /// The epoch pins an environment *policy*, not a machine, so hosted runners
    /// change underneath it. Comparisons crossing one of these are advisory.
    pub environment_annotations: Vec<EnvironmentAnnotation>,
    /// The flagging rule this epoch pins, so the page can state the bound it
    /// is reporting against rather than describing it vaguely.
    pub flagging: FlaggingRule,
    /// Absolute process-time targets whose gate this epoch adjudicates.
    ///
    /// Kept per epoch because hosted platform clocks are not interchangeable.
    pub process_elapsed_targets: BTreeMap<String, ProcessElapsedTargetData>,
    /// Noise-aware non-regression ratchets for this reference epoch.
    pub process_elapsed_ratchets: BTreeMap<String, ProcessElapsedRatchetData>,
    /// Standard-library changes within this epoch.
    ///
    /// Deliberately *not* advisory, unlike an environment annotation. `std` is
    /// part of the product being measured, so a change here is a real movement
    /// in what the series tracks — the same status as a compiler change, which
    /// gets no annotation only because every point already is one.
    pub stdlib_annotations: Vec<StdlibAnnotation>,
}

/// The epoch's pinned flagging rule, as published to the dashboard.
#[derive(Debug, Serialize)]
pub struct FlaggingRule {
    /// The multiplier applied to the pooled uncertainty of the two summaries.
    pub k: f64,
    /// How many prior runs form the trailing window.
    pub window: u32,
}

/// One reviewed external latency target rendered by the dashboard.
#[derive(Debug, Serialize)]
pub struct ProcessElapsedTargetData {
    pub process_elapsed_ns: u64,
    pub reference: String,
}

/// One pinned fresh-process non-regression ratchet rendered by the dashboard.
#[derive(Debug, Serialize)]
pub struct ProcessElapsedRatchetData {
    pub baseline_process_elapsed_ns: u64,
    pub baseline_mad_ns: u64,
    pub process_elapsed_limit_ns: u64,
    pub reference_run: String,
}

/// A point at which the standard library changed underneath a series.
#[derive(Debug, Serialize)]
pub struct StdlibAnnotation {
    /// The first commit measured against the new standard library.
    pub commit: String,
    pub finished_at: String,
    /// The resolved hashes either side of the change, for provenance.
    pub previous: String,
    pub current: String,
}

#[derive(Debug, Serialize)]
pub struct EnvironmentAnnotation {
    /// The first commit measured on the new fingerprint.
    pub commit: String,
    pub finished_at: String,
    /// A human-readable summary of what changed.
    pub changed: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PointData {
    pub run: String,
    pub commit: String,
    pub finished_at: String,
    /// Fingerprint identity, so the page can mark advisory comparisons.
    pub environment: String,
    /// Whether every suite workload completed validly.
    ///
    /// A headline index publishes only from a complete run; per-workload
    /// observations publish regardless.
    pub complete: bool,
    /// Suite workloads that did not complete validly.
    pub missing: Vec<String>,
    /// The dimensionless headline index per metric, when publishable.
    ///
    /// Absent when the run is partial or the epoch has no baseline yet. Never a
    /// wall-clock quantity, and never drawn stacked with milliseconds.
    pub index: Option<BTreeMap<String, f64>>,
    /// Per-workload figures, keyed by workload.
    pub workloads: BTreeMap<String, WorkloadPoint>,
}

#[derive(Debug, Serialize)]
pub struct WorkloadPoint {
    /// Median per-compilation latency.
    pub median_ns: u64,
    /// Median absolute deviation, the dispersion the flagging rule uses.
    pub mad_ns: u64,
    pub peak_memory_bytes: u64,
    pub output_binary_bytes: u64,
    /// Ratio against the epoch baseline, when one exists.
    pub ratio: Option<f64>,
    /// Whether this workload moved by more than the epoch's flagging rule.
    ///
    /// `None` when the trailing window is not yet full: too little history is
    /// not the same as no movement, and rendering it as "fine" would be a lie.
    pub flagged: Option<bool>,
    /// The trailing window's median, which `flagged` is the comparison against.
    ///
    /// Published so the page can say what a flag actually means — how far this
    /// point sits from the window, and in which direction — rather than
    /// asserting "flagged" and leaving the reader to guess. `None` exactly when
    /// `flagged` is.
    pub window_median_ns: Option<u64>,
    /// The additive stack, in absolute nanoseconds, summing to compiler root.
    pub bands_ns: BTreeMap<String, u64>,
    pub compiler_root_ns: u64,
    /// Externally measured process time.
    pub process_elapsed_ns: u64,
    /// Process time outside compiler root: startup, output publication, and
    /// other driver cost. Real time the user waits, outside the phase stack.
    pub driver_overhead_ns: u64,
}

/// Load every run object referenced by an index.
///
/// Each record keeps the name it was published under. Deriving that name here
/// instead would name records as this build of the schema would have written
/// them, which is a different name for every record written before the schema's
/// newest field — including whichever record a manifest baseline pins.
pub fn load_data_branch(data_root: &Path) -> Result<Vec<StoredRun>, String> {
    let runs_dir = data_root.join("runs");
    if !runs_dir.is_dir() {
        // An empty data branch is the normal first state, not an error. The
        // page must render honestly with nothing to show.
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    let listing = std::fs::read_dir(&runs_dir)
        .map_err(|error| format!("could not read {}: {error}", runs_dir.display()))?;
    for entry in listing {
        let path = entry
            .map_err(|error| format!("could not read {}: {error}", runs_dir.display()))?
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let stored = StoredRun::read(&text)
            .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
        runs.push(stored);
    }
    Ok(runs)
}

/// Derive the dashboard's view of a set of runs.
pub fn derive(manifest: &Manifest, runs: &[StoredRun]) -> SiteData {
    let bands = Band::all()
        .into_iter()
        .map(|band| band.wire_name().to_string())
        .collect();

    let mut workloads: Vec<WorkloadDescription> = Vec::new();
    let mut seen_workloads = BTreeSet::new();
    for suite in manifest.suites() {
        for workload in &suite.workloads {
            if seen_workloads.insert(workload.id.clone()) {
                workloads.push(WorkloadDescription {
                    id: workload.id.clone(),
                    question: workload.question.clone(),
                    scaling: workload.scaling.as_ref().map(|scaling| ScalingDescription {
                        axis: scaling.axis.clone(),
                        size: scaling.size,
                    }),
                });
            }
        }
    }

    let mut rejected = Vec::new();
    // Group appendable runs by (platform, epoch). An unappendable run is
    // counted as evidence but never enters a series.
    let mut grouped: BTreeMap<(String, u32), Vec<&StoredRun>> = BTreeMap::new();
    for stored in runs {
        let run = stored.record();
        let outcome = validate_run(manifest, run);
        if !outcome.is_appendable() {
            rejected.push(RejectedRun {
                run: stored.address().to_string(),
                platform: run.identity.platform.clone(),
                epoch: run.identity.epoch,
                commit: run.identity.commit.clone(),
                reasons: outcome.errors.iter().map(|e| e.to_string()).collect(),
            });
            continue;
        }
        grouped
            .entry((run.identity.platform.clone(), run.identity.epoch))
            .or_default()
            .push(stored);
    }

    let mut platforms: BTreeMap<String, Vec<EpochData>> = BTreeMap::new();
    for ((platform, epoch_id), mut epoch_runs) in grouped {
        // Measurement order, so the trailing window means what it says.
        epoch_runs.sort_by(|left, right| {
            left.record()
                .identity
                .finished_at
                .cmp(&right.record().identity.finished_at)
                .then_with(|| {
                    left.record()
                        .identity
                        .commit
                        .cmp(&right.record().identity.commit)
                })
        });
        let Some(epoch) = manifest.epoch(&platform, epoch_id) else {
            continue;
        };
        let Some(suite) = manifest.suite(epoch.suite_revision) else {
            continue;
        };

        let data = derive_epoch(manifest, epoch, suite, &epoch_runs);
        platforms.entry(platform).or_default().push(data);
    }

    let platforms = platforms
        .into_iter()
        .map(|(platform, mut epochs)| {
            epochs.sort_by_key(|epoch| epoch.epoch);
            PlatformData { platform, epochs }
        })
        .collect();

    SiteData {
        bands,
        workloads,
        platforms,
        rejected,
        runtime: None,
    }
}

/// One record in the store this build could not read.
///
/// Kept as data rather than raised as an error, because the store is
/// append-only: a record written by a future schema can never be removed from
/// it, so a reader that refused the whole directory on meeting one would break
/// the site build permanently, with no remedy available in this repository.
pub struct UnreadableRecord {
    /// The record's file name.
    pub name: String,
    /// Why this build could not read it.
    pub detail: String,
}

/// Read the durable store's runtime records.
///
/// A separate directory from `runs/`, because they are a separate record kind
/// whose reader must never have to guess a kind from which fields happen to
/// parse. An absent directory is the normal first state, not an error.
///
/// Records this build cannot parse are skipped and returned alongside the ones
/// it can, so the day `runtime_v2` lands the site keeps deriving every v1
/// record and says plainly which ones it passed over. Only a directory that
/// cannot be listed at all is an error: that is a broken checkout rather than a
/// record from the future.
pub fn load_runtime_records(
    data_root: &Path,
) -> Result<(Vec<StoredRuntimeReport>, Vec<UnreadableRecord>), String> {
    let directory = data_root.join("runtime");
    if !directory.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut reports = Vec::new();
    let mut unreadable = Vec::new();
    let listing = std::fs::read_dir(&directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    for entry in listing {
        let path = entry
            .map_err(|error| format!("could not read {}: {error}", directory.display()))?
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                unreadable.push(UnreadableRecord {
                    name,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        match StoredRuntimeReport::read(&text) {
            Ok(stored) => reports.push(stored),
            Err(error) => unreadable.push(UnreadableRecord {
                name,
                detail: error.to_string(),
            }),
        }
    }
    Ok((reports, unreadable))
}

/// Derive the runtime view of a set of records.
///
/// Nothing derived is stored, exactly as on the compile-time side: medians,
/// dispersion, and every comparison are recomputed here from the raw samples at
/// site build time.
pub fn derive_runtime(
    manifest: &RuntimeManifest,
    reports: &[StoredRuntimeReport],
    unreadable: &[UnreadableRecord],
) -> RuntimeData {
    let mut workloads: Vec<RuntimeWorkloadDescription> = Vec::new();
    let mut seen = BTreeSet::new();
    for suite in manifest.suites() {
        for workload in &suite.workloads {
            if seen.insert(workload.id.clone()) {
                workloads.push(RuntimeWorkloadDescription {
                    id: workload.id.clone(),
                    source: workload.source.clone(),
                    question: workload.question.clone(),
                });
            }
        }
    }

    // A record this build cannot read is reported in the same place a record it
    // read and refused is: from the page's point of view both are records that
    // exist and are not plotted, and the difference is the reason text.
    let mut rejected: Vec<RejectedRuntimeReport> = unreadable
        .iter()
        .map(|record| RejectedRuntimeReport {
            report: record.name.clone(),
            platform: String::new(),
            epoch: 0,
            commit: String::new(),
            reasons: vec![format!(
                "this build of the schema could not read the record: {}",
                record.detail
            )],
        })
        .collect();
    let mut grouped: BTreeMap<(String, u32), Vec<&StoredRuntimeReport>> = BTreeMap::new();
    for stored in reports {
        let report = stored.record();
        let outcome = validate_runtime_report(manifest, report);
        if !outcome.is_appendable() {
            // Surfaced rather than dropped. A report rejected because its
            // program printed the wrong answer is the single most useful thing
            // to see when the runtime page stops advancing.
            rejected.push(RejectedRuntimeReport {
                report: stored.address().to_string(),
                platform: report.identity.platform.clone(),
                epoch: report.identity.epoch,
                commit: report.identity.commit.clone(),
                reasons: outcome
                    .errors
                    .iter()
                    .map(|error| error.to_string())
                    .collect(),
            });
            continue;
        }
        grouped
            .entry((report.identity.platform.clone(), report.identity.epoch))
            .or_default()
            .push(stored);
    }

    let mut by_platform: BTreeMap<String, Vec<RuntimeEpochData>> = BTreeMap::new();
    for ((platform, epoch_id), mut stored) in grouped {
        let Some(epoch) = manifest.epoch(&platform, epoch_id) else {
            continue;
        };
        stored.sort_by(|left, right| {
            left.record()
                .identity
                .finished_at
                .cmp(&right.record().identity.finished_at)
                .then_with(|| left.address().cmp(right.address()))
        });
        by_platform
            .entry(platform)
            .or_default()
            .push(derive_runtime_epoch(manifest, epoch, &stored));
    }

    let platforms = by_platform
        .into_iter()
        .map(|(platform, mut epochs)| {
            epochs.sort_by_key(|epoch| epoch.epoch);
            annotate_suite_revisions(&mut epochs);
            RuntimePlatformData { platform, epochs }
        })
        .collect();

    RuntimeData {
        workloads,
        metrics: RuntimeMetric::ALL
            .into_iter()
            .map(|metric| metric.wire_name().to_string())
            .collect(),
        platforms,
        rejected,
    }
}

/// Mark the epoch at which a platform's workload contract changed.
///
/// A suite revision pins what is measured, so a change in it is the most
/// consequential event on the series — and the only one of ADR-0072 Decision 8's
/// four that cannot be seen from inside a single epoch, since an epoch pins one
/// revision for its whole life. It is therefore derived here, across the
/// platform's epochs in order, rather than in the per-epoch walk.
///
/// A new epoch on the *same* revision gets no annotation. The chart already
/// breaks at every epoch boundary, and labelling one "suite revision 1 → 1"
/// would be noise asserting a change that did not happen.
fn annotate_suite_revisions(epochs: &mut [RuntimeEpochData]) {
    let mut previous: Option<(u32, u32)> = None;
    for epoch in epochs.iter_mut() {
        if let Some((previous_epoch, previous_revision)) = previous
            && previous_revision != epoch.suite_revision
            && let Some(first) = epoch.points.first()
        {
            let annotation = RuntimeAnnotation {
                kind: "suite_revision".to_string(),
                workload: String::new(),
                commit: first.commit.clone(),
                finished_at: first.finished_at.clone(),
                segment: None,
                detail: format!(
                    "suite revision {previous_revision} (epoch {previous_epoch}) → {} (epoch {})",
                    epoch.suite_revision, epoch.epoch
                ),
            };
            // First, because it is the event that opened this epoch: everything
            // else annotated here happened during it.
            epoch.annotations.insert(0, annotation);
        }
        previous = Some((epoch.epoch, epoch.suite_revision));
    }
}

/// What one workload's series has seen so far within an epoch.
///
/// Carried across points rather than recomputed per point, because both things
/// it holds are history: which identity-matched segment the series is in, and
/// the medians the trailing window is taken over. The window lives here — not in
/// a scan over finished points — so that a discontinuity can clear it, which is
/// the mechanism that stops a comparison from crossing a corpus change.
#[derive(Default)]
struct RuntimeWorkloadHistory {
    /// Identities of the last published point: fixture, then workload source.
    last_identity: Option<(String, String)>,
    segment: u32,
    /// In-segment wall-clock summaries, oldest first. Cleared on a boundary.
    window: Vec<Summary>,
}

fn derive_runtime_epoch(
    manifest: &RuntimeManifest,
    epoch: &rue_perf_schema::RuntimeEpoch,
    stored: &[&StoredRuntimeReport],
) -> RuntimeEpochData {
    let mut history: BTreeMap<String, RuntimeWorkloadHistory> = BTreeMap::new();
    let mut annotations: Vec<RuntimeAnnotation> = Vec::new();
    let mut previous_compiler: Option<String> = None;
    let mut points: Vec<RuntimePointData> = Vec::new();

    for stored in stored {
        let report = stored.record();
        let outcome = validate_runtime_report(manifest, report);
        let identity = &report.identity;

        // A compiler release is a run-level event: it is the same for every
        // workload in the report, so it is annotated once rather than per
        // series. Compiler *commits* are not annotated at all, because every
        // point already is one.
        if let Some(previous) = &previous_compiler
            && previous != &identity.compiler_version
        {
            annotations.push(RuntimeAnnotation {
                kind: "compiler_release".to_string(),
                workload: String::new(),
                commit: identity.commit.clone(),
                finished_at: identity.finished_at.clone(),
                segment: None,
                detail: format!(
                    "compiler version {previous} → {}",
                    identity.compiler_version
                ),
            });
        }
        previous_compiler = Some(identity.compiler_version.clone());

        let mut workloads = BTreeMap::new();
        for observation in &report.workloads {
            if !outcome.publishes_workload(&observation.workload) {
                // A workload that did not complete has no point. Publishing a
                // median over a truncated sample set would look like a
                // measurement and be an artifact of the crash that ended it.
                continue;
            }
            let fixture = observation
                .recorded_inputs
                .iter()
                .find(|input| input.name == FIXTURE_INPUT_NAME);
            let fixture_identity = fixture
                .map(|input| input.identity_sha256.clone())
                .unwrap_or_default();
            let source_identity = identity
                .workload_source_hashes
                .get(&observation.workload)
                .cloned()
                .unwrap_or_default();

            let state = history.entry(observation.workload.clone()).or_default();
            // Either identity moving is a discontinuity: the recorded-input
            // bargain is that a consumer can see when the input moved and
            // segment on it, and the workload's own source is recorded on the
            // same terms. One boundary even when both move at once — the
            // annotations below say which, the segment says only that the
            // scale changed.
            if let Some((last_fixture, last_source)) = &state.last_identity
                && (last_fixture != &fixture_identity || last_source != &source_identity)
            {
                if last_fixture != &fixture_identity {
                    annotations.push(RuntimeAnnotation {
                        kind: "corpus_change".to_string(),
                        workload: observation.workload.clone(),
                        commit: identity.commit.clone(),
                        finished_at: identity.finished_at.clone(),
                        segment: Some(state.segment + 1),
                        detail: format!(
                            "input identity {} → {}",
                            short_digest(last_fixture),
                            short_digest(&fixture_identity)
                        ),
                    });
                }
                if last_source != &source_identity {
                    annotations.push(RuntimeAnnotation {
                        kind: "workload_change".to_string(),
                        workload: observation.workload.clone(),
                        commit: identity.commit.clone(),
                        finished_at: identity.finished_at.clone(),
                        segment: Some(state.segment + 1),
                        detail: format!(
                            "program source {} → {}",
                            short_digest(last_source),
                            short_digest(&source_identity)
                        ),
                    });
                }
                state.segment += 1;
                // The prior window measured a different program or a different
                // input, so it is not a window on this one. Clearing it is what
                // makes `flagged` restart at "not enough history" rather than
                // report the discontinuity itself as a regression.
                state.window.clear();
            }
            state.last_identity = Some((fixture_identity.clone(), source_identity.clone()));

            let metrics: BTreeMap<String, SummaryData> = RuntimeMetric::ALL
                .into_iter()
                .filter_map(|metric| {
                    summarize(observation, metric).map(|summary| {
                        (
                            metric.wire_name().to_string(),
                            SummaryData {
                                median: summary.median,
                                mad: summary.mad,
                                count: summary.count,
                            },
                        )
                    })
                })
                .collect();

            // Wall time is the metric a flag is about. Peak RSS and binary size
            // are published as figures, not judged: neither has this suite's
            // dispersion story, and flagging a binary that grew by a byte would
            // train a reader to ignore the word.
            //
            // No declared bound means no verdict. The history is still kept, so
            // the day an epoch declares one the series is judged from its own
            // past rather than starting blind.
            let rule = epoch.flagging(&observation.workload);
            let current = summarize(observation, RuntimeMetric::WallClock);
            let trailing = rule.and_then(|rule| {
                let window_length = rule.window as usize;
                if window_length == 0 || state.window.len() < window_length {
                    return None;
                }
                let medians: Vec<u64> = state.window[state.window.len() - window_length..]
                    .iter()
                    .map(|summary| summary.median)
                    .collect();
                Summary::of(&medians)
            });
            let flagged = rule.and_then(|rule| {
                current
                    .zip(trailing)
                    .map(|(current, window)| flags_movement(current, window, rule.k))
            });
            let window_median_ns = trailing.filter(|_| current.is_some()).map(|w| w.median);
            if let Some(current) = current {
                state.window.push(current);
            }

            workloads.insert(
                observation.workload.clone(),
                RuntimeWorkloadPoint {
                    fixture_identity,
                    fixture_bytes: fixture.map(|input| input.bytes).unwrap_or(0),
                    source_identity,
                    flag_posture: format!("{:?}", epoch.flag_posture(&observation.workload))
                        .to_lowercase(),
                    segment: state.segment,
                    flagged,
                    window_median_ns,
                    metrics,
                },
            );
        }

        points.push(RuntimePointData {
            report: stored.address().to_string(),
            commit: identity.commit.clone(),
            compiler_version: identity.compiler_version.clone(),
            finished_at: identity.finished_at.clone(),
            complete: matches!(outcome.completeness, RuntimeCompleteness::Complete),
            workloads,
            failures: report.failures.iter().map(RuntimeFailureNote::of).collect(),
        });
    }

    // Every workload the epoch samples that has a rule at all, not only the
    // ones observed, so the page can state the bound for a workload whose first
    // point has not landed. A workload with no declared bound is absent rather
    // than present with invented constants, and the page renders the absence.
    let flagging = epoch
        .sampling
        .keys()
        .filter_map(|workload| {
            let rule = epoch.flagging(workload)?;
            Some((
                workload.clone(),
                RuntimeFlaggingRule {
                    k: rule.k,
                    window: rule.window,
                    posture: format!("{:?}", rule.posture).to_lowercase(),
                    reference: rule.reference.unwrap_or_default().to_string(),
                },
            ))
        })
        .collect();

    let comparison = latest_comparison(manifest, stored);

    RuntimeEpochData {
        epoch: epoch.id,
        suite_revision: epoch.suite_revision,
        optimization: format!("{:?}", epoch.optimization).to_lowercase(),
        thread_policy: format!("{:?}", epoch.thread_policy).to_lowercase(),
        hardware_counters: format!("{:?}", epoch.hardware_counters).to_lowercase(),
        flagging,
        annotations,
        points,
        comparison,
    }
}

/// The cross-tool table, joining this epoch's newest run to its latest full
/// peer leg.
///
/// THE JOIN IS THE POINT, and leaving it out is how this table silently became
/// a two-tool one. Peers are re-measured on events rather than on a clock
/// (ADR-0072 Decision 9), while the canary — one single-threaded build by one
/// peer — rides *every* run. So the newest run carrying any peer observation is
/// almost always canary-only, and a table built from it alone would show
/// gazette against Zola-pinned and nothing else: no Hugo, no default-parallel
/// row, no second scale. The three-tool table would appear for exactly one push
/// after each event and then vanish, falsifying both the page's caption and
/// Decision 5's promise that the parallel figures are published rather than
/// hidden.
///
/// So rows come from two places, and each row says which:
///
///   * The Rue program and the canary come from the NEWEST run, measured in the
///     same job on the same machine. This is the same-run denominator Decision
///     9 exists to provide.
///   * Every other peer configuration is JOINED from the latest run that
///     carried a full peer leg, and only when both recorded identities match
///     the newest run's for that workload. Matching WORKLOAD identities mean
///     the two runs built literally the same input; matching COMPARISON
///     identities mean they configured the peers the same way, down to the peer
///     ports and the versions they were pinned at. A differing one of either
///     means the peer leg is due and has not run, and stale rows are dropped
///     rather than published against a corpus or a pin they never saw.
///
/// TWO RULES ABOUT THE NEWEST RUN, both of which this got wrong before RUE-1493
/// and both of which decide whether a published ratio is evidence:
///
///   * The table is built from the newest run's PUBLISHABLE observations, not
///     from its appendable ones. A report missing its required canary is
///     deliberately appendable-but-partial — the evidence is kept and no point
///     publishes — and a report with a truncated Rue observation is suppressed
///     by `derive_runtime_epoch` for the same reason. Publishing a ratio from
///     either would route around both decisions.
///   * If the newest run has no publishable peer observation, the answer is the
///     empty state, not an older table. Reaching back would show a comparison
///     that reads as current, dated by an older commit that a reader has no
///     reason to notice, while the thing it actually says — this run has no
///     valid denominator — goes unsaid.
fn latest_comparison(
    manifest: &RuntimeManifest,
    stored: &[&StoredRuntimeReport],
) -> Option<RuntimeComparison> {
    let mut appendable: Vec<(&&StoredRuntimeReport, RuntimeValidationOutcome)> = stored
        .iter()
        .map(|report| (report, validate_runtime_report(manifest, report.record())))
        .filter(|(_, outcome)| outcome.is_appendable())
        .collect();
    appendable.sort_by(|(left, _), (right, _)| {
        left.record()
            .identity
            .finished_at
            .cmp(&right.record().identity.finished_at)
            .then_with(|| left.address().cmp(right.address()))
    });

    // Publishable, per workload: an observation that publishes no median has no
    // ratio either, and the numerator of every row here is that median.
    fn carries_publishable_full_leg(
        report: &StoredRuntimeReport,
        outcome: &RuntimeValidationOutcome,
    ) -> bool {
        report.record().workloads.iter().any(|observation| {
            outcome.publishes_workload(&observation.workload)
                && observation
                    .peers
                    .iter()
                    .any(|peer| peer.role == PeerRole::Full)
        })
    }

    let (base, base_outcome) = appendable.last()?;
    let base = *base;
    let full = appendable
        .iter()
        .rev()
        .find(|(report, outcome)| carries_publishable_full_leg(report, outcome));

    let mut rows: Vec<RuntimeComparisonRow> = Vec::new();
    let mut fixture = String::new();
    let mut observations: Vec<&rue_perf_schema::RuntimeObservation> = base
        .record()
        .workloads
        .iter()
        .filter(|observation| base_outcome.publishes_workload(&observation.workload))
        .filter(|observation| !observation.peers.is_empty())
        .collect();
    // Ascending by the scale the peers built, so the table reads as a
    // page-count ladder rather than in workload-id order.
    observations.sort_by_key(|observation| {
        observation
            .peers
            .first()
            .map(|peer| peer.scale)
            .unwrap_or_default()
    });

    for observation in observations {
        let Some(rue) = summarize(observation, RuntimeMetric::WallClock) else {
            continue;
        };
        let scale = observation
            .peers
            .first()
            .map(|peer| peer.scale)
            .unwrap_or_default();
        let identity = fixture_identity(observation);
        let comparison = comparison_identity(observation);
        if fixture.is_empty() {
            fixture = short_digest(&identity);
        }
        rows.push(RuntimeComparisonRow {
            tool: format!("gazette ({})", observation.workload),
            scale: format!("{scale}x"),
            // Not a policy choice: Rue has no concurrency, which is the whole
            // reason the peers are pinned to one thread for the primary ratio.
            threads: "1 (Rue has no concurrency)".to_string(),
            median_ns: rue.median,
            ratio: None,
            version: base.record().identity.compiler_version.clone(),
            secondary: false,
            commit: base.record().identity.commit.clone(),
            joined: false,
        });

        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let push_peers = |rows: &mut Vec<RuntimeComparisonRow>,
                          peers: &[rue_perf_schema::PeerObservation],
                          commit: &str,
                          joined: bool,
                          seen: &mut BTreeSet<(String, String)>| {
            let mut ordered: Vec<&rue_perf_schema::PeerObservation> = peers.iter().collect();
            ordered.sort_by(|left, right| {
                (&left.tool, format!("{:?}", left.thread_policy))
                    .cmp(&(&right.tool, format!("{:?}", right.thread_policy)))
            });
            for peer in ordered {
                let key = (peer.tool.clone(), format!("{:?}", peer.thread_policy));
                if !seen.insert(key) {
                    // Already covered by the same-run rows. A joined row can
                    // only ever ADD a configuration, never replace a
                    // measurement taken beside the Rue number it divides.
                    continue;
                }
                let values: Vec<u64> = peer
                    .samples
                    .iter()
                    .map(|sample| sample.process_elapsed_ns)
                    .collect();
                let Some(summary) = Summary::of(&values) else {
                    continue;
                };
                rows.push(RuntimeComparisonRow {
                    tool: peer.tool.clone(),
                    scale: format!("{}x", peer.scale),
                    threads: match peer.thread_policy {
                        PeerThreadPolicy::PinnedSingleThread => "1 (pinned)".to_string(),
                        PeerThreadPolicy::ToolDefaultParallel => {
                            "tool default (parallel)".to_string()
                        }
                    },
                    median_ns: summary.median,
                    ratio: (rue.median > 0).then(|| summary.median as f64 / rue.median as f64),
                    version: peer.version.clone(),
                    secondary: peer.thread_policy != PeerThreadPolicy::PinnedSingleThread,
                    commit: commit.to_string(),
                    joined,
                });
            }
        };

        push_peers(
            &mut rows,
            &observation.peers,
            &base.record().identity.commit,
            false,
            &mut seen,
        );

        // BOTH identities, and the second is the one RUE-1493 added. Matching
        // workload identities say the two runs built the same input; they say
        // nothing about the peers, because the workload identity deliberately
        // no longer moves when a peer port does. The comparison identity is
        // what covers the peer ports and the peer versions, so without it a
        // bumped peer whose full leg failed would go on being joined in — the
        // row self-labelled with the version it was measured under, which
        // misattributes no number and hides the fact that the pinned version
        // has never successfully run.
        if let Some((full, full_outcome)) = full
            && !std::ptr::eq(**full, *base)
            && full_outcome.publishes_workload(&observation.workload)
            && let Some(earlier) = full.record().observation(&observation.workload)
            && fixture_identity(earlier) == identity
            && comparison_identity(earlier) == comparison
        {
            push_peers(
                &mut rows,
                &earlier.peers,
                &full.record().identity.commit,
                true,
                &mut seen,
            );
        }
    }
    if rows.is_empty() {
        return None;
    }
    Some(RuntimeComparison {
        commit: base.record().identity.commit.clone(),
        fixture,
        rows,
    })
}

/// The digest of the input one observation consumed, or empty when it recorded
/// none — in which case no join is possible, which is the safe answer.
fn fixture_identity(observation: &rue_perf_schema::RuntimeObservation) -> String {
    observation
        .recorded_inputs
        .iter()
        .find(|input| input.name == FIXTURE_INPUT_NAME)
        .map(|input| input.identity_sha256.clone())
        .unwrap_or_default()
}

/// The comparison configuration one observation was taken under.
///
/// `None` for an observation from an epoch that recorded none, and comparing
/// two `None`s equal is deliberate: those records were collected under a join
/// condition of the workload identity alone, and re-judging them by a rule
/// their epoch never declared would rewrite history rather than correct it.
/// Within an epoch that pins the identity, validation guarantees both sides
/// carry one, so the comparison is between two real digests.
fn comparison_identity(observation: &rue_perf_schema::RuntimeObservation) -> Option<String> {
    observation
        .comparison
        .as_ref()
        .map(|comparison| comparison.identity_sha256.clone())
}

/// A digest abbreviated for a human, never for comparison.
fn short_digest(digest: &str) -> String {
    digest.chars().take(12).collect()
}

fn derive_epoch(
    manifest: &Manifest,
    epoch: &rue_perf_schema::PlatformEpoch,
    suite: &rue_perf_schema::SuiteRevision,
    runs: &[&StoredRun],
) -> EpochData {
    // The baseline is the run the epoch names, not "the first one we have":
    // an attempted or partial run must never define ratio 1.0. Matched on the
    // name each record was published under, which is the name the manifest
    // pins; a name re-derived from the parsed record would drift out from under
    // the pin every time the schema gained a field.
    let baseline_run = epoch
        .baseline
        .as_ref()
        .and_then(|baseline| runs.iter().find(|stored| stored.address() == baseline.run));
    let baseline_medians = baseline_run.map(|stored| workload_medians(stored.record(), suite));

    let mut points: Vec<PointData> = Vec::new();
    let mut environment_annotations = Vec::new();
    let mut previous_environment: Option<(String, &rue_perf_schema::EnvironmentFingerprint)> = None;
    let mut stdlib_annotations = Vec::new();
    let mut previous_stdlib: Option<&str> = None;

    // History of each workload's summary, so the trailing window can be taken
    // over the *medians* of prior runs, matching the flagging rule's definition.
    let mut history: BTreeMap<String, Vec<Summary>> = BTreeMap::new();

    for stored in runs {
        let run = stored.record();
        let outcome = validate_run(manifest, run);
        let address = stored.address().to_string();
        let fingerprint = run
            .identity
            .environment
            .fingerprint_id()
            .unwrap_or_else(|_| "<unfingerprinted>".to_string());

        if let Some((previous_id, previous_fingerprint)) = &previous_environment
            && *previous_id != fingerprint
        {
            environment_annotations.push(EnvironmentAnnotation {
                commit: run.identity.commit.clone(),
                finished_at: run.identity.finished_at.clone(),
                changed: describe_environment_change(
                    previous_fingerprint,
                    &run.identity.environment,
                ),
            });
        }
        previous_environment = Some((fingerprint.clone(), &run.identity.environment));

        // Recorded, never a reason to exclude the run: a std change moves the
        // product, so the page says where it moved rather than stop drawing.
        let stdlib = run.identity.pins.stdlib_hash.as_str();
        if let Some(previous) = previous_stdlib
            && previous != stdlib
        {
            stdlib_annotations.push(StdlibAnnotation {
                commit: run.identity.commit.clone(),
                finished_at: run.identity.finished_at.clone(),
                previous: previous.to_string(),
                current: stdlib.to_string(),
            });
        }
        previous_stdlib = Some(stdlib);

        let invalid: BTreeSet<(String, u32)> = outcome
            .invalid_samples
            .iter()
            .map(|sample| (sample.workload.clone(), sample.sample_index))
            .collect();

        let mut workload_points = BTreeMap::new();
        for workload in &suite.workloads {
            let Some(observation) = run.observation(&workload.id) else {
                continue;
            };
            // Invalid samples are excluded from every statistic while staying
            // on disk. Deriving from them would publish a measurement the
            // schema already declared untrustworthy.
            let valid: Vec<&rue_perf_schema::Sample> = observation
                .samples
                .iter()
                .enumerate()
                .filter(|(index, _)| !invalid.contains(&(workload.id.clone(), *index as u32)))
                .map(|(_, sample)| sample)
                .collect();
            if valid.is_empty() {
                continue;
            }

            let latencies: Vec<u64> = valid
                .iter()
                .map(|sample| sample_value(sample, Metric::Latency))
                .collect();
            let Some(summary) = Summary::of(&latencies) else {
                continue;
            };

            let window_length = epoch.flagging.window as usize;
            let past = history.entry(workload.id.clone()).or_default();
            // `None` rather than `false` when the window is not yet full:
            // insufficient history is not evidence of stability.
            let trailing = if past.len() >= window_length && window_length > 0 {
                let window_medians: Vec<u64> = past[past.len() - window_length..]
                    .iter()
                    .map(|summary| summary.median)
                    .collect();
                Summary::of(&window_medians)
            } else {
                None
            };
            let flagged = trailing.map(|window| flags_movement(summary, window, epoch.flagging.k));
            let window_median_ns = trailing.map(|window| window.median);
            past.push(summary);

            // Everything here is per compilation, matching `sample_value`'s
            // latency convention. Mixing a per-compilation band with a
            // per-batch total would make the stack fail to sum to its own
            // reported total by exactly the batch size.
            let bands_ns = average_bands(&valid);
            // The reported root is the sum of the averaged bands, not an
            // independently averaged root. Each band's mean truncates, so an
            // independent average would miss their sum by up to a nanosecond
            // per band — and a stack that does not add up to its own stated
            // total is the exact dishonesty the additive model exists to
            // prevent. Defining the total as the sum makes it hold by
            // construction rather than by luck.
            let compiler_root_ns = bands_ns.values().sum();
            let process_elapsed_ns = mean(
                &valid
                    .iter()
                    .map(|sample| sample.process_elapsed_ns / u64::from(sample.batch_size).max(1))
                    .collect::<Vec<_>>(),
            );

            let workload_ratio = baseline_medians
                .as_ref()
                .and_then(|medians| medians.get(&workload.id))
                .and_then(|baseline| ratio(summary.median, *baseline));

            workload_points.insert(
                workload.id.clone(),
                WorkloadPoint {
                    median_ns: summary.median,
                    mad_ns: median_absolute_deviation(&latencies).unwrap_or(0),
                    peak_memory_bytes: median_of(&valid, Metric::PeakMemory),
                    output_binary_bytes: median_of(&valid, Metric::BinarySize),
                    ratio: workload_ratio,
                    flagged,
                    window_median_ns,
                    bands_ns,
                    compiler_root_ns,
                    process_elapsed_ns,
                    driver_overhead_ns: process_elapsed_ns.saturating_sub(compiler_root_ns),
                },
            );
        }

        let complete = matches!(outcome.completeness, Completeness::Complete);
        let missing = match &outcome.completeness {
            Completeness::Complete => Vec::new(),
            Completeness::Partial { missing } => missing.clone(),
        };

        // A headline point publishes only from a complete run: the index's
        // cohort is fixed, so computing it over whatever happened to succeed
        // would let corpus membership move the headline.
        let index = if complete && baseline_medians.is_some() {
            headline_index(&workload_points, suite)
        } else {
            None
        };

        points.push(PointData {
            run: address,
            commit: run.identity.commit.clone(),
            finished_at: run.identity.finished_at.clone(),
            environment: fingerprint,
            complete,
            missing,
            index,
            workloads: workload_points,
        });
    }

    EpochData {
        epoch: epoch.id,
        suite_revision: epoch.suite_revision,
        baseline_commit: epoch.baseline.as_ref().map(|b| b.commit.clone()),
        points,
        environment_annotations,
        flagging: FlaggingRule {
            k: epoch.flagging.k,
            window: epoch.flagging.window,
        },
        process_elapsed_targets: epoch
            .process_elapsed_targets
            .iter()
            .map(|(workload, target)| {
                (
                    workload.clone(),
                    ProcessElapsedTargetData {
                        process_elapsed_ns: target.process_elapsed_ns,
                        reference: target.reference.clone(),
                    },
                )
            })
            .collect(),
        process_elapsed_ratchets: epoch
            .process_elapsed_ratchets
            .iter()
            .map(|(workload, ratchet)| {
                (
                    workload.clone(),
                    ProcessElapsedRatchetData {
                        baseline_process_elapsed_ns: ratchet.baseline_process_elapsed_ns,
                        baseline_mad_ns: ratchet.baseline_mad_ns,
                        process_elapsed_limit_ns: ratchet.process_elapsed_limit_ns,
                        reference_run: ratchet.reference_run.clone(),
                    },
                )
            })
            .collect(),
        stdlib_annotations,
    }
}

/// The geometric mean of per-workload ratios, per metric.
///
/// Equal weighting in log space, so no workload's absolute size weights the
/// aggregate. It answers "is anything moving", not "how much" — a 10% move in
/// one of n workloads shifts the index by roughly 10%/n, which is why the
/// per-workload small multiples carry the full-size signal.
fn headline_index(
    workloads: &BTreeMap<String, WorkloadPoint>,
    suite: &rue_perf_schema::SuiteRevision,
) -> Option<BTreeMap<String, f64>> {
    let mut ratios = Vec::new();
    for workload in &suite.workloads {
        let point = workloads.get(&workload.id)?;
        ratios.push(point.ratio?);
    }
    let latency = geometric_mean(&ratios)?;
    let mut index = BTreeMap::new();
    index.insert(Metric::Latency.wire_name().to_string(), latency);
    Some(index)
}

fn workload_medians(
    run: &RunObject,
    suite: &rue_perf_schema::SuiteRevision,
) -> BTreeMap<String, u64> {
    let mut medians = BTreeMap::new();
    for workload in &suite.workloads {
        let Some(observation) = run.observation(&workload.id) else {
            continue;
        };
        let values: Vec<u64> = observation
            .samples
            .iter()
            .map(|sample| sample_value(sample, Metric::Latency))
            .collect();
        if let Some(summary) = Summary::of(&values) {
            medians.insert(workload.id.clone(), summary.median);
        }
    }
    medians
}

fn median_of(samples: &[&rue_perf_schema::Sample], metric: Metric) -> u64 {
    let values: Vec<u64> = samples
        .iter()
        .map(|sample| sample_value(sample, metric))
        .collect();
    Summary::of(&values)
        .map(|summary| summary.median)
        .unwrap_or(0)
}

/// Mean band durations across a workload's samples, per compilation.
///
/// The mean rather than the median, because the bands must still sum to the
/// reported compiler root: medians are taken per band independently and would
/// not add up, turning a stacked chart into a lie about its own total.
fn average_bands(samples: &[&rue_perf_schema::Sample]) -> BTreeMap<String, u64> {
    let mut bands = BTreeMap::new();
    for band in Band::all() {
        let values: Vec<u64> = samples
            .iter()
            .map(|sample| {
                let batch = u64::from(sample.batch_size).max(1);
                sample.phases.band_ns(band) / batch
            })
            .collect();
        bands.insert(band.wire_name().to_string(), mean(&values));
    }
    bands
}

fn mean(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let total: u128 = values.iter().map(|value| u128::from(*value)).sum();
    (total / values.len() as u128) as u64
}

fn describe_environment_change(
    previous: &rue_perf_schema::EnvironmentFingerprint,
    current: &rue_perf_schema::EnvironmentFingerprint,
) -> Vec<String> {
    let mut changes = Vec::new();
    let mut note = |field: &str, before: &str, after: &str| {
        if before != after {
            changes.push(format!("{field}: {before} -> {after}"));
        }
    };
    note(
        "runner image version",
        &previous.runner_image_version,
        &current.runner_image_version,
    );
    note("CPU", &previous.cpu_model, &current.cpu_model);
    note("kernel", &previous.kernel_version, &current.kernel_version);
    note("OS", &previous.os_version, &current.os_version);
    if previous.core_count != current.core_count {
        changes.push(format!(
            "cores: {} -> {}",
            previous.core_count, current.core_count
        ));
    }
    if changes.is_empty() {
        changes.push("environment fingerprint changed".to_string());
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_perf_schema::{
        EnvironmentFingerprint, Invocation, Phase, PhaseAccounting, RUN_SCHEMA_VERSION,
        ResolvedPins, RunIdentity, Sample, WorkloadObservation,
    };

    fn manifest_for_batch(batch_size: u32) -> Manifest {
        Manifest::parse(&MANIFEST.replace("batch_size = 1", &format!("batch_size = {batch_size}")))
            .expect("fixture manifest")
    }

    const MANIFEST: &str = r#"
[[suite]]
revision = 1
timing_schema_version = 1
protocol_version = 1

[[suite.workloads]]
id = "startup"
source = "performance/workloads/startup/main.rue"
question = "What does a minimal fresh compilation cost end to end?"

[[epoch]]
id = 1
platform = "probe"
suite_revision = 1
target = "x86_64-unknown-linux-gnu"
args = []
toolchain_hash = "toolchain"

[epoch.workload_source_hashes]
startup = "startup-hash"

[epoch.environment]
runner_label = "github-hosted"
runner_image = "ubuntu-24.04"

[epoch.sampling.startup]
samples = 3
batch_size = 1

[epoch.flagging]
k = 2.0
window = 3
"#;

    fn accounting(root_ns: u64) -> PhaseAccounting {
        let mut phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        // Split across two bands so the stack has shape, still summing exactly.
        phase_ns.insert(Phase::SemanticAnalysis, root_ns * 3 / 4);
        phase_ns.insert(Phase::Backend, root_ns - (root_ns * 3 / 4));
        PhaseAccounting {
            phase_ns,
            mixed_parallel_ns: 0,
            unattributed_ns: 0,
            compiler_root_ns: root_ns,
        }
    }

    fn run_at(commit: &str, finished: &str, latencies: [u64; 3]) -> RunObject {
        run_at_batched(commit, finished, latencies, 1)
    }

    /// A batch size greater than one is the case that matters: it is the only
    /// way a per-compilation band and a per-batch total can disagree.
    fn run_at_batched(
        commit: &str,
        finished: &str,
        latencies: [u64; 3],
        batch_size: u32,
    ) -> RunObject {
        RunObject {
            schema_version: RUN_SCHEMA_VERSION,
            identity: RunIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "probe".to_string(),
                commit: commit.repeat(40)[..40].to_string(),
                started_at: finished.to_string(),
                finished_at: finished.to_string(),
                pins: ResolvedPins {
                    toolchain_hash: "toolchain".to_string(),
                    stdlib_hash: "stdlib".to_string(),
                    workload_source_hashes: BTreeMap::from([(
                        "startup".to_string(),
                        "startup-hash".to_string(),
                    )]),
                    invocation: Invocation {
                        target: "x86_64-unknown-linux-gnu".to_string(),
                        args: Vec::new(),
                    },
                },
                environment: fingerprint("AMD EPYC 7763"),
            },
            boundary: None,
            // Direct-constructed stored records carry a placeholder
            // commitment; validation checks the shape, and only encode_v2
            // computes the real address.
            full_evidence: Some("f".repeat(64)),
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
                boundary: None,
                samples: latencies
                    .iter()
                    .map(|ns| {
                        // The runner stores batch totals, so a sample of K
                        // compilations records K times the per-compile figures.
                        let batch = u64::from(batch_size);
                        Sample {
                            batch_size,
                            process_elapsed_ns: (ns + 1_000_000) * batch,
                            peak_memory_bytes: 32 * 1024 * 1024,
                            output_binary_bytes: 12_288,
                            phases: accounting(ns * batch),
                            boundary_evidence: Vec::new(),
                            boundary_processes: Vec::new(),
                            boundary_work_processes: Vec::new(),
                        }
                    })
                    .collect(),
            }],
            failures: Vec::new(),
        }
    }

    fn fingerprint(cpu: &str) -> EnvironmentFingerprint {
        EnvironmentFingerprint {
            runner_label: "github-hosted".to_string(),
            runner_image: "ubuntu-24.04".to_string(),
            runner_image_version: "ubuntu24/20250720.1.0".to_string(),
            cpu_model: cpu.to_string(),
            core_count: 4,
            memory_bytes: 16 * 1024 * 1024 * 1024,
            kernel_version: "6.8.0".to_string(),
            os_version: "Ubuntu 24.04".to_string(),
            architecture: "x86_64".to_string(),
        }
    }

    /// Records as they would arrive from storage.
    ///
    /// Through `StoredRun::read` rather than `minted`, so a fixture is named the
    /// way the data branch names one: by its published bytes.
    fn stored(runs: impl IntoIterator<Item = RunObject>) -> Vec<StoredRun> {
        runs.into_iter()
            .map(|run| {
                let text = rue_perf_schema::canonical_json(&run).expect("addressable");
                StoredRun::read(&text).expect("readable")
            })
            .collect()
    }

    fn manifest_with_baseline(runs: &[RunObject]) -> Manifest {
        let address = runs[0].content_address().expect("addressable");
        let commit = runs[0].identity.commit.clone();
        let text =
            format!("{MANIFEST}\n[epoch.baseline]\ncommit = \"{commit}\"\nrun = \"{address}\"\n");
        Manifest::parse(&text).expect("fixture manifest")
    }

    #[test]
    fn an_empty_data_branch_derives_nothing_without_failing() {
        // The first real state of the page. It must render honestly rather
        // than error.
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let data = derive(&manifest, &[]);
        assert!(data.platforms.is_empty());
        assert!(data.rejected.is_empty());
        assert_eq!(data.workloads.len(), 1);
        assert_eq!(data.bands.len(), Phase::ALL.len() + 2);
    }

    #[test]
    fn an_unappendable_run_is_reported_but_never_enters_a_series() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let mut run = run_at("a", "2026-07-28T00:00:00Z", [100, 101, 102]);
        run.identity.pins.toolchain_hash = "drifted".to_string();
        let data = derive(&manifest, &stored([run]));

        assert!(
            data.platforms.is_empty(),
            "a rejected run must not be plotted"
        );
        assert_eq!(data.rejected.len(), 1);
        assert!(
            data.rejected[0]
                .reasons
                .iter()
                .any(|r| r.contains("toolchain_hash"))
        );
    }

    #[test]
    fn ratios_and_the_index_are_measured_against_the_declared_baseline() {
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let manifest = manifest_with_baseline(std::slice::from_ref(&base));
        let later = run_at("b", "2026-07-28T01:00:00Z", [110, 110, 110]);
        let data = derive(&manifest, &stored([base, later]));

        let points = &data.platforms[0].epochs[0].points;
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].workloads["startup"].ratio, Some(1.0));
        assert_eq!(points[1].workloads["startup"].ratio, Some(1.1));

        // One workload, so the index equals its ratio. With more workloads the
        // geometric mean would attenuate it.
        let index = points[1]
            .index
            .as_ref()
            .expect("complete run publishes an index");
        assert!((index["latency"] - 1.1).abs() < 1e-9, "{index:?}");
    }

    #[test]
    fn a_baseline_still_resolves_after_the_record_schema_moves_on() {
        // RUE-1486. Record fields are additive, so a stored record acquires a
        // zero-valued field the moment the compiler gains a phase to count. If
        // the baseline is matched on a name re-derived from the parsed record,
        // that addition silently unnames the record the manifest pins: the
        // epoch keeps collecting, every point loses its ratio, and the headline
        // index disappears with no rejected run and no failed workflow.
        //
        // Epoch 5 lost its index this way within hours of declaring a baseline.
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let published = rue_perf_schema::canonical_json(&base).expect("addressable");

        // The record as an older writer stored it, carrying a field this build
        // of the schema does not write.
        let mut value: serde_json::Value = serde_json::from_str(&published).unwrap();
        value["workloads"][0]["samples"][0]
            .as_object_mut()
            .unwrap()
            .insert("boundary_evidence".to_string(), serde_json::json!([]));
        let as_written = rue_perf_schema::canonical_json(&value).expect("addressable");
        let stored_base = StoredRun::read(&as_written).expect("readable");
        assert_ne!(
            stored_base.address(),
            stored_base.record().content_address().unwrap(),
            "the fixture must be a record that does not round-trip"
        );

        // The manifest pins the name the record was published under, which is
        // the only name anything else could have written down.
        let published_address = stored_base.address().to_string();
        let text = format!(
            "{MANIFEST}\n[epoch.baseline]\ncommit = \"{}\"\nrun = \"{published_address}\"\n",
            base.identity.commit,
        );
        let manifest = Manifest::parse(&text).expect("fixture manifest");

        let later = run_at("b", "2026-07-28T01:00:00Z", [110, 110, 110]);
        let mut runs = vec![stored_base];
        runs.extend(stored([later]));
        let data = derive(&manifest, &runs);

        let epoch = &data.platforms[0].epochs[0];
        assert_eq!(epoch.points[0].workloads["startup"].ratio, Some(1.0));
        assert_eq!(epoch.points[1].workloads["startup"].ratio, Some(1.1));
        let index = epoch.points[1]
            .index
            .as_ref()
            .expect("the epoch's headline index survives a schema addition");
        assert!((index["latency"] - 1.1).abs() < 1e-9, "{index:?}");

        // The plotted point names the record as stored, so a reader following
        // it from the page reaches the file that is actually on the branch.
        assert_eq!(epoch.points[0].run, published_address);
    }

    #[test]
    fn a_partial_run_publishes_its_workloads_but_no_headline_point() {
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let manifest = manifest_with_baseline(std::slice::from_ref(&base));
        let mut partial = run_at("b", "2026-07-28T01:00:00Z", [110, 110, 110]);
        // One sample short of the policy: a partial run, still appendable.
        partial.workloads[0].samples.truncate(2);
        let data = derive(&manifest, &stored([base, partial]));

        let point = &data.platforms[0].epochs[0].points[1];
        assert!(!point.complete);
        assert_eq!(point.missing, vec!["startup".to_string()]);
        assert!(
            point.index.is_none(),
            "a fixed cohort means no headline from a partial run"
        );
        assert!(
            point.workloads.contains_key("startup"),
            "the per-workload observation still publishes"
        );
    }

    #[test]
    fn movement_is_unknown_until_the_trailing_window_is_full() {
        // Too little history is not the same as "stable", and rendering it as
        // stable would be a lie.
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let manifest = manifest_with_baseline(std::slice::from_ref(&base));
        let runs = vec![
            base,
            run_at("b", "2026-07-28T01:00:00Z", [100, 100, 100]),
            run_at("c", "2026-07-28T02:00:00Z", [100, 100, 100]),
            run_at("d", "2026-07-28T03:00:00Z", [100, 100, 100]),
        ];
        let data = derive(&manifest, &stored(runs));
        let points = &data.platforms[0].epochs[0].points;

        // window = 3, so the first three have no full window behind them.
        assert_eq!(points[0].workloads["startup"].flagged, None);
        assert_eq!(points[1].workloads["startup"].flagged, None);
        assert_eq!(points[2].workloads["startup"].flagged, None);
        assert!(points[3].workloads["startup"].flagged.is_some());

        // The window median travels with the flag, because the page reports
        // what a flag means by naming the value it was compared against. It is
        // present exactly when the flag is, or the page would have to render a
        // comparison with one side missing.
        for point in &points[..3] {
            assert_eq!(point.workloads["startup"].window_median_ns, None);
        }
        assert_eq!(
            points[3].workloads["startup"].window_median_ns,
            Some(100),
            "the median of the three preceding medians"
        );
    }

    #[test]
    fn the_epoch_publishes_the_flagging_rule_it_pins() {
        // The page states the bound it is reporting against. Leaving `k` and
        // the window length out would reduce "flagged" to an assertion the
        // reader has no way to check.
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let manifest = manifest_with_baseline(std::slice::from_ref(&base));
        let data = derive(&manifest, &stored([base]));
        let flagging = &data.platforms[0].epochs[0].flagging;
        assert_eq!(flagging.window, 3);
        assert!((flagging.k - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_epoch_publishes_only_its_own_external_latency_targets() {
        let text = MANIFEST.replace(
            "[epoch.flagging]",
            "[epoch.process_elapsed_targets.startup]\n\
             process_elapsed_ns = 250000000\n\
             reference = \"ADR-0071\"\n\n\
             [epoch.flagging]",
        );
        let manifest = Manifest::parse(&text).expect("target manifest");
        let data = derive(
            &manifest,
            &stored([run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100])]),
        );
        let targets = &data.platforms[0].epochs[0].process_elapsed_targets;
        assert_eq!(targets.len(), 1);
        assert_eq!(targets["startup"].process_elapsed_ns, 250_000_000);
        assert_eq!(targets["startup"].reference, "ADR-0071");
    }

    #[test]
    fn the_epoch_publishes_its_baseline_ratchet_separately_from_the_target() {
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let address = base.content_address().expect("addressable");
        let text = format!(
            "{MANIFEST}\n\
             [epoch.baseline]\n\
             commit = \"{}\"\n\
             run = \"{address}\"\n\n\
             [epoch.process_elapsed_ratchets.startup]\n\
             baseline_process_elapsed_ns = 101000000\n\
             baseline_mad_ns = 1000000\n\
             process_elapsed_limit_ns = 107000000\n\
             reference_run = \"{address}\"\n",
            base.identity.commit
        );
        let manifest = Manifest::parse(&text).expect("ratcheted manifest");
        let data = derive(&manifest, &stored([base]));
        let ratchets = &data.platforms[0].epochs[0].process_elapsed_ratchets;
        assert_eq!(ratchets.len(), 1);
        assert_eq!(ratchets["startup"].baseline_process_elapsed_ns, 101_000_000);
        assert_eq!(ratchets["startup"].process_elapsed_limit_ns, 107_000_000);
        assert_eq!(ratchets["startup"].reference_run, address);
    }

    #[test]
    fn the_derived_bands_still_sum_to_the_reported_compiler_root() {
        // A stacked chart whose bands do not add to its own total is the exact
        // dishonesty the additive model exists to prevent.
        //
        // Batch sizes above one are the case that matters, and the case an
        // earlier version of this test missed: with `batch_size: 1` a
        // per-compilation band and a per-batch total are identical, so the
        // assertion passed while the real data was wrong by exactly the batch
        // size.
        for batch in [1u32, 2, 5] {
            let manifest = manifest_for_batch(batch);
            let run = run_at_batched("a", "2026-07-28T00:00:00Z", [100, 130, 170], batch);
            let data = derive(&manifest, &stored([run]));
            let point = &data.platforms[0].epochs[0].points[0].workloads["startup"];
            let summed: u64 = point.bands_ns.values().sum();
            assert_eq!(
                summed, point.compiler_root_ns,
                "batch {batch}: bands {:?} must sum to root {}",
                point.bands_ns, point.compiler_root_ns
            );
        }
    }

    #[test]
    fn every_reported_figure_is_per_compilation() {
        // The median comes from `sample_value`, which divides by the batch.
        // The root total and the bands must use the same convention, or the
        // page shows a total the median contradicts.
        let unbatched = derive(
            &manifest_for_batch(1),
            &stored([run_at_batched(
                "a",
                "2026-07-28T00:00:00Z",
                [100, 100, 100],
                1,
            )]),
        );
        let batched = derive(
            &manifest_for_batch(4),
            &stored([run_at_batched(
                "a",
                "2026-07-28T00:00:00Z",
                [100, 100, 100],
                4,
            )]),
        );
        let one = &unbatched.platforms[0].epochs[0].points[0].workloads["startup"];
        let four = &batched.platforms[0].epochs[0].points[0].workloads["startup"];

        assert_eq!(one.median_ns, four.median_ns);
        assert_eq!(one.compiler_root_ns, four.compiler_root_ns);
        assert_eq!(one.process_elapsed_ns, four.process_elapsed_ns);
        assert_eq!(one.driver_overhead_ns, four.driver_overhead_ns);
    }

    #[test]
    fn driver_overhead_is_process_time_outside_compiler_root() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let data = derive(
            &manifest,
            &stored([run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100])]),
        );
        let point = &data.platforms[0].epochs[0].points[0].workloads["startup"];
        assert_eq!(point.driver_overhead_ns, 1_000_000);
    }

    #[test]
    fn a_fingerprint_change_within_an_epoch_is_annotated() {
        // The epoch pins a policy, not a machine. Drift is recorded so the page
        // can mark comparisons across it as advisory.
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let first = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let mut second = run_at("b", "2026-07-28T01:00:00Z", [100, 100, 100]);
        second.identity.environment = fingerprint("Intel Xeon Platinum 8370C");

        let data = derive(&manifest, &stored([first, second]));
        let annotations = &data.platforms[0].epochs[0].environment_annotations;
        assert_eq!(annotations.len(), 1);
        assert!(
            annotations[0].changed.iter().any(|c| c.contains("CPU")),
            "{annotations:?}"
        );
    }

    #[test]
    fn a_standard_library_change_annotates_the_series_without_breaking_it() {
        // A std change must annotate the series, not truncate it: rejecting
        // these runs would silently stop the published page at the last point
        // measured before the edit.
        let first = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let manifest = manifest_with_baseline(std::slice::from_ref(&first));
        let mut second = run_at("b", "2026-07-28T01:00:00Z", [110, 110, 110]);
        second.identity.pins.stdlib_hash = "a-new-standard-library".to_string();

        let data = derive(&manifest, &stored([first, second]));

        assert!(data.rejected.is_empty(), "{:?}", data.rejected);
        let epoch = &data.platforms[0].epochs[0];
        assert_eq!(epoch.points.len(), 2, "both points belong to one series");

        assert_eq!(epoch.stdlib_annotations.len(), 1);
        assert_eq!(epoch.stdlib_annotations[0].commit, "b".repeat(40));
        assert_eq!(
            epoch.stdlib_annotations[0].current,
            "a-new-standard-library"
        );

        // The point across the change is still measured against the same
        // baseline: std moving is movement in the product, not a discontinuity
        // that resets what 1.0 means.
        assert_eq!(epoch.points[1].workloads["startup"].ratio, Some(1.1));
    }

    #[test]
    fn an_unchanged_standard_library_produces_no_annotation() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let data = derive(
            &manifest,
            &stored([
                run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]),
                run_at("b", "2026-07-28T01:00:00Z", [100, 100, 100]),
            ]),
        );
        assert!(
            data.platforms[0].epochs[0].stdlib_annotations.is_empty(),
            "an annotation on every point would be noise, not a finding"
        );
    }

    #[test]
    fn points_are_ordered_by_measurement_time() {
        // The trailing window is meaningless if the series is not in order.
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let late = run_at("b", "2026-07-28T05:00:00Z", [100, 100, 100]);
        let early = run_at("a", "2026-07-28T01:00:00Z", [100, 100, 100]);
        let data = derive(&manifest, &stored([late, early]));
        let points = &data.platforms[0].epochs[0].points;
        assert!(points[0].finished_at < points[1].finished_at);
    }

    #[test]
    fn an_invalid_sample_is_excluded_from_the_published_median() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let mut run = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        // Break one sample's invariant and record it, as the runner would.
        run.workloads[0].samples[2].phases.unattributed_ns = 500;
        run.failures
            .push(rue_perf_schema::FailureRecord::PhaseInvariant {
                workload: "startup".to_string(),
                sample_index: 2,
                compiler_root_ns: 100,
                attributed_ns: 600,
            });
        let data = derive(&manifest, &stored([run]));
        let point = &data.platforms[0].epochs[0].points[0].workloads["startup"];
        // The surviving two samples are both 100; a median including the
        // invalid one would still be 100, so assert on the band totals instead,
        // which the broken sample would have inflated.
        assert_eq!(point.compiler_root_ns, 100);
        assert_eq!(point.bands_ns.values().sum::<u64>(), 100);
    }

    // -----------------------------------------------------------------------
    // Runtime series (ADR-0072)
    // -----------------------------------------------------------------------

    const RUNTIME_MANIFEST: &str = r#"
schema_version = 1

[[suite]]
revision = 1
protocol_version = 1
measured_boundary = "spawn_to_exit_v1"

[[suite.workloads]]
id = "wordfreq"
source = "examples/wordfreq/main.rue"
question = "How fast does compiled Rue count words?"
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
description = "deterministic ASCII word text"

[suite.workloads.oracle]
kind = "golden_stdout"
path = "performance/fixtures/wordfreq/expected-stdout.txt"

[[epoch]]
id = 1
platform = "probe"
suite_revision = 1
target = "x86-64-linux"
compiler_args = ["-O3"]
optimization = "o3"
thread_policy = "single_threaded"
hardware_counters = "unavailable_on_hosted_runner"
collection = true

[epoch.environment]
runner_label = "probe"
runner_image = "probe"

[epoch.sampling.wordfreq]
samples = 3
"#;

    fn runtime_report(commit: char, elapsed: [u64; 3]) -> rue_perf_schema::RuntimeReport {
        use rue_perf_schema::*;
        RuntimeReport {
            record_kind: RUNTIME_RECORD_KIND.to_string(),
            schema_version: RUNTIME_REPORT_SCHEMA_VERSION,
            identity: RuntimeIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "probe".to_string(),
                commit: std::iter::repeat_n(commit, 40).collect(),
                compiler_version: "rue 0.1.0".to_string(),
                started_at: "2026-08-13T00:00:00Z".to_string(),
                finished_at: format!("2026-08-13T00:0{}:00Z", elapsed[0] % 10),
                toolchain_hash: "1".repeat(64),
                stdlib_hash: "2".repeat(64),
                workload_source_hashes: BTreeMap::from([("wordfreq".to_string(), "3".repeat(64))]),
                environment: EnvironmentFingerprint {
                    runner_label: "probe".to_string(),
                    runner_image: "probe".to_string(),
                    runner_image_version: "1".to_string(),
                    cpu_model: "probe".to_string(),
                    core_count: 4,
                    memory_bytes: 1,
                    kernel_version: "probe".to_string(),
                    os_version: "probe".to_string(),
                    architecture: "x86_64".to_string(),
                },
            },
            regime: RuntimeRegime {
                measured_boundary: RuntimeBoundary::SpawnToExitV1,
                program_state: "fresh_process".to_string(),
                os_page_cache: "uncontrolled".to_string(),
                fixture_preparation_measured: false,
                oracle_comparison_measured: false,
                optimization: OptimizationLevel::O3,
                compiler_args: vec!["-O3".to_string()],
                target: "x86-64-linux".to_string(),
                thread_policy: ThreadPolicy::SingleThreaded,
                hardware_counters: HardwareCounterPolicy::UnavailableOnHostedRunner,
            },
            workloads: vec![RuntimeObservation {
                workload: "wordfreq".to_string(),
                source: "examples/wordfreq/main.rue".to_string(),
                question: "How fast does compiled Rue count words?".to_string(),
                program_args: vec![FIXTURE_ARGUMENT.to_string()],
                recorded_inputs: vec![RecordedInput {
                    name: FIXTURE_INPUT_NAME.to_string(),
                    category: InputCategory::Recorded,
                    description: "deterministic ASCII word text".to_string(),
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
                    binary_bytes: 65_536,
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
                samples: elapsed
                    .into_iter()
                    .map(|process_elapsed_ns| RuntimeSample {
                        process_elapsed_ns,
                        peak_memory_bytes: 1024,
                        exit_code: 0,
                        stdout_bytes: 8,
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

    fn stored_runtime(
        reports: impl IntoIterator<Item = rue_perf_schema::RuntimeReport>,
    ) -> Vec<StoredRuntimeReport> {
        reports
            .into_iter()
            .map(|report| {
                StoredRuntimeReport::read(&rue_perf_schema::canonical_json(&report).unwrap())
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn runtime_points_carry_medians_derived_from_raw_samples() {
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([runtime_report('a', [7, 5, 6])]),
            &[],
        );
        let point = &data.platforms[0].epochs[0].points[0];
        let workload = &point.workloads["wordfreq"];
        assert!(point.complete);
        assert_eq!(workload.metrics["wall_clock"].median, 6);
        assert_eq!(workload.metrics["binary_size"].median, 65_536);
        // Nothing derived is stored, so the identity that makes two raw
        // medians comparable has to ride the point.
        assert_eq!(workload.fixture_identity, "f".repeat(64));
        assert_eq!(workload.flag_posture, "advisory");
        assert_eq!(data.platforms[0].epochs[0].optimization, "o3");
    }

    #[test]
    fn a_report_whose_program_was_wrong_is_surfaced_rather_than_plotted() {
        // The runtime counterpart of a rejected run: the single most useful
        // thing to see when the page stops advancing is why.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let mut report = runtime_report('a', [7, 5, 6]);
        report.workloads[0].oracle.verdict = rue_perf_schema::OracleVerdict::Mismatch;
        let data = derive_runtime(&manifest, &stored_runtime([report]), &[]);
        assert!(data.platforms.is_empty());
        assert_eq!(data.rejected.len(), 1);
        assert!(
            data.rejected[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("oracle")),
            "{:?}",
            data.rejected[0].reasons
        );
    }

    #[test]
    fn an_incomplete_workload_publishes_no_median() {
        // A median over a truncated sample set would look like a measurement
        // and be an artifact of whatever ended the run.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let mut report = runtime_report('a', [7, 5, 6]);
        report.workloads[0].samples.truncate(1);
        let data = derive_runtime(&manifest, &stored_runtime([report]), &[]);
        let point = &data.platforms[0].epochs[0].points[0];
        assert!(!point.complete);
        assert!(point.workloads.is_empty());
    }

    #[test]
    fn runtime_points_are_ordered_by_measurement_time() {
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([
                runtime_report('b', [3, 3, 3]),
                runtime_report('a', [1, 1, 1]),
            ]),
            &[],
        );
        let points = &data.platforms[0].epochs[0].points;
        assert_eq!(points.len(), 2);
        assert!(points[0].finished_at <= points[1].finished_at);
    }

    #[test]
    fn a_runtime_point_carries_both_identities_it_may_be_segmented_on() {
        // Recording rather than pinning the workload's source is only
        // defensible if a consumer can see when it moved, so both the fixture
        // digest and the source digest have to reach the derived data.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([runtime_report('a', [7, 5, 6])]),
            &[],
        );
        let workload = &data.platforms[0].epochs[0].points[0].workloads["wordfreq"];
        assert_eq!(workload.fixture_identity, "f".repeat(64));
        assert_eq!(workload.source_identity, "3".repeat(64));
    }

    /// A manifest whose workload is a corpus tree with peers, for the
    /// cross-tool table. Separate from `RUNTIME_MANIFEST` deliberately: a peer
    /// policy on a workload that builds no site is refused by the schema, and
    /// adding gazette to the seeded manifest would make every other test in
    /// this module report a workload it never measured.
    const RUNTIME_PEER_MANIFEST: &str = r#"
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
excluded = []
root_name = "gazette-site"
description = "the live corpus"

[suite.workloads.oracle]
kind = "semantic_site_pages"
path = "scripts/gazette-corpus-diff.py"

[[epoch]]
id = 1
platform = "probe"
suite_revision = 1
target = "x86-64-linux"
compiler_args = ["-O3"]
optimization = "o3"
thread_policy = "single_threaded"
hardware_counters = "unavailable_on_hosted_runner"
collection = true

[epoch.environment]
runner_label = "probe"
runner_image = "probe"

[epoch.sampling.gazette]
samples = 3

[epoch.peers.gazette]
tools = ["zola"]
primary_thread_policy = "pinned_single_thread"
secondary_thread_policy = "tool_default_parallel"
canary_tool = "zola"
canary_scale = 1
records_comparison_identity = true
"#;

    /// The epoch's own sample count, because a peer taken under another
    /// sampling policy is not a comparable denominator (RUE-1493).
    fn peer_samples(elapsed: u64) -> Vec<rue_perf_schema::RuntimeSample> {
        (0..3)
            .map(|index| rue_perf_schema::RuntimeSample {
                process_elapsed_ns: elapsed + index,
                peak_memory_bytes: 1024,
                exit_code: 0,
                stdout_bytes: 0,
                stdout_sha256: "a".repeat(64),
                artifact_sha256: Some("8".repeat(64)),
            })
            .collect()
    }

    /// The comparison configuration the peer legs were taken under.
    ///
    /// Distinct from the workload identity on purpose: the join now requires
    /// both to match, and a test that reused one digest for both could not tell
    /// which rule it was exercising.
    fn comparison_identity(digest: char) -> rue_perf_schema::ComparisonIdentity {
        rue_perf_schema::ComparisonIdentity {
            identity_sha256: std::iter::repeat_n(digest, 64).collect(),
            peer_port_revision: 3,
            peer_versions: BTreeMap::from([("zola".to_string(), "0.21.0".to_string())]),
        }
    }

    /// A gazette report carrying the canary and the secondary peer row.
    fn gazette_peer_report() -> rue_perf_schema::RuntimeReport {
        use rue_perf_schema::*;
        let mut report = runtime_report('a', [4, 4, 4]);
        report.identity.workload_source_hashes =
            BTreeMap::from([("gazette".to_string(), "3".repeat(64))]);
        let samples: Vec<RuntimeSample> = (0..3)
            .map(|index| RuntimeSample {
                process_elapsed_ns: 4_000_000_000 + index,
                peak_memory_bytes: 1024,
                exit_code: 0,
                stdout_bytes: 0,
                stdout_sha256: "e".repeat(64),
                artifact_sha256: Some("7".repeat(64)),
            })
            .collect();
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
                    excluded: Vec::new(),
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
            samples,
            peers: vec![
                PeerObservation {
                    tool: "zola".to_string(),
                    version: "0.21.0".to_string(),
                    role: PeerRole::Canary,
                    thread_policy: PeerThreadPolicy::PinnedSingleThread,
                    scale: 1,
                    output_sha256: "8".repeat(64),
                    emitted_files: 131,
                    samples: peer_samples(2_000_000_000),
                },
                PeerObservation {
                    tool: "zola".to_string(),
                    version: "0.21.0".to_string(),
                    role: PeerRole::Full,
                    thread_policy: PeerThreadPolicy::ToolDefaultParallel,
                    scale: 1,
                    // The same tree as the pinned row above: the same tool on
                    // the same corpus does the same work, and RUE-1493 refuses
                    // a secondary row that does not.
                    output_sha256: "8".repeat(64),
                    emitted_files: 131,
                    samples: peer_samples(1_000_000_000),
                },
            ],
            comparison: Some(comparison_identity('a')),
        }];
        report
    }

    #[test]
    fn the_cross_tool_table_comes_from_one_run_and_names_its_thread_policy() {
        // ADR-0072 Decisions 5 and 9. Two properties are asserted because both
        // are easy to break and each breaks the published claim: the primary
        // ratio's denominator is the peer PINNED TO ONE THREAD, and the peers'
        // default parallel row is published beside it, labelled, rather than
        // dropped or promoted.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let report = gazette_peer_report();
        let data = derive_runtime(&manifest, &stored_runtime([report]), &[]);
        let comparison = data.platforms[0].epochs[0]
            .comparison
            .as_ref()
            .expect("a run carrying peers publishes a comparison");
        assert_eq!(comparison.rows.len(), 3);
        assert_eq!(comparison.rows[0].tool, "gazette (gazette)");
        assert_eq!(
            comparison.rows[0].ratio, None,
            "the Rue row is the baseline"
        );
        let pinned = &comparison.rows[1];
        assert_eq!(pinned.threads, "1 (pinned)");
        assert!(!pinned.secondary);
        assert_eq!(pinned.version, "0.21.0");
        let parallel = &comparison.rows[2];
        assert_eq!(parallel.threads, "tool default (parallel)");
        assert!(
            parallel.secondary,
            "the default-parallel row is published as a labelled secondary, never as the ratio"
        );
        // The pinned peer took twice the parallel one's time here, so the two
        // ratios must differ — a table that published one number for both would
        // be publishing core count as though it were per-unit work.
        assert!(pinned.ratio.unwrap() > parallel.ratio.unwrap());
    }

    #[test]
    fn a_canary_only_run_keeps_the_three_tool_table_by_joining_the_last_full_leg() {
        // The regression this join exists to prevent, and the one the first
        // implementation shipped: the canary rides EVERY run and the full peer
        // matrix runs only on events, so the newest peers-carrying run is
        // almost always canary-only. Without the join the published table lost
        // Hugo and both default-parallel rows on the very next push, one push
        // after each event — falsifying the page's own caption.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut full = gazette_peer_report();
        full.identity.commit = "1".repeat(40);
        full.identity.finished_at = "2026-08-13T00:01:00Z".to_string();

        let mut canary_only = gazette_peer_report();
        canary_only.identity.commit = "2".repeat(40);
        canary_only.identity.finished_at = "2026-08-13T00:02:00Z".to_string();
        canary_only.workloads[0]
            .peers
            .retain(|peer| peer.role == rue_perf_schema::PeerRole::Canary);
        assert_eq!(canary_only.workloads[0].peers.len(), 1);

        let data = derive_runtime(&manifest, &stored_runtime([full, canary_only]), &[]);
        let comparison = data.platforms[0].epochs[0]
            .comparison
            .as_ref()
            .expect("a comparison");
        // Rue, the same-run canary, and the joined parallel row.
        assert_eq!(comparison.rows.len(), 3);
        // The same-run rows come from the newest run; the joined one names the
        // run it was actually measured in, so the page can say so.
        assert_eq!(comparison.commit, "2".repeat(40));
        assert!(!comparison.rows[0].joined);
        assert!(!comparison.rows[1].joined, "the canary is a same-run row");
        assert_eq!(comparison.rows[1].commit, "2".repeat(40));
        let joined = &comparison.rows[2];
        assert!(joined.joined, "the parallel row survives by being joined");
        assert!(joined.secondary);
        assert_eq!(joined.commit, "1".repeat(40));
    }

    #[test]
    fn a_joined_row_is_dropped_when_the_corpus_it_measured_is_not_this_one() {
        // The join's whole licence is a matching fixture identity: it means the
        // two runs built literally the same input. A differing identity means
        // the peer leg is due and has not run, and publishing the older rows
        // would compare this corpus against a peer's time on another one.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut full = gazette_peer_report();
        full.identity.commit = "1".repeat(40);
        full.identity.finished_at = "2026-08-13T00:01:00Z".to_string();

        let mut canary_only = gazette_peer_report();
        canary_only.identity.commit = "2".repeat(40);
        canary_only.identity.finished_at = "2026-08-13T00:02:00Z".to_string();
        canary_only.workloads[0]
            .peers
            .retain(|peer| peer.role == rue_perf_schema::PeerRole::Canary);
        // The corpus moved between the two runs.
        canary_only.workloads[0].recorded_inputs[0].identity_sha256 = "c".repeat(64);

        let data = derive_runtime(&manifest, &stored_runtime([full, canary_only]), &[]);
        let comparison = data.platforms[0].epochs[0]
            .comparison
            .as_ref()
            .expect("a comparison");
        assert_eq!(
            comparison.rows.len(),
            2,
            "only the same-run rows survive a corpus change: {:?}",
            comparison.rows
        );
        assert!(comparison.rows.iter().all(|row| !row.joined));
    }

    #[test]
    fn a_joined_row_is_dropped_when_the_peers_it_measured_are_not_these_peers() {
        // The other half of the join condition, and the one RUE-1493 added.
        // The corpus is identical here — the workload identity matches exactly,
        // as it now does across a peer-port or peer-version change, since
        // neither is an input to gazette. What differs is the comparison
        // configuration, which is the only thing that can say so.
        //
        // The failure this prevents: bump the Hugo shim, watch the full leg
        // fail, and every subsequent canary-only run splices in a peer row
        // measured under a version the project no longer pins. The row names
        // its own version, so no number is misattributed — and the table goes
        // on publishing against a pin that has never successfully run.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut full = gazette_peer_report();
        full.identity.commit = "1".repeat(40);
        full.identity.finished_at = "2026-08-13T00:01:00Z".to_string();

        let mut canary_only = gazette_peer_report();
        canary_only.identity.commit = "2".repeat(40);
        canary_only.identity.finished_at = "2026-08-13T00:02:00Z".to_string();
        canary_only.workloads[0]
            .peers
            .retain(|peer| peer.role == rue_perf_schema::PeerRole::Canary);
        // Same corpus, different peer configuration.
        canary_only.workloads[0].comparison = Some(comparison_identity('b'));
        assert_eq!(
            canary_only.workloads[0].recorded_inputs[0].identity_sha256,
            full.workloads[0].recorded_inputs[0].identity_sha256,
            "the workload identity is deliberately unmoved by a peer change"
        );

        let data = derive_runtime(&manifest, &stored_runtime([full, canary_only]), &[]);
        let comparison = data.platforms[0].epochs[0]
            .comparison
            .as_ref()
            .expect("a comparison");
        assert_eq!(
            comparison.rows.len(),
            2,
            "only the same-run rows survive a peer-configuration change: {:?}",
            comparison.rows
        );
        assert!(comparison.rows.iter().all(|row| !row.joined));
    }

    #[test]
    fn a_run_that_lost_its_canary_publishes_no_comparison() {
        // ADR-0072 Decision 9 makes a canary-less observation appendable and
        // NOT publishable: the evidence is kept and the ratio it was meant to
        // anchor is exactly what cannot be computed. Before RUE-1493 this
        // function filtered on appendability alone, so a report deliberately
        // held back from the series still published a full cross-tool table
        // from whatever peers it happened to carry.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut report = gazette_peer_report();
        // The full leg ran and the canary did not, which is the state a
        // canary failure leaves behind: both rows are legitimate measurements
        // and neither is the same-run denominator Decision 9 requires.
        for peer in &mut report.workloads[0].peers {
            peer.role = rue_perf_schema::PeerRole::Full;
        }
        let stored = stored_runtime([report]);
        let outcome = validate_runtime_report(&manifest, stored[0].record());
        assert!(outcome.is_appendable(), "{:?}", outcome.errors);
        assert!(
            !outcome.publishes_workload("gazette"),
            "deliberately partial"
        );

        let data = derive_runtime(&manifest, &stored, &[]);
        assert!(
            data.platforms[0].epochs[0].comparison.is_none(),
            "a partial observation publishes no median, so it has no ratio either"
        );
    }

    #[test]
    fn a_truncated_observation_publishes_no_comparison_either() {
        // The same rule reached the other way. `derive_runtime_epoch` already
        // suppresses a median over a truncated sample set; a ratio computed
        // from the same samples would be that suppressed median with a
        // denominator attached.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut report = gazette_peer_report();
        report.workloads[0].samples.truncate(1);
        let data = derive_runtime(&manifest, &stored_runtime([report]), &[]);
        let epoch = &data.platforms[0].epochs[0];
        assert!(epoch.points[0].workloads.is_empty());
        assert!(epoch.comparison.is_none());
    }

    #[test]
    fn a_newest_run_without_peers_shows_no_table_rather_than_an_older_one() {
        // The fallback this replaces reached back to the newest run that
        // carried any peer at all, so a run whose peer leg produced nothing
        // rendered an older comparison that reads as current. What the reader
        // needs to know in that state is precisely that this run has no valid
        // denominator, and an older table is the one thing that cannot say it.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut full = gazette_peer_report();
        full.identity.commit = "1".repeat(40);
        full.identity.finished_at = "2026-08-13T00:01:00Z".to_string();

        let mut peerless = gazette_peer_report();
        peerless.identity.commit = "2".repeat(40);
        peerless.identity.finished_at = "2026-08-13T00:02:00Z".to_string();
        // Every peer build failed. The comparison configuration is still
        // recorded — the preparer laid the peers' sites out and read their
        // versions before anything was measured — so the record says which
        // peers this run WOULD have compared against and carries not one of
        // their measurements.
        peerless.workloads[0].peers.clear();

        let data = derive_runtime(&manifest, &stored_runtime([full, peerless]), &[]);
        assert!(
            data.platforms[0].epochs[0].comparison.is_none(),
            "the newest run has no denominator, and an older table would hide that"
        );
    }

    #[test]
    fn a_failure_inside_an_appendable_report_reaches_the_page() {
        // RUE-1493. A peer row refused for work equivalence is dropped from a
        // report that stays complete and publishable, so the table loses a row
        // and nothing else changes. The record carried the runner's reason all
        // along and no consumer read it; without this the only trace of a
        // default-parallel row that built a different site would be a job log
        // nobody keeps.
        let manifest = RuntimeManifest::parse(RUNTIME_PEER_MANIFEST).expect("peer manifest");
        let mut report = gazette_peer_report();
        report.workloads[0]
            .peers
            .retain(|peer| peer.thread_policy == PeerThreadPolicy::PinnedSingleThread);
        report
            .failures
            .push(rue_perf_schema::RuntimeFailure::ValidationRejected {
                workload: "gazette".to_string(),
                detail: "peer zola at 1x under ToolDefaultParallel emitted a different tree"
                    .to_string(),
            });

        let data = derive_runtime(&manifest, &stored_runtime([report]), &[]);
        let point = &data.platforms[0].epochs[0].points[0];
        assert!(point.complete, "the Rue measurement is untouched");
        assert!(
            data.platforms[0].epochs[0].comparison.is_some(),
            "the ratio still publishes; only the labelled secondary row is gone"
        );
        assert_eq!(point.failures.len(), 1);
        assert_eq!(point.failures[0].kind, "validation_rejected");
        assert_eq!(point.failures[0].workload, "gazette");
        assert!(
            point.failures[0].detail.contains("different tree"),
            "{:?}",
            point.failures[0].detail
        );
    }

    #[test]
    fn a_store_with_no_peer_measurement_publishes_no_comparison() {
        // The honest empty state: nothing is estimated, and the page renders
        // "no peer measurements have been collected yet" rather than a table.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([runtime_report('a', [7, 5, 6])]),
            &[],
        );
        assert!(data.platforms[0].epochs[0].comparison.is_none());
    }

    /// A report placed explicitly in the series, with the identities that decide
    /// which segment it lands in.
    ///
    /// The ordinary helper derives its timestamp from its samples, which is fine
    /// for one or two points and useless for a series whose whole subject is
    /// order.
    fn runtime_report_at(
        commit: char,
        minute: u32,
        elapsed: [u64; 3],
        fixture_identity: &str,
        source_identity: &str,
        compiler_version: &str,
    ) -> rue_perf_schema::RuntimeReport {
        let mut report = runtime_report(commit, elapsed);
        report.identity.finished_at = format!("2026-08-13T00:{minute:02}:00Z");
        report.identity.started_at = format!("2026-08-13T00:{minute:02}:00Z");
        report.identity.compiler_version = compiler_version.to_string();
        report
            .identity
            .workload_source_hashes
            .insert("wordfreq".to_string(), source_identity.repeat(64));
        report.workloads[0].recorded_inputs[0].identity_sha256 = fixture_identity.repeat(64);
        report
    }

    fn wordfreq_points(data: &RuntimeData) -> Vec<&RuntimeWorkloadPoint> {
        data.platforms[0].epochs[0]
            .points
            .iter()
            .map(|point| &point.workloads["wordfreq"])
            .collect()
    }

    #[test]
    fn a_corpus_change_opens_a_new_segment_rather_than_moving_the_series() {
        // ADR-0072 Decision 9. The input changed, so the medians either side are
        // not on the same scale; a consumer that drew one line across this
        // boundary would publish a false trend.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([
                runtime_report_at('a', 1, [10, 10, 10], "f", "3", "rue 0.1.0"),
                runtime_report_at('b', 2, [10, 10, 10], "f", "3", "rue 0.1.0"),
                // Same program, larger corpus.
                runtime_report_at('c', 3, [90, 90, 90], "e", "3", "rue 0.1.0"),
                runtime_report_at('d', 4, [90, 90, 90], "e", "3", "rue 0.1.0"),
            ]),
            &[],
        );
        let segments: Vec<u32> = wordfreq_points(&data)
            .iter()
            .map(|point| point.segment)
            .collect();
        assert_eq!(segments, vec![0, 0, 1, 1]);

        let corpus: Vec<&RuntimeAnnotation> = data.platforms[0].epochs[0]
            .annotations
            .iter()
            .filter(|note| note.kind == "corpus_change")
            .collect();
        assert_eq!(corpus.len(), 1);
        assert_eq!(corpus[0].commit, "c".repeat(40));
        assert_eq!(corpus[0].segment, Some(1));
        assert_eq!(corpus[0].workload, "wordfreq");
    }

    #[test]
    fn a_workload_source_change_segments_the_series_the_same_way() {
        // The other half of the recorded-not-pinned bargain: this suite records
        // the program's identity instead of pinning it, which is only defensible
        // if a movement in it reads as a discontinuity rather than as a result.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([
                runtime_report_at('a', 1, [10, 10, 10], "f", "3", "rue 0.1.0"),
                runtime_report_at('b', 2, [10, 10, 10], "f", "4", "rue 0.1.0"),
            ]),
            &[],
        );
        let segments: Vec<u32> = wordfreq_points(&data)
            .iter()
            .map(|point| point.segment)
            .collect();
        assert_eq!(segments, vec![0, 1]);
        assert!(
            data.platforms[0].epochs[0]
                .annotations
                .iter()
                .any(|note| note.kind == "workload_change"),
        );
    }

    #[test]
    fn the_trailing_window_never_reaches_across_a_discontinuity() {
        // The reason segmentation is derived rather than left to the page. With
        // a five-run window the sixth point is the first that can be judged; a
        // corpus change at the fifth restarts the count, so the sixth reports
        // "not enough history" instead of reporting the input change as a
        // hundredfold regression.
        let manifest = runtime_manifest_with_bound();
        let mut reports = Vec::new();
        for (index, commit) in "abcd".chars().enumerate() {
            reports.push(runtime_report_at(
                commit,
                index as u32 + 1,
                [10, 10, 10],
                "f",
                "3",
                "rue 0.1.0",
            ));
        }
        reports.push(runtime_report_at(
            'e',
            5,
            [1000, 1000, 1000],
            "e",
            "3",
            "rue 0.1.0",
        ));
        reports.push(runtime_report_at(
            '1',
            6,
            [1000, 1000, 1000],
            "e",
            "3",
            "rue 0.1.0",
        ));
        let data = derive_runtime(&manifest, &stored_runtime(reports), &[]);

        let flagged: Vec<Option<bool>> = wordfreq_points(&data)
            .iter()
            .map(|point| point.flagged)
            .collect();
        assert_eq!(flagged, vec![None, None, None, None, None, None]);
        assert!(
            wordfreq_points(&data)
                .iter()
                .all(|point| point.window_median_ns.is_none())
        );
    }

    /// The probe manifest with a provisional flagging bound declared, which is
    /// the only way a runtime workload is judged at all.
    fn runtime_manifest_with_bound() -> RuntimeManifest {
        RuntimeManifest::parse(&format!(
            "{RUNTIME_MANIFEST}\n[epoch.flagging.wordfreq]\nk = 3.0\nwindow = 5\n"
        ))
        .expect("runtime manifest with a declared bound")
    }

    #[test]
    fn movement_within_one_segment_is_flagged_against_its_own_window() {
        let manifest = runtime_manifest_with_bound();
        let mut reports: Vec<rue_perf_schema::RuntimeReport> = "abcde"
            .chars()
            .enumerate()
            .map(|(index, commit)| {
                runtime_report_at(
                    commit,
                    index as u32 + 1,
                    [10, 10, 10],
                    "f",
                    "3",
                    "rue 0.1.0",
                )
            })
            .collect();
        // Same corpus, same program, twenty times slower.
        reports.push(runtime_report_at(
            '1',
            6,
            [200, 200, 200],
            "f",
            "3",
            "rue 0.1.0",
        ));
        let data = derive_runtime(&manifest, &stored_runtime(reports), &[]);

        let points = wordfreq_points(&data);
        assert_eq!(points[4].flagged, None, "the window is not full until six");
        assert_eq!(points[5].flagged, Some(true));
        assert_eq!(points[5].window_median_ns, Some(10));
        // Uncalibrated, so a maintainer reads this as a triage item and the page
        // is obliged to say so.
        assert_eq!(points[5].flag_posture, "advisory");
    }

    #[test]
    fn a_workload_with_no_declared_bound_publishes_no_rule_and_no_verdict() {
        // What `k` and `window` should be here is ADR-0072's open question 4.
        // Deriving a verdict from constants nobody chose would answer it on the
        // dashboard, which is the last place that decision should be made.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([runtime_report('a', [7, 5, 6])]),
            &[],
        );
        let epoch = &data.platforms[0].epochs[0];
        assert!(epoch.flagging.is_empty());
        let workload = &epoch.points[0].workloads["wordfreq"];
        assert_eq!(workload.flagged, None);
        assert_eq!(workload.window_median_ns, None);
        // The posture is still published: it is what says the absence is
        // pending calibration rather than a permanent policy.
        assert_eq!(workload.flag_posture, "advisory");
    }

    #[test]
    fn a_declared_bound_publishes_the_rule_it_judged_by() {
        // Asserting "flagged" without publishing the rule behind it would leave
        // a reader unable to tell a measured bound from a provisional one.
        let data = derive_runtime(
            &runtime_manifest_with_bound(),
            &stored_runtime([runtime_report('a', [7, 5, 6])]),
            &[],
        );
        let rule = &data.platforms[0].epochs[0].flagging["wordfreq"];
        assert_eq!(rule.posture, "advisory");
        assert_eq!(rule.k, 3.0);
        assert_eq!(rule.window, 5);
        assert_eq!(rule.reference, "");
    }

    #[test]
    fn a_compiler_release_is_annotated_once_for_the_run() {
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([
                runtime_report_at('a', 1, [10, 10, 10], "f", "3", "rue 0.1.0"),
                runtime_report_at('b', 2, [10, 10, 10], "f", "3", "rue 0.2.0"),
                runtime_report_at('c', 3, [10, 10, 10], "f", "3", "rue 0.2.0"),
            ]),
            &[],
        );
        let releases: Vec<&RuntimeAnnotation> = data.platforms[0].epochs[0]
            .annotations
            .iter()
            .filter(|note| note.kind == "compiler_release")
            .collect();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].commit, "b".repeat(40));
        assert!(releases[0].workload.is_empty());
        assert!(releases[0].detail.contains("rue 0.1.0 → rue 0.2.0"));
    }

    #[test]
    fn a_suite_revision_change_is_annotated_on_the_epoch_it_opened() {
        // The one event of ADR-0072 Decision 8's four that cannot be seen from
        // inside a single epoch, since an epoch pins one revision for its life.
        let text = RUNTIME_MANIFEST.replace("collection = true", "collection = false")
            + r#"
[[suite]]
revision = 2
protocol_version = 1
measured_boundary = "spawn_to_exit_v1"

[[suite.workloads]]
id = "wordfreq"
source = "examples/wordfreq/main.rue"
question = "How fast does compiled Rue count words?"
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
description = "deterministic ASCII word text"

[suite.workloads.oracle]
kind = "golden_stdout"
path = "performance/fixtures/wordfreq/expected-stdout.txt"

[[epoch]]
id = 2
platform = "probe"
suite_revision = 2
target = "x86-64-linux"
compiler_args = ["-O3"]
optimization = "o3"
thread_policy = "single_threaded"
hardware_counters = "unavailable_on_hosted_runner"
collection = true

[epoch.environment]
runner_label = "probe"
runner_image = "probe"

[epoch.sampling.wordfreq]
samples = 3
"#;
        let manifest = RuntimeManifest::parse(&text).expect("two-revision manifest");
        let mut later = runtime_report_at('b', 2, [10, 10, 10], "f", "3", "rue 0.1.0");
        later.identity.epoch = 2;
        later.identity.suite_revision = 2;
        let data = derive_runtime(
            &manifest,
            &stored_runtime([
                runtime_report_at('a', 1, [10, 10, 10], "f", "3", "rue 0.1.0"),
                later,
            ]),
            &[],
        );
        let epochs = &data.platforms[0].epochs;
        assert_eq!(epochs.len(), 2);
        assert!(
            epochs[0].annotations.is_empty(),
            "the first epoch opened nothing"
        );
        let opening = &epochs[1].annotations[0];
        assert_eq!(opening.kind, "suite_revision");
        assert_eq!(opening.commit, "b".repeat(40));
        assert!(opening.detail.contains("revision 1"), "{}", opening.detail);
        assert!(opening.detail.contains("→ 2"), "{}", opening.detail);
    }

    #[test]
    fn a_record_this_build_cannot_read_is_reported_rather_than_fatal() {
        // The store is append-only, so a record from a future schema can never
        // be removed from it. A reader that failed the whole derivation on
        // meeting one would break the site build permanently.
        let manifest = RuntimeManifest::parse(RUNTIME_MANIFEST).expect("runtime manifest");
        let data = derive_runtime(
            &manifest,
            &stored_runtime([runtime_report('a', [7, 5, 6])]),
            &[UnreadableRecord {
                name: "deadbeef.json".to_string(),
                detail: "unknown field `peer_versions`".to_string(),
            }],
        );
        // The readable record still derives.
        assert_eq!(data.platforms[0].epochs[0].points.len(), 1);
        assert_eq!(data.rejected.len(), 1);
        assert_eq!(data.rejected[0].report, "deadbeef.json");
        assert!(
            data.rejected[0].reasons[0].contains("could not read the record"),
            "{:?}",
            data.rejected[0].reasons
        );
    }

    #[test]
    fn an_unreadable_record_on_disk_does_not_abort_loading() {
        let directory = tempfile::tempdir().expect("temp dir");
        let runtime = directory.path().join("runtime");
        std::fs::create_dir(&runtime).expect("create");
        let good = rue_perf_schema::canonical_json(&runtime_report('a', [7, 5, 6])).unwrap();
        std::fs::write(runtime.join("good.json"), &good).expect("write");
        std::fs::write(
            runtime.join("future.json"),
            r#"{"record_kind":"runtime_v2"}"#,
        )
        .expect("write");

        let (reports, unreadable) = load_runtime_records(directory.path()).expect("loaded");
        assert_eq!(reports.len(), 1);
        assert_eq!(unreadable.len(), 1);
        assert_eq!(unreadable[0].name, "future.json");
    }

    #[test]
    fn a_derivation_without_a_runtime_manifest_has_no_runtime_section() {
        // A page must be able to tell "not asked for" from "asked for and
        // nothing collected yet".
        let manifest = manifest_for_batch(1);
        assert!(derive(&manifest, &[]).runtime.is_none());
    }
}
