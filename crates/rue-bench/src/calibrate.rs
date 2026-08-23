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
//! ADR-0072's runtime suite asks the same four questions of a different record
//! kind, and gets different answers to two of them. [`calibrate_runtime`] is
//! that half. It shares this file because the method is the same one — measure
//! something that is not changing, and read the spread as noise — and because
//! the flagging sweep is literally the same sweep over the same
//! [`SweepPoint`]s. What it cannot share is the run shape: a runtime
//! observation has no batching factor to recommend, its five samples are far
//! fewer than the compile-time twelve, and what must be held constant for a
//! difference to be false by construction is not the compiler revision.
//!
//! Nothing produced here may enter a series. These are workflow artifacts; the
//! collector never reads them.

use std::collections::BTreeMap;
use std::path::Path;

use rue_perf_schema::{
    FIXTURE_INPUT_NAME, Metric, OracleVerdict, RunObject, RuntimeMetric, RuntimeObservation,
    RuntimeReport, StoredRuntimeReport, Summary, flags_movement, sample_value, summarize,
};

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
#[derive(Debug, Clone, Copy, serde::Serialize)]
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

// ---------------------------------------------------------------------------
// The ADR-0072 runtime suite
// ---------------------------------------------------------------------------

/// The fewest constant-condition observations any dispersion figure may rest
/// on.
///
/// Eight, and the reason is the estimator rather than the sweep. Both figures
/// below are medians over per-observation quantities — the within-observation
/// one over each observation's own relative MAD, the run-to-run one over
/// observation medians — and a median over fewer than a handful of coarse
/// inputs is itself coarse. The sweep's sufficiency is a separate question with
/// its own floor at [`MINIMUM_SWEEP_COMPARISONS`], which eight observations do
/// NOT satisfy at any window; the two are not derived from each other and
/// neither implies the other.
const MINIMUM_CALIBRATION_OBSERVATIONS: usize = 8;

/// The fewest samples per observation a *within-observation* dispersion figure
/// may rest on.
///
/// A MAD includes the deviation of the median from itself, which is zero. At
/// three samples the MAD is therefore the smaller of the two remaining
/// deviations, biased low by construction — and a dispersion figure biased low
/// produces a threshold that is too tight and a flag that is too eager, which
/// is the direction that trains a reader to ignore the word.
///
/// This suite genuinely collects below that floor and does so deliberately:
/// `performance/runtime.toml` declares `samples = 3` for `gazette_10x` on every
/// hosted epoch, because that rung exists for the shape of the curve rather
/// than for a tight median, and its cost is a maintainer decision under
/// ADR-0072 Decision 9 rather than a calibration one. So this floor gates the
/// within-observation figure and [`RuntimeWorkloadCalibration::recommended_samples`]
/// ALONE. It deliberately does not gate the flagging sweep, which stays valid
/// at three samples for the reason
/// [`RuntimeWorkloadCalibration::flagging_confidence`] gives: the sweep computes
/// each observation's summary exactly as the dashboard does, from the same
/// samples, so whatever bias the sample count carries is carried identically by
/// the rule being calibrated and by the rule as it will be applied.
const MINIMUM_CALIBRATION_SAMPLES: u32 = 5;

/// How far run-to-run dispersion must exceed within-observation dispersion
/// before sampling harder stops being the answer.
///
/// Twice: at that point over three quarters of the variance a published median
/// carries comes from between-run terms that no sample count touches.
const RUN_TO_RUN_DOMINANCE: f64 = 2.0;

/// The fewest comparisons a clean `(window, k)` pairing may rest on.
///
/// Ten, from the rule of three — no event in ten trials bounds the rate at
/// roughly 30% and no better — with one correction that makes even that
/// optimistic, and it is stated here rather than left for a reader to notice.
/// **Successive comparisons are not independent trials.** At window `W` each
/// comparison shares `W - 1` observations with the one before it, so a segment
/// of `S` observations yields `S - W` nominal comparisons containing roughly
/// `S / (W + 1)` independent ones. Twenty observations at window 10 give ten
/// nominal comparisons and about two independent ones; at window 3 they give
/// seventeen and about five.
///
/// So this is the floor at which a recommendation is worth *making*, not the
/// point at which it becomes a threshold, and the longest-window row of a sweep
/// is always its weakest. That is exactly why the analysis feeds ADR-0072's
/// open question 4 rather than answering it, and why the remedy for a thin
/// sweep is more repetitions rather than a rounder number.
const MINIMUM_SWEEP_COMPARISONS: usize = 10;

/// Roughly how many independent trials a run of overlapping comparisons holds.
///
/// A comparison at window `W` consumes `W + 1` observations, and consecutive
/// comparisons reuse all but one of them, so non-overlapping blocks are what an
/// independence claim can be made about. Reported beside the nominal count so a
/// recommendation cannot read as stronger than it is.
fn independent_comparisons(segment_length: usize, window: usize) -> usize {
    segment_length / (window + 1)
}

/// What the flagging sweep found, beyond the pairing it did or did not pick.
///
/// The distinctions matter because they call for different actions, and a
/// single absent recommendation conflates all of them: "nothing to compare"
/// asks for more collection, "clean but barely tried" asks for a longer sweep,
/// and "never clean" asks whether this platform can carry a flag at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlaggingVerdict {
    /// A pairing flagged nothing across enough comparisons to be worth citing.
    Recommended,
    /// The dispersion figures behind the sweep are too thin to recommend from,
    /// whatever the sweep found.
    EvidenceTooThin,
    /// No constant-condition segment is longer than the shortest swept window,
    /// so no comparison could be drawn at all.
    NoComparisons,
    /// Some pairing flagged nothing, but across too few comparisons to mean it.
    TooFewComparisons,
    /// Every pairing flagged something, including the largest multiplier swept.
    NeverClean,
}

/// How far a dispersion figure can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispersionConfidence {
    /// Enough repeated observations of an unchanged program, at enough samples
    /// each, for the figure to be read as a number.
    Sufficient,
    /// Not enough of either. The figure is reported so a reader can see what
    /// there is, and nothing is recommended from it.
    Thin,
}

/// One machine class's share of a workload's run-to-run dispersion.
///
/// A hosted pool hands out several CPU models, and a series sampled across them
/// has a run-to-run MAD that is a MIXTURE statistic: it is neither the noise of
/// one machine nor the size of the jump between machines, and reporting it
/// alone overstates the first while understating the second. On the real
/// `x86_64-linux` epoch 1 the pooled figure is 3.6% while each individual model
/// sits below 0.7% and the largest excursion is 32%.
///
/// This decomposition is presentation, not segmentation. The sweep deliberately
/// keeps measuring across machines, because production `derive` segments on the
/// fixture and workload-source identities alone and its trailing windows
/// therefore span machines too — a `k` calibrated on same-machine windows could
/// not survive real collection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvironmentDispersion {
    /// The CPU model the runner reported.
    pub cpu_model: String,
    /// Observations of this workload taken on it, within the longest segment.
    pub observations: usize,
    /// Median wall time on this machine class, in nanoseconds.
    pub median_wall_clock_ns: u64,
    /// Dispersion of observation medians within this machine class, or `None`
    /// when too few landed on it to have one.
    pub run_to_run_relative_mad: Option<f64>,
}

/// One observation that could not feed a dispersion estimate.
///
/// Reported rather than silently dropped: a calibration that quietly excluded
/// half its evidence would produce a tighter answer than the platform deserves,
/// and the exclusion is the more interesting half of the finding.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkippedObservation {
    /// The workload.
    pub workload: String,
    /// The record's own name.
    pub report: String,
    /// Why it was not used.
    pub reason: String,
}

