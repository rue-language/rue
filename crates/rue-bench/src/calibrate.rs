//! Turning repeated runs into the constants an epoch pins.
//!
//! Calibration answers four questions the ADR deliberately leaves empty until
//! there is evidence: how many samples a workload needs, how much a very short
//! workload must batch, how large the flagging multiplier `k` must be, and how
//! long the trailing window should be.
//!
//! The method rests on one property. Every calibration run measures the *same*
//! compiler revision, so nothing real is changing. Any difference the flagging
//! rule reports is therefore a false positive by construction, and `k` can be
//! chosen as the smallest multiplier that produces none. That is a measurement
//! of the hardware's noise rather than a guess dressed up as a threshold.
//!
//! Nothing produced here may enter a series. These are workflow artifacts; the
//! collector never reads them.

use std::collections::BTreeMap;
use std::path::Path;

use rue_perf_schema::{Metric, RunObject, Summary, flags_movement, sample_value};

/// Candidate multipliers swept when choosing `k`.
const K_CANDIDATES: &[f64] = &[1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0];

/// Candidate trailing-window lengths swept alongside `k`.
const WINDOW_CANDIDATES: &[usize] = &[3, 5, 10];

/// Target relative uncertainty on a published median.
///
/// One percent: tight enough that a real regression of a few percent is
/// visible, loose enough not to demand impractical sample counts on shared
/// hardware.
const TARGET_MEDIAN_UNCERTAINTY: f64 = 0.01;

/// The shortest sample duration that does not fight the clock.
///
/// Below roughly this, per-process jitter and timer resolution dominate what is
/// supposed to be a measurement of the compiler, which is what batching exists
/// to fix.
const MINIMUM_SAMPLE_NS: f64 = 50_000_000.0;

/// Converting MAD to a standard-deviation-equivalent for a normal distribution.
const MAD_TO_SIGMA: f64 = 1.4826;

/// The median's standard error is this factor times the mean's, asymptotically.
const MEDIAN_EFFICIENCY: f64 = 1.2533;

/// What calibration recommends for one workload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkloadCalibration {
    /// The workload calibrated.
    pub workload: String,
    /// Median per-compilation latency across every calibration run, in
    /// nanoseconds.
    pub median_latency_ns: u64,
    /// Relative dispersion of latency, pooled across runs.
    pub relative_mad: f64,
    /// Samples needed for the median's relative uncertainty to reach the
    /// target.
    pub recommended_samples: u32,
    /// Compilations per sample so that a sample is long enough to measure.
    pub recommended_batch_size: u32,
    /// The batch size the calibration runs actually used.
    pub observed_batch_size: u32,
}

/// One point of the multiplier sweep.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepPoint {
    /// The trailing-window length.
    pub window: usize,
    /// The multiplier.
    pub k: f64,
    /// How many comparisons this pairing flagged.
    ///
    /// Every one is false: nothing changed between calibration runs.
    pub false_flags: usize,
    /// How many comparisons were possible at this window length.
    pub comparisons: usize,
}

/// What calibration recommends for the flagging rule.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlaggingRecommendation {
    /// The smallest swept `(window, k)` producing no false flags.
    ///
    /// Absent when even the largest candidate still flags, which means the
    /// hardware is too noisy for this corpus and the answer is more samples or
    /// different hardware, not a larger multiplier.
    pub recommended_window: Option<usize>,
    /// The recommended multiplier.
    pub recommended_k: Option<f64>,
    /// The full sweep, so the choice is auditable rather than asserted.
    pub sweep: Vec<SweepPoint>,
}

/// The calibration report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrationReport {
    /// The platform calibrated.
    pub platform: String,
    /// The epoch the calibration runs used.
    pub epoch: u32,
    /// How many runs were analysed.
    pub runs: usize,
    /// How many distinct environment fingerprints appeared across them.
    ///
    /// More than one means the hosted runner changed underneath the
    /// calibration, which is itself a finding: it sets expectations for how
    /// often environment annotations will appear in real collection.
    pub distinct_environments: usize,
    /// Per-workload recommendations.
    pub workloads: Vec<WorkloadCalibration>,
    /// The flagging-rule recommendation.
    pub flagging: FlaggingRecommendation,
}

