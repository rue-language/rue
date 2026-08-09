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
    Band, Completeness, Manifest, Metric, RunObject, Summary, flags_movement, geometric_mean,
    median_absolute_deviation, ratio, sample_value, validate_run,
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
}

#[derive(Debug, Serialize)]
pub struct WorkloadDescription {
    pub id: String,
    pub question: String,
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
pub fn load_data_branch(data_root: &Path) -> Result<Vec<RunObject>, String> {
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
        let run: RunObject = serde_json::from_str(&text)
            .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
        runs.push(run);
    }
    Ok(runs)
}

/// Derive the dashboard's view of a set of runs.
pub fn derive(manifest: &Manifest, runs: &[RunObject]) -> SiteData {
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
                });
            }
        }
    }

    let mut rejected = Vec::new();
    // Group appendable runs by (platform, epoch). An unappendable run is
    // counted as evidence but never enters a series.
    let mut grouped: BTreeMap<(String, u32), Vec<&RunObject>> = BTreeMap::new();
    for run in runs {
        let outcome = validate_run(manifest, run);
        let address = run
            .content_address()
            .unwrap_or_else(|_| "<unaddressable>".to_string());
        if !outcome.is_appendable() {
            rejected.push(RejectedRun {
                run: address,
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
            .push(run);
    }

    let mut platforms: BTreeMap<String, Vec<EpochData>> = BTreeMap::new();
    for ((platform, epoch_id), mut epoch_runs) in grouped {
        // Measurement order, so the trailing window means what it says.
        epoch_runs.sort_by(|left, right| {
            left.identity
                .finished_at
                .cmp(&right.identity.finished_at)
                .then_with(|| left.identity.commit.cmp(&right.identity.commit))
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
    }
}

fn derive_epoch(
    manifest: &Manifest,
    epoch: &rue_perf_schema::PlatformEpoch,
    suite: &rue_perf_schema::SuiteRevision,
    runs: &[&RunObject],
) -> EpochData {
    // The baseline is the run the epoch names, not "the first one we have":
    // an attempted or partial run must never define ratio 1.0.
    let baseline_run = epoch.baseline.as_ref().and_then(|baseline| {
        runs.iter().find(|run| {
            run.content_address()
                .map(|address| address == baseline.run)
                .unwrap_or(false)
        })
    });
    let baseline_medians = baseline_run.map(|run| workload_medians(run, suite));

    let mut points: Vec<PointData> = Vec::new();
    let mut environment_annotations = Vec::new();
    let mut previous_environment: Option<(String, &rue_perf_schema::EnvironmentFingerprint)> = None;
    let mut stdlib_annotations = Vec::new();
    let mut previous_stdlib: Option<&str> = None;

    // History of each workload's summary, so the trailing window can be taken
    // over the *medians* of prior runs, matching the flagging rule's definition.
    let mut history: BTreeMap<String, Vec<Summary>> = BTreeMap::new();

    for run in runs {
        let outcome = validate_run(manifest, run);
        let address = run
            .content_address()
            .unwrap_or_else(|_| "<unaddressable>".to_string());
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
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
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
        let data = derive(&manifest, &[run]);

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
        let data = derive(&manifest, &[base, later]);

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
    fn a_partial_run_publishes_its_workloads_but_no_headline_point() {
        let base = run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]);
        let manifest = manifest_with_baseline(std::slice::from_ref(&base));
        let mut partial = run_at("b", "2026-07-28T01:00:00Z", [110, 110, 110]);
        // One sample short of the policy: a partial run, still appendable.
        partial.workloads[0].samples.truncate(2);
        let data = derive(&manifest, &[base, partial]);

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
        let data = derive(&manifest, &runs);
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
        let data = derive(&manifest, &[base]);
        let flagging = &data.platforms[0].epochs[0].flagging;
        assert_eq!(flagging.window, 3);
        assert!((flagging.k - 2.0).abs() < f64::EPSILON);
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
            let data = derive(&manifest, &[run]);
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
            &[run_at_batched(
                "a",
                "2026-07-28T00:00:00Z",
                [100, 100, 100],
                1,
            )],
        );
        let batched = derive(
            &manifest_for_batch(4),
            &[run_at_batched(
                "a",
                "2026-07-28T00:00:00Z",
                [100, 100, 100],
                4,
            )],
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
            &[run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100])],
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

        let data = derive(&manifest, &[first, second]);
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

        let data = derive(&manifest, &[first, second]);

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
            &[
                run_at("a", "2026-07-28T00:00:00Z", [100, 100, 100]),
                run_at("b", "2026-07-28T01:00:00Z", [100, 100, 100]),
            ],
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
        let data = derive(&manifest, &[late, early]);
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
        let data = derive(&manifest, &[run]);
        let point = &data.platforms[0].epochs[0].points[0].workloads["startup"];
        // The surviving two samples are both 100; a median including the
        // invalid one would still be 100, so assert on the band totals instead,
        // which the broken sample would have inflated.
        assert_eq!(point.compiler_root_ns, 100);
        assert_eq!(point.bands_ns.values().sum::<u64>(), 100);
    }
}