/// What calibration recommends for one runtime workload on one platform.
///
/// Deliberately not [`WorkloadCalibration`]. That shape carries a batching
/// factor, and [`rue_perf_schema::RuntimeSamplingPolicy`] has none: these
/// workloads run for seconds, so there is no measurement floor to batch above,
/// and batching would average away the per-run dispersion this exists to
/// measure. It also carries no segmentation, because a compile-time workload's
/// input cannot move.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeWorkloadCalibration {
    /// The workload calibrated.
    pub workload: String,
    /// Observations that contributed.
    pub observations: usize,
    /// Constant-condition segments they fell into.
    pub segments: usize,
    /// The longest segment's length, which is what the figures below rest on.
    pub longest_segment: usize,
    /// Fewest samples any contributing observation carried.
    pub min_samples_per_observation: u32,
    /// Most samples any contributing observation carried.
    pub max_samples_per_observation: u32,
    /// Median wall time over the longest constant-condition segment, in
    /// nanoseconds.
    ///
    /// Taken within one segment rather than over everything, because a median
    /// pooled across a corpus change would be a median of two workloads.
    pub median_wall_clock_ns: u64,
    /// Dispersion inside a single observation, as the median of each
    /// observation's own relative MAD.
    pub within_observation_relative_mad: f64,
    /// How far that figure can be trusted.
    ///
    /// `Thin` below [`MINIMUM_CALIBRATION_SAMPLES`] samples an observation or
    /// [`MINIMUM_CALIBRATION_OBSERVATIONS`] observations. Separate from
    /// `flagging_confidence` because the two figures fail for different reasons
    /// and one failing does not implicate the other.
    pub within_observation_confidence: DispersionConfidence,
    /// Dispersion of observation medians inside a constant-condition segment.
    ///
    /// Taken only over segments of at least [`MINIMUM_CALIBRATION_OBSERVATIONS`]
    /// observations, so this figure is never itself the three-point estimate the
    /// sample floor above exists to refuse. Absent when no segment is that long.
    ///
    /// Read it beside `max_relative_excursion` and `by_cpu_model` rather than
    /// alone: on a heterogeneous pool it is a mixture of within-machine noise
    /// and between-machine jumps and describes neither.
    pub run_to_run_relative_mad: Option<f64>,
    /// Observations behind that figure.
    pub run_to_run_observations: usize,
    /// The largest single deviation from the segment median, as a fraction of
    /// it.
    ///
    /// The half a MAD is built to discard. A minority of excursions moves a MAD
    /// hardly at all — which is exactly why the published median is robust and
    /// exactly why a dispersion figure alone cannot tell a reader how far a
    /// hosted runner actually strays.
    pub max_relative_excursion: Option<f64>,
    /// Run-to-run dispersion broken down by machine class.
    ///
    /// Empty when the pool reported one CPU model, where the pooled figure is
    /// already the whole story.
    pub by_cpu_model: Vec<EnvironmentDispersion>,
    /// Whether run-to-run dispersion dwarfs the within-observation figure.
    ///
    /// When it does, `recommended_samples` is answering a question nobody is
    /// asking. More samples inside one observation tighten that observation's
    /// median and do nothing to the spread *between* observations, so a series
    /// whose noise is mostly between-run cannot be improved by sampling
    /// harder — the remedy is a quieter regime, or accepting the bound. On a
    /// hosted pool this is the usual state and not the exception.
    pub run_to_run_dominates: bool,
    /// Samples needed for the median's relative uncertainty to reach the
    /// target, or `None` when the evidence is too thin to say.
    ///
    /// Never below [`MINIMUM_CALIBRATION_SAMPLES`]. A recommendation under that
    /// floor would advise a sampling policy this very method then refuses to
    /// calibrate from, which is the one thing a calibration tool must not do.
    pub recommended_samples: Option<u32>,
    /// The flagging sweep for this workload.
    ///
    /// Per workload, unlike the compile-time sweep, because
    /// `performance/runtime.toml` declares `k` and `window` per workload per
    /// platform. One pooled sweep would recommend a constant no epoch table can
    /// express.
    pub flagging: FlaggingRecommendation,
    /// How far the sweep can be trusted.
    ///
    /// Gated on the observation count alone, and NOT on the sample count. The
    /// sweep summarizes each observation exactly as `derive` does, from the same
    /// samples the epoch declares, so a three-sample workload's downward-biased
    /// MAD is present identically on both sides: the rule being calibrated and
    /// the rule as the dashboard will apply it are the same rule over the same
    /// numbers. That is what keeps `gazette_10x` — three samples on every hosted
    /// epoch by deliberate cost policy — calibratable for the thing
    /// `[epoch.calibration]` actually declares, which is `k` and `window` and
    /// not a sample count.
    pub flagging_confidence: DispersionConfidence,
    /// What that sweep amounts to.
    pub flagging_verdict: FlaggingVerdict,
    /// Roughly how many *independent* comparisons back the recommended pairing.
    ///
    /// The sweep's own count treats overlapping trailing windows as separate
    /// trials, and they are not: see [`MINIMUM_SWEEP_COMPARISONS`]. This is the
    /// number a strength claim about the recommendation may actually use, and
    /// it is usually a small fraction of the nominal one.
    pub recommended_independent_comparisons: Option<usize>,
}

/// The runtime calibration report for one platform's epoch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuntimeCalibrationReport {
    /// The platform calibrated.
    pub platform: String,
    /// The epoch these reports were taken under.
    pub epoch: u32,
    /// The suite revision that epoch implements.
    ///
    /// Carried so a reader can see that a figure belongs to one workload
    /// contract. Pooling across suite revisions would be pooling across a
    /// change in what is measured, and partitioning by epoch prevents it: an
    /// epoch implements exactly one revision.
    pub suite_revision: u32,
    /// How many reports were analysed.
    pub reports: usize,
    /// How many distinct environment fingerprints appeared across them.
    pub distinct_environments: usize,
    /// The distinct CPU models those fingerprints named, sorted.
    ///
    /// The count alone understates this on a runtime suite. A compile-time
    /// calibration run is a burst on one runner; a runtime series is months of
    /// hosted allocations, and if the pool hands out several CPU models the
    /// dominant term in the dispersion below is which machine the job landed
    /// on. A maintainer reading a large `k` needs to know whether it is the
    /// workload or the pool.
    pub cpu_models: Vec<String>,
    /// Per-workload recommendations.
    pub workloads: Vec<RuntimeWorkloadCalibration>,
    /// Observations excluded, with the reason.
    pub skipped: Vec<SkippedObservation>,
}

/// Load every runtime report in a directory.
///
/// Takes the directory of records itself rather than the store root, so the
/// same call reads a workflow's freshly written reports and a checkout of
/// `performance-data-v1/runtime`.
///
/// A record this build cannot read is an error here, unlike in the derive step.
/// The store's reader must tolerate a record from a future schema forever,
/// because it cannot be removed from an append-only branch; a calibration is a
/// one-off analysis whose operator can narrow the directory, and silently
/// analysing a subset would report a dispersion for evidence it did not read.
pub fn load_runtime_reports(
    directory: &Path,
) -> Result<Vec<StoredRuntimeReport>, CalibrationError> {
    let listing = std::fs::read_dir(directory).map_err(|error| {
        CalibrationError(format!("could not read {}: {error}", directory.display()))
    })?;
    let mut reports = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|error| CalibrationError(error.to_string()))?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|error| {
            CalibrationError(format!("could not read {}: {error}", path.display()))
        })?;
        let stored = StoredRuntimeReport::read(&text).map_err(|error| {
            CalibrationError(format!(
                "{} is not a runtime report: {error}",
                path.display()
            ))
        })?;
        reports.push(stored);
    }
    Ok(reports)
}

/// One usable observation, reduced to what calibration reads.
struct RuntimePoint {
    /// Chronological key.
    finished_at: String,
    /// What has to be constant for a difference between two points to be noise.
    condition: ConstantCondition,
    /// The machine class this observation landed on. Not part of the condition
    /// — see [`EnvironmentDispersion`] for why it decomposes but never
    /// segments.
    cpu_model: String,
    /// Wall-clock summary over this observation's own samples.
    summary: Summary,
}

/// Everything that must be equal for two observations to differ only by noise.
///
/// This is the runtime answer to the compile-time calibrator's "all runs must
/// measure the same compiler revision". The parts are not the same, and only
/// one of them is an identity ADR-0072 Decision 2 defines.
///
/// The **workload identity** — the fixture's own digest — is the corpus, the
/// static assets, the preparer's assembly rules, and gazette's template port.
/// It is what `derive` segments the published Rue series on, and it is the
/// right key here for the same reason: it is exactly what gazette consumes. The
/// COMPARISON identity is deliberately not used. It adds the peer ports, the
/// peer preparer, every peer version, and the epoch — none of which any gazette
/// process can observe — so segmenting on it would discard Rue's own repeated
/// samples every time a peer moved, and produce a thinner and more optimistic
/// dispersion estimate from fewer points.
///
/// The **workload source digest** is the second half of `derive`'s key, kept so
/// that the segments a calibration reasons over are the segments the dashboard
/// draws.
///
/// The **program digest** is this calibration's own addition, and it replaces
/// the compile-time rule rather than supplementing it. That rule pins the
/// compiler revision because the compiler is the thing being measured. Here the
/// compiler is a tool and the measured thing is the executable it produced,
/// whose digest every observation already records. Two observations with equal
/// program and fixture digests ran a byte-identical program over byte-identical
/// input, so any difference between them is noise by construction — which is
/// what the method needs, and is strictly stronger than equal commits, since
/// most trunk commits do not change a given program's code at all.
#[derive(Clone, PartialEq, Eq)]
struct ConstantCondition {
    /// The workload identity: `recorded_inputs["fixture"].identity_sha256`.
    fixture: String,
    /// The workload's own source closure digest.
    source: String,
    /// The digest of the release-quality executable that was measured.
    program: String,
}