/// Why calibration could not run.
#[derive(Debug)]
pub struct CalibrationError(pub String);

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Load every run object in a directory.
pub fn load_runs(directory: &Path) -> Result<Vec<RunObject>, CalibrationError> {
    let listing = std::fs::read_dir(directory).map_err(|error| {
        CalibrationError(format!("could not read {}: {error}", directory.display()))
    })?;
    let mut runs = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|error| CalibrationError(error.to_string()))?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|error| {
            CalibrationError(format!("could not read {}: {error}", path.display()))
        })?;
        let run: RunObject = serde_json::from_str(&text).map_err(|error| {
            CalibrationError(format!("{} is not a run object: {error}", path.display()))
        })?;
        runs.push(run);
    }
    // Chronological order matters: the trailing window is a window in time.
    runs.sort_by(|left, right| left.identity.started_at.cmp(&right.identity.started_at));
    Ok(runs)
}

/// Analyse a set of calibration runs.
pub fn calibrate(runs: &[RunObject]) -> Result<CalibrationReport, CalibrationError> {
    let first = runs
        .first()
        .ok_or_else(|| CalibrationError("no calibration runs were supplied".to_string()))?;

    // Comparing across epochs would measure the configuration difference rather
    // than the hardware's noise, which is the one thing calibration must isolate.
    if runs.iter().any(|run| {
        run.identity.epoch != first.identity.epoch
            || run.identity.platform != first.identity.platform
    }) {
        return Err(CalibrationError(
            "calibration runs must all come from one platform and epoch".to_string(),
        ));
    }
    // The whole method depends on nothing real changing between runs.
    if runs
        .iter()
        .any(|run| run.identity.commit != first.identity.commit)
    {
        return Err(CalibrationError(
            "calibration runs must all measure the same compiler revision, or a flagged \
             difference would not be known to be false"
                .to_string(),
        ));
    }

    let mut fingerprints: Vec<String> = runs
        .iter()
        .filter_map(|run| run.identity.environment.fingerprint_id().ok())
        .collect();
    fingerprints.sort();
    fingerprints.dedup();

    let mut workload_ids: Vec<String> = runs
        .iter()
        .flat_map(|run| run.workloads.iter().map(|entry| entry.workload.clone()))
        .collect();
    workload_ids.sort();
    workload_ids.dedup();

    let mut workloads = Vec::new();
    for workload in &workload_ids {
        if let Some(calibration) = calibrate_workload(runs, workload) {
            workloads.push(calibration);
        }
    }

    let flagging = sweep_flagging(runs, &workload_ids);

    Ok(CalibrationReport {
        platform: first.identity.platform.clone(),
        epoch: first.identity.epoch,
        runs: runs.len(),
        distinct_environments: fingerprints.len(),
        workloads,
        flagging,
    })
}

/// Every latency observation for one workload, pooled across runs.
fn pooled_latencies(runs: &[RunObject], workload: &str) -> Vec<u64> {
    runs.iter()
        .filter_map(|run| run.observation(workload))
        .flat_map(|observation| observation.samples.iter())
        .map(|sample| sample_value(sample, Metric::Latency))
        .collect()
}

fn calibrate_workload(runs: &[RunObject], workload: &str) -> Option<WorkloadCalibration> {
    let values = pooled_latencies(runs, workload);
    let summary = Summary::of(&values)?;

    let observed_batch_size = runs
        .iter()
        .filter_map(|run| run.observation(workload))
        .flat_map(|observation| observation.samples.iter())
        .map(|sample| sample.batch_size)
        .next()
        .unwrap_or(1);

    Some(WorkloadCalibration {
        workload: workload.to_string(),
        median_latency_ns: summary.median,
        relative_mad: summary.relative_mad(),
        recommended_samples: recommended_samples(summary.relative_mad()),
        recommended_batch_size: recommended_batch_size(summary.median),
        observed_batch_size,
    })
}

/// Samples needed for the median's relative uncertainty to reach the target.
///
/// From the asymptotic standard error of the median, `1.2533 * sigma / sqrt(n)`,
/// with `sigma` estimated as `1.4826 * MAD`. Clamped to a sane range: at least
/// three samples so a median means anything, and no more than 99 so one
/// pathologically noisy workload cannot make collection unaffordable — a
/// recommendation at the ceiling is itself the finding.
fn recommended_samples(relative_mad: f64) -> u32 {
    if relative_mad <= 0.0 {
        return 3;
    }
    let relative_sigma = MAD_TO_SIGMA * relative_mad;
    let needed = (MEDIAN_EFFICIENCY * relative_sigma / TARGET_MEDIAN_UNCERTAINTY).powi(2);
    (needed.ceil() as u64).clamp(3, 99) as u32
}

