//! Robust statistics over raw samples.
//!
//! Everything here is derived, never stored. It lives with the schema rather
//! than in one consumer because the calibrator and the dashboard must agree
//! exactly on what "moved" means — two implementations of the flagging rule
//! would drift, and the one that drifted would be the one declaring
//! regressions.
//!
//! Medians and MAD rather than means and standard deviations: a single
//! descheduled sample on a hosted runner is a large outlier, and the mean would
//! carry it into the published figure.

use crate::run::Sample;
use crate::series::Metric;

/// The value one sample contributes for a metric.
///
/// Batched samples report the batch's total, so latency is divided back down to
/// a per-compilation figure. Memory and binary size are properties of a single
/// compilation regardless of batching, so they are taken as-is.
pub fn sample_value(sample: &Sample, metric: Metric) -> u64 {
    match metric {
        Metric::Latency => {
            let batch = u64::from(sample.batch_size).max(1);
            sample.phases.compiler_root_ns / batch
        }
        Metric::PeakMemory => sample.peak_memory_bytes,
        Metric::BinarySize => sample.output_binary_bytes,
    }
}

/// The median of a set of observations.
///
/// Even-sized sets average the two central values, rounding down. Returns
/// `None` for an empty set: there is no median of nothing, and inventing zero
/// would silently poison every comparison downstream.
pub fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        // Averaged as u128 so two large values cannot overflow on the way to a
        // value that would have fit.
        ((u128::from(sorted[middle - 1]) + u128::from(sorted[middle])) / 2) as u64
    })
}

/// Median absolute deviation: the median of `|value - median|`.
///
/// This is the dispersion measure the system publishes. Unlike a standard
/// deviation it does not move when one sample is wild, which on shared hosted
/// hardware is the common case rather than the exception.
pub fn median_absolute_deviation(values: &[u64]) -> Option<u64> {
    let center = median(values)?;
    let deviations: Vec<u64> = values.iter().map(|value| value.abs_diff(center)).collect();
    median(&deviations)
}

/// Median and dispersion together, the pair every published figure carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// The median observation.
    pub median: u64,
    /// The median absolute deviation.
    pub mad: u64,
    /// How many observations went into it.
    pub count: usize,
}

impl Summary {
    /// Summarize a set of observations.
    pub fn of(values: &[u64]) -> Option<Summary> {
        Some(Summary {
            median: median(values)?,
            mad: median_absolute_deviation(values)?,
            count: values.len(),
        })
    }

    /// Dispersion as a fraction of the median.
    ///
    /// The comparable figure across workloads of wildly different absolute
    /// cost, and the one calibration reads to choose sample counts.
    pub fn relative_mad(&self) -> f64 {
        if self.median == 0 {
            return 0.0;
        }
        self.mad as f64 / self.median as f64
    }
}

/// Whether a current observation counts as movement against a trailing window.
///
/// Implements ADR-0067 §5: flag only when the difference between the current
/// median and the trailing-window median exceeds `k` times the pooled
/// uncertainty of the two. Anything inside that bound is uncertain, not
/// movement, and renders as such.
///
/// Pooling is in quadrature, treating the two dispersions as independent. That
/// is an approximation — successive runs on one hosted runner are not fully
/// independent — and it errs toward a *smaller* pooled figure than simple
/// addition would give, so `k` is what absorbs the difference. Calibration
/// chooses `k` against real data rather than deriving it, which is why the
/// approximation is acceptable here.
pub fn flags_movement(current: Summary, window: Summary, k: f64) -> bool {
    let difference = current.median.abs_diff(window.median) as f64;
    let pooled = pooled_uncertainty(current, window);
    // A zero pooled uncertainty means both sets were perfectly consistent. Any
    // real difference is then movement; an identical median is not.
    if pooled == 0.0 {
        return difference > 0.0;
    }
    difference > k * pooled
}

/// The pooled uncertainty of two summaries, combined in quadrature.
pub fn pooled_uncertainty(current: Summary, window: Summary) -> f64 {
    let a = current.mad as f64;
    let b = window.mad as f64;
    (a * a + b * b).sqrt()
}