/// Why this observation cannot contribute, if it cannot.
///
/// Judged from the record's own evidence rather than by running
/// `validate_runtime_report`. A calibration lane deliberately measures one
/// commit repeatedly, so several of its reports are unappendable for reasons
/// that say nothing about dispersion — a sample count that is not the epoch's,
/// an environment policy a local run does not satisfy. What does matter is
/// whether the program produced the right answer, and the record says so.
fn runtime_observation_defect(
    report: &RuntimeReport,
    observation: &RuntimeObservation,
) -> Option<String> {
    if let Some(failure) = report
        .failures
        .iter()
        .find(|failure| failure.workload() == observation.workload)
    {
        return Some(format!("the report records a failure: {failure:?}"));
    }
    match observation.oracle.verdict {
        OracleVerdict::Match => {}
        verdict => {
            return Some(format!(
                "the oracle returned {verdict:?}; timing a program that produced the \
                 wrong answer measures the wrong program"
            ));
        }
    }
    if !observation.oracle.deterministic_across_samples {
        return Some(
            "the samples did not agree on their output, so they did not do the same work"
                .to_string(),
        );
    }
    if observation.samples.len() < 2 {
        return Some(format!(
            "{} sample(s); dispersion needs at least two",
            observation.samples.len()
        ));
    }
    None
}

/// Analyse stored runtime reports, one report per platform epoch.
///
/// Partitioned rather than refused, unlike [`calibrate`]. That function reads a
/// directory a workflow just wrote for one platform; this one must also read
/// the durable store, which holds every platform and every epoch in one
/// directory, and erroring on the mixture would make the store unreadable.
/// Nothing is pooled across a partition, which is the property that mattered.
pub fn calibrate_runtime(
    reports: &[StoredRuntimeReport],
) -> Result<Vec<RuntimeCalibrationReport>, CalibrationError> {
    if reports.is_empty() {
        return Err(CalibrationError(
            "no runtime reports were supplied".to_string(),
        ));
    }

    let mut partitions: BTreeMap<(String, u32), Vec<&StoredRuntimeReport>> = BTreeMap::new();
    for stored in reports {
        let identity = &stored.record().identity;
        partitions
            .entry((identity.platform.clone(), identity.epoch))
            .or_default()
            .push(stored);
    }

    Ok(partitions
        .into_values()
        .map(|mut partition| {
            // The trailing window is a window in time, so the order the store
            // happened to list files in must not decide it.
            partition.sort_by(|left, right| {
                left.record()
                    .identity
                    .finished_at
                    .cmp(&right.record().identity.finished_at)
                    .then_with(|| left.address().cmp(right.address()))
            });
            calibrate_runtime_partition(&partition)
        })
        .collect())
}

fn calibrate_runtime_partition(partition: &[&StoredRuntimeReport]) -> RuntimeCalibrationReport {
    let first = partition[0].record();

    let mut fingerprints: Vec<String> = partition
        .iter()
        .filter_map(|stored| stored.record().identity.environment.fingerprint_id().ok())
        .collect();
    fingerprints.sort();
    fingerprints.dedup();

    let mut cpu_models: Vec<String> = partition
        .iter()
        .map(|stored| stored.record().identity.environment.cpu_model.clone())
        .collect();
    cpu_models.sort();
    cpu_models.dedup();

    let mut skipped: Vec<SkippedObservation> = Vec::new();
    let mut series: BTreeMap<String, Vec<RuntimePoint>> = BTreeMap::new();
    for stored in partition {
        let report = stored.record();
        for observation in &report.workloads {
            if let Some(reason) = runtime_observation_defect(report, observation) {
                skipped.push(SkippedObservation {
                    workload: observation.workload.clone(),
                    report: stored.address().to_string(),
                    reason,
                });
                continue;
            }
            let Some(summary) = summarize(observation, RuntimeMetric::WallClock) else {
                continue;
            };
            let fixture = observation
                .recorded_inputs
                .iter()
                .find(|input| input.name == FIXTURE_INPUT_NAME)
                .map(|input| input.identity_sha256.clone())
                .unwrap_or_default();
            let source = report
                .identity
                .workload_source_hashes
                .get(&observation.workload)
                .cloned()
                .unwrap_or_default();
            series
                .entry(observation.workload.clone())
                .or_default()
                .push(RuntimePoint {
                    finished_at: report.identity.finished_at.clone(),
                    condition: ConstantCondition {
                        fixture,
                        source,
                        program: observation.program.sha256.clone(),
                    },
                    cpu_model: report.identity.environment.cpu_model.clone(),
                    summary,
                });
        }
    }

    let workloads = series
        .into_iter()
        .filter_map(|(workload, mut points)| {
            points.sort_by(|left, right| left.finished_at.cmp(&right.finished_at));
            calibrate_runtime_workload(&workload, &points)
        })
        .collect();

    RuntimeCalibrationReport {
        platform: first.identity.platform.clone(),
        epoch: first.identity.epoch,
        suite_revision: first.identity.suite_revision,
        reports: partition.len(),
        distinct_environments: fingerprints.len(),
        cpu_models,
        workloads,
        skipped,
    }
}

/// The median of a set of relative dispersions.
///
/// `rue_perf_schema::median` is over integers, which every stored measurement
/// is; a relative MAD is a ratio and has no integer form to take a median of.
/// Even-sized sets average the two central values, as that one does.
fn median_of(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    })
}

/// Split a chronological series into maximal runs of unchanging condition.
///
/// Maximal *consecutive* runs, matching `derive`: a corpus that changes and
/// later changes back opens a second segment rather than rejoining the first,
/// because the trailing window between them spans the change.
fn constant_condition_segments(points: &[RuntimePoint]) -> Vec<&[RuntimePoint]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for index in 1..points.len() {
        if points[index].condition != points[index - 1].condition {
            segments.push(&points[start..index]);
            start = index;
        }
    }
    if !points.is_empty() {
        segments.push(&points[start..]);
    }
    segments
}

fn calibrate_runtime_workload(
    workload: &str,
    points: &[RuntimePoint],
) -> Option<RuntimeWorkloadCalibration> {
    if points.is_empty() {
        return None;
    }
    let segments = constant_condition_segments(points);
    let longest = segments
        .iter()
        .max_by_key(|segment| segment.len())
        .copied()?;

    // Relative dispersion is comparable across corpora — that is what makes it
    // relative — so the within-observation figure pools every point. The median
    // of many coarse per-observation MADs is the estimate; a mean would carry a
    // descheduled sample straight into it.
    let within: Vec<f64> = points
        .iter()
        .map(|point| point.summary.relative_mad())
        .collect();
    let within_observation_relative_mad = median_of(&within).unwrap_or(0.0);

    // Run-to-run dispersion is not comparable across corpora, so it is taken
    // inside a segment — and only inside one long enough that this figure is
    // not itself the three-point estimate the sample floor exists to refuse.
    // Admitting shorter segments produced exactly that: a "0.17%" computed from
    // three observations, indistinguishable in the table from one computed from
    // sixteen, and `run_to_run_dominates` decided from it.
    let qualifying: Vec<&&[RuntimePoint]> = segments
        .iter()
        .filter(|segment| segment.len() >= MINIMUM_CALIBRATION_OBSERVATIONS)
        .collect();
    let per_segment: Vec<f64> = qualifying
        .iter()
        .filter_map(|segment| {
            let medians: Vec<u64> = segment.iter().map(|point| point.summary.median).collect();
            Summary::of(&medians).map(|summary| summary.relative_mad())
        })
        .collect();
    let run_to_run_relative_mad = median_of(&per_segment);
    let run_to_run_observations: usize = qualifying.iter().map(|segment| segment.len()).sum();

    let min_samples = points
        .iter()
        .map(|point| point.summary.count as u32)
        .min()
        .unwrap_or(0);
    let max_samples = points
        .iter()
        .map(|point| point.summary.count as u32)
        .max()
        .unwrap_or(0);

    let enough_observations = longest.len() >= MINIMUM_CALIBRATION_OBSERVATIONS;
    // Two confidences, because the two figures fail for different reasons. The
    // sample count bites the within-observation MAD and nothing else: the sweep
    // reads observation MEDIANS and each observation's own summary, computed
    // exactly as `derive` computes them, so it is self-consistent at any sample
    // count the epoch declares.
    let within_observation_confidence =
        if enough_observations && min_samples >= MINIMUM_CALIBRATION_SAMPLES {
            DispersionConfidence::Sufficient
        } else {
            DispersionConfidence::Thin
        };
    let flagging_confidence = if enough_observations {
        DispersionConfidence::Sufficient
    } else {
        DispersionConfidence::Thin
    };

    let segment_medians: Vec<u64> = longest.iter().map(|point| point.summary.median).collect();
    let median_wall_clock_ns = rue_perf_schema::median(&segment_medians).unwrap_or(0);
    let max_relative_excursion = (median_wall_clock_ns != 0).then(|| {
        segment_medians
            .iter()
            .map(|median| {
                median.abs_diff(median_wall_clock_ns) as f64 / median_wall_clock_ns as f64
            })
            .fold(0.0, f64::max)
    });

    let sweep = sweep_runtime_flagging(&segments);
    let (flagging, flagging_verdict, recommended_independent_comparisons) =
        recommend_from_sweep(sweep, flagging_confidence, &segments);

    Some(RuntimeWorkloadCalibration {
        workload: workload.to_string(),
        observations: points.len(),
        segments: segments.len(),
        longest_segment: longest.len(),
        min_samples_per_observation: min_samples,
        max_samples_per_observation: max_samples,
        median_wall_clock_ns,
        within_observation_relative_mad,
        within_observation_confidence,
        run_to_run_relative_mad,
        run_to_run_observations,
        max_relative_excursion,
        by_cpu_model: dispersion_by_cpu_model(longest),
        run_to_run_dominates: run_to_run_relative_mad.is_some_and(|between| {
            between > RUN_TO_RUN_DOMINANCE * within_observation_relative_mad
        }),
        // A recommendation from evidence this thin would be a number with a
        // decimal point and nothing behind it, which is worse than the absence.
        recommended_samples: (within_observation_confidence == DispersionConfidence::Sufficient)
            .then(|| recommended_runtime_samples(within_observation_relative_mad)),
        flagging,
        flagging_confidence,
        flagging_verdict,
        recommended_independent_comparisons,
    })
}