/// Compilations per sample so a sample is long enough to measure.
fn recommended_batch_size(median_latency_ns: u64) -> u32 {
    if median_latency_ns == 0 {
        return 1;
    }
    let needed = (MINIMUM_SAMPLE_NS / median_latency_ns as f64).ceil();
    (needed.max(1.0) as u64).clamp(1, 1000) as u32
}

/// Sweep `(window, k)` and count false flags.
///
/// A comparison is made for every run with a full trailing window behind it.
/// The window's summary is taken over the *medians* of its runs, matching §5:
/// the pooled uncertainty combines the current run's dispersion with the
/// dispersion of the trailing window's medians.
fn sweep_flagging(runs: &[RunObject], workloads: &[String]) -> FlaggingRecommendation {
    let mut per_workload_medians: BTreeMap<&str, Vec<Summary>> = BTreeMap::new();
    for workload in workloads {
        let summaries: Vec<Summary> = runs
            .iter()
            .filter_map(|run| run.observation(workload))
            .filter_map(|observation| {
                let values: Vec<u64> = observation
                    .samples
                    .iter()
                    .map(|sample| sample_value(sample, Metric::Latency))
                    .collect();
                Summary::of(&values)
            })
            .collect();
        per_workload_medians.insert(workload.as_str(), summaries);
    }

    let mut sweep = Vec::new();
    for &window in WINDOW_CANDIDATES {
        for &k in K_CANDIDATES {
            let mut false_flags = 0;
            let mut comparisons = 0;
            for summaries in per_workload_medians.values() {
                if summaries.len() <= window {
                    continue;
                }
                for index in window..summaries.len() {
                    let trailing: Vec<u64> = summaries[index - window..index]
                        .iter()
                        .map(|summary| summary.median)
                        .collect();
                    let Some(window_summary) = Summary::of(&trailing) else {
                        continue;
                    };
                    comparisons += 1;
                    if flags_movement(summaries[index], window_summary, k) {
                        false_flags += 1;
                    }
                }
            }
            sweep.push(SweepPoint {
                window,
                k,
                false_flags,
                comparisons,
            });
        }
    }

    // Prefer the shortest window that works, then the smallest multiplier: a
    // shorter window reacts sooner, and a smaller multiplier detects more.
    let clean = sweep
        .iter()
        .filter(|point| point.comparisons > 0 && point.false_flags == 0)
        .min_by(|left, right| {
            left.window.cmp(&right.window).then(
                left.k
                    .partial_cmp(&right.k)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });

    FlaggingRecommendation {
        recommended_window: clean.map(|point| point.window),
        recommended_k: clean.map(|point| point.k),
        sweep,
    }
}

/// Render the report as reviewable prose.
pub fn render(report: &CalibrationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Calibration: {} epoch {}\n\n{} runs, {} distinct environment fingerprint(s).\n\n",
        report.platform, report.epoch, report.runs, report.distinct_environments
    ));
    if report.distinct_environments > 1 {
        out.push_str(
            "The hosted runner changed underneath this calibration. That sets the expectation \
             for how often environment annotations will appear in real collection.\n\n",
        );
    }

    out.push_str("## Per-workload\n\n");
    out.push_str("| workload | median | rel. MAD | samples | batch (observed) |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
    for workload in &report.workloads {
        out.push_str(&format!(
            "| {} | {:.3} ms | {:.2}% | {} | {} ({}) |\n",
            workload.workload,
            workload.median_latency_ns as f64 / 1e6,
            workload.relative_mad * 100.0,
            workload.recommended_samples,
            workload.recommended_batch_size,
            workload.observed_batch_size,
        ));
    }

    out.push_str("\n## Flagging rule\n\n");
    match (
        report.flagging.recommended_window,
        report.flagging.recommended_k,
    ) {
        (Some(window), Some(k)) => out.push_str(&format!(
            "Recommended: k = {k}, window = {window}. Smallest swept pairing with no false \
             flags across runs of one unchanged revision.\n\n",
        )),
        _ => out.push_str(
            "No swept pairing eliminated false flags. The hardware is noisier than this corpus \
             can absorb; the answer is more samples or different hardware, not a larger \
             multiplier.\n\n",
        ),
    }
    out.push_str("| window | k | false flags | comparisons |\n| ---: | ---: | ---: | ---: |\n");
    for point in &report.flagging.sweep {
        if point.comparisons == 0 {
            continue;
        }
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            point.window, point.k, point.false_flags, point.comparisons
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_perf_schema::{
        EnvironmentFingerprint, Invocation, Phase, PhaseAccounting, RUN_SCHEMA_VERSION,
        ResolvedPins, RunIdentity, Sample, WorkloadObservation,
    };

    fn sample(latency_ns: u64, batch: u32) -> Sample {
        let phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        Sample {
            batch_size: batch,
            process_elapsed_ns: latency_ns * u64::from(batch) + 1,
            peak_memory_bytes: 1024,
            output_binary_bytes: 512,
            phases: PhaseAccounting {
                phase_ns,
                mixed_parallel_ns: 0,
                unattributed_ns: 0,
                compiler_root_ns: latency_ns * u64::from(batch),
            },
        }
    }

    fn run(index: u32, latencies: &[u64], batch: u32) -> RunObject {
        RunObject {
            schema_version: RUN_SCHEMA_VERSION,
            identity: RunIdentity {
                suite_revision: 1,
                epoch: 1,
                platform: "aarch64-macos".to_string(),
                commit: "a".repeat(40),
                started_at: format!("2026-07-28T00:{index:02}:00Z"),
                finished_at: format!("2026-07-28T00:{index:02}:30Z"),
                pins: ResolvedPins {
                    toolchain_hash: "t".to_string(),
                    stdlib_hash: "s".to_string(),
                    workload_source_hashes: BTreeMap::new(),
                    invocation: Invocation {
                        target: "aarch64-apple-darwin".to_string(),
                        args: Vec::new(),
                    },
                },
                environment: EnvironmentFingerprint {
                    runner_label: "github-hosted".to_string(),
                    runner_image: "macos-15".to_string(),
                    runner_image_version: "macos15/1.0".to_string(),
                    cpu_model: "Apple M2".to_string(),
                    core_count: 4,
                    memory_bytes: 1,
                    kernel_version: "probe".to_string(),
                    os_version: "probe".to_string(),
                    architecture: "aarch64".to_string(),
                },
            },
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
                samples: latencies.iter().map(|ns| sample(*ns, batch)).collect(),
            }],
            failures: Vec::new(),
        }
    }

    fn quiet_runs(count: u32) -> Vec<RunObject> {
        (0..count)
            .map(|index| {
                run(
                    index,
                    &[1_000_000, 1_001_000, 999_000, 1_000_500, 1_000_000],
                    1,
                )
            })
            .collect()
    }

    #[test]
    fn calibration_refuses_runs_of_different_revisions() {
        // The whole method rests on nothing real changing between runs.
        let mut runs = quiet_runs(4);
        runs[2].identity.commit = "b".repeat(40);
        let error = calibrate(&runs).unwrap_err();
        assert!(
            error.to_string().contains("same compiler revision"),
            "{error}"
        );
    }

    #[test]
    fn calibration_refuses_runs_from_different_epochs() {
        let mut runs = quiet_runs(4);
        runs[1].identity.epoch = 2;
        assert!(calibrate(&runs).is_err());
    }

    #[test]
    fn calibration_needs_at_least_one_run() {
        assert!(calibrate(&[]).is_err());
    }

    #[test]
    fn quiet_hardware_yields_a_small_multiplier_and_few_samples() {
        let report = calibrate(&quiet_runs(12)).unwrap();
        assert_eq!(report.runs, 12);
        assert_eq!(report.distinct_environments, 1);

        let workload = &report.workloads[0];
        assert!(workload.relative_mad < 0.01, "{}", workload.relative_mad);
        assert!(
            workload.recommended_samples <= 10,
            "{}",
            workload.recommended_samples
        );

        // Identical runs cannot produce a false flag at any multiplier, so the
        // sweep settles on the shortest window and smallest k.
        assert_eq!(report.flagging.recommended_window, Some(3));
        assert_eq!(report.flagging.recommended_k, Some(1.0));
    }

    #[test]
    fn noisy_hardware_demands_a_larger_multiplier() {
        // Run-to-run medians swing widely, so small multipliers flag.
        let latencies = [
            vec![1_000_000, 1_010_000, 990_000],
            vec![1_400_000, 1_390_000, 1_410_000],
            vec![1_000_000, 1_005_000, 995_000],
            vec![1_500_000, 1_490_000, 1_510_000],
            vec![1_000_000, 1_002_000, 998_000],
            vec![1_600_000, 1_590_000, 1_610_000],
            vec![1_000_000, 1_004_000, 996_000],
            vec![1_450_000, 1_440_000, 1_460_000],
        ];
        let runs: Vec<RunObject> = latencies
            .iter()
            .enumerate()
            .map(|(index, values)| run(index as u32, values, 1))
            .collect();
        let report = calibrate(&runs).unwrap();

        let smallest = report
            .flagging
            .sweep
            .iter()
            .find(|point| point.window == 3 && point.k == 1.0)
            .unwrap();
        assert!(smallest.false_flags > 0, "noisy data must flag at k = 1");

        // Whatever it recommends must genuinely be clean at that pairing.
        if let (Some(window), Some(k)) = (
            report.flagging.recommended_window,
            report.flagging.recommended_k,
        ) {
            let chosen = report
                .flagging
                .sweep
                .iter()
                .find(|point| point.window == window && point.k == k)
                .unwrap();
            assert_eq!(chosen.false_flags, 0);
        }
    }

    #[test]
    fn a_noisier_workload_is_recommended_more_samples() {
        assert!(recommended_samples(0.05) > recommended_samples(0.005));
    }

    #[test]
    fn sample_recommendations_stay_within_a_usable_range() {
        // A floor so a median means something, a ceiling so one pathological
        // workload cannot make collection unaffordable.
        assert_eq!(recommended_samples(0.0), 3);
        assert_eq!(recommended_samples(f64::MAX), 99);
        assert!((3..=99).contains(&recommended_samples(0.02)));
    }

    #[test]
    fn a_short_workload_is_recommended_batching_and_a_long_one_is_not() {
        // 1 ms per compile needs batching to clear the measurement floor.
        assert!(recommended_batch_size(1_000_000) > 1);
        // 200 ms per compile is already well clear of it.
        assert_eq!(recommended_batch_size(200_000_000), 1);
    }

    #[test]
    fn batched_runs_report_per_compilation_latency() {
        // Batching must not inflate the recommendation: a 1 ms compile measured
        // in batches of 50 is still a 1 ms compile.
        let runs: Vec<RunObject> = (0..4)
            .map(|index| run(index, &[1_000_000, 1_000_000, 1_000_000], 50))
            .collect();
        let report = calibrate(&runs).unwrap();
        assert_eq!(report.workloads[0].median_latency_ns, 1_000_000);
        assert_eq!(report.workloads[0].observed_batch_size, 50);
    }

    #[test]
    fn environment_drift_across_calibration_is_counted_and_surfaced() {
        let mut runs = quiet_runs(6);
        runs[3].identity.environment.runner_image_version = "macos15/2.0".to_string();
        let report = calibrate(&runs).unwrap();
        assert_eq!(report.distinct_environments, 2);
        assert!(render(&report).contains("changed underneath"));
    }

    #[test]
    fn the_report_renders_its_sweep_for_audit() {
        let report = calibrate(&quiet_runs(8)).unwrap();
        let rendered = render(&report);
        assert!(rendered.contains("aarch64-macos"));
        assert!(rendered.contains("Flagging rule"));
        assert!(rendered.contains("startup"));
        // The full sweep is shown, so a recommendation can be checked rather
        // than taken on faith.
        assert!(rendered.matches('|').count() > 20);
    }

    #[test]
    fn runs_are_analysed_in_chronological_order() {
        // The trailing window is a window in time; loading must not depend on
        // filesystem ordering.
        let directory = tempfile::tempdir().unwrap();
        for index in [2u32, 0, 1] {
            let run = run(index, &[1_000_000], 1);
            std::fs::write(
                directory.path().join(format!("{index}.json")),
                serde_json::to_string(&run).unwrap(),
            )
            .unwrap();
        }
        let loaded = load_runs(directory.path()).unwrap();
        let starts: Vec<&str> = loaded
            .iter()
            .map(|run| run.identity.started_at.as_str())
            .collect();
        assert_eq!(
            starts,
            vec![
                "2026-07-28T00:00:00Z",
                "2026-07-28T00:01:00Z",
                "2026-07-28T00:02:00Z"
            ]
        );
    }
}