/// The ratio of an observation to its baseline, as used by the headline index.
///
/// `None` when the baseline is zero, which is not a ratio and must not become
/// one.
pub fn ratio(observed: u64, baseline: u64) -> Option<f64> {
    (baseline != 0).then(|| observed as f64 / baseline as f64)
}

/// The geometric mean of per-workload ratios: the headline index.
///
/// Equal weighting in log space, so corpus mass cannot weight the aggregate —
/// which is the defect that made the previous system's headline move for
/// reasons unrelated to the compiler. The cost is attenuation: a 10% regression
/// in one of *n* workloads moves this by roughly 10%/*n*, which is why the index
/// answers "is anything moving" and the per-workload views answer "how much".
///
/// `None` for an empty set or any non-positive ratio, since the logarithm of a
/// non-positive number is not a number this can average.
pub fn geometric_mean(ratios: &[f64]) -> Option<f64> {
    if ratios.is_empty() || ratios.iter().any(|ratio| *ratio <= 0.0) {
        return None;
    }
    let sum: f64 = ratios.iter().map(|ratio| ratio.ln()).sum();
    Some((sum / ratios.len() as f64).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_handle_both_parities() {
        assert_eq!(median(&[3, 1, 2]), Some(2));
        assert_eq!(median(&[4, 1, 3, 2]), Some(2));
        assert_eq!(median(&[7]), Some(7));
    }

    #[test]
    fn there_is_no_median_of_nothing() {
        // Returning zero would silently poison every ratio computed from it.
        assert_eq!(median(&[]), None);
        assert_eq!(median_absolute_deviation(&[]), None);
        assert_eq!(Summary::of(&[]), None);
    }

    #[test]
    fn averaging_two_large_medians_does_not_overflow() {
        assert_eq!(median(&[u64::MAX, u64::MAX]), Some(u64::MAX));
    }

    #[test]
    fn the_median_resists_a_single_wild_sample() {
        // The case this whole choice exists for: one descheduled sample on a
        // shared runner must not move the published figure.
        let clean = [100, 101, 99, 100, 102];
        let with_outlier = [100, 101, 99, 100, 100_000];
        assert_eq!(median(&clean), Some(100));
        assert_eq!(median(&with_outlier), Some(100));
    }

    #[test]
    fn dispersion_measures_spread_not_magnitude() {
        let tight = Summary::of(&[1000, 1001, 999, 1000]).unwrap();
        let loose = Summary::of(&[1000, 1200, 800, 1000]).unwrap();
        assert!(tight.mad < loose.mad);
        assert!(tight.relative_mad() < loose.relative_mad());
    }

    #[test]
    fn relative_dispersion_is_comparable_across_scales() {
        // Two workloads a thousandfold apart in cost, equally noisy in
        // proportion, must report the same relative figure.
        let small = Summary::of(&[100, 110, 90, 100]).unwrap();
        let large = Summary::of(&[100_000, 110_000, 90_000, 100_000]).unwrap();
        assert!((small.relative_mad() - large.relative_mad()).abs() < 1e-12);
    }

    #[test]
    fn a_zero_median_reports_no_relative_dispersion_rather_than_dividing() {
        let zeroed = Summary::of(&[0, 0, 0]).unwrap();
        assert_eq!(zeroed.relative_mad(), 0.0);
    }

    #[test]
    fn movement_inside_the_uncertainty_bound_is_not_flagged() {
        let current = Summary {
            median: 1050,
            mad: 40,
            count: 9,
        };
        let window = Summary {
            median: 1000,
            mad: 40,
            count: 9,
        };
        // Difference 50 against pooled ~56.6; at k=3 the bound is ~170.
        assert!(!flags_movement(current, window, 3.0));
    }

    #[test]
    fn movement_beyond_the_bound_is_flagged() {
        let current = Summary {
            median: 1400,
            mad: 40,
            count: 9,
        };
        let window = Summary {
            median: 1000,
            mad: 40,
            count: 9,
        };
        assert!(flags_movement(current, window, 3.0));
    }

    #[test]
    fn a_larger_multiplier_flags_strictly_less() {
        let current = Summary {
            median: 1200,
            mad: 40,
            count: 9,
        };
        let window = Summary {
            median: 1000,
            mad: 40,
            count: 9,
        };
        assert!(flags_movement(current, window, 3.0));
        assert!(!flags_movement(current, window, 10.0));
    }

    #[test]
    fn noisier_data_raises_the_bar_for_the_same_difference() {
        let window = Summary {
            median: 1000,
            mad: 40,
            count: 9,
        };
        let noisy_window = Summary {
            median: 1000,
            mad: 400,
            count: 9,
        };
        let current = Summary {
            median: 1200,
            mad: 40,
            count: 9,
        };
        assert!(flags_movement(current, window, 3.0));
        assert!(!flags_movement(current, noisy_window, 3.0));
    }

    #[test]
    fn perfect_consistency_flags_any_real_difference_but_not_equality() {
        let current = Summary {
            median: 1001,
            mad: 0,
            count: 9,
        };
        let same = Summary {
            median: 1000,
            mad: 0,
            count: 9,
        };
        assert!(flags_movement(current, same, 3.0));
        assert!(!flags_movement(same, same, 3.0));
    }

    #[test]
    fn flagging_is_symmetric_in_direction() {
        // An improvement of the same size as a regression is equally movement;
        // the rule detects change, and direction is presentation.
        let window = Summary {
            median: 1000,
            mad: 40,
            count: 9,
        };
        let faster = Summary {
            median: 600,
            mad: 40,
            count: 9,
        };
        let slower = Summary {
            median: 1400,
            mad: 40,
            count: 9,
        };
        assert!(flags_movement(faster, window, 3.0));
        assert!(flags_movement(slower, window, 3.0));
    }

    #[test]
    fn the_index_is_one_at_the_baseline() {
        assert_eq!(geometric_mean(&[1.0, 1.0, 1.0]), Some(1.0));
    }

    #[test]
    fn the_index_weights_workloads_equally_regardless_of_cost() {
        // The defect that motivated the reset: a corpus reweighting must not
        // move the headline. Two workloads, one a thousandfold more expensive,
        // each regressed 10% — the index moves 10%, not somewhere between.
        let index = geometric_mean(&[1.1, 1.1]).unwrap();
        assert!((index - 1.1).abs() < 1e-12);
    }

    #[test]
    fn the_index_attenuates_a_single_workload_regression() {
        // Documents the acknowledged cost: a 10% regression in one of four
        // workloads moves the index by roughly 10%/4.
        let index = geometric_mean(&[1.1, 1.0, 1.0, 1.0]).unwrap();
        assert!(index > 1.02 && index < 1.03, "{index}");
    }

    #[test]
    fn the_index_is_symmetric_in_log_space() {
        // A halving and a doubling cancel, which an arithmetic mean would not
        // manage — it would report 1.25.
        let index = geometric_mean(&[0.5, 2.0]).unwrap();
        assert!((index - 1.0).abs() < 1e-12, "{index}");
    }

    #[test]
    fn a_non_positive_ratio_has_no_index() {
        assert_eq!(geometric_mean(&[1.0, 0.0]), None);
        assert_eq!(geometric_mean(&[]), None);
    }

    #[test]
    fn a_zero_baseline_yields_no_ratio() {
        assert_eq!(ratio(100, 0), None);
        assert_eq!(ratio(100, 50), Some(2.0));
    }

    #[test]
    fn batched_latency_is_reported_per_compilation() {
        use crate::run::{Phase, PhaseAccounting};
        use std::collections::BTreeMap;

        let phase_ns: BTreeMap<Phase, u64> =
            Phase::ALL.into_iter().map(|phase| (phase, 0)).collect();
        let sample = Sample {
            batch_size: 40,
            process_elapsed_ns: 1,
            peak_memory_bytes: 4096,
            output_binary_bytes: 512,
            phases: PhaseAccounting {
                phase_ns,
                mixed_parallel_ns: 0,
                unattributed_ns: 0,
                compiler_root_ns: 40_000,
            },
            boundary_evidence: Vec::new(),
        };
        // Latency divides by the batch; the other metrics are per-compilation
        // properties already and must not be.
        assert_eq!(sample_value(&sample, Metric::Latency), 1_000);
        assert_eq!(sample_value(&sample, Metric::PeakMemory), 4096);
        assert_eq!(sample_value(&sample, Metric::BinarySize), 512);
    }
}