/// Samples the median needs, floored at what this method can calibrate from.
///
/// [`recommended_samples`] clamps at three, which is right for the compile-time
/// suites and wrong here: three samples is precisely the count
/// [`MINIMUM_CALIBRATION_SAMPLES`] refuses to estimate a within-observation
/// dispersion from, so recommending it would advise a sampling policy that
/// makes the workload permanently uncalibratable by this very tool. A tool must
/// not advise its way into its own refusal.
///
/// The floor binds often rather than rarely: these workloads run for seconds
/// and their within-observation dispersion is a fraction of a percent, so the
/// uncertainty target is met by a handful of samples and the honest reading of
/// the recommendation is "the sampling policy is not what limits this series".
fn recommended_runtime_samples(relative_mad: f64) -> u32 {
    recommended_samples(relative_mad).max(MINIMUM_CALIBRATION_SAMPLES)
}

/// Decompose a segment's run-to-run dispersion by machine class.
///
/// Empty for a homogeneous pool, where the pooled figure already is the whole
/// story and a one-row table would only imply otherwise.
fn dispersion_by_cpu_model(segment: &[RuntimePoint]) -> Vec<EnvironmentDispersion> {
    let mut grouped: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for point in segment {
        grouped
            .entry(point.cpu_model.as_str())
            .or_default()
            .push(point.summary.median);
    }
    if grouped.len() < 2 {
        return Vec::new();
    }
    grouped
        .into_iter()
        .map(|(cpu_model, medians)| EnvironmentDispersion {
            cpu_model: cpu_model.to_string(),
            observations: medians.len(),
            median_wall_clock_ns: rue_perf_schema::median(&medians).unwrap_or(0),
            // Three, not the calibration floor: this figure is published as a
            // decomposition to be read against the pooled one, never as a bound
            // to calibrate from, and suppressing the small groups would hide
            // the very machines a mixture is a mixture of. The observation
            // count rides beside it so a reader can weigh it.
            run_to_run_relative_mad: (medians.len() >= 3)
                .then(|| Summary::of(&medians).map(|summary| summary.relative_mad()))
                .flatten(),
        })
        .collect()
}

/// Choose a pairing from a sweep, or say why none was chosen.
///
/// Split from the sweep itself so the sweep table is always published whole. A
/// report that hid its evidence whenever it declined to recommend would leave a
/// maintainer with an opinion and nothing to check it against.
fn recommend_from_sweep(
    sweep: Vec<SweepPoint>,
    confidence: DispersionConfidence,
    segments: &[&[RuntimePoint]],
) -> (FlaggingRecommendation, FlaggingVerdict, Option<usize>) {
    let tried: usize = sweep
        .iter()
        .map(|point| point.comparisons)
        .max()
        .unwrap_or(0);
    let clean = sweep
        .iter()
        .filter(|point| point.comparisons > 0 && point.false_flags == 0)
        // Prefer the shortest window that works, then the smallest multiplier:
        // a shorter window reacts sooner, and a smaller multiplier detects
        // more. The shortest window is also the one whose comparisons overlap
        // least, so this preference happens to pick the best-supported row of
        // the sweep as well as the most responsive rule.
        .min_by(|left, right| {
            left.window.cmp(&right.window).then(
                left.k
                    .partial_cmp(&right.k)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        })
        .copied();

    let citable = clean.filter(|point| point.comparisons >= MINIMUM_SWEEP_COMPARISONS);
    let verdict = match (confidence, tried, clean, citable) {
        (DispersionConfidence::Thin, ..) => FlaggingVerdict::EvidenceTooThin,
        (_, 0, ..) => FlaggingVerdict::NoComparisons,
        (_, _, None, _) => FlaggingVerdict::NeverClean,
        (_, _, Some(_), None) => FlaggingVerdict::TooFewComparisons,
        (_, _, Some(_), Some(_)) => FlaggingVerdict::Recommended,
    };
    let chosen = matches!(verdict, FlaggingVerdict::Recommended)
        .then_some(citable)
        .flatten();

    // How much of the chosen pairing's evidence is actually independent. The
    // nominal count is what the sweep tried; this is what it is entitled to
    // claim, and the gap between them widens with the window.
    let independent = chosen.map(|point| {
        segments
            .iter()
            .map(|segment| independent_comparisons(segment.len(), point.window))
            .sum()
    });

    (
        FlaggingRecommendation {
            recommended_window: chosen.map(|point| point.window),
            recommended_k: chosen.map(|point| point.k),
            sweep,
        },
        verdict,
        independent,
    )
}

/// Sweep `(window, k)` over one workload's constant-condition segments.
///
/// The same sweep as [`sweep_flagging`], over the same [`SweepPoint`]s. What
/// differs is where a comparison may be drawn: only inside a segment, and never
/// across one. A comparison spanning a corpus change would count a real
/// difference as a false flag, and every such miscount pushes the recommended
/// multiplier up — so the failure mode is a bound too loose to detect anything,
/// arrived at from data that looked plentiful.
fn sweep_runtime_flagging(segments: &[&[RuntimePoint]]) -> Vec<SweepPoint> {
    let mut sweep = Vec::new();
    for &window in WINDOW_CANDIDATES {
        for &k in K_CANDIDATES {
            let mut false_flags = 0;
            let mut comparisons = 0;
            for segment in segments {
                if segment.len() <= window {
                    continue;
                }
                for index in window..segment.len() {
                    let trailing: Vec<u64> = segment[index - window..index]
                        .iter()
                        .map(|point| point.summary.median)
                        .collect();
                    let Some(window_summary) = Summary::of(&trailing) else {
                        continue;
                    };
                    comparisons += 1;
                    if flags_movement(segment[index].summary, window_summary, k) {
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
    sweep
}

/// Mark a figure whose evidence does not support recommending from it.
fn thin_marked(rendered: String, confidence: DispersionConfidence) -> String {
    match confidence {
        DispersionConfidence::Sufficient => rendered,
        DispersionConfidence::Thin => format!("{rendered} †"),
    }
}

/// Render the runtime reports as reviewable prose.
pub fn render_runtime(reports: &[RuntimeCalibrationReport]) -> String {
    let mut out = String::new();
    out.push_str(
        "# Runtime calibration (ADR-0072 Decision 5)\n\n\
         Dispersion is a property of the workload, so nothing here transfers from the compiler \
         suites, from another platform, or from another workload on this one. Each figure below \
         is measured from one workload's own repeated samples on one platform's epoch.\n\n\
         THIS RECOMMENDS; IT DOES NOT DECIDE. What `k` and `window` should be is ADR-0072's open \
         question 4 and a maintainer's call. Writing a recommendation into \
         `performance/runtime.toml` is a reviewed edit that cites this analysis, and until one \
         happens every runtime flag stays advisory.\n\n",
    );

    for report in reports {
        out.push_str(&format!(
            "## {} epoch {} (suite revision {})\n\n{} report(s), {} distinct environment \
             fingerprint(s).\n\n",
            report.platform,
            report.epoch,
            report.suite_revision,
            report.reports,
            report.distinct_environments,
        ));
        // Said whatever the CPU models look like, because a fingerprint covers
        // the runner image, kernel, and OS version too — and on a platform that
        // reports its CPU as `unknown`, drift in those is the only evidence of
        // a moving pool there is.
        if report.distinct_environments > 1 {
            out.push_str(&format!(
                "The hosted environment changed underneath this series: {} distinct fingerprints \
                 across {} report(s). Some of the dispersion below is that drift rather than the \
                 workload.\n\n",
                report.distinct_environments, report.reports,
            ));
        }
        if report.cpu_models.len() > 1 {
            out.push_str(&format!(
                "The runner pool handed out {} different CPU models: {}. On a series sampled over \
                 months this is usually the largest term in the dispersion below, and it is not \
                 the workload — a multiplier chosen to absorb it is absorbing the pool. The \
                 per-workload decompositions below separate the two.\n\n",
                report.cpu_models.len(),
                report.cpu_models.join(", "),
            ));
        }

        out.push_str(
            "| workload | median | within-obs. rel. MAD | run-to-run rel. MAD | max excursion | \
             samples/obs. | obs. | segments | longest | rec. samples |\n\
             | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        );
        for workload in &report.workloads {
            let samples =
                if workload.min_samples_per_observation == workload.max_samples_per_observation {
                    workload.min_samples_per_observation.to_string()
                } else {
                    format!(
                        "{}–{}",
                        workload.min_samples_per_observation, workload.max_samples_per_observation
                    )
                };
            out.push_str(&format!(
                "| {} | {:.1} ms | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                workload.workload,
                workload.median_wall_clock_ns as f64 / 1e6,
                // Marked rather than hidden when thin: the number is evidence
                // about how little there is, and only the recommendation
                // derived from it is withheld.
                thin_marked(
                    format!("{:.2}%", workload.within_observation_relative_mad * 100.0),
                    workload.within_observation_confidence,
                ),
                workload
                    .run_to_run_relative_mad
                    .map(|value| format!("{:.2}%", value * 100.0))
                    .unwrap_or_else(|| "—".to_string()),
                workload
                    .max_relative_excursion
                    .map(|value| format!("{:.1}%", value * 100.0))
                    .unwrap_or_else(|| "—".to_string()),
                samples,
                workload.observations,
                workload.segments,
                workload.longest_segment,
                workload
                    .recommended_samples
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "—".to_string()),
            ));
        }
        out.push_str(
            "\nA figure marked † rests on too little evidence to recommend from; see each \
             workload below. `max excursion` is the largest single deviation from the segment \
             median — the half a MAD is built to discard, and the one that says how far a hosted \
             runner actually strays.\n\n",
        );

        for workload in &report.workloads {
            out.push_str(&format!("### {}\n\n", workload.workload));

            if workload.min_samples_per_observation < MINIMUM_CALIBRATION_SAMPLES {
                out.push_str(&format!(
                    "**This workload collects {} sample(s) per observation, and its \
                     within-observation dispersion therefore cannot be estimated by this method \
                     at any number of repetitions.** A MAD includes the median's own zero \
                     deviation, so at {} samples it is the smaller of the two that remain and is \
                     biased low by construction. The remedy would be raising `samples` in \
                     `performance/runtime.toml`, which ADR-0072 Decision 9 makes a runner-cost \
                     decision rather than a calibration one — `gazette_10x` carries three by \
                     deliberate policy, because that rung exists for the shape of the curve \
                     rather than for a tight median.\n\n\
                     Its FLAGGING rule is unaffected and is calibrated below. The sweep \
                     summarizes each observation exactly as the dashboard's derive step does, \
                     from these same {} samples, so whatever bias the count carries is carried \
                     identically by the rule being calibrated and by the rule as it will be \
                     applied. What `[epoch.calibration]` declares is `k` and `window`, not a \
                     sample count.\n\n",
                    workload.min_samples_per_observation,
                    workload.min_samples_per_observation,
                    workload.min_samples_per_observation,
                ));
            }

            if !workload.by_cpu_model.is_empty() {
                let excursion = workload
                    .max_relative_excursion
                    .map(|value| format!("{:.1}%", value * 100.0))
                    .unwrap_or_else(|| "unknown".to_string());
                // Two wordings, because the strong one is a claim about a
                // pooled figure and there is not always a pooled figure to make
                // it about.
                match workload.run_to_run_relative_mad {
                    Some(pooled) => out.push_str(&format!(
                        "Its run-to-run figure is a MIXTURE and reads as neither of the things it \
                         mixes. Pooled it is {:.2}%; within one machine class the series is \
                         quieter than that, and across classes it jumps further, the largest \
                         single excursion being {excursion}. The sweep deliberately keeps \
                         measuring across machines, because production `derive` segments on the \
                         fixture and workload-source identities alone and its trailing windows \
                         span machines too — a bound calibrated on same-machine windows could not \
                         survive real collection. This table is how to read the one above, not a \
                         second segmentation.\n\n",
                        pooled * 100.0,
                    )),
                    None => out.push_str(&format!(
                        "Observations of this workload landed on more than one machine class, so \
                         any dispersion here would be a mixture of within-machine noise and \
                         between-machine jumps. There are too few for a pooled figure at all; the \
                         medians below are what there is, and the largest single excursion is \
                         {excursion}.\n\n",
                    )),
                }
                out.push_str(
                    "| CPU model | obs. | median | run-to-run rel. MAD |\n\
                     | --- | ---: | ---: | ---: |\n",
                );
                for environment in &workload.by_cpu_model {
                    out.push_str(&format!(
                        "| {} | {} | {:.1} ms | {} |\n",
                        environment.cpu_model,
                        environment.observations,
                        environment.median_wall_clock_ns as f64 / 1e6,
                        environment
                            .run_to_run_relative_mad
                            .map(|value| format!("{:.2}%", value * 100.0))
                            .unwrap_or_else(|| "—".to_string()),
                    ));
                }
                out.push('\n');
            }

            // Only where a sample count was recommended, since the note is
            // about what that count does and does not buy.
            if workload.run_to_run_dominates && workload.recommended_samples.is_some() {
                out.push_str(&format!(
                    "Run-to-run dispersion ({:.2}%) is more than {RUN_TO_RUN_DOMINANCE:.0}x the \
                     dispersion inside one observation ({:.2}%). A larger sample count tightens \
                     the second and leaves the first exactly where it is, so the recommended \
                     sample count above is answering the smaller question — and it is a floor \
                     rather than a target: it never drops below the {MINIMUM_CALIBRATION_SAMPLES} \
                     this method itself needs. What a flagging bound must absorb here is the \
                     between-run term.\n\n",
                    workload.run_to_run_relative_mad.unwrap_or_default() * 100.0,
                    workload.within_observation_relative_mad * 100.0,
                ));
            }

            match workload.flagging_verdict {
                FlaggingVerdict::Recommended => out.push_str(&format!(
                    "Smallest swept pairing with no false flag inside a constant-condition \
                     segment: k = {}, window = {}. It rests on at least \
                     {MINIMUM_SWEEP_COMPARISONS} comparisons, of which roughly {} are \
                     independent — successive comparisons at window {} share {} observations. A \
                     recommendation to review, not a threshold to apply.\n\n",
                    workload.flagging.recommended_k.unwrap_or_default(),
                    workload.flagging.recommended_window.unwrap_or_default(),
                    workload.recommended_independent_comparisons.unwrap_or(0),
                    workload.flagging.recommended_window.unwrap_or_default(),
                    workload
                        .flagging
                        .recommended_window
                        .unwrap_or_default()
                        .saturating_sub(1),
                )),
                FlaggingVerdict::EvidenceTooThin => out.push_str(&format!(
                    "Evidence is too thin to recommend from: the longest run of observations \
                     measuring an unchanged program over an unchanged corpus is {} and needs \
                     {MINIMUM_CALIBRATION_OBSERVATIONS}. The sweep is below for inspection and \
                     nothing is chosen from it. The remedy is repeated collection of one \
                     unchanged program, which is what the calibration workflow's runtime lane \
                     produces.\n\n",
                    workload.longest_segment,
                )),
                FlaggingVerdict::NoComparisons => out.push_str(
                    "No comparison was possible: no constant-condition segment is longer than \
                     the shortest swept window. This workload's calibration is waiting on \
                     repeated observations of an unchanged program.\n\n",
                ),
                FlaggingVerdict::TooFewComparisons => out.push_str(&format!(
                    "A pairing flagged nothing, and it is not citable: every clean pairing rests \
                     on fewer than {MINIMUM_SWEEP_COMPARISONS} comparisons, and no event in a \
                     handful of trials bounds a rate at anything useful — the fewer still once \
                     overlapping windows are discounted. Run the calibration lane for more \
                     repetitions rather than reading the table below as an answer.\n\n",
                )),
                FlaggingVerdict::NeverClean => out.push_str(
                    "No swept pairing eliminated false flags, including the largest multiplier \
                     swept. The runner is noisier than this workload can absorb; the honest \
                     answers are a quieter regime, more observations, or accepting that this \
                     platform carries no flag — a multiplier beyond the sweep is not one of \
                     them.\n\n",
                ),
            }
            let rows: Vec<&SweepPoint> = workload
                .flagging
                .sweep
                .iter()
                .filter(|point| point.comparisons > 0)
                .collect();
            if !rows.is_empty() {
                out.push_str(
                    "| window | k | false flags | comparisons | ~independent |\n\
                     | ---: | ---: | ---: | ---: | ---: |\n",
                );
                for point in rows {
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {} |\n",
                        point.window,
                        point.k,
                        point.false_flags,
                        point.comparisons,
                        independent_comparisons(workload.longest_segment, point.window),
                    ));
                }
                out.push('\n');
            }
        }

        if !report.skipped.is_empty() {
            out.push_str(&format!(
                "### Excluded observations ({})\n\n| workload | report | reason |\n\
                 | --- | --- | --- |\n",
                report.skipped.len()
            ));
            for skipped in &report.skipped {
                out.push_str(&format!(
                    "| {} | {} | {} |\n",
                    skipped.workload,
                    &skipped.report[..skipped.report.len().min(12)],
                    skipped.reason
                ));
            }
            out.push('\n');
        }
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
            boundary_evidence: Vec::new(),
            boundary_processes: Vec::new(),
            boundary_work_processes: Vec::new(),
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
            boundary: None,
            full_evidence: None,
            workloads: vec![WorkloadObservation {
                workload: "startup".to_string(),
                boundary: None,
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

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use rue_perf_schema::{
        EnvironmentFingerprint, GeneratedProvenance, HardwareCounterPolicy, InputCategory,
        OptimizationLevel, OracleKind, OracleOutcome, ProgramIdentity, RUNTIME_RECORD_KIND,
        RUNTIME_REPORT_SCHEMA_VERSION, RecordedInput, RuntimeBoundary, RuntimeIdentity,
        RuntimeRegime, RuntimeSample, ThreadPolicy,
    };

    /// What one synthetic observation varies.
    ///
    /// Every field here is one the calibration reasons about, so a test says
    /// what it is holding constant by leaving the default in place.
    #[derive(Clone)]
    struct Observation {
        platform: &'static str,
        epoch: u32,
        suite_revision: u32,
        /// Distinct per observation by default: per-push collection never
        /// repeats one, and the calibration must not need it to.
        commit: char,
        minute: u32,
        /// The WORKLOAD identity.
        fixture: char,
        /// The workload's own source closure.
        source: char,
        /// The measured executable.
        program: char,
        /// The COMPARISON identity, which must not segment anything.
        comparison: Option<char>,
        cpu_model: &'static str,
        runner_image_version: &'static str,
        samples: Vec<u64>,
        verdict: OracleVerdict,
        deterministic: bool,
    }

    impl Default for Observation {
        fn default() -> Self {
            Observation {
                platform: "x86_64-linux",
                epoch: 3,
                suite_revision: 3,
                commit: 'a',
                minute: 0,
                fixture: 'f',
                source: 's',
                program: 'p',
                comparison: None,
                cpu_model: "AMD EPYC 7763",
                runner_image_version: "1",
                samples: vec![1_000, 1_010, 990, 1_000, 1_005],
                verdict: OracleVerdict::Match,
                deterministic: true,
            }
        }
    }

    fn report(observation: &Observation) -> RuntimeReport {
        RuntimeReport {
            record_kind: RUNTIME_RECORD_KIND.to_string(),
            schema_version: RUNTIME_REPORT_SCHEMA_VERSION,
            identity: RuntimeIdentity {
                suite_revision: observation.suite_revision,
                epoch: observation.epoch,
                platform: observation.platform.to_string(),
                commit: std::iter::repeat_n(observation.commit, 40).collect(),
                compiler_version: "rue 0.1.0".to_string(),
                started_at: format!("2026-08-13T00:{:02}:00Z", observation.minute),
                finished_at: format!("2026-08-13T00:{:02}:30Z", observation.minute),
                toolchain_hash: "1".repeat(64),
                stdlib_hash: "2".repeat(64),
                workload_source_hashes: BTreeMap::from([(
                    "gazette".to_string(),
                    std::iter::repeat_n(observation.source, 64).collect(),
                )]),
                environment: EnvironmentFingerprint {
                    runner_label: "github-hosted".to_string(),
                    runner_image: "ubuntu-24.04".to_string(),
                    runner_image_version: observation.runner_image_version.to_string(),
                    cpu_model: observation.cpu_model.to_string(),
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
                workload: "gazette".to_string(),
                source: "examples/gazette/main.rue".to_string(),
                question: "How fast does compiled Rue build the site?".to_string(),
                program_args: vec!["build".to_string()],
                recorded_inputs: vec![RecordedInput {
                    name: FIXTURE_INPUT_NAME.to_string(),
                    category: InputCategory::Recorded,
                    description: "the live corpus".to_string(),
                    identity_sha256: std::iter::repeat_n(observation.fixture, 64).collect(),
                    files: 96,
                    bytes: 525_479,
                    provenance: Some(GeneratedProvenance {
                        generator: "probe".to_string(),
                        generator_revision: 1,
                        seed: 1,
                        vocabulary_size: 1,
                    }),
                    tree: None,
                }],
                program: ProgramIdentity {
                    binary_bytes: 65_536,
                    sha256: std::iter::repeat_n(observation.program, 64).collect(),
                },
                oracle: OracleOutcome {
                    kind: OracleKind::GoldenStdout,
                    reference: "probe".to_string(),
                    reference_sha256: "c".repeat(64),
                    observed_sha256: "c".repeat(64),
                    verdict: observation.verdict,
                    deterministic_across_samples: observation.deterministic,
                    detail: String::new(),
                },
                samples: observation
                    .samples
                    .iter()
                    .map(|process_elapsed_ns| RuntimeSample {
                        process_elapsed_ns: *process_elapsed_ns,
                        peak_memory_bytes: 1024,
                        exit_code: 0,
                        stdout_bytes: 0,
                        stdout_sha256: "c".repeat(64),
                        artifact_sha256: None,
                    })
                    .collect(),
                peers: Vec::new(),
                comparison: observation.comparison.map(|digest| {
                    rue_perf_schema::ComparisonIdentity {
                        identity_sha256: std::iter::repeat_n(digest, 64).collect(),
                        peer_port_revision: 1,
                        peer_versions: BTreeMap::from([("zola".to_string(), "0.21.0".to_string())]),
                    }
                }),
            }],
            failures: Vec::new(),
        }
    }

    fn stored(observations: &[Observation]) -> Vec<StoredRuntimeReport> {
        observations
            .iter()
            .map(|observation| {
                let text = rue_perf_schema::canonical_json(&report(observation)).unwrap();
                StoredRuntimeReport::read(&text).unwrap()
            })
            .collect()
    }

    /// `count` observations, each at its own commit and minute, otherwise
    /// identical. The shape a calibration lane produces, and the shape a quiet
    /// per-push series produces too.
    fn repeated(count: u32) -> Vec<Observation> {
        (0..count)
            .map(|index| Observation {
                commit: char::from(b'a' + (index % 26) as u8),
                minute: index,
                ..Observation::default()
            })
            .collect()
    }

    fn only(reports: Vec<RuntimeCalibrationReport>) -> RuntimeCalibrationReport {
        assert_eq!(reports.len(), 1, "expected one platform epoch");
        reports.into_iter().next().unwrap()
    }

    fn gazette(report: &RuntimeCalibrationReport) -> &RuntimeWorkloadCalibration {
        report
            .workloads
            .iter()
            .find(|workload| workload.workload == "gazette")
            .expect("the gazette workload")
    }

    #[test]
    fn a_moving_comparison_identity_does_not_segment_rues_own_series() {
        // THE LOAD-BEARING TEST. ADR-0072 Decision 2 splits the recorded
        // identity in two, and only the workload half describes what gazette
        // consumed. A peer template edit or a Hugo version bump moves the
        // comparison identity and cannot move one instruction gazette executes,
        // so segmenting on it would throw away Rue's own repeated samples every
        // time a peer moved — leaving a thinner and more optimistic dispersion
        // estimate from fewer points, with nothing in the report saying so.
        let mut observations = repeated(14);
        for (index, observation) in observations.iter_mut().enumerate() {
            // The peers move three times across the series. Nothing gazette
            // reads moves at all.
            observation.comparison = Some(char::from(b'0' + (index / 4) as u8));
        }
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);
        assert_eq!(workload.segments, 1, "the peers are not gazette's input");
        assert_eq!(workload.observations, 14);
        assert_eq!(workload.longest_segment, 14);
        assert_eq!(workload.flagging_verdict, FlaggingVerdict::Recommended);
    }

    #[test]
    fn a_corpus_change_opens_a_new_segment_and_no_comparison_crosses_it() {
        // The other direction of the same rule: the fixture digest IS what
        // gazette consumed, and a comparison spanning a change in it would
        // count a real difference as noise.
        let mut observations = repeated(14);
        for observation in observations.iter_mut().skip(7) {
            observation.fixture = 'g';
        }
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);
        assert_eq!(workload.segments, 2);
        assert_eq!(workload.longest_segment, 7);
        // Seven observations per segment: window 3 yields four comparisons in
        // each, and none of the eight straddles the boundary at index seven.
        let window_three = workload
            .flagging
            .sweep
            .iter()
            .find(|point| point.window == 3 && point.k == 1.0)
            .unwrap();
        assert_eq!(window_three.comparisons, 8);
    }

    #[test]
    fn a_recompiled_program_opens_a_new_segment() {
        // The runtime replacement for the compile-time rule that every run
        // measure one compiler revision. What must be constant is the thing
        // measured, and here that is the executable.
        let mut observations = repeated(14);
        for observation in observations.iter_mut().skip(9) {
            observation.program = 'q';
        }
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        assert_eq!(gazette(&report).segments, 2);
        assert_eq!(gazette(&report).longest_segment, 9);
    }

    #[test]
    fn differing_commits_over_one_unchanged_program_stay_one_segment() {
        // Why the commit is not the constancy key. Every observation here is at
        // its own commit — per-push collection never repeats one — and the
        // program digest says the measured executable never changed, so all
        // fourteen are repeated measurements of one thing. Requiring equal
        // commits would find no evidence at all in a store full of it.
        let observations = repeated(14);
        let commits: std::collections::BTreeSet<char> =
            observations.iter().map(|entry| entry.commit).collect();
        assert_eq!(commits.len(), 14, "the fixture must actually vary commits");

        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        assert_eq!(gazette(&report).segments, 1);
        assert_eq!(
            gazette(&report).flagging_confidence,
            DispersionConfidence::Sufficient
        );
    }

    #[test]
    fn quiet_repeated_observations_recommend_the_shortest_window_and_smallest_k() {
        let report = only(calibrate_runtime(&stored(&repeated(14))).unwrap());
        let workload = gazette(&report);
        assert_eq!(workload.flagging_verdict, FlaggingVerdict::Recommended);
        assert_eq!(workload.flagging.recommended_window, Some(3));
        assert_eq!(workload.flagging.recommended_k, Some(1.0));
        assert_eq!(workload.median_wall_clock_ns, 1_000);
        assert!(workload.recommended_samples.is_some());
        // Fourteen observations at window 3 give eleven nominal comparisons and
        // three independent ones, and the report may only claim the latter.
        assert_eq!(workload.recommended_independent_comparisons, Some(3));
        assert!(
            render_runtime(&[report]).contains("roughly 3 are independent"),
            "the recommendation must state what it actually rests on"
        );
    }

    #[test]
    fn a_recommended_sample_count_never_falls_below_what_this_method_can_calibrate() {
        // The defect this floor exists for. The compile-time clamp bottoms out
        // at three, and three samples is exactly the count
        // MINIMUM_CALIBRATION_SAMPLES refuses to estimate a within-observation
        // dispersion from — so the unclamped path advised a sampling policy
        // that would make the workload permanently uncalibratable by this very
        // tool. These workloads are quiet inside one observation, so the floor
        // binds on real data rather than in principle.
        assert_eq!(recommended_samples(0.0038), 3, "the compile-time clamp");
        assert_eq!(
            recommended_runtime_samples(0.0038),
            MINIMUM_CALIBRATION_SAMPLES
        );
        // A genuinely noisy workload still gets a real recommendation.
        assert!(recommended_runtime_samples(0.05) > MINIMUM_CALIBRATION_SAMPLES);

        let report = only(calibrate_runtime(&stored(&repeated(14))).unwrap());
        let recommended = gazette(&report).recommended_samples.unwrap();
        assert!(
            recommended >= MINIMUM_CALIBRATION_SAMPLES,
            "recommended {recommended}"
        );
    }

    #[test]
    fn three_samples_an_observation_block_the_sample_figure_but_not_the_flagging_rule() {
        // `gazette_10x` collects three samples on EVERY hosted epoch, by
        // deliberate cost policy under ADR-0072 Decision 9, and this issue
        // exists to give every declared workload a route out of advisory. A
        // blanket sample floor took that route away from one of the three.
        //
        // The split: a three-sample MAD is biased low, so no within-observation
        // dispersion and no sample count are reported. The sweep is unaffected,
        // because it summarizes each observation exactly as `derive` does from
        // these same three samples — the rule being calibrated and the rule as
        // applied are the same rule over the same numbers.
        let observations: Vec<Observation> = repeated(14)
            .into_iter()
            .map(|observation| Observation {
                samples: vec![1_000, 1_010, 990],
                ..observation
            })
            .collect();
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);
        assert_eq!(
            workload.within_observation_confidence,
            DispersionConfidence::Thin
        );
        assert_eq!(workload.recommended_samples, None);

        assert_eq!(
            workload.flagging_confidence,
            DispersionConfidence::Sufficient
        );
        assert_eq!(workload.flagging_verdict, FlaggingVerdict::Recommended);
        assert_eq!(workload.flagging.recommended_k, Some(1.0));

        let rendered = render_runtime(&[report]);
        assert!(rendered.contains("cannot be estimated by this method"));
        assert!(rendered.contains("Its FLAGGING rule is unaffected"));
    }

    #[test]
    fn too_few_observations_are_too_thin_to_recommend_from() {
        let report = only(calibrate_runtime(&stored(&repeated(4))).unwrap());
        let workload = gazette(&report);
        assert_eq!(workload.longest_segment, 4);
        assert_eq!(
            workload.within_observation_confidence,
            DispersionConfidence::Thin
        );
        assert_eq!(workload.flagging_confidence, DispersionConfidence::Thin);
        assert_eq!(workload.recommended_samples, None);
        assert_eq!(workload.flagging_verdict, FlaggingVerdict::EvidenceTooThin);
        // The observation count is the only criterion that failed, so it is the
        // only one the prose cites.
        let rendered = render_runtime(&[report]);
        assert!(rendered.contains("is 4 and needs 8"));
        assert!(!rendered.contains("sample(s) each (needs"));
    }

    #[test]
    fn a_clean_pairing_resting_on_too_few_comparisons_is_not_cited() {
        // Nine observations clear the confidence floor, and the shortest window
        // then yields six comparisons. Six trials with no false flag bound the
        // false-flag rate at roughly half, which is not a recommendation.
        let report = only(calibrate_runtime(&stored(&repeated(9))).unwrap());
        let workload = gazette(&report);
        assert_eq!(
            workload.flagging_confidence,
            DispersionConfidence::Sufficient
        );
        assert_eq!(
            workload.flagging_verdict,
            FlaggingVerdict::TooFewComparisons
        );
        assert_eq!(workload.recommended_independent_comparisons, None);
        assert_eq!(workload.flagging.recommended_k, None);
        let window_three = workload
            .flagging
            .sweep
            .iter()
            .find(|point| point.window == 3 && point.k == 1.0)
            .unwrap();
        assert_eq!(window_three.comparisons, 6);
        assert_eq!(window_three.false_flags, 0);
    }

    #[test]
    fn a_noisy_runner_that_never_flags_clean_says_so_rather_than_reaching_higher() {
        // The real x86_64-linux shape: a mostly quiet series in which the
        // hosted pool occasionally hands out a different machine, on an
        // unchanged program. Every excursion is a false flag at every swept
        // multiplier, because a quiet trailing window has almost no dispersion
        // for one to be measured against. The honest report is that this regime
        // cannot carry a flag — not a larger number.
        //
        // Note what the run-to-run figure does here: a MAD ignores a minority
        // of excursions entirely, so it stays near zero while the sweep is
        // full of false flags. The two figures answer different questions and
        // this is the case that shows it.
        let observations: Vec<Observation> = repeated(14)
            .into_iter()
            .enumerate()
            .map(|(index, observation)| Observation {
                samples: if index % 4 == 3 {
                    vec![3_000, 3_002, 2_998, 3_000, 3_001]
                } else {
                    vec![1_000, 1_002, 998, 1_000, 1_001]
                },
                ..observation
            })
            .collect();
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);
        assert_eq!(workload.flagging_verdict, FlaggingVerdict::NeverClean);
        assert_eq!(workload.flagging.recommended_k, None);
        assert!(
            workload
                .flagging
                .sweep
                .iter()
                .all(|point| point.comparisons == 0 || point.false_flags > 0)
        );
        assert!(render_runtime(&[report]).contains("noisier than this workload can absorb"));
    }

    #[test]
    fn run_to_run_dispersion_is_reported_beside_the_within_observation_figure() {
        // The two are different quantities and the difference is the whole
        // reason a sample count is not always the remedy.
        let observations: Vec<Observation> = repeated(14)
            .into_iter()
            .enumerate()
            .map(|(index, observation)| Observation {
                // Tight inside a run, loose between runs.
                samples: vec![
                    1_000 + index as u64 * 20,
                    1_001 + index as u64 * 20,
                    999 + index as u64 * 20,
                    1_000 + index as u64 * 20,
                    1_000 + index as u64 * 20,
                ],
                ..observation
            })
            .collect();
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);
        let between = workload
            .run_to_run_relative_mad
            .expect("a between-run figure");
        assert!(
            between > workload.within_observation_relative_mad,
            "{between} vs {}",
            workload.within_observation_relative_mad
        );
        assert!(workload.run_to_run_dominates);
        assert_eq!(workload.run_to_run_observations, 14);
    }

    #[test]
    fn a_run_to_run_figure_is_never_itself_a_three_point_estimate() {
        // The same objection this file raises against a three-SAMPLE MAD
        // applies to a three-OBSERVATION one, and the first implementation
        // admitted the latter: a segment of three published a run-to-run figure
        // indistinguishable in the table from one computed over sixteen, and
        // `run_to_run_dominates` was then decided from it.
        let mut observations = repeated(6);
        for (index, observation) in observations.iter_mut().enumerate() {
            // Two segments of three, so nothing reaches the floor.
            observation.fixture = if index < 3 { 'f' } else { 'g' };
        }
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);
        assert_eq!(workload.segments, 2);
        assert_eq!(workload.longest_segment, 3);
        assert_eq!(workload.run_to_run_relative_mad, None);
        assert_eq!(workload.run_to_run_observations, 0);
        assert!(!workload.run_to_run_dominates);
    }

    #[test]
    fn a_heterogeneous_pools_dispersion_is_decomposed_rather_than_reported_as_one_number() {
        // The real x86_64-linux shape, and the reason a single figure misleads
        // twice over: pooled it reads far noisier than any one machine, while
        // the excursion that actually moved the series is larger than the
        // pooled figure by an order of magnitude, because a MAD discards a
        // minority.
        let observations: Vec<Observation> = repeated(14)
            .into_iter()
            .enumerate()
            .map(|(index, observation)| {
                let slow = index % 4 == 3;
                Observation {
                    cpu_model: if slow { "SLOW-MODEL" } else { "FAST-MODEL" },
                    samples: if slow {
                        vec![2_000, 2_002, 1_998, 2_000, 2_001]
                    } else {
                        vec![1_000, 1_002, 998, 1_000, 1_001]
                    },
                    ..observation
                }
            })
            .collect();
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        let workload = gazette(&report);

        // Each machine class is quiet on its own.
        assert_eq!(workload.by_cpu_model.len(), 2);
        for environment in &workload.by_cpu_model {
            assert!(
                environment.run_to_run_relative_mad.unwrap() < 0.01,
                "{environment:?}"
            );
        }
        // The excursion the MAD discarded is the one worth knowing about.
        let excursion = workload.max_relative_excursion.expect("an excursion");
        assert!(excursion > 0.9, "{excursion}");
        assert!(excursion > workload.run_to_run_relative_mad.unwrap_or(0.0));

        let rendered = render_runtime(&[report]);
        assert!(rendered.contains("MIXTURE"));
        assert!(rendered.contains("SLOW-MODEL"));
        assert!(rendered.contains("max excursion"));
    }

    #[test]
    fn a_homogeneous_pool_gets_no_decomposition_table() {
        // A one-row table would imply a comparison there is nothing to make.
        let report = only(calibrate_runtime(&stored(&repeated(14))).unwrap());
        assert!(gazette(&report).by_cpu_model.is_empty());
        assert!(!render_runtime(&[report]).contains("MIXTURE"));
    }

    #[test]
    fn a_wrong_answer_is_excluded_from_the_dispersion_and_reported() {
        // Timing a program that produced the wrong output measures a different
        // program. Excluding it silently would be worse than including it.
        let mut observations = repeated(14);
        observations[5].verdict = OracleVerdict::Mismatch;
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        assert_eq!(gazette(&report).observations, 13);
        assert_eq!(report.skipped.len(), 1);
        assert!(
            report.skipped[0].reason.contains("Mismatch"),
            "{:?}",
            report.skipped
        );
        // The neighbours close over the gap rather than splitting: the program
        // and the corpus either side of the excluded point are the same ones,
        // so the observations that remain are still repeated measurements of
        // one thing. What must not happen is that the exclusion goes unsaid.
        assert_eq!(gazette(&report).segments, 1);
        assert!(render_runtime(&[report]).contains("Excluded observations"));
    }

    #[test]
    fn samples_that_disagreed_on_their_output_are_excluded() {
        let mut observations = repeated(14);
        observations[3].deterministic = false;
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        assert_eq!(gazette(&report).observations, 13);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("did not agree"));
    }

    #[test]
    fn platform_epochs_are_analysed_apart_and_never_pooled() {
        // An epoch implements exactly one suite revision, so partitioning by
        // epoch is what stops a sweep pooling across a change in what is
        // measured. Epochs 1 and 2 are retired and still declared; records
        // naming them are still in the store.
        let mut observations = repeated(10);
        for observation in observations.iter_mut().skip(5) {
            observation.epoch = 2;
            observation.suite_revision = 2;
        }
        observations.push(Observation {
            platform: "aarch64-macos",
            minute: 40,
            ..Observation::default()
        });

        let reports = calibrate_runtime(&stored(&observations)).unwrap();
        assert_eq!(reports.len(), 3);
        let described: Vec<(String, u32, u32, usize)> = reports
            .iter()
            .map(|report| {
                (
                    report.platform.clone(),
                    report.epoch,
                    report.suite_revision,
                    report.reports,
                )
            })
            .collect();
        assert_eq!(
            described,
            vec![
                ("aarch64-macos".to_string(), 3, 3, 1),
                ("x86_64-linux".to_string(), 2, 2, 5),
                ("x86_64-linux".to_string(), 3, 3, 5),
            ]
        );
    }

    #[test]
    fn a_heterogeneous_runner_pool_is_named_rather_than_absorbed() {
        // The real x86_64-linux store hands out four CPU models and the wall
        // times track them. A maintainer reading a large multiplier has to be
        // able to tell the pool from the workload.
        let observations: Vec<Observation> = repeated(14)
            .into_iter()
            .enumerate()
            .map(|(index, observation)| Observation {
                cpu_model: if index % 2 == 0 {
                    "AMD EPYC 7763"
                } else {
                    "INTEL(R) XEON(R) PLATINUM 8573C"
                },
                ..observation
            })
            .collect();
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        assert_eq!(report.cpu_models.len(), 2);
        assert!(report.distinct_environments > 1);
        let rendered = render_runtime(&[report]);
        assert!(rendered.contains("different CPU models"));
        assert!(rendered.contains("8573C"));
    }

    #[test]
    fn environment_drift_is_surfaced_even_where_the_cpu_model_is_unknown() {
        // Every aarch64-linux record in the real store reports `cpu_model:
        // "unknown"`, so the CPU-model warning can never fire there — while
        // three and four distinct fingerprints went past unremarked. The
        // fingerprint covers the runner image, kernel, and OS version too, and
        // on that platform it is the only evidence of a moving pool there is.
        let observations: Vec<Observation> = repeated(14)
            .into_iter()
            .enumerate()
            .map(|(index, observation)| Observation {
                cpu_model: "unknown",
                runner_image_version: if index % 2 == 0 { "1" } else { "2" },
                ..observation
            })
            .collect();
        let report = only(calibrate_runtime(&stored(&observations)).unwrap());
        assert_eq!(report.cpu_models, vec!["unknown".to_string()]);
        assert_eq!(report.distinct_environments, 2);
        let rendered = render_runtime(&[report]);
        assert!(rendered.contains("hosted environment changed underneath"));
        assert!(!rendered.contains("different CPU models"));
    }

    #[test]
    fn the_report_says_it_does_not_decide_the_policy() {
        // ADR-0072's open question 4 is a maintainer's, and a tool that reads
        // as though it had answered it would be answering it.
        let rendered = render_runtime(&calibrate_runtime(&stored(&repeated(14))).unwrap());
        assert!(rendered.contains("THIS RECOMMENDS; IT DOES NOT DECIDE"));
        assert!(rendered.contains("advisory"));
        assert!(rendered.contains("x86_64-linux epoch 3 (suite revision 3)"));
    }

    #[test]
    fn an_empty_directory_is_refused_rather_than_reported_as_quiet() {
        assert!(calibrate_runtime(&[]).is_err());
    }

    #[test]
    fn reports_are_analysed_in_chronological_order_not_filesystem_order() {
        // The trailing window is a window in time, and a content-addressed
        // store lists its records in digest order.
        let directory = tempfile::tempdir().unwrap();
        for index in [2u32, 0, 1] {
            let observation = Observation {
                minute: index,
                commit: char::from(b'a' + index as u8),
                ..Observation::default()
            };
            std::fs::write(
                directory.path().join(format!("{index}.json")),
                rue_perf_schema::canonical_json(&report(&observation)).unwrap(),
            )
            .unwrap();
        }
        let loaded = load_runtime_reports(directory.path()).unwrap();
        let analysed = only(calibrate_runtime(&loaded).unwrap());
        assert_eq!(analysed.reports, 3);
        assert_eq!(gazette(&analysed).segments, 1);
    }

    #[test]
    fn a_record_this_build_cannot_read_is_an_error_rather_than_a_thinner_answer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("broken.json"), "{}").unwrap();
        let error = load_runtime_reports(directory.path()).unwrap_err();
        assert!(
            error.to_string().contains("not a runtime report"),
            "{error}"
        );
    }
}
